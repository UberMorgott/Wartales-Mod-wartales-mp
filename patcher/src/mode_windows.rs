// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a mode switch of the shared party no longer closes a player's
// character sheet (ui.win.UnitInfo) or paths window (ui.win.Paths); only a
// battle does.
//
// Every GameMode constructor ends with `closeWindows()` (GameMode.hx:220):
//
//   game.ctrl.closeWindow(UnitInfo, null); game.ctrl.closeWindow(Paths, null);
//
// Controller.closeWindow is the hxbit RPC netCloseWindow: on the host it runs
// on every machine, on a client it runs on the host. Both windows live in
// game.globalUI, so the switch itself keeps them; this close is what removed
// them whenever any machine built a mode (a town, a tavern, a camp, a dialog
// place, the world map back from a place), on every player's screen.
//
// The fix, in front of op 0:
//   if (game != null && game.get_isMulti() && !Std.isOfType(this, battle.Battle)) return;
// `this` already has the subclass's type while the base constructor runs. Solo
// play and the battle mode keep the vanilla close.
//
// Part S: the sheet of a unit outside the troop (`isInTroop() != true`: a
// tavern recruit, a prisoner, a quest NPC, the dialog's class-unlock preview
// unit) belongs to the place and closes when the party switches mode. A leave
// goes back to the previous mode (Game.leaveMode -> switchToMode, no
// constructor), so the close sits in front of Game.switchToMode op 0:
// `mwStrangers(this)`, on every machine (each switches itself), local only (the
// RPC would also close the others' own sheets):
//   if (!game.get_isMulti()) return;      // solo: vanilla
//   for (i = windows.length - 1 .. 0)     // close() removes from the list
//       if (w is UnitInfo && w.unit != null && w.unit.isInTroop() != true) w.close();
// force_leave's guard 3 still closes an NPC-locked sheet before the leave (the
// lock would hold it); this covers sheets without a lock and a leave that lands
// first. Validated and skipped on its own.
//
// Part R: a camp rest no longer closes a client's sheet. The host's
// Controller.startRest RPC runs `startRest__impl` on every client, which removes
// the CampFire window and then `globalUI.getWindow(UnitInfo)` (Controller.hx:272).
// That second block is skipped: its first op becomes a jump past it (same op
// count). Validated and skipped on its own.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use hlbc::types::RefGlobal;

struct Plan {
    fi: usize,
    game: (RefField, RefType),
    is_multi: RefFun,
    is_of_type: RefFun,
    battle_cls: (RefGlobal, RefType),
    bool_t: RefType,
}

/// Part S: Game.switchToMode and what mwStrangers reads.
struct SPlan {
    fi: usize,
    game_t: RefType,
    is_multi: RefFun,
    bool_t: RefType,
    g_gui: (RefField, RefType),
    u_windows: RefField,
    arr_t: RefType,
    a_len: RefField,
    a_raw: (RefField, RefType),
    base_check: RefFun,
    win_t: RefType,
    ui_cls: (RefGlobal, RefType),
    ui_t: RefType,
    ui_unit: (RefField, RefType),
    in_troop: RefFun,
    in_troop_t: RefType,
    w_close: RefField,
    i32_t: RefType,
    void_t: RefType,
    dyn_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let mode_t = obj_type(code, "GameMode")?;
    let game_t = obj_type(code, "Game")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    if !is_sub(code, battle_t, mode_t) {
        bail!("battle.Battle is not a GameMode");
    }
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let game = field(code, mode_t, "game")?;
    if game.1 != game_t {
        bail!("GameMode.game is not a Game");
    }
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], bool_t) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let is_of_type = crate::diag::static_fn(code, "$Std", "isOfType")?.findex;
    if sig(code, is_of_type)? != (vec![dyn_t, dyn_t], bool_t) {
        bail!("unexpected Std.isOfType signature");
    }
    let battle_cls = class_global(code, "battle.Battle")?;
    let f = method(code, mode_t, "closeWindows")?;
    let fi = fun_index(code, f.findex)?;
    let close = method(code, obj_type(code, "st.Controller")?, "closeWindow")?.findex;
    let (unit_info, _) = class_global(code, "ui.win.UnitInfo")?;
    let (paths, _) = class_global(code, "ui.win.Paths")?;
    let o = &f.ops;
    // Vanilla: two `game.ctrl.closeWindow(<class>, null)` calls, then Ret.
    let calls = o
        .iter()
        .filter(|x| matches!(x, Opcode::Call3 { fun, .. } if *fun == close))
        .count();
    let globals: Vec<RefGlobal> = o
        .iter()
        .filter_map(|x| match x {
            Opcode::GetGlobal { global, .. } => Some(*global),
            _ => None,
        })
        .collect();
    if o.len() != 15 || calls != 2 || globals != [unit_info, paths] {
        bail!("GameMode.closeWindows: unexpected body (already applied?)");
    }
    if !matches!(o[0], Opcode::GetThis { field, .. } if field == game.0) {
        bail!("GameMode.closeWindows: does not start with this.game");
    }
    if !matches!(o.last(), Some(Opcode::Ret { .. })) {
        bail!("GameMode.closeWindows: no Ret at the end");
    }
    Ok(Plan {
        fi,
        game,
        is_multi,
        is_of_type,
        battle_cls,
        bool_t,
    })
}

