// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op client's skill bar is redrawn when a round reset arrives (issue #2:
// Inhalation greyed out on the client at round start until any action).
//
// `battle.Unit.set_apSkillPlayed(v)` is the hxbit setter of the per-round
// "skills already played" list that `canUseSkill` checks. A round reset
// (`resetTurn`, an empty list) reaches a client through `networkSync`, which
// calls the setter, but nothing there marks the battle UI dirty: the only
// synced setter that does is `set_skillPersistValues`, and only while the
// battle is not locked. So the client's skill bar kept the previous round's
// availability until an action redrew it. On a client only, for the unit the
// skill bar shows, an emptied list now marks the UI dirty:
//
//   ...; apSkillPlayed = v;
//   if (game != null && !game.isAuth && v != null && battle != null
//       && battle.currentUnit == this && v.length == 0) battle.setUIDirty();
//   return v;
//
// `setUIDirty` only sets a flag; the next `Battle.update` redraws the skill bar
// (`skillBar.setUnit(currentUnit)`), as vanilla does after an action. The host
// and solo (`isAuth`) are unchanged.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    unit_game: RefField,
    game_t: RefType,
    game_auth: RefField,
    bool_t: RefType,
    unit_battle: RefField,
    battle_t: RefType,
    current: RefField,
    unit_t: RefType,
    len: RefFun,
    i32_t: RefType,
    dirty: RefFun,
    void_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "battle.Unit")?;
    let game_t = obj_type(code, "Game")?;
    let unit_game = typed(code, unit_t, "game", game_t)?;
    let (game_auth, bool_t) = field(code, game_t, "isAuth")?;
    if !matches!(code.types[bool_t.0], Type::Bool) {
        bail!("Game.isAuth is not a Bool");
    }
    let (unit_battle, battle_t) = field(code, unit_t, "battle")?;
    let current = typed(code, battle_t, "currentUnit", unit_t)?;
    let (played_field, proxy_t) = field(code, unit_t, "apSkillPlayed")?;
    let len = method(code, proxy_t, "get_length")?.findex;
    let (len_args, i32_t) = sig(code, len)?;
    if len_args[..] != [proxy_t] || !matches!(code.types[i32_t.0], Type::I32) {
        bail!("ArrayProxyData.get_length: unexpected signature");
    }
    let dirty = method(code, battle_t, "setUIDirty")?.findex;
    let (_, void_t) = sig(code, dirty)?;

    let f = method(code, unit_t, "set_apSkillPlayed")?;
    if calls(f, dirty) {
        bail!("set_apSkillPlayed: already applied");
    }
    if fun_args(code, f)[..] != [unit_t, proxy_t] {
        bail!("set_apSkillPlayed: unexpected signature");
    }
    match &f.ops[..] {
        [.., Opcode::SetThis { field, src: Reg(1) }, Opcode::Ret { ret: Reg(1) }]
            if *field == played_field => {}
        _ => bail!("set_apSkillPlayed: does not end with `apSkillPlayed = v; return v`"),
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        unit_game,
        game_t,
        game_auth,
        bool_t,
        unit_battle,
        battle_t,
        current,
        unit_t,
        len,
        i32_t,
        dirty,
        void_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let g = new_reg(f, p.game_t);
    let k = new_reg(f, p.bool_t);
    let b = new_reg(f, p.battle_t);
    let u = new_reg(f, p.unit_t);
    let n = new_reg(f, p.i32_t);
    let z = new_reg(f, p.i32_t);
    let v = new_reg(f, p.void_t);
    let zero = int_const(code, 0);
    let f = &mut code.functions[p.fi];
    // Before the final Ret; every skip lands on it (op i skips to i + 1 + offset).
    let end = |i: i32| 12 - i;
    let at = f.ops.len() - 1;
    insert_ops(
        f,
        at,
        vec![
            Opcode::GetThis {
                dst: g,
                field: p.unit_game,
            },
            Opcode::JNull {
                reg: g,
                offset: end(1),
            },
            Opcode::Field {
                dst: k,
                obj: g,
                field: p.game_auth,
            },
            Opcode::JTrue {
                cond: k,
                offset: end(3),
            },
            Opcode::JNull {
                reg: Reg(1),
                offset: end(4),
            },
            Opcode::GetThis {
                dst: b,
                field: p.unit_battle,
            },
            Opcode::JNull {
                reg: b,
                offset: end(6),
            },
            Opcode::Field {
                dst: u,
                obj: b,
                field: p.current,
            },
            Opcode::JNotEq {
                a: u,
                b: Reg(0),
                offset: end(8),
            },
            Opcode::Call1 {
                dst: n,
                fun: p.len,
                arg0: Reg(1),
            },
            Opcode::Int { dst: z, ptr: zero },
            Opcode::JNotEq {
                a: n,
                b: z,
                offset: end(11),
            },
            Opcode::Call1 {
                dst: v,
                fun: p.dirty,
                arg0: b,
            },
        ],
    );
    eprintln!(
        "patched skill sync fn@{}: a client's round reset redraws the skill bar",
        f.findex.0
    );
}

/// Redraws a client's skill bar when a synced round reset arrives, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_skill_sync(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("skill sync skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, shifted, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, dirty) = (p.fi, p.dirty);
        let mut code = read(&image);
        patch_skill_sync(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let at = a.ops.len() - 1;
        shifted(a, b, at, 13);
        let ret = b.ops.len() - 1;
        for j in [1, 3, 4, 6, 8, 11] {
            assert_eq!(jump_targets(b, at + j), vec![ret], "op {j}");
        }
        assert!(matches!(b.ops[at + 12], Opcode::Call1 { fun, .. } if fun == dirty));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_skill_sync(&mut again);
        assert!(write(&again) == patched);
    }
}
