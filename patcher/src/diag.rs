// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Diagnostics for the co-op "black screen" softlock: print, never change.
//
// Every mode switch (leaving a town or the tavern, entering a place) goes
// through one host barrier in st.Controller: syncLeaveMode / syncEnterMode
// fade the host to black, `waitForClients` puts every connected client into
// `waitLocks`, each client fades too (once it is not locked: `waitAlive`) and
// answers `onClientReady`, the host switches the mode and releases the clients
// with `onServerReady`, everyone fades back in. Nothing times out: one missing
// answer leaves every screen black. Where it stops is invisible today: the
// game's own error reports (a dropped fade, a refused switch, a client refusing
// a mode it is not in) go only to Shiro's online store.
//
// The pass adds `Sys.println` calls and nothing else:
//   - shiro.online.Log.logError: the message (`Std.string` of it), once it is
//     past the game's own online error cap and its repeat check (`lastERROR`),
//     so a spamming error prints as rarely as the game reports it; then
//     `mp: error stack: <line> | <line> ...`, the stack logError already
//     builds for its online report (where an uncaught error was thrown);
//   - the barrier, one line per step:
//       syncLeaveMode / syncEnterMode   lockSync, waitLocks.length
//       waitForClients                  waitLocks.length, game.host.clients.length
//       onClientReady (host, after the removal)   waitLocks / callbacks left
//       onServerReady (client, after the removal) waitLocks / callbacks left
//       doLeaveMode / doEnterMode (client)  what waitAlive waits on: game.lockAlives,
//                                       game.fading, fadeParams / onBreak set,
//                                       waitGameAlive.length
//       leave host faded / all clients ready / client alive / client faded
//       enter client alive / enter client faded (a client that never answers an
//       enter shows where it stopped: parked in waitAlive, or its fade never ended)
//     Every read is guarded against null (prints -1 / false).
// `Sys.println` ends in the libhl native `hl_sys_print`, which the winmm shim
// copies into shim.log as "game:" lines, timestamped (buffered: the game thread
// never writes the file). A String reaches println's Dyn argument as is, the
// way the compiler passes it; only I32/Bool values are boxed (ToDyn).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::job_xp::str_global;
use super::*;
use hlbc::types::ValBool;

struct Fns {
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
}

struct Types {
    str_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
}

/// What a part prints, read from the object at the end of its path.
enum Leaf {
    /// An I32 or Bool field (its type).
    Field(RefField, RefType),
    /// Whether the object itself is set (Bool).
    Set,
    /// Whether a field of the object is set (Bool); the field's type.
    FieldSet(RefField, RefType),
}

/// `label` followed by a value read from `this.<path...>.<leaf>`.
struct Part {
    label: &'static str,
    path: Vec<(RefField, RefType)>,
    leaf: Leaf,
}

struct Probe {
    fi: usize,
    at: usize,
    tag: &'static str,
    parts: Vec<Part>,
}

struct Plan {
    log_error: usize,
    /// Where logError prints, and the register holding its String message.
    log_at: usize,
    log_msg: Reg,
    /// logError's call stack lines (`Array<String>`, later its log entry's
    /// `stack`), complete at `log_at`; joined by `ArrayObj.join`.
    log_stack: Reg,
    arr_join: RefFun,
    fns: Fns,
    types: Types,
    probes: Vec<Probe>,
}

pub(crate) fn static_fn<'a>(code: &'a Bytecode, class: &str, name: &str) -> Result<&'a Function> {
    let mut hits = code.functions.iter().filter(|f| {
        s(code, f.name) == name
            && f.parent.is_some_and(
                |p| matches!(&code.types[p.0], Type::Obj(o) if s(code, o.name) == class),
            )
    });
    let f = hits
        .next()
        .with_context(|| format!("{class}.{name} not found"))?;
    if hits.next().is_some() {
        bail!("{class}.{name} is ambiguous");
    }
    Ok(f)
}