fn plan_switch(code: &Bytecode) -> Result<SPlan> {
    let game_t = obj_type(code, "Game")?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], bool_t) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let f = method(code, game_t, "switchToMode")?;
    let fi = fun_index(code, f.findex)?;
    let ui = field(code, game_t, "ui")?.0;
    // vanilla op 0: `this.ui` (for ui.stopPick()); no jump goes back to it
    if !matches!(f.ops[0], Opcode::GetThis { field, .. } if field == ui) {
        bail!("Game.switchToMode: does not start with this.ui (already applied?)");
    }
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
        bail!("Game.switchToMode: a jump targets op 0");
    }
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let win_t = obj_type(code, "ui.Window")?;
    let ui_t = obj_type(code, "ui.win.UnitInfo")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    if !is_sub(code, ui_t, win_t) {
        bail!("ui.win.UnitInfo is not a ui.Window");
    }
    let a_len = field(code, arr_t, "length")?;
    if a_len.1 != i32_t {
        bail!("ArrayObj.length is not an i32");
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
        (a, r) if a == [unit_t] && matches!(code.types[r.0], Type::Null(x) if x == bool_t) => r,
        _ => bail!("unexpected Unit.isInTroop signature"),
    };
    Ok(SPlan {
        fi,
        game_t,
        is_multi,
        bool_t,
        g_gui,
        u_windows,
        arr_t,
        a_len: a_len.0,
        a_raw: field(code, arr_t, "array")?,
        base_check: method(code, obj_type(code, "hl.BaseType")?, "check")?.findex,
        win_t,
        ui_cls: class_global(code, "ui.win.UnitInfo")?,
        ui_t,
        ui_unit,
        in_troop,
        in_troop_t,
        w_close: crate::force_leave::slot(code, win_t, "close")?,
        i32_t,
        void_t,
        dyn_t,
    })
}

/// `mwStrangers(game)`: closes this machine's sheets of units outside the troop (see the header).
fn strangers(code: &mut Bytecode, p: &SPlan, dbg_file: usize) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.game_t]);
    let game = Reg(0);
    let (v, gui, list, i, zero, raw, d, w) = (
        r.r(p.void_t),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.win_t),
    );
    let (uc, b, ui, un, nb) = (
        r.r(p.ui_cls.1),
        r.r(p.bool_t),
        r.r(p.ui_t),
        r.r(p.ui_unit.1),
        r.r(p.in_troop_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
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
        "end",
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
        "end",
    );
    a.op(Opcode::Field {
        dst: i,
        obj: list,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.loop_head("win");
    a.jmp(
        Opcode::JSLte {
            a: i,
            b: zero,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Decr { dst: i });
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
    a.jmp(Opcode::JAlways { offset: 0 }, "win");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], p.void_t, r.0, a.finish(), dbg_file)
}

fn apply_switch(code: &mut Bytecode, p: &SPlan) -> Result<()> {
    let dbg = code.functions[p.fi]
        .debug_info
        .as_ref()
        .map_or(0, |d| d[0].0);
    let s = strangers(code, p, dbg)?;
    let f = &mut code.functions[p.fi];
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        0,
        vec![Opcode::Call1 {
            dst: v,
            fun: s,
            arg0: Reg(0),
        }],
    );
    eprintln!(
        "patched mode windows fn@{}: a co-op mode switch closes this machine's sheets of units outside the troop (fn@{})",
        f.findex.0, s.0
    );
    Ok(())
}

