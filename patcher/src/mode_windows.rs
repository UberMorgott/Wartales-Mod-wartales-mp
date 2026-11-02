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
// Part R: a camp rest no longer closes a client's sheet. The host's
// Controller.startRest RPC runs `startRest__impl` on every client, which removes
// the CampFire window and then `globalUI.getWindow(UnitInfo)` (Controller.hx:272).
// That second block is skipped: its first op becomes a jump past it (same op
// count). Validated and skipped on its own.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use hlbc::types::RefGlobal;

struct Plan {
    fi: usize,
    game: (RefField, RefType),
    is_multi: RefFun,
    is_of_type: RefFun,
    battle_cls: (RefGlobal, RefType),
    bool_t: RefType,
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

fn apply(code: &mut Bytecode, p: Plan) {
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
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("mode windows skipped: {e:#}")),
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
    use crate::asm::testutil::{game, read, write};

    /// Only closeWindows changes: the 8-op guard in front, its jumps land on the
    /// vanilla body, which is kept as it was; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let fi = plan(&orig).expect("plan").fi;
        let rfi = plan_rest(&orig).expect("plan_rest").0;
        let mut code = read(&image);
        patch_mode_windows(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != fi && i != rfi,
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
}