/// Name of field `field` read through a register of type `t` (object or virtual).
fn field_name_of(code: &Bytecode, t: RefType, field: RefField) -> Option<&str> {
    let fields = match &code.types[t.0] {
        Type::Virtual { fields } => fields,
        other => &other.get_type_obj()?.fields,
    };
    fields.get(field.0).map(|f| s(code, f.name))
}

/// Callee and argument registers of a direct call.
pub(crate) fn call_of(op: &Opcode) -> Option<(RefFun, Vec<Reg>)> {
    Some(match op {
        Opcode::Call0 { fun, .. } => (*fun, vec![]),
        Opcode::Call1 { fun, arg0, .. } => (*fun, vec![*arg0]),
        Opcode::Call2 {
            fun, arg0, arg1, ..
        } => (*fun, vec![*arg0, *arg1]),
        Opcode::Call3 {
            fun,
            arg0,
            arg1,
            arg2,
            ..
        } => (*fun, vec![*arg0, *arg1, *arg2]),
        Opcode::Call4 {
            fun,
            arg0,
            arg1,
            arg2,
            arg3,
            ..
        } => (*fun, vec![*arg0, *arg1, *arg2, *arg3]),
        Opcode::CallN { fun, args, .. } => (*fun, args.clone()),
        _ => return None,
    })
}

pub(crate) fn calls(f: &Function, fun: RefFun) -> bool {
    f.ops
        .iter()
        .any(|op| call_of(op).is_some_and(|(g, _)| g == fun))
}

/// The closure function that `f` (at `fi`) passes to its only call of `callee`.
pub(crate) fn closure_passed_to(code: &Bytecode, fi: usize, callee: RefFun) -> Result<usize> {
    let f = &code.functions[fi];
    let mut sites = f.ops.iter().enumerate().filter_map(|(j, op)| {
        call_of(op)
            .filter(|(g, _)| *g == callee)
            .map(|(_, a)| (j, a))
    });
    let (j, args) = sites
        .next()
        .with_context(|| format!("fn@{} never calls fn@{}", f.findex.0, callee.0))?;
    if sites.next().is_some() {
        bail!("fn@{} calls fn@{} more than once", f.findex.0, callee.0);
    }
    let c = f.ops[..j]
        .iter()
        .rev()
        .find_map(|op| match op {
            Opcode::InstanceClosure { dst, fun, .. } if args.contains(dst) => Some(*fun),
            _ => None,
        })
        .with_context(|| format!("fn@{}: no closure passed to fn@{}", f.findex.0, callee.0))?;
    fun_index(code, c)
}

