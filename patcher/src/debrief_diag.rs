// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
// Bounded diagnostics for the remaining post-battle hover/click failure.
// No input, loot or window state is changed. The previous cure-list fix is
// installed, so rebuilding and tooltip hit-test churn must be distinguished.

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use hlbc::types::RefGlobal;

const MARKER: &str = "[mp debrief] rebuild";
// Once per second while a Debrief updates: rebuilds since the previous line
// (only printed when non-zero), independent of the per-line caps below.
const RATE: &str = "[mp debrief] rate";

// Where the logger finds the Debrief: the hooked function's `this`, an
// Element's window, or a ui.Window that may be a Debrief.
#[derive(Clone, Copy, PartialEq)]
enum Src {
    Debrief,
    Element,
    Window,
}

struct Site {
    fi: usize,
    at: usize,
    tag: &'static str,
    src: Src,
    element: bool,
    values: Vec<(&'static str, Reg)>,
}

fn native(code: &Bytecode, name: &str, ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .natives
        .iter()
        .filter(|n| {
            s(code, n.name) == name
                && n.t
                    .as_fun(code)
                    .is_some_and(|t| t.args.is_empty() && t.ret == ret)
        })
        .map(|n| n.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one native {name}, found {}", hits.len()),
    }
}

struct Plan {
    sites: Vec<Site>,
    debrief: RefType,
    element: RefType,
    window: RefType,
    string: RefType,
    dyn_: RefType,
    void: RefType,
    int: RefType,
    bool_: RefType,
    f64_: RefType,
    sys_time: RefFun,
    get_window: RefFun,
    check: RefFun,
    class: RefGlobal,
    println: RefFun,
    stringify: RefFun,
    add: RefFun,
    debug_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|s| s.as_str() == MARKER) {
        bail!("already applied");
    }
    let debrief = obj_type(code, "ui.win.Debrief")?;
    let element = obj_type(code, "ui.comp.Element")?;
    let window = obj_type(code, "ui.Window")?;
    let string = obj_type(code, "String")?;
    let prim = |pred: fn(&Type) -> bool| {
        code.types
            .iter()
            .position(pred)
            .map(RefType)
            .context("primitive missing")
    };
    let dyn_ = prim(|t| matches!(t, Type::Dyn))?;
    let void = prim(|t| matches!(t, Type::Void))?;
    let int = prim(|t| matches!(t, Type::I32))?;
    let bool_ = prim(|t| matches!(t, Type::Bool))?;
    let f64_ = prim(|t| matches!(t, Type::F64))?;
    let sys_time = native(code, "sys_time", f64_)?;
    let rebuild = method(code, debrief, "rebuild")?;
    let update = method(code, debrief, "update")?;
    let update_fi = fun_index(code, update.findex)?;
    // The rate site precedes update's mismatch sites: hooks are inserted in
    // reverse site order, so later (higher) offsets in update go in first.
    let mut sites = vec![
        Site {
            fi: fun_index(code, rebuild.findex)?,
            at: 0,
            tag: MARKER,
            src: Src::Debrief,
            element: false,
            values: vec![],
        },
        Site {
            fi: update_fi,
            at: 0,
            tag: RATE,
            src: Src::Debrief,
            element: false,
            values: vec![],
        },
    ];
    let repairs = field(code, debrief, "repairBtn")?.0;
    let cure = field(code, debrief, "cureBtn")?.0;
    let enable = field(code, element, "enable")?.0;
    let mut branch = None;
    for (at, op) in update.ops.iter().enumerate() {
        if let Opcode::GetThis { field, .. } = op {
            if *field == repairs {
                branch = Some("[mp debrief] repair-mismatch");
            }
            if *field == cure {
                branch = Some("[mp debrief] cure-mismatch");
            }
        }
        if !matches!(op, Opcode::Call1 { fun, arg0: Reg(0), .. } if *fun == rebuild.findex) {
            continue;
        }
        let Some(Opcode::JEq {
            a: desired,
            b: current,
            ..
        }) = update.ops.get(at.wrapping_sub(1))
        else {
            bail!("update rebuild is not guarded by the availability comparison");
        };
        if !matches!(update.ops.get(at - 2), Some(Opcode::Field { dst, field, .. }) if dst == current && *field == enable)
        {
            bail!("update comparison does not read button.enable");
        }
        let n = update.ops[..at]
            .iter()
            .rev()
            .find_map(|o| match o {
                Opcode::JSGte { b, .. } => Some(*b),
                _ => None,
            })
            .context("availability count missing")?;
        if update.regs[n.0 as usize] != int {
            bail!("availability count is not I32");
        }
        // Log the actual registers used by the branch, without re-running any
        // inventory/counting calls (which would change the evidence).
        sites.push(Site {
            fi: update_fi,
            at,
            tag: branch.context("unknown rebuild branch")?,
            src: Src::Debrief,
            element: false,
            values: vec![
                (" count=", n),
                (" desired=", *desired),
                (" current=", *current),
            ],
        });
        if branch == Some("[mp debrief] cure-mismatch") {
            let remedies = update.ops[..at]
                .iter()
                .rev()
                .find_map(|o| match o {
                    Opcode::JSLt { a, b, .. } if *b == n => Some(*a),
                    _ => None,
                })
                .context("cure availability remedy sum missing")?;
            sites
                .last_mut()
                .unwrap()
                .values
                .push((" remedies=", remedies));
        }
    }
    if sites.len() != 4 {
        bail!("expected two availability rebuilds");
    }
    let make = method(code, element, "makeInteractive")?;
    let interactive = obj_type(code, "h2d.Interactive")?;
    // Press and release (or release outside) show whether both halves of a
    // click reach the same element, and what is topmost at each moment.
    for (event, tag) in [
        ("onOver", "[mp debrief] over"),
        ("onOut", "[mp debrief] out"),
        ("onClick", "[mp debrief] click"),
        ("onPush", "[mp debrief] press"),
        ("onRelease", "[mp debrief] release"),
        ("onReleaseOutside", "[mp debrief] release-outside"),
    ] {
        let event_field = field(code, interactive, event)?.0;
        let mut matches_ = vec![];
        for pair in make.ops.windows(2) {
            if let [Opcode::InstanceClosure { dst, fun, .. }, Opcode::SetField { field, src, .. }] =
                pair
            {
                if *field == event_field && dst == src {
                    matches_.push(*fun);
                }
            }
        }
        let [fun] = matches_[..] else {
            bail!("expected one interactive {event} closure");
        };
        let fi = fun_index(code, fun)?;
        if code.functions[fi].regs.first() != Some(&element) {
            bail!("unexpected event context");
        }
        sites.push(Site {
            fi,
            at: 0,
            tag,
            src: Src::Element,
            element: true,
            values: vec![],
        });
    }
    for (class, method_, tag) in [
        (
            "ui.comp.Inventory",
            "rebuild",
            "[mp debrief] inventory-rebuild",
        ),
        ("ui.comp.Slot", "redraw", "[mp debrief] slot-redraw"),
    ] {
        let f = method(code, obj_type(code, class)?, method_)?;
        sites.push(Site {
            fi: fun_index(code, f.findex)?,
            at: 0,
            tag,
            src: Src::Element,
            element: true,
            values: vec![],
        });
    }
    // Rebuild trigger paths. The line printed just before a "rebuild" line
    // names its cause: a dirty window (Controller.netRebuildWindow ->
    // markForRebuild), a network rebuild (netRebuild / _netRebuild ->
    // doRebuild) or an availability mismatch (above). A rebuild with no
    // trigger line came from a direct call or a bound closure (e.g. a closed
    // UnitInfo's onClose, set by Debrief.showUnit).
    let w_update = method(code, window, "update")?;
    let dirty = field(code, window, "dirty")?.0;
    let set = w_update
        .ops
        .iter()
        .position(|o| matches!(o, Opcode::SetThis { field, .. } if *field == dirty))
        .context("Window.update: dirty reset missing")?;
    if !matches!(w_update.ops.get(set + 1), Some(Opcode::CallThis { .. })) {
        bail!("Window.update: dirty reset is not followed by the rebuild call");
    }
    let do_rebuild = method(code, window, "doRebuild")?;
    for (f, at, tag, src) in [
        (w_update, set + 1, "[mp debrief] trigger dirty", Src::Window),
        (do_rebuild, 0, "[mp debrief] trigger net", Src::Window),
    ] {
        let fi = fun_index(code, f.findex)?;
        if code.functions[fi].regs.first() != Some(&window) {
            bail!("unexpected trigger context");
        }
        sites.push(Site {
            fi,
            at,
            tag,
            src,
            element: false,
            values: vec![],
        });
    }
    let get_window = method(code, obj_type(code, "ui.comp.BaseElement")?, "getWindow")?.findex;
    let class = RefGlobal(
        obj(code, debrief)?
            .global
            .0
            .checked_sub(1)
            .context("Debrief class global missing")?,
    );
    let check = method(code, obj_type(code, "hl.BaseType")?, "check")?.findex;
    let println = diag::static_fn(code, "$Sys", "println")?.findex;
    let stringify = diag::static_fn(code, "$Std", "string")?.findex;
    let add = diag::static_fn(code, "$String", "__add__")?.findex;
    Ok(Plan {
        sites,
        debrief,
        element,
        window,
        string,
        dyn_,
        void,
        int,
        bool_,
        f64_,
        sys_time,
        get_window,
        check,
        class,
        println,
        stringify,
        add,
        debug_file: debug_file(code, "src/ui/win/Debrief.hx")?,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_value(
    a: &mut Asm,
    code: &Bytecode,
    regs: &Regs,
    p: &Plan,
    msg: Reg,
    tmp: Reg,
    boxed: Reg,
    result: Reg,
    value: Reg,
) {
    // Only plain values (Int, Float, Bool) are boxed. A String or other
    // object reaches the Dyn argument as is, the way the compiler passes it;
    // ToDyn would box the pointer itself and print garbage (see diag.rs).
    let arg = if asm::self_describing(&code.types[regs.0[value.0 as usize].0]) {
        value
    } else {
        a.op(Opcode::ToDyn {
            dst: boxed,
            src: value,
        });
        boxed
    };
    a.op(Opcode::Call1 {
        dst: tmp,
        fun: p.stringify,
        arg0: arg,
    });
    a.op(Opcode::Call2 {
        dst: result,
        fun: p.add,
        arg0: msg,
        arg1: tmp,
    });
    a.op(Opcode::Mov {
        dst: msg,
        src: result,
    });
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    // Independent caps preserve mismatch evidence after many normal hover
    // events. Each counter resets only for a new Debrief object, never rebuild.
    let last = RefGlobal(code.globals.len());
    code.globals.push(p.debrief);
    let hover_count = RefGlobal(code.globals.len());
    code.globals.push(p.int);
    let rebuild_count = RefGlobal(code.globals.len());
    code.globals.push(p.int);
    let render_count = RefGlobal(code.globals.len());
    code.globals.push(p.int);
    let rate_count = RefGlobal(code.globals.len());
    code.globals.push(p.int);
    let rate_lines = RefGlobal(code.globals.len());
    code.globals.push(p.int);
    let rate_start = RefGlobal(code.globals.len());
    code.globals.push(p.f64_);
    let mut hooks = vec![];
    for site in &p.sites {
        let source_t = match site.src {
            Src::Element => p.element,
            Src::Debrief => p.debrief,
            Src::Window => p.window,
        };
        let mut args = vec![source_t];
        args.extend(
            site.values
                .iter()
                .map(|(_, r)| code.functions[site.fi].regs[r.0 as usize]),
        );
        let mut r = Regs(args.clone());
        let v = r.r(p.void);
        let win = r.r(p.window);
        let current = r.r(p.debrief);
        let previous = r.r(p.debrief);
        let class = r.r(code.globals[p.class.0]);
        let ok = r.r(p.bool_);
        let count = r.r(p.int);
        let cap = r.r(p.int);
        let msg = r.r(p.string);
        let tmp = r.r(p.string);
        let result = r.r(p.string);
        let boxed = r.r(p.dyn_);
        let exc = r.r(p.dyn_);
        let now = r.r(p.f64_);
        let mut a = Asm::new();
        a.jmp(Opcode::Trap { exc, offset: 0 }, "caught");
        if site.src != Src::Debrief {
            a.jmp(
                Opcode::JNull {
                    reg: Reg(0),
                    offset: 0,
                },
                "done",
            );
            if site.src == Src::Element {
                a.op(Opcode::Call1 {
                    dst: win,
                    fun: p.get_window,
                    arg0: Reg(0),
                });
            } else {
                a.op(Opcode::Mov {
                    dst: win,
                    src: Reg(0),
                });
            }
            a.op(Opcode::GetGlobal {
                dst: class,
                global: p.class,
            });
            a.op(Opcode::Call2 {
                dst: ok,
                fun: p.check,
                arg0: class,
                arg1: win,
            });
            a.jmp(
                Opcode::JFalse {
                    cond: ok,
                    offset: 0,
                },
                "done",
            );
            a.op(Opcode::UnsafeCast {
                dst: current,
                src: win,
            });
        } else {
            a.op(Opcode::Mov {
                dst: current,
                src: Reg(0),
            });
        }
        a.op(Opcode::Call0 {
            dst: now,
            fun: p.sys_time,
        });
        a.op(Opcode::GetGlobal {
            dst: previous,
            global: last,
        });
        a.jmp(
            Opcode::JEq {
                a: current,
                b: previous,
                offset: 0,
            },
            "same",
        );
        a.op(Opcode::SetGlobal {
            global: last,
            src: current,
        });
        a.op(Opcode::Int {
            dst: count,
            ptr: int_const(code, 0),
        });
        a.op(Opcode::SetGlobal {
            global: hover_count,
            src: count,
        });
        a.op(Opcode::SetGlobal {
            global: rebuild_count,
            src: count,
        });
        a.op(Opcode::SetGlobal {
            global: render_count,
            src: count,
        });
        a.op(Opcode::SetGlobal {
            global: rate_count,
            src: count,
        });
        a.op(Opcode::SetGlobal {
            global: rate_lines,
            src: count,
        });
        a.op(Opcode::SetGlobal {
            global: rate_start,
            src: now,
        });
        a.label("same");
        let input_event = matches!(
            site.tag,
            "[mp debrief] over"
                | "[mp debrief] out"
                | "[mp debrief] click"
                | "[mp debrief] press"
                | "[mp debrief] release"
                | "[mp debrief] release-outside"
        );
        let grid_event = matches!(
            site.tag,
            "[mp debrief] inventory-rebuild" | "[mp debrief] slot-redraw"
        );
        let rate_n = r.r(p.int);
        if site.tag == MARKER {
            // Every rebuild counts toward the rate, even past the line cap.
            a.op(Opcode::GetGlobal {
                dst: rate_n,
                global: rate_count,
            });
            a.op(Opcode::Incr { dst: rate_n });
            a.op(Opcode::SetGlobal {
                global: rate_count,
                src: rate_n,
            });
        }
        if site.tag == RATE {
            let start = r.r(p.f64_);
            let one = r.r(p.f64_);
            a.op(Opcode::GetGlobal {
                dst: start,
                global: rate_start,
            });
            a.op(Opcode::Sub {
                dst: start,
                a: now,
                b: start,
            });
            a.op(Opcode::Float {
                dst: one,
                ptr: float_const(code, 1.0),
            });
            a.jmp(
                Opcode::JSLt {
                    a: start,
                    b: one,
                    offset: 0,
                },
                "done",
            );
            a.op(Opcode::SetGlobal {
                global: rate_start,
                src: now,
            });
            a.op(Opcode::GetGlobal {
                dst: rate_n,
                global: rate_count,
            });
            a.op(Opcode::Int {
                dst: count,
                ptr: int_const(code, 0),
            });
            a.op(Opcode::SetGlobal {
                global: rate_count,
                src: count,
            });
            a.jmp(
                Opcode::JSGte {
                    a: count,
                    b: rate_n,
                    offset: 0,
                },
                "done",
            );
        }
        let counter = if site.tag == RATE {
            rate_lines
        } else if input_event {
            hover_count
        } else if grid_event {
            render_count
        } else {
            rebuild_count
        };
        a.op(Opcode::GetGlobal {
            dst: count,
            global: counter,
        });
        a.op(Opcode::Int {
            dst: cap,
            ptr: int_const(
                code,
                if site.tag == RATE {
                    600
                } else if input_event {
                    160
                } else {
                    48
                },
            ),
        });
        a.jmp(
            Opcode::JSGte {
                a: count,
                b: cap,
                offset: 0,
            },
            "done",
        );
        a.op(Opcode::Incr { dst: count });
        a.op(Opcode::SetGlobal {
            global: counter,
            src: count,
        });
        a.op(Opcode::GetGlobal {
            dst: msg,
            global: job_xp::str_global(code, p.string, site.tag),
        });
        for (label, name) in [
            (" window=", "__uid"),
            (" repairTot=", "repairTot"),
            (" cureTot=", "cureTot"),
        ] {
            let value = r.r(p.int);
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, label),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            a.op(Opcode::Field {
                dst: value,
                obj: current,
                field: field(code, p.debrief, name)?.0,
            });
            append_value(&mut a, code, &r, p, msg, tmp, boxed, result, value);
        }
        for (i, (label, _)) in site.values.iter().enumerate() {
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, label),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            append_value(
                &mut a,
                code,
                &r,
                p,
                msg,
                tmp,
                boxed,
                result,
                Reg((i + 1) as u32),
            );
        }
        if site.tag == "[mp debrief] repair-mismatch" {
            // The repair branch holds a hasItemWithChest boolean, not the
            // underlying quantity. Its read-only count counterpart exposes it.
            let game_t = obj_type(code, "Game")?;
            let player_t = obj_type(code, "ent.BasePlayer")?;
            let inv_t = obj_type(code, "st.Inventory")?;
            let game = r.r(game_t);
            let player = r.r(player_t);
            let inv = r.r(inv_t);
            let item = r.r(p.string);
            let qty = r.r(p.int);
            let count_wc = code
                .functions
                .iter()
                .find(|f| s(code, f.name) == "countWithChest")
                .context("countWithChest missing")?
                .findex;
            a.op(Opcode::Field {
                dst: game,
                obj: current,
                field: field(code, p.debrief, "game")?.0,
            });
            a.op(Opcode::Field {
                dst: player,
                obj: game,
                field: field(code, game_t, "me")?.0,
            });
            a.op(Opcode::Field {
                dst: inv,
                obj: player,
                field: field(code, player_t, "inventory")?.0,
            });
            a.op(Opcode::GetGlobal {
                dst: item,
                global: job_xp::str_global(code, p.string, "RawMaterials"),
            });
            a.op(Opcode::Call2 {
                dst: qty,
                fun: count_wc,
                arg0: inv,
                arg1: item,
            });
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, " rawMaterials="),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            append_value(&mut a, code, &r, p, msg, tmp, boxed, result, qty);
        }
        if site.element {
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, " element="),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            append_value(&mut a, code, &r, p, msg, tmp, boxed, result, Reg(0));
            for (label, name) in [(" name=", "name"), (" x=", "absX"), (" y=", "absY")] {
                let (fl, ty) = field(code, p.element, name)?;
                let value = r.r(ty);
                a.op(Opcode::GetGlobal {
                    dst: tmp,
                    global: job_xp::str_global(code, p.string, label),
                });
                a.op(Opcode::Call2 {
                    dst: result,
                    fun: p.add,
                    arg0: msg,
                    arg1: tmp,
                });
                a.op(Opcode::Mov {
                    dst: msg,
                    src: result,
                });
                a.op(Opcode::Field {
                    dst: value,
                    obj: Reg(0),
                    field: fl,
                });
                append_value(&mut a, code, &r, p, msg, tmp, boxed, result, value);
            }
            // Record the actual top target after OVER creates its tooltip, or
            // before OUT removes it. A tooltip overlay is visible in this field.
            let game = r.r(obj_type(code, "Game")?);
            let globalui = r.r(obj_type(code, "ui.GlobalUI")?);
            let ignored = r.r(obj_type(code, "hl.types.ArrayObj")?);
            let top = r.r(p.element);
            a.op(Opcode::Field {
                dst: game,
                obj: Reg(0),
                field: field(code, p.element, "game")?.0,
            });
            a.op(Opcode::Field {
                dst: globalui,
                obj: game,
                field: field(code, obj_type(code, "Game")?, "globalUI")?.0,
            });
            a.op(Opcode::Null { dst: ignored });
            a.op(Opcode::Call2 {
                dst: top,
                fun: method(
                    code,
                    obj_type(code, "ui.BaseUI")?,
                    "getTopInteractiveElement",
                )?
                .findex,
                arg0: globalui,
                arg1: ignored,
            });
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, " top="),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            append_value(&mut a, code, &r, p, msg, tmp, boxed, result, top);
        }
        let mut tail: Vec<(&'static str, Reg)> = vec![];
        if site.tag == RATE {
            tail.push((" rebuilds/s=", rate_n));
        }
        // Wall clock (Sys.time, seconds since 1970) lines up machines; the
        // shim's tick prefix lines up this machine's sdr/barrier lines.
        tail.push((" t=", now));
        for (label, value) in tail {
            a.op(Opcode::GetGlobal {
                dst: tmp,
                global: job_xp::str_global(code, p.string, label),
            });
            a.op(Opcode::Call2 {
                dst: result,
                fun: p.add,
                arg0: msg,
                arg1: tmp,
            });
            a.op(Opcode::Mov {
                dst: msg,
                src: result,
            });
            append_value(&mut a, code, &r, p, msg, tmp, boxed, result, value);
        }
        // A String goes to println's Dyn argument as is (see append_value).
        a.op(Opcode::Call1 {
            dst: v,
            fun: p.println,
            arg0: msg,
        });
        a.label("done");
        a.op(Opcode::EndTrap { exc });
        a.op(Opcode::Ret { ret: v });
        a.label("caught");
        a.op(Opcode::Ret { ret: v });
        let logger = push_fn(code, args, p.void, r.0, a.finish(), p.debug_file)?;
        let f = &mut code.functions[site.fi];
        let void_reg = Reg(f.regs.len() as u32);
        f.regs.push(p.void);
        let mut call_args = vec![Reg(0)];
        call_args.extend(site.values.iter().map(|(_, r)| *r));
        hooks.push((
            site.fi,
            site.at,
            Opcode::CallN {
                dst: void_reg,
                fun: logger,
                args: call_args,
            },
        ));
    }
    // Insert the OVER trace after setTip so the tooltip exists; OUT and CLICK
    // trace at entry, before their vanilla side effects.
    for (fi, at, call) in hooks.into_iter().rev() {
        let at = if p
            .sites
            .iter()
            .any(|s| s.fi == fi && s.tag == "[mp debrief] over")
        {
            let set_tip = method(code, obj_type(code, "ui.GlobalUI")?, "setTip")?.findex;
            code.functions[fi]
                .ops
                .iter()
                .position(|o| matches!(o, Opcode::Call4 { fun, .. } if *fun == set_tip))
                .context("setTip missing")?
                + 1
        } else {
            at
        };
        insert_ops(&mut code.functions[fi], at, vec![call]);
    }
    Ok(())
}

