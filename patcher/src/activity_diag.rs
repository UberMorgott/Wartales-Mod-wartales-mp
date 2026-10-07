// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Diagnostics for the co-op activity / mini-game freeze: print, never change.
//
// Report (2026-10-02): in the ruins a player played a mini-game; once it
// ended that player's UI stayed frozen until a reload. The client's shim.log
// holds nothing for that time (no mode switch, no error), and the host's log
// was not available. Activities (lock picking, fishing, gambling, the ruins
// puzzles NinePuzzle / CylinderPuzzle / BoardPuzzle) never switch the mode.
// Their end runs through `ui.win.Activity.onActionDone`: on the player's
// machine it asks the host for the result with an RPC that answers
// (`st.Unit.successActivity`, or `st.player.Progress.computeActivityLoot`
// without a unit), sets `game.endingActivity = true` and waits with
// `globalEvent.waitUntil` for that answer, then (optionally through a
// `ctrl.netFade`) `doResult` / `onClose` free the UI. An answer that never
// comes, or an RPC refused by `networkAllow` (hxbit's `NetworkHost.logError`,
// which throws), leaves the UI parked without a line in any log.
//
// The pass adds one `Sys.println` at the entry of each step, host and clients alike:
//   - hxbit.NetworkHost.logError: its message ("Calling the RPC x on a not
//     allowed object", ...), before it throws;
//   - Activity.start__impl / onActionDone / doResult__impl / _cancel__impl:
//     the activity id (inf.id), game.isAuth, whether a unit is set, and for
//     onActionDone whether the end goes through a camera fade;
//   - the onActionDone continuations: successActivity answered,
//     computeActivityLoot answered, and the final onReady that closes it;
//   - st.Unit.successActivity__impl / Progress.computeActivityLoot__impl (they
//     run on the host) with the activity id;
//   - every end step of the three ruins puzzle windows, with isGameFinished
//     and game.isAuth.
// The first missing line shows where the end stopped, and on which machine.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::diag::{calls, closure_passed_to, static_fn};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

type Path = Vec<(RefField, RefType)>;

/// What a part prints after its label.
enum Val {
    /// A Bool field at the end of a field path read from `this` (false when a link is null).
    Bool(Path, RefField),
    /// Whether the object at the end of a non-empty field path read from `this` is set.
    Set(Path),
    /// A String field of the object at the end of a non-empty path (skipped when null).
    Str(Path, RefField),
    /// A String argument register (skipped when null).
    Arg(Reg),
}

struct Part {
    label: &'static str,
    val: Val,
}

struct Probe {
    fi: usize,
    tag: &'static str,
    parts: Vec<Part>,
}

struct Plan {
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    str_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    probes: Vec<Probe>,
}

