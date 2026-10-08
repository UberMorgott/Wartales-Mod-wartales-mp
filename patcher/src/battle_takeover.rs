// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: when a player leaves during a battle, the host plays that player's
// units, so the battle goes on; when the player comes back they are theirs again.
//
// Vanilla: a lost client only flips `ent.BasePlayer.connected` to false on the
// host (BasePlayer.checkConnection, from Game.update; a replicated property,
// plus the "DisconnectMsg" chat line). The player, its units and their owner
// stay; no battle code reads `connected`. The battle control gate is
// `st.Unit.isControllable` (Unit.hx:2024):
//
//   if (battle?.battleMode?.coopIgnoreOwners()) return isPlayer();
//   return isOwnedBy(game.me);                  // owner == null || owner == me
//
// so nobody can act with the absent player's units. If one of them had begun
// its turn (`battle.State.startedPlaying`), the host may not pick another unit
// either (Battle.onOverlayEvent refuses skills of any other unit) and the battle
// is stuck for good. Mounts have the same gate in Mount.canPlayMount
// (Mount.hx:179: a rider's `owner.player == p`).
//
// The fix, host only (game.isAuth), two inline edits and no new function:
//
//   T1 isControllable, op 0:
//        if (game != null && game.isAuth && game.battle != null
//            && owner != null && !owner.connected) return true;
//      (the vanilla coopIgnoreOwners rule, Pit's "anyone plays any player
//      unit", for the absent player's units only)
//   T2 canPlayMount, in front of the rider test `if (rider.owner.player != p)`:
//        a rider whose player is not connected counts as the host's (same test).
//
// Ownership never changes, so nothing has to be given back: when the player
// rejoins, checkConnection sets `connected` again and their units answer to
// them only (battle_rejoin.rs hands them the running battle).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use hlbc::types::ValBool;

struct Plan {
    bool_t: RefType,
    game_t: RefType,
    battle_t: RefType,
    bp_t: RefType,
    unit_game: RefField,
    unit_owner: RefField,
    game_auth: RefField,
    game_battle: RefField,
    bp_game: RefField,
    bp_connected: RefField,
    /// isControllable's function index.
    ctrl_fi: usize,
    /// canPlayMount's function index, the rider-test op and its player register.
    mount_fi: usize,
    mount_at: usize,
    mount_rider: Reg,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let unit_t = obj_type(code, "st.Unit")?;
    let game_t = obj_type(code, "Game")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let mount_t = obj_type(code, "battle.unit.Mount")?;
    let bplayer_t = obj_type(code, "battle.Player")?;
    let unit_game = typed(code, unit_t, "game", game_t)?;
    let unit_owner = typed(code, unit_t, "owner", bp_t)?;
    let game_auth = typed(code, game_t, "isAuth", bool_t)?;
    let (game_battle, battle_t) = field(code, game_t, "battle")?;
    let bp_game = typed(code, bp_t, "game", game_t)?;
    let bp_connected = typed(code, bp_t, "connected", bool_t)?;
    let (bplayer_player, _) = field(code, bplayer_t, "player")?;

    // T1: isControllable is the vanilla 23-op body ending in isOwnedBy(game.me).
    let ctrl = method(code, unit_t, "isControllable")?;
    let owned_by = method(code, unit_t, "isOwnedBy")?.findex;
    if fun_args(code, ctrl) != [unit_t] || ctrl.ops.len() != 23 || !calls(ctrl, owned_by) {
        bail!("st.Unit.isControllable: unexpected shape (already patched?)");
    }
    if (0..ctrl.ops.len()).any(|i| jump_targets(ctrl, i).contains(&0)) {
        bail!("st.Unit.isControllable: a jump targets op 0");
    }
    let ctrl_fi = fun_index(code, ctrl.findex)?;

