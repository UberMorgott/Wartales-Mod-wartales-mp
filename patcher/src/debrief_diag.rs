// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
// Bounded diagnostics for the remaining post-battle hover/click failure.
// No input, loot or window state is changed. The previous cure-list fix is
// installed, so rebuilding and tooltip hit-test churn must be distinguished.

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use hlbc::types::RefGlobal;

const MARKER: &str = "[mp debrief] rebuild";

struct Site {
    fi: usize,
    at: usize,
    tag: &'static str,
    element: bool,
    values: Vec<(&'static str, Reg)>,
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
    get_window: RefFun,
    check: RefFun,
    class: RefGlobal,
    println: RefFun,
    stringify: RefFun,
    add: RefFun,
    debug_file: usize,
}

fn index(code: &Bytecode, fun: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == fun)
        .context("function missing")
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
    let rebuild = method(code, debrief, "rebuild")?;
    let mut sites = vec![Site {
        fi: index(code, rebuild.findex)?,
        at: 0,
        tag: MARKER,
        element: false,
        values: vec![],
    }];
    let update = method(code, debrief, "update")?;
    let update_fi = index(code, update.findex)?;
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
    if sites.len() != 3 {
        bail!("expected two availability rebuilds");
    }
    let make = method(code, element, "makeInteractive")?;
    let interactive = obj_type(code, "h2d.Interactive")?;
    for (event, tag) in [
        ("onOver", "[mp debrief] over"),
        ("onOut", "[mp debrief] out"),
        ("onClick", "[mp debrief] click"),
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
        let fi = index(code, fun)?;
        if code.functions[fi].regs.first() != Some(&element) {
            bail!("unexpected event context");
        }
        sites.push(Site {
            fi,
            at: 0,
            tag,
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
            fi: index(code, f.findex)?,
            at: 0,
            tag,
            element: true,
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
        get_window,
        check,
        class,
        println,
        stringify,
        add,
        debug_file: debug_file(code, "src/ui/win/Debrief.hx")?,
    })
}

fn append_value(a: &mut Asm, p: &Plan, msg: Reg, tmp: Reg, boxed: Reg, result: Reg, value: Reg) {
    a.op(Opcode::ToDyn {
        dst: boxed,
        src: value,
    });
    a.op(Opcode::Call1 {
        dst: tmp,
        fun: p.stringify,
        arg0: boxed,
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
    let mut hooks = vec![];
    for site in &p.sites {
        let source_t = if site.element { p.element } else { p.debrief };
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
        let mut a = Asm::new();
        a.jmp(Opcode::Trap { exc, offset: 0 }, "caught");
        if site.element {
            a.jmp(
                Opcode::JNull {
                    reg: Reg(0),
                    offset: 0,
                },
                "done",
            );
            a.op(Opcode::Call1 {
                dst: win,
                fun: p.get_window,
                arg0: Reg(0),
            });
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
        a.label("same");
        let input_event = matches!(
            site.tag,
            "[mp debrief] over" | "[mp debrief] out" | "[mp debrief] click"
        );
        let grid_event = matches!(
            site.tag,
            "[mp debrief] inventory-rebuild" | "[mp debrief] slot-redraw"
        );
        let counter = if input_event {
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
            ptr: int_const(code, if input_event { 96 } else { 24 }),
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
            append_value(&mut a, p, msg, tmp, boxed, result, value);
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
            append_value(&mut a, p, msg, tmp, boxed, result, Reg((i + 1) as u32));
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
            append_value(&mut a, p, msg, tmp, boxed, result, qty);
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
            append_value(&mut a, p, msg, tmp, boxed, result, Reg(0));
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
                append_value(&mut a, p, msg, tmp, boxed, result, value);
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
            append_value(&mut a, p, msg, tmp, boxed, result, top);
        }
        a.op(Opcode::ToDyn {
            dst: boxed,
            src: msg,
        });
        a.op(Opcode::Call1 {
            dst: v,
            fun: p.println,
            arg0: boxed,
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

    #[test]
    fn refuses_unrecognized_availability_rebuild_shape_without_edits() {
        let image = std::fs::read(HLBOOT).expect("installed game fixture");
        let mut code = read(&image);
        let p = plan(&code).expect("plan");
        let first = &p.sites[1];
        code.functions[first.fi].ops[first.at - 1] = Opcode::Label;
        let before = write(&code);
        patch(&mut code);
        assert_eq!(write(&code), before);
    }
}