fn apply(code: &mut Bytecode, p: &Plan) {
    let f = &mut code.functions[p.fi];
    let Some(&Opcode::Ret { ret }) = f.ops.last() else {
        unreachable!("checked by plan")
    };
    let base = f.regs.len() as u32;
    let (g, b, c) = (Reg(base), Reg(base + 1), Reg(base + 2));
    f.regs.extend([p.game.1, p.bool_t, p.battle_cls.1]);
    // 8 new ops; the jumps go to op 8, the vanilla body. `this` goes to the
    // Dynamic argument as is (as vanilla passes objects): a ToDyn would box it
    // with its static type GameMode.
    insert_ops(
        f,
        0,
        vec![
            Opcode::Field {
                dst: g,
                obj: Reg(0),
                field: p.game.0,
            },
            Opcode::JNull { reg: g, offset: 6 },
            Opcode::Call1 {
                dst: b,
                fun: p.is_multi,
                arg0: g,
            },
            Opcode::JFalse { cond: b, offset: 4 },
            Opcode::GetGlobal {
                dst: c,
                global: p.battle_cls.0,
            },
            Opcode::Call2 {
                dst: b,
                fun: p.is_of_type,
                arg0: Reg(0),
                arg1: c,
            },
            Opcode::JTrue { cond: b, offset: 1 },
            Opcode::Ret { ret },
        ],
    );
    eprintln!(
        "patched mode windows fn@{}: a co-op mode switch keeps UnitInfo / Paths open (a battle still closes them)",
        f.findex.0
    );
}

/// Part R: `(fi, at, end)` of startRest__impl's UnitInfo removal block, ops
/// at..end (`this.game.globalUI.getWindow(UnitInfo)` ... `parent.removeChild(w)`).
fn plan_rest(code: &Bytecode) -> Result<(usize, usize, usize)> {
    let ctrl_t = obj_type(code, "st.Controller")?;
    let game_t = obj_type(code, "Game")?;
    let game = field(code, ctrl_t, "game")?.0;
    let gui = field(code, game_t, "globalUI")?.0;
    let (unit_info, _) = class_global(code, "ui.win.UnitInfo")?;
    let f = method(code, ctrl_t, "startRest__impl")?;
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    let k = o
        .iter()
        .position(|x| matches!(x, Opcode::GetGlobal { global, .. } if *global == unit_info))
        .context("startRest__impl: no UnitInfo read")?;
    let at = k
        .checked_sub(4)
        .context("startRest__impl: UnitInfo read too early")?;
    if !matches!(o[at], Opcode::GetThis { field, .. } if field == game)
        || !matches!(o[at + 2], Opcode::Field { field, .. } if field == gui)
    {
        bail!("startRest__impl: the UnitInfo block does not start with this.game.globalUI (already applied?)");
    }
    // The block's `w == null` tests all jump to its end, right after the removal call.
    let end = *jump_targets(f, k + 8)
        .first()
        .context("startRest__impl: no null test after getWindow")?;
    if end <= k + 8 || !matches!(o.get(end - 1), Some(Opcode::CallMethod { .. })) {
        bail!("startRest__impl: the UnitInfo block does not end with the removal call");
    }
    for i in (0..o.len()).filter(|i| !(at..end).contains(i)) {
        if jump_targets(f, i).iter().any(|t| (at + 1..end).contains(t)) {
            bail!("startRest__impl: op {i} jumps into the UnitInfo block");
        }
    }
    Ok((fi, at, end))
}

