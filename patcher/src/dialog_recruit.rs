// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a tavern recruit can be hired while another player reads its info sheet.
//
// `Npc.inspect` (right click, or the dialog "inspect" choice) locks the reader
// with the NPC on their own machine only (npc_inspect.rs), so the dialog's
// Recruit choice is free on every other screen. On the reader's machine the
// recruit must first close that sheet: recruitUnit destroys the Npc under it.
//
// The choice handler's `recruitUnit(sa, elt, dialog)` becomes
// `dlgRecruit(sa, elt, dialog)`, on every machine:
//
//   me = dialog.game.me;
//   if (me.lockedWith == elt)            // this player reads the recruit
//       for (w in game.globalUI.windows)  // Npc.inspect's UnitInfo lives there
//           if (w is UnitInfo && w.unit != null && w.unit.isInTroop() != true)
//               { w.close(); break; }     // its onClose clears the lock
//   recruitUnit(sa, elt, dialog);
//
// The close is force_leave's NPC-inspect close (guard 3: a UnitInfo of a unit
// outside the troop, while this player is locked with the NPC), with the
// window's own close(): onClose clears lockedWith and, for the dialog
// inspect, unhides the dialog, before recruitUnit destroys the Npc.
//
// Double recruit stays impossible: one shared dialog, one networked Recruit
// button, onRecruit destroys the Npc.
// Op count unchanged (one Call3 target). Validated before editing; a mismatch
// skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::force_leave::slot;

struct Plan {
    h_fi: usize,
    /// The handler's `Call3 recruitUnit(sa, elt, dialog)` op.
    rec_at: usize,
    arr_t: RefType,
    a_len: RefField,
    a_raw: (RefField, RefType),
    base_check: RefFun,
    rec_fn: RefFun,
    rec_args: Vec<RefType>,
    d_game: (RefField, RefType),
    g_me: (RefField, RefType),
    bp_locked: (RefField, RefType),
    g_gui: (RefField, RefType),
    u_windows: RefField,
    win_t: RefType,
    ui_cls: (RefGlobal, RefType),
    ui_t: RefType,
    ui_unit: (RefField, RefType),
    in_troop: RefFun,
    in_troop_t: RefType,
    w_close: RefField,
    bool_: RefType,
    i32_: RefType,
    void_: RefType,
    dyn_t: RefType,
    dbg_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let game_t = obj_type(code, "Game")?;
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let win_t = obj_type(code, "ui.Window")?;
    let ui_t = obj_type(code, "ui.win.UnitInfo")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    if !is_sub(code, ui_t, win_t) {
        bail!("ui.win.UnitInfo is not a ui.Window");
    }
    let a_len = field(code, arr_t, "length")?;
    if a_len.1 != i32_ {
        bail!("ArrayObj.length is not an i32");
    }
    let a_len = a_len.0;
    let a_raw = field(code, arr_t, "array")?;