    // T2: canPlayMount(p): the one `x = owner.player; if (x != p) next rider`.
    let mount = method(code, mount_t, "canPlayMount")?;
    if fun_args(code, mount) != [mount_t, bp_t] {
        bail!("Mount.canPlayMount: unexpected signature");
    }
    let sites: Vec<(usize, Reg)> = mount
        .ops
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| match (&w[0], &w[1]) {
            (Opcode::Field { dst, obj, field }, Opcode::JNotEq { a, b, .. })
                if *field == bplayer_player
                    && mount.regs[obj.0 as usize] == bplayer_t
                    && a == dst
                    && *b == Reg(1) =>
            {
                Some((i + 1, *dst))
            }
            _ => None,
        })
        .collect();
    let [(mount_at, mount_rider)] = sites[..] else {
        bail!("Mount.canPlayMount: {} rider tests, want 1", sites.len());
    };
    if mount.regs[mount_rider.0 as usize] != bp_t {
        bail!("Mount.canPlayMount: the rider's player is not an ent.BasePlayer");
    }
    if (0..mount.ops.len()).any(|i| jump_targets(mount, i).contains(&mount_at)) {
        bail!("Mount.canPlayMount: a jump targets the rider test");
    }
    let mount_fi = fun_index(code, mount.findex)?;
    Ok(Plan {
        bool_t,
        game_t,
        battle_t,
        bp_t,
        unit_game,
        unit_owner,
        game_auth,
        game_battle,
        bp_game,
        bp_connected,
        ctrl_fi,
        mount_fi,
        mount_at,
        mount_rider,
    })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    // T1: 12 ops in front of op 0; every skip lands on the vanilla op 0 (index 12).
    let f = &mut code.functions[p.ctrl_fi];
    let g = new_reg(f, p.game_t);
    let b = new_reg(f, p.battle_t);
    let o = new_reg(f, p.bp_t);
    let k = new_reg(f, p.bool_t);
    let end = |i: i32| 12 - i - 1;
    insert_ops(
        f,
        0,
        vec![
            Opcode::GetThis { dst: g, field: p.unit_game },
            Opcode::JNull { reg: g, offset: end(1) },
            Opcode::Field { dst: k, obj: g, field: p.game_auth },
            Opcode::JFalse { cond: k, offset: end(3) },
            Opcode::Field { dst: b, obj: g, field: p.game_battle },
            Opcode::JNull { reg: b, offset: end(5) },
            Opcode::GetThis { dst: o, field: p.unit_owner },
            Opcode::JNull { reg: o, offset: end(7) },
            Opcode::Field { dst: k, obj: o, field: p.bp_connected },
            Opcode::JTrue { cond: k, offset: end(9) },
            Opcode::Bool { dst: k, value: ValBool(true) },
            Opcode::Ret { ret: k },
        ],
    );
    let ctrl = f.findex.0;

    // T2: 7 ops in front of the rider test (index 7 after the insert); a
    // disconnected rider's player jumps to the match branch right after it (8).
    let f = &mut code.functions[p.mount_fi];
    let x = p.mount_rider;
    let g = new_reg(f, p.game_t);
    let k = new_reg(f, p.bool_t);
    let to = |target: i32, i: i32| target - i - 1;
    insert_ops(
        f,
        p.mount_at,
        vec![
            Opcode::JNull { reg: x, offset: to(7, 0) },
            Opcode::Field { dst: g, obj: x, field: p.bp_game },
            Opcode::JNull { reg: g, offset: to(7, 2) },
            Opcode::Field { dst: k, obj: g, field: p.game_auth },
            Opcode::JFalse { cond: k, offset: to(7, 4) },
            Opcode::Field { dst: k, obj: x, field: p.bp_connected },
            Opcode::JFalse { cond: k, offset: to(8, 6) },
        ],
    );
    eprintln!(
        "patched battle takeover: isControllable fn@{ctrl} op 0, canPlayMount fn@{} op {}: the host plays a disconnected player's battle units",
        f.findex.0, p.mount_at
    );
}

/// Lets the host play the units of a player who left during a battle, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_battle_takeover(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => crate::skipped(format!("battle takeover skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only the two functions change, by the inserted ops alone; every jump of
    /// the inserted ops lands where the comments say; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_battle_takeover(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != p.ctrl_fi && i != p.mount_fi, "fn #{i}");
        }

        let (a, b) = (&orig.functions[p.ctrl_fi], &back.functions[p.ctrl_fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 12);
        assert_eq!(format!("{:?}", &b.ops[12..]), format!("{:?}", a.ops));
        for i in [1, 3, 5, 7, 9] {
            assert_eq!(jump_targets(b, i), [12], "isControllable op {i}");
        }
        assert!(matches!(b.ops[11], Opcode::Ret { .. }));

        let (a, b) = (&orig.functions[p.mount_fi], &back.functions[p.mount_fi]);
        let at = p.mount_at;
        assert_eq!(b.ops.len(), a.ops.len() + 7);
        for i in [0, 2, 4] {
            assert_eq!(jump_targets(b, at + i), [at + 7], "canPlayMount op {}", at + i);
        }
        assert_eq!(jump_targets(b, at + 6), [at + 8]);
        assert!(matches!(b.ops[at + 7], Opcode::JNotEq { .. }));
        // The vanilla rider test still skips to the same op (the next rider).
        let old = jump_targets(a, at)[0];
        assert_eq!(jump_targets(b, at + 7)[0], old + 7);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_battle_takeover(&mut again);
        assert!(write(&again) == patched);
    }
}