/// Keeps a co-op player's sheet and paths window open across a non-battle mode
/// switch and a camp rest, or leaves `code` untouched and logs why.
pub(crate) fn patch_mode_windows(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => crate::skipped(format!("mode windows skipped: {e:#}")),
    }
    // apply_switch fails only in push_fn, before switchToMode is edited
    let snap = Snap::take(code);
    if let Err(e) = plan_switch(code).and_then(|p| apply_switch(code, &p)) {
        snap.restore(code);
        crate::skipped(format!("mode windows strangers skipped: {e:#}"));
    }
    match plan_rest(code) {
        Ok((fi, at, end)) => {
            let f = &mut code.functions[fi];
            f.ops[at] = Opcode::JAlways {
                offset: (end - at - 1) as i32,
            };
            eprintln!(
                "patched mode windows fn@{} op {at}: a camp rest keeps a client's sheet open",
                f.findex.0
            );
        }
        Err(e) => crate::skipped(format!("mode windows rest skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    /// closeWindows gets the 8-op guard in front, its jumps land on the vanilla
    /// body, which is kept as it was; switchToMode gets one call in front of op
    /// 0; one well-typed function is appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let fi = plan(&orig).expect("plan").fi;
        let rfi = plan_rest(&orig).expect("plan_rest").0;
        let sfi = plan_switch(&orig).expect("plan_switch").fi;
        let mut code = read(&image);
        patch_mode_windows(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 1);
        let new = back.functions.last().unwrap();
        check_flow(new);
        check_types(&back, new, 0..new.ops.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != fi && i != rfi && i != sfi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 8);
        assert_eq!(format!("{:?}", &b.ops[8..]), format!("{:?}", a.ops));
        for i in [1, 3, 6] {
            assert_eq!(jump_targets(b, i), [8], "op {i}");
        }
        assert!(matches!(b.ops[7], Opcode::Ret { .. }));
        // part S: one call in front of switchToMode, the rest shifted by one
        let (a, b) = (&orig.functions[sfi], &back.functions[sfi]);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        assert!(matches!(b.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == new.findex));
        check_types(&back, b, 0..1);
        check_flow(b);
        assert!(plan_switch(&back).is_err());
        // part R: one op of startRest__impl becomes the jump past the block
        let (rfi, at, end) = plan_rest(&orig).expect("plan_rest");
        let (a, b) = (&orig.functions[rfi], &back.functions[rfi]);
        assert_eq!(a.ops.len(), b.ops.len());
        assert_eq!(jump_targets(b, at), [end]);
        for i in (0..a.ops.len()).filter(|&i| i != at) {
            assert_eq!(
                format!("{:?}", b.ops[i]),
                format!("{:?}", a.ops[i]),
                "op {i}"
            );
        }
        assert!(plan_rest(&back).is_err());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_mode_windows(&mut again);
        assert!(write(&again) == patched);
    }

    /// mwStrangers in the interpreter: closes every sheet of a unit outside the
    /// troop (isInTroop null or false), keeps troop sheets and other windows.
    #[test]
    fn sim_strangers() {
        use crate::testsim::{Sim, V};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan_switch(&orig).expect("plan_switch");
        let mut code = read(&image);
        patch_mode_windows(&mut code);
        let sf = code.functions.last().expect("new fn").findex;
        let (len, raw) = (p.a_len, p.a_raw.0);
        let pp = &p;
        let mut sim = Sim::new(
            &code,
            orig.functions.len(),
            move |c, f, a| {
                Some(if f == pp.is_multi {
                    c.key_get(&a[0], "multi")
                } else if f == pp.base_check {
                    V::B(c.key_get(&a[1], "cls") == a[0])
                } else if f == pp.in_troop {
                    c.key_get(&a[0], "troop")
                } else {
                    return None;
                })
            },
            |c, _, a| {
                c.log.push(("close", vec![a[0].clone()]));
                V::Null
            },
        );
        sim.c.globals.insert(p.ui_cls.0 .0, V::S("ui".into()));
        let c = &mut sim.c;
        let sheet = |c: &mut crate::testsim::Core, troop: V| {
            let u = c.obj(&[]);
            c.key_set(&u, "troop".into(), troop);
            let w = c.obj(&[(p.ui_unit.0, u)]);
            c.key_set(&w, "cls".into(), V::S("ui".into()));
            w
        };
        let other = c.obj(&[]);
        c.key_set(&other, "cls".into(), V::S("paths".into()));
        let recruit = sheet(c, V::Null);
        let own = sheet(c, V::B(true));
        let prisoner = sheet(c, V::B(false));
        let wins = c.arr(
            len,
            raw,
            vec![recruit.clone(), other, own, prisoner.clone()],
        );
        let gui = c.obj(&[(p.u_windows, wins)]);
        let g = c.obj(&[(p.g_gui.0, gui)]);
        // solo: nothing closes
        c.key_set(&g, "multi".into(), V::B(false));
        sim.run(sf, vec![g.clone()]);
        assert!(sim.c.take("close").is_empty());
        sim.c.key_set(&g, "multi".into(), V::B(true));
        sim.run(sf, vec![g]);
        let closed: Vec<V> = sim
            .c
            .take("close")
            .into_iter()
            .map(|a| a[0].clone())
            .collect();
        assert_eq!(closed, vec![prisoner, recruit]);
    }
}