    // The choice handler: a Switch over every special action ctor, then the one
    // `Call3 recruitUnit(sa, elt, dialog)`.
    let rec_fn = static_or_method(code, dlg_t, "recruitUnit")?.findex;
    let (rec_args, rec_ret) = sig(code, rec_fn)?;
    if rec_args.len() != 3 || rec_args[2] != dlg_t || rec_ret != void_ {
        bail!("unexpected Dialog.recruitUnit signature");
    }
    let n_sa = match &code.types[rec_args[0].0] {
        Type::Enum {
            name, constructs, ..
        } if s(code, *name) == "DialogSpecialAction" => constructs.len(),
        _ => bail!("recruitUnit does not take a DialogSpecialAction"),
    };
    let handlers: Vec<(usize, usize)> = code
        .functions
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| {
            f.ops
                .iter()
                .enumerate()
                .filter(|(_, x)| matches!(x, Opcode::Call3 { fun, .. } if *fun == rec_fn))
                .map(move |(i, _)| (fi, i))
        })
        .filter(|&(fi, at)| {
            code.functions[fi].ops[..at]
                .iter()
                .any(|x| matches!(x, Opcode::Switch { offsets, .. } if offsets.len() == n_sa))
        })
        .collect();
    let [(h_fi, rec_at)] = handlers[..] else {
        bail!(
            "expected one recruitUnit call in a choice handler, found {}",
            handlers.len()
        );
    };

    let base_t = obj_type(code, "hl.BaseType")?;
    let base_check = method(code, base_t, "check")?.findex;
    let ui_cls = class_global(code, "ui.win.UnitInfo")?;
    let d_game = field(code, dlg_t, "game")?;
    if d_game.1 != game_t {
        bail!("Dialog.game is not a Game");
    }
    let g_me = field(code, game_t, "me")?;
    let bp_locked = field(code, bp_t, "lockedWith")?;
    if !is_sub(code, g_me.1, bp_t) && g_me.1 != bp_t {
        bail!("Game.me is not a BasePlayer");
    }
    let g_gui = field(code, game_t, "globalUI")?;
    let (u_windows, uw_t) = field(code, g_gui.1, "windows")?;
    if uw_t != arr_t {
        bail!("globalUI.windows is not an ArrayObj");
    }
    let ui_unit = field(code, ui_t, "unit")?;
    if ui_unit.1 != unit_t {
        bail!("UnitInfo.unit is not an st.Unit");
    }
    let in_troop = method(code, unit_t, "isInTroop")?.findex;
    let in_troop_t = match sig(code, in_troop)? {
        (a, r) if a == [unit_t] && matches!(code.types[r.0], Type::Null(x) if x == bool_) => r,
        _ => bail!("unexpected Unit.isInTroop signature"),
    };
    let w_close = slot(code, win_t, "close")?;
    Ok(Plan {
        h_fi,
        rec_at,
        arr_t,
        a_len,
        a_raw,
        base_check,
        rec_fn,
        rec_args,
        d_game,
        g_me,
        bp_locked,
        g_gui,
        u_windows,
        win_t,
        ui_cls,
        ui_t,
        ui_unit,
        in_troop,
        in_troop_t,
        w_close,
        bool_,
        i32_,
        void_,
        dyn_t,
        dbg_file: debug_file(code, "src/ui/win/Dialog.hx")?,
    })
}

/// A static function `name` of class `t` (its `$Cls` statics) or an instance method.
fn static_or_method<'a>(code: &'a Bytecode, t: RefType, name: &str) -> Result<&'a Function> {
    method(code, t, name).or_else(|_| {
        let cls = s(code, obj(code, t)?.name).to_string();
        let (pkg, c) = cls.rsplit_once('.').unwrap_or(("", &cls));
        let st = if pkg.is_empty() {
            format!("${c}")
        } else {
            format!("{pkg}.${c}")
        };
        crate::diag::static_fn(code, &st, name)
    })
}

/// `dlgRecruit(sa, elt, dialog)` (see the header).
fn add_recruit(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let mut r = Regs(p.rec_args.clone());
    let (sa, elt, dlg) = (Reg(0), Reg(1), Reg(2));
    let (v, game, me, lw, gui, list, n, i, raw, d, w) = (
        r.r(p.void_),
        r.r(p.d_game.1),
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.win_t),
    );
    let (uc, b, ui, un, nb) = (
        r.r(p.ui_cls.1),
        r.r(p.bool_),
        r.r(p.ui_t),
        r.r(p.ui_unit.1),
        r.r(p.in_troop_t),
    );
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: dlg,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: game,
        obj: dlg,
        field: p.d_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "call");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "call");
    a.jmp(
        Opcode::JNotEq {
            a: lw,
            b: elt,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: gui,
        obj: game,
        field: p.g_gui.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: list,
        obj: gui,
        field: p.u_windows,
    });
    a.jmp(
        Opcode::JNull {
            reg: list,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: list,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("win");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: list,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: w, src: d });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "win");
    a.op(Opcode::GetGlobal {
        dst: uc,
        global: p.ui_cls.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: uc,
        arg1: w,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "win");
    // guard 3: the sheet of a unit outside the troop (isInTroop null or false)
    a.op(Opcode::UnsafeCast { dst: ui, src: w });
    a.op(Opcode::Field {
        dst: un,
        obj: ui,
        field: p.ui_unit.0,
    });
    a.jmp(Opcode::JNull { reg: un, offset: 0 }, "win");
    a.op(Opcode::Call1 {
        dst: nb,
        fun: p.in_troop,
        arg0: un,
    });
    a.jmp(Opcode::JNull { reg: nb, offset: 0 }, "close");
    a.op(Opcode::SafeCast { dst: b, src: nb });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "win");
    a.label("close");
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.w_close,
        args: vec![w],
    });
    a.label("call");
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.rec_fn,
        arg0: sa,
        arg1: elt,
        arg2: dlg,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        p.rec_args.clone(),
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let recruit = add_recruit(code, p)?;
    let h = &mut code.functions[p.h_fi];
    let Opcode::Call3 {
        dst,
        arg0,
        arg1,
        arg2,
        ..
    } = h.ops[p.rec_at]
    else {
        unreachable!()
    };
    h.ops[p.rec_at] = Opcode::Call3 {
        dst,
        fun: recruit,
        arg0,
        arg1,
        arg2,
    };
    eprintln!(
        "patched dialog recruit fn@{} op {}: closes this player's NPC sheet first (fn@{})",
        h.findex.0, p.rec_at, recruit.0
    );
    Ok(())
}