/// The only closure created inside function `fi` whose body calls `callee`.
fn closure_calling(code: &Bytecode, fi: usize, callee: RefFun) -> Result<usize> {
    let f = &code.functions[fi];
    let mut hits = vec![];
    for op in &f.ops {
        if let Opcode::InstanceClosure { fun, .. } = op {
            let ci = fun_index(code, *fun)?;
            if calls(&code.functions[ci], callee) && !hits.contains(&ci) {
                hits.push(ci);
            }
        }
    }
    let [ci] = hits[..] else {
        bail!(
            "fn@{}: expected one closure calling fn@{}, found {}",
            f.findex.0,
            callee.0,
            hits.len()
        );
    };
    Ok(ci)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let println_f = static_fn(code, "$Sys", "println")?;
    let println = println_f.findex;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    let str_t = obj_type(code, "String")?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    if fun_args(code, println_f) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let game_t = obj_type(code, "Game")?;
    let (is_auth, is_auth_t) = field(code, game_t, "isAuth")?;
    if is_auth_t != bool_t {
        bail!("Game.isAuth is not a Bool");
    }

    // hxbit.NetworkHost.logError(msg: String, objectId: Null<Int>).
    let nh_t = obj_type(code, "hxbit.NetworkHost")?;
    let net_err = method(code, nh_t, "logError")?;
    if fun_args(code, net_err).get(1) != Some(&str_t) {
        bail!("NetworkHost.logError: message is not a String");
    }
    if calls(net_err, println) {
        bail!("already applied");
    }
    let net_err = fun_index(code, net_err.findex)?;

    // ui.win.Activity and the continuations of its end.
    let act_t = obj_type(code, "ui.win.Activity")?;
    let (game_f, game_ft) = field(code, act_t, "game")?;
    if game_ft != game_t {
        bail!("Activity.game is not Game");
    }
    let (inf_f, inf_t) = field(code, act_t, "inf")?;
    let (id_f, id_t) = field_of_virtual(code, inf_t, "id")?;
    if id_t != str_t {
        bail!("Activity.inf.id is not a String");
    }
    let (unit_f, unit_t) = field(code, act_t, "unit")?;
    let (cam_f, cam_t) = field(code, act_t, "changeCamera")?;
    if cam_t != bool_t {
        bail!("Activity.changeCamera is not a Bool");
    }
    let act_m = |name: &str| -> Result<usize> { fun_index(code, proto(code, act_t, name)?) };
    let start = act_m("start__impl")?;
    let done = act_m("onActionDone")?;
    let result = act_m("doResult__impl")?;
    let cancel = act_m("_cancel__impl")?;
    let do_result = proto(code, act_t, "doResult")?;
    let unit_success = method(code, unit_t, "successActivity")?.findex;
    let progress_t = obj_type(code, "st.player.Progress")?;
    let loot = method(code, progress_t, "computeActivityLoot")?.findex;
    let wait = method(code, obj_type(code, "hxd.WaitEvent")?, "wait")?.findex;
    let xp_reply = closure_passed_to(code, done, unit_success)?;
    let loot_reply = closure_passed_to(code, done, loot)?;
    let after_wait = closure_passed_to(code, done, wait)?;
    let on_ready = closure_calling(code, after_wait, do_result)?;
    let host_success = fun_index(code, method(code, unit_t, "successActivity__impl")?.findex)?;
    let host_loot = fun_index(
        code,
        method(code, progress_t, "computeActivityLoot__impl")?.findex,
    )?;

    let id = || Part {
        label: " id=",
        val: Val::Str(vec![(inf_f, inf_t)], id_f),
    };
    let auth = || Part {
        label: " auth=",
        val: Val::Bool(vec![(game_f, game_t)], is_auth),
    };
    let unit = || Part {
        label: " unit=",
        val: Val::Set(vec![(unit_f, unit_t)]),
    };
    let arg1 = || {
        vec![Part {
            label: "",
            val: Val::Arg(Reg(1)),
        }]
    };
    let tag = |fi: usize, tag: &'static str| Probe {
        fi,
        tag,
        parts: vec![],
    };
    let mut probes = vec![
        Probe {
            fi: net_err,
            tag: "mp: net logError ",
            parts: arg1(),
        },
        Probe {
            fi: start,
            tag: "mp: activity start",
            parts: vec![id(), auth(), unit()],
        },
        Probe {
            fi: done,
            tag: "mp: activity done",
            parts: vec![
                id(),
                auth(),
                unit(),
                Part {
                    label: " camera=",
                    val: Val::Bool(vec![], cam_f),
                },
            ],
        },
        tag(xp_reply, "mp: activity successActivity answered"),
        tag(loot_reply, "mp: activity computeActivityLoot answered"),
        tag(on_ready, "mp: activity close"),
        Probe {
            fi: result,
            tag: "mp: activity doResult",
            parts: vec![id(), auth()],
        },
        Probe {
            fi: cancel,
            tag: "mp: activity cancel",
            parts: vec![id(), auth()],
        },
        Probe {
            fi: host_success,
            tag: "mp: host successActivity ",
            parts: arg1(),
        },
        Probe {
            fi: host_loot,
            tag: "mp: host computeActivityLoot ",
            parts: arg1(),
        },
    ];

    // The ruins puzzle windows: every end step.
    let puzzles: [(&str, [(&str, &'static str); 4]); 3] = [
        (
            "ui.win.NinePuzzle",
            [
                ("doSuccess", "mp: NinePuzzle doSuccess"),
                ("successActivity", "mp: NinePuzzle successActivity"),
                ("leaveActivity", "mp: NinePuzzle leaveActivity"),
                ("cancelActivity", "mp: NinePuzzle cancelActivity"),
            ],
        ),
        (
            "ui.win.CylinderPuzzle",
            [
                ("successActivity", "mp: CylinderPuzzle successActivity"),
                ("failActivity", "mp: CylinderPuzzle failActivity"),
                ("leaveActivity", "mp: CylinderPuzzle leaveActivity"),
                ("cancelActivity", "mp: CylinderPuzzle cancelActivity"),
            ],
        ),
        (
            "ui.win.BoardPuzzle",
            [
                (
                    "checkPuzzleSolved__impl",
                    "mp: BoardPuzzle checkPuzzleSolved",
                ),
                ("successActivity", "mp: BoardPuzzle successActivity"),
                ("leaveActivity__impl", "mp: BoardPuzzle leaveActivity"),
                ("getOut__impl", "mp: BoardPuzzle getOut"),
            ],
        ),
    ];
    for (class, steps) in puzzles {
        let t = obj_type(code, class)?;
        let (fin_f, fin_t) = field(code, t, "isGameFinished")?;
        let (wgame_f, wgame_t) = field(code, t, "game")?;
        if fin_t != bool_t || wgame_t != game_t {
            bail!("{class}: isGameFinished / game have unexpected types");
        }
        for (name, tag) in steps {
            probes.push(Probe {
                fi: fun_index(code, proto(code, t, name)?)?,
                tag,
                parts: vec![
                    Part {
                        label: " finished=",
                        val: Val::Bool(vec![], fin_f),
                    },
                    Part {
                        label: " auth=",
                        val: Val::Bool(vec![(wgame_f, wgame_t)], is_auth),
                    },
                ],
            });
        }
    }

    let mut seen = vec![];
    for p in &probes {
        let f = &code.functions[p.fi];
        if seen.contains(&p.fi) {
            bail!("fn@{} probed twice", f.findex.0);
        }
        seen.push(p.fi);
        for q in &p.parts {
            match &q.val {
                Val::Arg(r) => {
                    if f.regs.get(r.0 as usize) != Some(&str_t) {
                        bail!("fn@{}: r{} is not a String", f.findex.0, r.0);
                    }
                }
                Val::Set(path) | Val::Str(path, _) if path.is_empty() => {
                    bail!("fn@{}: empty path", f.findex.0)
                }
                _ => {
                    if code.types[f.regs[0].0].get_type_obj().is_none() {
                        bail!("fn@{}: `this` is not an object", f.findex.0);
                    }
                }
            }
        }
    }
    Ok(Plan {
        println,
        std_string,
        str_add,
        str_t,
        dyn_t,
        void_t,
        bool_t,
        probes,
    })
}

fn new_reg(regs: &mut Vec<RefType>, t: RefType) -> Reg {
    regs.push(t);
    Reg((regs.len() - 1) as u32)
}

/// Loads `path` from `this` into fresh registers; every link is null-checked
/// (its JNull index pushed to `skips`). Returns the last register (`this` when empty).
fn walk(
    regs: &mut Vec<RefType>,
    ops: &mut Vec<Opcode>,
    path: &Path,
    skips: &mut Vec<usize>,
) -> Reg {
    let mut cur = Reg(0);
    for (k, &(fld, t)) in path.iter().enumerate() {
        let r = new_reg(regs, t);
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
    cur
}

/// The probe's ops (fresh registers appended to `regs`), jumps resolved.
fn probe_ops(
    p: &Plan,
    regs: &mut Vec<RefType>,
    tag_g: RefGlobal,
    parts: &[(RefGlobal, &Val)],
) -> Vec<Opcode> {
    let acc = new_reg(regs, p.str_t);
    let d = new_reg(regs, p.dyn_t);
    let v = new_reg(regs, p.void_t);
    let mut ops = vec![Opcode::GetGlobal {
        dst: acc,
        global: tag_g,
    }];
    let mut jumps = vec![];
    for &(label_g, val) in parts {
        let label = new_reg(regs, p.str_t);
        ops.push(Opcode::GetGlobal {
            dst: label,
            global: label_g,
        });
        ops.push(Opcode::Call2 {
            dst: acc,
            fun: p.str_add,
            arg0: acc,
            arg1: label,
        });
        let mut skips = vec![];
        match val {
            Val::Bool(path, _) | Val::Set(path) => {
                let out = new_reg(regs, p.bool_t);
                ops.push(Opcode::Bool {
                    dst: out,
                    value: ValBool(false),
                });
                let cur = walk(regs, &mut ops, path, &mut skips);
                ops.push(match val {
                    Val::Bool(path, leaf) if path.is_empty() => Opcode::GetThis {
                        dst: out,
                        field: *leaf,
                    },
                    Val::Bool(_, leaf) => Opcode::Field {
                        dst: out,
                        obj: cur,
                        field: *leaf,
                    },
                    _ => Opcode::Bool {
                        dst: out,
                        value: ValBool(true),
                    },
                });
                // A null link prints false.
                let printed = ops.len();
                jumps.extend(skips.drain(..).map(|i| (i, printed)));
                let text = new_reg(regs, p.str_t);
                ops.extend([
                    Opcode::ToDyn { dst: d, src: out },
                    Opcode::Call1 {
                        dst: text,
                        fun: p.std_string,
                        arg0: d,
                    },
                    Opcode::Call2 {
                        dst: acc,
                        fun: p.str_add,
                        arg0: acc,
                        arg1: text,
                    },
                ]);
            }
            Val::Str(path, leaf) => {
                let cur = walk(regs, &mut ops, path, &mut skips);
                let s = new_reg(regs, p.str_t);
                ops.push(Opcode::Field {
                    dst: s,
                    obj: cur,
                    field: *leaf,
                });
                skips.push(ops.len());
                ops.push(Opcode::JNull { reg: s, offset: 0 });
                ops.push(Opcode::Call2 {
                    dst: acc,
                    fun: p.str_add,
                    arg0: acc,
                    arg1: s,
                });
            }
            Val::Arg(r) => {
                skips.push(ops.len());
                ops.push(Opcode::JNull { reg: *r, offset: 0 });
                ops.push(Opcode::Call2 {
                    dst: acc,
                    fun: p.str_add,
                    arg0: acc,
                    arg1: *r,
                });
            }
        }
        let end = ops.len();
        jumps.extend(skips.into_iter().map(|i| (i, end)));
    }
    // A String goes to println's Dyn argument as is (see diag.rs).
    ops.push(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: acc,
    });
    resolve_jumps(&mut ops, &jumps);
    ops
}

fn apply(code: &mut Bytecode, p: Plan) {
    for pr in &p.probes {
        let tag_g = str_global(code, p.str_t, pr.tag);
        let labels: Vec<RefGlobal> = pr
            .parts
            .iter()
            .map(|q| str_global(code, p.str_t, q.label))
            .collect();
        let parts: Vec<(RefGlobal, &Val)> = labels
            .into_iter()
            .zip(pr.parts.iter().map(|q| &q.val))
            .collect();
        let f = &mut code.functions[pr.fi];
        let ops = probe_ops(&p, &mut f.regs, tag_g, &parts);
        // At the entry: a jump back to op 0 (a loop head) lands after the probe.
        insert_ops(f, 0, ops);
    }
    eprintln!(
        "patched activity diagnostics: {} activity / mini-game / refused-RPC steps are printed",
        p.probes.len()
    );
}

/// Prints the steps of an activity's end (mini-games, ruins puzzles) and every
/// refused hxbit RPC, or leaves `code` untouched and logs why.
pub(crate) fn patch_activity_diag(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("activity diagnostics skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::{check_types, read, shifted, write, HLBOOT};
    use super::super::diag::call_of;
    use super::*;

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let println = p.println;
        let str_t = p.str_t;
        let probed: Vec<usize> = p.probes.iter().map(|pr| pr.fi).collect();
        assert_eq!(probed.len(), 22);
        let mut code = read(&image);
        patch_activity_diag(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let changed = format!("{:?}", a.ops) != format!("{:?}", b.ops) || a.regs != b.regs;
            assert_eq!(
                changed,
                probed.contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        for &fi in &probed {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let n = b.ops.len() - a.ops.len();
            // The probe sits whole at the entry and ends in Sys.println.
            shifted(a, b, 0, n);
            assert!(matches!(b.ops[n - 1], Opcode::Call1 { fun, .. } if fun == println));
            check_types(&back, b, 0..n);
            for k in 0..n {
                for t in jump_targets(b, k) {
                    assert!(t > k && t < n, "fn@{} probe jump {k}->{t}", a.findex.0);
                }
                let w = match &b.ops[k] {
                    Opcode::GetGlobal { dst, .. }
                    | Opcode::GetThis { dst, .. }
                    | Opcode::Field { dst, .. }
                    | Opcode::Bool { dst, .. }
                    | Opcode::ToDyn { dst, .. }
                    | Opcode::Call1 { dst, .. }
                    | Opcode::Call2 { dst, .. } => Some(*dst),
                    Opcode::JNull { .. } => None,
                    other => panic!("fn@{}: unexpected probe op {other:?}", a.findex.0),
                };
                // The probe writes only its own (new) registers.
                if let Some(r) = w {
                    assert!(
                        r.0 as usize >= a.regs.len(),
                        "fn@{} writes r{}",
                        a.findex.0,
                        r.0
                    );
                }
                // No String is boxed with ToDyn on its way to println.
                if let Opcode::ToDyn { src, .. } = &b.ops[k] {
                    assert_ne!(b.regs[src.0 as usize], str_t, "fn@{}", a.findex.0);
                }
            }
        }
        // The refused-RPC probe prints logError's own message argument.
        let le = &back.functions[probed[0]];
        assert!(le.ops[..8]
            .iter()
            .any(|o| matches!(o, Opcode::Call2 { arg1, .. } if *arg1 == Reg(1))));

        // A second application is refused and changes nothing.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_activity_diag(&mut again);
        assert!(write(&again) == patched);
    }

    #[test]
    fn call_of_sees_direct_calls_only() {
        let op = Opcode::Call1 {
            dst: Reg(0),
            fun: RefFun(7),
            arg0: Reg(1),
        };
        assert_eq!(call_of(&op).map(|(f, a)| (f.0, a.len())), Some((7, 1)));
        assert!(call_of(&Opcode::Ret { ret: Reg(0) }).is_none());
    }
}