/// Index of the op right after the first `ArrayObj.remove(this.waitLocks, ..)`.
fn after_wait_locks_remove(f: &Function, wait_locks: RefField, remove: RefFun) -> Result<usize> {
    for (j, op) in f.ops.iter().enumerate() {
        let Opcode::Call2 { fun, arg0, .. } = op else {
            continue;
        };
        if *fun != remove {
            continue;
        }
        let loaded = f.ops[j.saturating_sub(4)..j].iter().any(
            |o| matches!(o, Opcode::GetThis { dst, field } if dst == arg0 && *field == wait_locks),
        );
        if loaded {
            return Ok(j + 1);
        }
    }
    bail!("fn@{}: no waitLocks.remove", f.findex.0)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let log_error_f = static_fn(code, "shiro.online.$Log", "logError")?;
    let log_error = fun_index(code, log_error_f.findex)?;
    let println = static_fn(code, "$Sys", "println")?;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    let str_t = obj_type(code, "String")?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    if fun_args(code, log_error_f).get(2) != Some(&dyn_t) {
        bail!("logError: message argument is not Dyn");
    }
    let lops = &log_error_f.ops;
    if lops
        .iter()
        .any(|op| matches!(op, Opcode::Call1 { fun, .. } if *fun == println.findex))
    {
        bail!("already applied");
    }
    // logError returns early past the online error cap (`errorCount >=
    // maxErrorCount`) and for a repeat of the last error (`lastERROR == msg +
    // first stack line`), then stores `lastERROR`. The message is printed right
    // after that store, so the game's own cap and dedup bound the output.
    // `msg` is `Std.string(message)` (the driver-error case swaps in its own text).
    let (msg, msg_t) = lops
        .iter()
        .take(32)
        .find_map(|op| match op {
            Opcode::Call1 { dst, fun, arg0 } if *fun == std_string && *arg0 == Reg(2) => {
                Some((*dst, log_error_f.regs[dst.0 as usize]))
            }
            _ => None,
        })
        .context("logError: does not stringify its message")?;
    if msg_t != str_t {
        bail!("logError: the stringified message is not a String");
    }
    let field_named = |op: &Opcode, name: &str, set: bool| match op {
        Opcode::Field { obj, field, .. } if !set => {
            field_name_of(code, log_error_f.regs[obj.0 as usize], *field) == Some(name)
        }
        Opcode::SetField { obj, field, .. } if set => {
            field_name_of(code, log_error_f.regs[obj.0 as usize], *field) == Some(name)
        }
        _ => false,
    };
    let stores: Vec<usize> = (0..lops.len())
        .filter(|&i| field_named(&lops[i], "lastERROR", true))
        .collect();
    let [store] = stores[..] else {
        bail!(
            "logError: expected one `lastERROR =` store, found {}",
            stores.len()
        );
    };
    let cap = lops[..store]
        .iter()
        .position(|op| field_named(op, "maxErrorCount", false))
        .context("logError: no maxErrorCount check before the dedup")?;
    if !(0..store).any(|i| field_named(&lops[i], "lastERROR", false))
        || !lops[cap..store]
            .iter()
            .any(|op| matches!(op, Opcode::Ret { .. }))
    {
        bail!("logError: no early return between the cap and the lastERROR store");
    }
    let log_at = store + 1;
    if (0..lops.len()).any(|i| jump_targets(log_error_f, i).contains(&log_at)) {
        bail!("logError: the op after the lastERROR store is a jump target");
    }
    // The stack lines: the Array<String> stored as the entry's `stack` after
    // the dedup, already filled (no write to it between the store and there).
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let (set_at, log_stack) = (store..lops.len())
        .find_map(|i| match lops[i] {
            Opcode::SetField { src, .. } if field_named(&lops[i], "stack", true) => Some((i, src)),
            _ => None,
        })
        .context("logError: no `stack` store after the dedup")?;
    if log_error_f.regs[log_stack.0 as usize] != arr_t {
        bail!("logError: the stack is not an ArrayObj");
    }
    let wrote = format!("dst: Reg({})", log_stack.0);
    if lops[store..set_at]
        .iter()
        .any(|op| format!("{op:?}").contains(&wrote))
        || !lops[..store]
            .iter()
            .any(|op| format!("{op:?}").contains(&wrote))
    {
        bail!("logError: the stack is not complete at the lastERROR store");
    }
    let arr_join = proto(code, arr_t, "join")?;
    if fun_args(code, &code.functions[fun_index(code, arr_join)?]) != [arr_t, str_t] {
        bail!("unexpected ArrayObj.join signature");
    }

    let ctrl_t = obj_type(code, "st.Controller")?;
    let game_t = obj_type(code, "Game")?;
    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let (wait_locks, arr_t) = field(code, ctrl_t, "waitLocks")?;
    let (callbs, callbs_t) = field(code, ctrl_t, "waitLockCallbs")?;
    let (alive_q, alive_t) = field(code, ctrl_t, "waitGameAlive")?;
    if callbs_t != arr_t || alive_t != arr_t {
        bail!("controller wait lists are not ArrayObj");
    }
    let len = typed(arr_t, "length", i32_t)?;
    let lock_sync = typed(ctrl_t, "lockSyncMode", bool_t)?;
    let (game_f, game_ft) = field(code, ctrl_t, "game")?;
    if game_ft != game_t {
        bail!("controller.game is not Game");
    }
    let (host_f, host_t) = field(code, game_t, "host")?;
    let (clients_f, clients_t) = field(code, host_t, "clients")?;
    let clients_len = typed(clients_t, "length", i32_t)?;
    let lock_alives = typed(game_t, "lockAlives", bool_t)?;
    let fading = typed(game_t, "fading", bool_t)?;
    let (fade_params, fp_t) = field(code, game_t, "fadeParams")?;
    let (on_break, on_break_t) = field_of_virtual(code, fp_t, "onBreak")?;

    let arr_len = |label: &'static str, list: RefField| Part {
        label,
        path: vec![(list, arr_t)],
        leaf: Leaf::Field(len, i32_t),
    };
    let game_bool = |label: &'static str, f: RefField| Part {
        label,
        path: vec![(game_f, game_t)],
        leaf: Leaf::Field(f, bool_t),
    };
    let sync_parts = || {
        vec![
            Part {
                label: " lockSync=",
                path: vec![],
                leaf: Leaf::Field(lock_sync, bool_t),
            },
            arr_len(" waitLocks=", wait_locks),
        ]
    };
    let ready_parts = || {
        vec![
            arr_len(" left=", wait_locks),
            arr_len(" callbacks=", callbs),
        ]
    };

    let m = |name: &str| -> Result<usize> { fun_index(code, method(code, ctrl_t, name)?.findex) };
    let remove = method(code, arr_t, "remove")?.findex;
    let sync_leave = m("syncLeaveMode")?;
    let sync_enter = m("syncEnterMode")?;
    let wait_clients = m("waitForClients")?;
    let client_ready = m("onClientReady__impl")?;
    let server_ready = m("onServerReady__impl")?;
    let do_leave = m("doLeaveMode__impl")?;
    let wait_unlock = method(code, ctrl_t, "waitForUnlock")?.findex;
    let leave_mode = method(code, game_t, "leaveMode")?.findex;
    let on_client_ready = method(code, ctrl_t, "onClientReady")?.findex;

    // The leave sequence: syncLeaveMode -> host faded (waitForUnlock) -> all
    // ready (Game.leaveMode); doLeaveMode -> client alive -> client faded
    // (onClientReady).
    let fade_in = method(code, game_t, "fadeIn")?.findex;
    let wait_alive = method(code, ctrl_t, "waitAlive")?.findex;
    let host_faded = closure_passed_to(code, sync_leave, fade_in)?;
    let all_ready = closure_passed_to(code, host_faded, wait_unlock)?;
    let client_alive = closure_passed_to(code, do_leave, wait_alive)?;
    let client_faded = closure_passed_to(code, client_alive, fade_in)?;
    let f = |i: usize| &code.functions[i];
    if !calls(f(all_ready), leave_mode)
        || !calls(f(all_ready), wait_unlock)
        || !calls(f(client_faded), on_client_ready)
    {
        bail!("leave sequence closures do not match");
    }
    // The client side of an enter: doEnterMode -> client alive (waitAlive) ->
    // client faded (onClientReady).
    let do_enter = m("doEnterMode__impl")?;
    let enter_alive = closure_passed_to(code, do_enter, wait_alive)?;
    let enter_faded = closure_passed_to(code, enter_alive, fade_in)?;
    if !calls(f(enter_faded), on_client_ready) {
        bail!("enter sequence closures do not match");
    }
    let alive_parts = || {
        vec![
            game_bool(" lockAlives=", lock_alives),
            game_bool(" fading=", fading),
            Part {
                label: " fadeParams=",
                path: vec![(game_f, game_t), (fade_params, fp_t)],
                leaf: Leaf::Set,
            },
            Part {
                label: " onBreak=",
                path: vec![(game_f, game_t), (fade_params, fp_t)],
                leaf: Leaf::FieldSet(on_break, on_break_t),
            },
            arr_len(" pending=", alive_q),
        ]
    };

    let tag_only = |fi: usize, tag: &'static str| Probe {
        fi,
        at: 0,
        tag,
        parts: vec![],
    };
    let probes = vec![
        Probe {
            fi: sync_leave,
            at: 0,
            tag: "mp: syncLeaveMode",
            parts: sync_parts(),
        },
        Probe {
            fi: sync_enter,
            at: 0,
            tag: "mp: syncEnterMode",
            parts: sync_parts(),
        },
        Probe {
            fi: wait_clients,
            at: 0,
            tag: "mp: waitForClients",
            parts: vec![
                arr_len(" waitLocks=", wait_locks),
                Part {
                    label: " clients=",
                    path: vec![(game_f, game_t), (host_f, host_t), (clients_f, clients_t)],
                    leaf: Leaf::Field(clients_len, i32_t),
                },
            ],
        },
        Probe {
            fi: client_ready,
            at: after_wait_locks_remove(f(client_ready), wait_locks, remove)?,
            tag: "mp: onClientReady",
            parts: ready_parts(),
        },
        Probe {
            fi: server_ready,
            at: after_wait_locks_remove(f(server_ready), wait_locks, remove)?,
            tag: "mp: onServerReady",
            parts: ready_parts(),
        },
        Probe {
            fi: do_leave,
            at: 0,
            tag: "mp: doLeaveMode",
            parts: alive_parts(),
        },
        Probe {
            fi: do_enter,
            at: 0,
            tag: "mp: doEnterMode",
            parts: alive_parts(),
        },
        tag_only(host_faded, "mp: leave host faded"),
        tag_only(all_ready, "mp: leave all clients ready"),
        tag_only(client_alive, "mp: leave client alive"),
        tag_only(client_faded, "mp: leave client faded"),
        tag_only(enter_alive, "mp: enter client alive"),
        tag_only(enter_faded, "mp: enter client faded"),
    ];
    for p in &probes {
        let g = f(p.fi);
        if (0..g.ops.len()).any(|i| jump_targets(g, i).contains(&p.at)) {
            bail!(
                "fn@{}: probe position {} is a jump target",
                g.findex.0,
                p.at
            );
        }
    }
    Ok(Plan {
        log_error,
        log_at,
        log_msg: msg,
        log_stack,
        arr_join,
        fns: Fns {
            println: println.findex,
            std_string,
            str_add,
        },
        types: Types {
            str_t,
            dyn_t,
            void_t,
            i32_t,
            bool_t,
        },
        probes,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let Plan {
        log_error,
        log_at,
        log_msg,
        log_stack,
        arr_join,
        fns,
        types,
        probes,
    } = p;

    // logError: Sys.println(msg) once past the error cap and the repeat check,
    // then println("mp: error stack: " + stack.join(" | ")): where it threw.
    let (pre, sep) = (
        str_global(code, types.str_t, "mp: error stack: "),
        str_global(code, types.str_t, " | "),
    );
    let f = &mut code.functions[log_error];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (v, s1, s2) = (reg(types.void_t), reg(types.str_t), reg(types.str_t));
    insert_ops(
        f,
        log_at,
        vec![
            Opcode::Call1 {
                dst: v,
                fun: fns.println,
                arg0: log_msg,
            },
            Opcode::GetGlobal {
                dst: s1,
                global: sep,
            },
            Opcode::Call2 {
                dst: s2,
                fun: arr_join,
                arg0: log_stack,
                arg1: s1,
            },
            Opcode::GetGlobal {
                dst: s1,
                global: pre,
            },
            Opcode::Call2 {
                dst: s1,
                fun: fns.str_add,
                arg0: s1,
                arg1: s2,
            },
            Opcode::Call1 {
                dst: v,
                fun: fns.println,
                arg0: s1,
            },
        ],
    );

    let minus_one = int_const(code, -1);
    for pr in probes {
        let tag_g = str_global(code, types.str_t, pr.tag);
        let label_g: Vec<_> = pr
            .parts
            .iter()
            .map(|part| str_global(code, types.str_t, part.label))
            .collect();
        let f = &mut code.functions[pr.fi];
        let mut reg = |t: RefType| {
            f.regs.push(t);
            Reg((f.regs.len() - 1) as u32)
        };
        let acc = reg(types.str_t);
        let d = reg(types.dyn_t);
        let v = reg(types.void_t);
        let mut ops = vec![Opcode::GetGlobal {
            dst: acc,
            global: tag_g,
        }];
        let mut jumps = Vec::new();
        for (part, g) in pr.parts.iter().zip(label_g) {
            let out_t = match part.leaf {
                Leaf::Field(_, t) => t,
                Leaf::Set | Leaf::FieldSet(..) => types.bool_t,
            };
            let out = reg(out_t);
            ops.push(if out_t == types.i32_t {
                Opcode::Int {
                    dst: out,
                    ptr: minus_one,
                }
            } else {
                Opcode::Bool {
                    dst: out,
                    value: ValBool(false),
                }
            });
            let mut skips = Vec::new();
            let mut cur = Reg(0);
            for (k, &(fld, t)) in part.path.iter().enumerate() {
                let r = reg(t);
                ops.push(if k == 0 {
                    Opcode::GetThis { dst: r, field: fld }
                } else {
                    Opcode::Field {
                        dst: r,
                        obj: cur,
                        field: fld,
                    }
                });
                skips.push(ops.len());
                ops.push(Opcode::JNull { reg: r, offset: 0 });
                cur = r;
            }
            match part.leaf {
                Leaf::Field(fld, _) => ops.push(Opcode::Field {
                    dst: out,
                    obj: cur,
                    field: fld,
                }),
                Leaf::Set => ops.push(Opcode::Bool {
                    dst: out,
                    value: ValBool(true),
                }),
                Leaf::FieldSet(fld, t) => {
                    let r = reg(t);
                    ops.push(Opcode::Field {
                        dst: r,
                        obj: cur,
                        field: fld,
                    });
                    skips.push(ops.len());
                    ops.push(Opcode::JNull { reg: r, offset: 0 });
                    ops.push(Opcode::Bool {
                        dst: out,
                        value: ValBool(true),
                    });
                }
            }
            let end = ops.len();
            jumps.extend(skips.into_iter().map(|i| (i, end)));
            let label = reg(types.str_t);
            let text = reg(types.str_t);
            ops.extend([
                Opcode::ToDyn { dst: d, src: out },
                Opcode::Call1 {
                    dst: text,
                    fun: fns.std_string,
                    arg0: d,
                },
                Opcode::GetGlobal {
                    dst: label,
                    global: g,
                },
                Opcode::Call2 {
                    dst: acc,
                    fun: fns.str_add,
                    arg0: acc,
                    arg1: label,
                },
                Opcode::Call2 {
                    dst: acc,
                    fun: fns.str_add,
                    arg0: acc,
                    arg1: text,
                },
            ]);
        }
        // A String goes to a Dyn argument as is (that is what the compiler
        // emits); ToDyn would box the pointer and print garbage.
        ops.push(Opcode::Call1 {
            dst: v,
            fun: fns.println,
            arg0: acc,
        });
        resolve_jumps(&mut ops, &jumps);
        insert_ops(f, pr.at, ops);
    }
    eprintln!("patched diagnostics: game errors and the co-op mode-switch barrier are printed");
}

/// Prints game errors and the co-op mode-switch barrier, or leaves `code` untouched and logs why.
pub(crate) fn patch_diag(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("diagnostics skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{read, HLBOOT};

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let println = p.fns.println;
        let mut at: Vec<(usize, usize)> = p.probes.iter().map(|pr| (pr.fi, pr.at)).collect();
        at.push((p.log_error, p.log_at));
        let mut code = read(&image);
        patch_diag(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let changed = format!("{:?}", a.ops) != format!("{:?}", b.ops) || a.regs != b.regs;
            let want = at.iter().any(|&(fi, _)| fi == i);
            assert_eq!(changed, want, "function #{i} (fn@{})", a.findex.0);
        }
        for &(fi, pos) in &at {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let n = b.ops.len() - a.ops.len();
            assert!(n >= 1, "fn@{}", a.findex.0);
            // Original regs kept, new ones appended.
            assert_eq!(&b.regs[..a.regs.len()], &a.regs[..]);
            // The probe ends in Sys.println and is inserted whole at `pos`.
            assert!(matches!(b.ops[pos + n - 1], Opcode::Call1 { fun, .. } if fun == println));
            assert_eq!(
                format!("{:?}", &b.ops[..pos]),
                format!("{:?}", &a.ops[..pos])
            );
            assert_eq!(
                format!("{:?}", &b.ops[pos + n..]),
                format!("{:?}", &a.ops[pos..])
            );
            // The probe writes only its own (new) registers.
            for op in &b.ops[pos..pos + n] {
                let w = match op {
                    Opcode::GetGlobal { dst, .. }
                    | Opcode::GetThis { dst, .. }
                    | Opcode::Field { dst, .. }
                    | Opcode::Int { dst, .. }
                    | Opcode::Bool { dst, .. }
                    | Opcode::ToDyn { dst, .. }
                    | Opcode::Call1 { dst, .. }
                    | Opcode::Call2 { dst, .. } => Some(*dst),
                    Opcode::JNull { .. } | Opcode::JFalse { .. } => None,
                    other => panic!("fn@{}: unexpected probe op {other:?}", a.findex.0),
                };
                if let Some(r) = w {
                    assert!(
                        r.0 as usize >= a.regs.len(),
                        "fn@{} writes r{}",
                        a.findex.0,
                        r.0
                    );
                }
            }
            // Every original jump keeps its target (shifted past the probe).
            let map = |t: usize| if t < pos { t } else { t + n };
            for i in 0..a.ops.len() {
                let want: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
                assert_eq!(
                    jump_targets(b, map(i)),
                    want,
                    "fn@{} jump at op {i}",
                    a.findex.0
                );
            }
            // The probe's own jumps go forward, at most to the op after it.
            for k in pos..pos + n {
                for t in jump_targets(b, k) {
                    assert!(
                        t > k && t <= pos + n,
                        "fn@{} probe jump {k}->{t}",
                        a.findex.0
                    );
                }
            }
        }

        // logError prints its String message right after the `lastERROR` store,
        // i.e. past the cap and the repeat check.
        let lb = &back.functions[p.log_error];
        assert!(
            matches!(lb.ops[p.log_at], Opcode::Call1 { fun, arg0, .. } if fun == println && arg0 == p.log_msg)
        );
        assert!(matches!(lb.ops[p.log_at - 1], Opcode::SetField { .. }));
        // ... and its call stack, joined, on the next line.
        assert!(matches!(lb.ops[p.log_at + 2],
            Opcode::Call2 { fun, arg0, .. } if fun == p.arr_join && arg0 == p.log_stack));
        assert!(matches!(lb.ops[p.log_at + 5], Opcode::Call1 { fun, .. } if fun == println));
        crate::asm::testutil::check_types(&back, lb, p.log_at..p.log_at + 6);
        // No String is boxed with ToDyn on its way to println.
        for &(fi, _) in &at {
            let b = &back.functions[fi];
            for op in &b.ops {
                if let Opcode::ToDyn { src, .. } = op {
                    assert_ne!(b.regs[src.0 as usize], p.types.str_t, "fn@{}", b.findex.0);
                }
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_diag(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