pub(crate) fn patch(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("debrief diagnostics skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    let before: Vec<_> = p
        .sites
        .iter()
        .map(|s| (s.fi, code.functions[s.fi].clone()))
        .collect();
    if let Err(e) = apply(code, &p) {
        snap.restore(code);
        for (fi, f) in before {
            code.functions[fi] = f;
        }
        eprintln!("debrief diagnostics skipped: {e:#}");
    } else {
        eprintln!(
            "patched debrief diagnostics: bounded rebuild causes and loot hover/click targets"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    #[test]
    fn traces_real_debrief_branches_and_hover_without_changing_game_ops() {
        let image = std::fs::read(HLBOOT).expect("installed game fixture");
        let mut code = read(&image);
        crate::party_inventory::patch_party_counts(&mut code);
        crate::party_inventory::patch_party_lists(&mut code);
        crate::debrief_cure::patch_debrief_cure(&mut code);
        let before = read(&write(&code));
        let p = plan(&code).expect("plan");
        patch(&mut code);
        // Re-applying is a no-op, including after the existing inventory passes.
        assert!(code.strings.iter().any(|s| s.as_str() == MARKER));
        let patched = write(&code);
        let back = read(&patched);
        for (fi, original) in before.functions.iter().enumerate() {
            let mut expected = original.clone();
            let mut sites: Vec<_> = p
                .sites
                .iter()
                .enumerate()
                .filter(|(_, s)| s.fi == fi)
                .collect();
            sites.sort_by_key(|(_, s)| std::cmp::Reverse(s.at));
            for (ordinal, site) in sites {
                let helper = back.functions[before.functions.len() + ordinal].findex;
                let changed = &back.functions[fi];
                let (actual_at, call) = changed
                    .ops
                    .iter()
                    .enumerate()
                    .find(|(_, op)| matches!(op, Opcode::CallN { fun, .. } if *fun == helper))
                    .expect("diagnostic callsite");
                check_types(&back, changed, actual_at..actual_at + 1);
                let at = if site.tag == "[mp debrief] over" {
                    let set_tip =
                        method(&before, obj_type(&before, "ui.GlobalUI").unwrap(), "setTip")
                            .unwrap()
                            .findex;
                    original
                        .ops
                        .iter()
                        .position(|op| matches!(op, Opcode::Call4 { fun, .. } if *fun == set_tip))
                        .unwrap()
                        + 1
                } else {
                    site.at
                };
                insert_ops(&mut expected, at, vec![call.clone()]);
            }
            assert_eq!(
                format!("{:?}", expected.ops),
                format!("{:?}", back.functions[fi].ops),
                "fn@{} original ops/jumps",
                original.findex.0
            );
            assert_eq!(expected.debug_info, back.functions[fi].debug_info);
            assert_eq!(expected.assigns, back.functions[fi].assigns);
            assert_eq!(
                &back.functions[fi].regs[..original.regs.len()],
                original.regs.as_slice()
            );
            if p.sites.iter().any(|s| s.fi == fi) {
                check_flow(&back.functions[fi]);
            }
        }
        for f in back.functions.iter().skip(before.functions.len()) {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }
        patch(&mut code);
        assert_eq!(write(&code), patched);
        let full = read(&crate::patch_image(&image).expect("full patch chain"));
        assert!(full.strings.iter().any(|s| s.as_str() == MARKER));
        for site in &p.sites {
            let tag_globals: Vec<_> = full.constants.iter().flatten().filter(|c| matches!(c.fields.first(), Some(si) if full.strings[*si].as_str() == site.tag)).map(|c| c.global).collect();
            let logger = full.functions.iter().find(|f| f.ops.iter().any(|op| matches!(op, Opcode::GetGlobal { global, .. } if tag_globals.contains(global)))).expect("full-chain diagnostic logger");
            check_types(&full, logger, 0..logger.ops.len());
            check_flow(logger);
        }
    }

    /// Runs every appended logger once in a small interpreter that boxes like
    /// the HL runtime: ToDyn wraps the value with the register's static type,
    /// so a boxed String/object prints garbage instead of its text.
    #[test]
    fn loggers_print_readable_text() {
        use std::collections::HashMap;
        #[derive(Clone, Debug, PartialEq)]
        enum V {
            Null,
            I(i32),
            F(f64),
            B(bool),
            S(String),
            O(usize),
            Boxed(Box<V>),
        }
        fn text(v: &V) -> String {
            match v {
                V::Null => "null".into(),
                V::I(x) => x.to_string(),
                V::F(x) => x.to_string(),
                V::B(x) => x.to_string(),
                V::S(s) => s.clone(),
                V::O(i) => format!("obj{i}"),
                V::Boxed(inner) => match **inner {
                    V::I(_) | V::F(_) | V::B(_) => text(inner),
                    _ => "<garbage>".into(),
                },
            }
        }
        let image = std::fs::read(HLBOOT).expect("installed game fixture");
        let mut code = read(&image);
        crate::party_inventory::patch_party_counts(&mut code);
        crate::party_inventory::patch_party_lists(&mut code);
        crate::debrief_cure::patch_debrief_cure(&mut code);
        let nf = code.functions.len();
        let p = plan(&code).expect("plan");
        patch(&mut code);
        let code = read(&write(&code));
        let strs: HashMap<usize, String> = code
            .constants
            .iter()
            .flatten()
            .filter_map(|c| {
                let si = *c.fields.first()?;
                Some((c.global.0, code.strings[si].as_str().to_string()))
            })
            .collect();
        let count_wc = code
            .functions
            .iter()
            .find(|f| s(&code, f.name) == "countWithChest")
            .unwrap()
            .findex;
        let top = method(
            &code,
            obj_type(&code, "ui.BaseUI").unwrap(),
            "getTopInteractiveElement",
        )
        .unwrap()
        .findex;
        let default = |t: RefType, objs: &mut usize| -> V {
            match &code.types[t.0] {
                Type::I32 => V::I(5),
                Type::F64 => V::F(1.5),
                Type::Bool => V::B(true),
                _ if t == p.string => V::S("x".into()),
                Type::Obj(_) => {
                    *objs += 1;
                    V::O(*objs)
                }
                _ => V::Null,
            }
        };
        for (ordinal, site) in p.sites.iter().enumerate() {
            let f = &code.functions[nf + ordinal];
            let mut objs = 0usize;
            let mut globals: HashMap<usize, V> = HashMap::new();
            let mut r: Vec<V> = f.regs.iter().map(|_| V::Null).collect();
            let nargs = f.t.as_fun(&code).unwrap().args.len();
            for i in 0..nargs {
                r[i] = default(f.regs[i], &mut objs);
            }
            let mut printed = vec![];
            let mut pc = 0;
            loop {
                let op = &f.ops[pc];
                let mut next = pc + 1;
                let jump = |off: i32| (pc as i64 + 1 + off as i64) as usize;
                let x = |reg: &Reg| reg.0 as usize;
                let num = |v: &V| match v {
                    V::I(i) => *i as f64,
                    V::F(f) => *f,
                    o => panic!("not a number {o:?}"),
                };
                match op {
                    Opcode::Trap { .. } | Opcode::EndTrap { .. } => {}
                    Opcode::Ret { .. } => break,
                    Opcode::Mov { dst, src } | Opcode::UnsafeCast { dst, src } => {
                        r[x(dst)] = r[x(src)].clone()
                    }
                    Opcode::ToDyn { dst, src } => r[x(dst)] = V::Boxed(Box::new(r[x(src)].clone())),
                    Opcode::Int { dst, ptr } => r[x(dst)] = V::I(code.ints[ptr.0]),
                    Opcode::Float { dst, ptr } => r[x(dst)] = V::F(code.floats[ptr.0]),
                    Opcode::Null { dst } => r[x(dst)] = V::Null,
                    Opcode::Incr { dst } => r[x(dst)] = V::I(num(&r[x(dst)]) as i32 + 1),
                    Opcode::Sub { dst, a, b } => r[x(dst)] = V::F(num(&r[x(a)]) - num(&r[x(b)])),
                    Opcode::GetGlobal { dst, global } => {
                        r[x(dst)] = match strs.get(&global.0) {
                            Some(s) => V::S(s.clone()),
                            None => globals.get(&global.0).cloned().unwrap_or_else(|| {
                                match code.types[code.globals[global.0].0] {
                                    Type::I32 => V::I(0),
                                    Type::F64 => V::F(0.0),
                                    _ => V::Null,
                                }
                            }),
                        }
                    }
                    Opcode::SetGlobal { global, src } => {
                        globals.insert(global.0, r[x(src)].clone());
                    }
                    Opcode::Field { dst, .. } => r[x(dst)] = default(f.regs[x(dst)], &mut objs),
                    Opcode::JNull { reg, offset } if r[x(reg)] == V::Null => next = jump(*offset),
                    Opcode::JFalse { cond, offset } if r[x(cond)] != V::B(true) => {
                        next = jump(*offset)
                    }
                    Opcode::JEq { a, b, offset } if r[x(a)] == r[x(b)] => next = jump(*offset),
                    Opcode::JNull { .. } | Opcode::JFalse { .. } | Opcode::JEq { .. } => {}
                    Opcode::JSLt { a, b, offset } if num(&r[x(a)]) < num(&r[x(b)]) => {
                        next = jump(*offset)
                    }
                    Opcode::JSGte { a, b, offset } if num(&r[x(a)]) >= num(&r[x(b)]) => {
                        next = jump(*offset)
                    }
                    Opcode::JSLt { .. } | Opcode::JSGte { .. } => {}
                    Opcode::Call0 { dst, fun } if *fun == p.sys_time => r[x(dst)] = V::F(100.0),
                    Opcode::Call1 { dst, fun, arg0 } => {
                        let a = r[x(arg0)].clone();
                        r[x(dst)] = if *fun == p.stringify {
                            V::S(text(&a))
                        } else if *fun == p.println {
                            printed.push(match a {
                                V::S(s) => s,
                                o => format!("<garbage {}>", text(&o)),
                            });
                            V::Null
                        } else if *fun == p.get_window {
                            V::O(1)
                        } else {
                            panic!("unexpected call fn@{}", fun.0)
                        }
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } => {
                        let (a, b) = (r[x(arg0)].clone(), r[x(arg1)].clone());
                        r[x(dst)] = if *fun == p.add {
                            let (V::S(a), V::S(b)) = (&a, &b) else {
                                panic!("String.__add__ on {a:?} {b:?}")
                            };
                            V::S(format!("{a}{b}"))
                        } else if *fun == p.check {
                            V::B(true)
                        } else if *fun == count_wc {
                            V::I(3)
                        } else if *fun == top {
                            V::O(99)
                        } else {
                            panic!("unexpected call fn@{}", fun.0)
                        }
                    }
                    o => panic!("{}: unsupported {o:?}", site.tag),
                }
                pc = next;
            }
            if site.tag == RATE {
                // The first update only starts the one-second window.
                assert!(printed.is_empty(), "{printed:?}");
                continue;
            }
            assert_eq!(printed.len(), 1, "{}: {printed:?}", site.tag);
            let line = &printed[0];
            assert!(line.starts_with(site.tag), "{}: {line}", site.tag);
            assert!(!line.contains("garbage"), "{line}");
            assert!(line.contains(" repairTot=5 cureTot=5"), "{line}");
            assert!(line.ends_with(" t=100"), "{line}");
            if site.element {
                assert!(line.contains(" name=x "), "{line}");
            }
        }
    }

    #[test]
    fn refuses_unrecognized_availability_rebuild_shape_without_edits() {
        let image = std::fs::read(HLBOOT).expect("installed game fixture");
        let mut code = read(&image);
        let p = plan(&code).expect("plan");
        let first = p
            .sites
            .iter()
            .find(|s| s.tag.ends_with("-mismatch"))
            .expect("availability site");
        code.functions[first.fi].ops[first.at - 1] = Opcode::Label;
        let before = write(&code);
        patch(&mut code);
        assert_eq!(write(&code), before);
    }
}