/// Closes this player's sheet of a recruit before the recruit, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_dialog_recruit(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("dialog recruit skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only the handler's recruitUnit call changes; one well-typed function is
    /// appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_dialog_recruit(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len() + 1);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != p.h_fi, "function #{i} (fn@{})", a.findex.0);
        }
        let rf = back.functions.last().expect("new fn");
        let (a, b) = (&orig.functions[p.h_fi], &back.functions[p.h_fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        assert_eq!(a.regs, b.regs);
        check_types(&back, b, p.rec_at..p.rec_at + 1);
        assert!(matches!(b.ops[p.rec_at], Opcode::Call3 { fun, .. } if fun == rf.findex));
        check_types(&back, rf, 0..rf.ops.len());
        check_flow(rf);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_dialog_recruit(&mut again);
        assert!(write(&again) == patched);
    }

    /// dlgRecruit in the interpreter: on the locked player's machine the recruit
    /// closes the NPC sheet before recruitUnit runs.
    #[test]
    fn sim_close() {
        use crate::testsim::{Sim, V};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let orig_n = orig.functions.len();
        let mut code = read(&image);
        patch_dialog_recruit(&mut code);
        let rf = code.functions.last().expect("new fn").findex;
        let pp = &p;
        let mut sim = Sim::new(
            &code,
            orig_n,
            move |c, f, a| {
                let p = pp;
                Some(if f == p.base_check {
                    V::B(c.key_get(&a[1], "cls") == a[0])
                } else if f == p.in_troop {
                    c.key_get(&a[0], "troop")
                } else if f == p.rec_fn {
                    c.log.push(("recruit", a.to_vec()));
                    V::Null
                } else {
                    return None;
                })
            },
            |c, field, a| {
                c.log
                    .push(("close", vec![V::I(field as i32), a[0].clone()]));
                V::Null
            },
        );
        sim.c.globals.insert(p.ui_cls.0 .0, V::S("ui".into()));
        let c = &mut sim.c;
        let elt = c.obj(&[]);
        // me locked with elt, its sheet (unit outside the troop) in globalUI
        let other_w = c.obj(&[]);
        c.key_set(&other_w, "cls".into(), V::S("x".into()));
        let unit = c.obj(&[]);
        let sheet = c.obj(&[(p.ui_unit.0, unit.clone())]);
        c.key_set(&sheet, "cls".into(), V::S("ui".into()));
        let wins = c.arr(p.a_len, p.a_raw.0, vec![other_w, sheet.clone()]);
        let gui = c.obj(&[(p.u_windows, wins)]);
        let me = c.obj(&[(p.bp_locked.0, elt.clone())]);
        let g = c.obj(&[(p.g_me.0, me.clone()), (p.g_gui.0, gui)]);
        let dlg = c.obj(&[(p.d_game.0, g)]);
        let sa = c.enm(6, vec![]);
        let recruit_run = |sim: &mut Sim| {
            sim.run(rf, vec![sa.clone(), elt.clone(), dlg.clone()]);
            let closed: Vec<V> = sim
                .c
                .take("close")
                .into_iter()
                .map(|a| a[1].clone())
                .collect();
            let rec = sim.c.take("recruit");
            assert_eq!(rec.len(), 1);
            assert_eq!(rec[0], vec![sa.clone(), elt.clone(), dlg.clone()]);
            closed
        };
        assert_eq!(recruit_run(&mut sim), vec![sheet.clone()]);
        // a troop unit's sheet stays
        sim.c.key_set(&unit, "troop".into(), V::B(true));
        assert!(recruit_run(&mut sim).is_empty());
        // not this player's lock: nothing closes
        sim.c.key_set(&unit, "troop".into(), V::B(false));
        sim.c.set(&me, p.bp_locked.0, V::Null);
        assert!(recruit_run(&mut sim).is_empty());
    }

    /// A handler without its recruitUnit call is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        code.functions[p.h_fi].ops[p.rec_at] = Opcode::Label;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_dialog_recruit(&mut code);
        assert!(write(&code) == before);
    }
}
