// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op client's skill bar is redrawn when the host's answer to its skill
// request releases the battle lock (weapon swap: the old weapon's passive stayed
// on the client's bar until the next action).
//
// On a client `Battle.executeSkill` does `lockCounter++` and sends
// `ctrl.battleExecuteSkill(..., cb)`; `cb` only does `lockCounter--`. The host
// runs the skill inside that call, between `battleLock` and `battleUnlock`
// (broadcast; `battleUnlock__impl` marks the UI dirty only once unlocked). A
// skill that ends synchronously on the host (weapon swap) has its unlock reach
// the client while the client's own lock still holds, so nothing redraws; the
// result then drops the lock without a redraw. Slower skills unlock after the
// result and redraw normally. `cb` now ends like `battleUnlock__impl`:
//
//   lockCounter--; if (!isLocked()) setUIDirty();
//
// `setUIDirty` only sets a flag; the next `Battle.update` redraws the skill bar
// (`skillBar.setUnit(currentUnit)`). `cb` exists on the client path only; the
// host and solo are unchanged.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    locked: RefFun,
    bool_t: RefType,
    dirty: RefFun,
    void_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let exec = method(code, battle_t, "executeSkill")?;
    let request = method(code, ctrl_t, "battleExecuteSkill")?.findex;
    let set_lock = method(code, battle_t, "set_lockCounter")?.findex;
    let locked = method(code, battle_t, "isLocked")?.findex;
    let (locked_args, bool_t) = sig(code, locked)?;
    if locked_args[..] != [battle_t] || !matches!(code.types[bool_t.0], Type::Bool) {
        bail!("Battle.isLocked: unexpected signature");
    }
    let dirty = method(code, battle_t, "setUIDirty")?.findex;
    let (dirty_args, void_t) = sig(code, dirty)?;
    if dirty_args[..] != [battle_t] {
        bail!("Battle.setUIDirty: unexpected signature");
    }

    // The request's callback: `this.<closure>` passed as its last argument.
    let cbs: Vec<RefFun> = exec
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::CallN { fun, args, .. } if *fun == request => args.last().copied(),
            _ => None,
        })
        .filter_map(|r| {
            exec.ops.iter().find_map(|o| match o {
                Opcode::InstanceClosure { dst, fun, obj: Reg(0) } if *dst == r => Some(*fun),
                _ => None,
            })
        })
        .collect();
    let [cb] = cbs[..] else {
        bail!("executeSkill: expected one battleExecuteSkill callback, found {}", cbs.len());
    };
    let fi = fun_index(code, cb)?;
    let f = &code.functions[fi];
    if calls(f, dirty) {
        bail!("skill request callback: already applied");
    }
    if fun_args(code, f).first() != Some(&battle_t) {
        bail!("skill request callback: not bound to the battle");
    }
    match &f.ops[..] {
        [.., Opcode::Call2 { fun, arg0: Reg(0), .. }, Opcode::Ret { .. }] if *fun == set_lock => {}
        _ => bail!("skill request callback: does not end with `set_lockCounter(..); return`"),
    }
    Ok(Plan {
        fi,
        locked,
        bool_t,
        dirty,
        void_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let k = new_reg(f, p.bool_t);
    let v = new_reg(f, p.void_t);
    let at = f.ops.len() - 1;
    insert_ops(
        f,
        at,
        vec![
            Opcode::Call1 {
                dst: k,
                fun: p.locked,
                arg0: Reg(0),
            },
            Opcode::JTrue { cond: k, offset: 1 },
            Opcode::Call1 {
                dst: v,
                fun: p.dirty,
                arg0: Reg(0),
            },
        ],
    );
    eprintln!(
        "patched skill result fn@{}: a client's released skill request redraws the skill bar",
        f.findex.0
    );
}

/// Redraws a client's skill bar when the host's answer to its skill request
/// unlocks the battle, or leaves `code` untouched and logs why.
pub(crate) fn patch_skill_result(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("skill result skipped: {e:#}")),
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
        let (fi, locked, dirty) = (p.fi, p.locked, p.dirty);
        let mut code = read(&image);
        patch_skill_result(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let at = a.ops.len() - 1;
        shifted(a, b, at, 3);
        let ret = b.ops.len() - 1;
        assert!(matches!(b.ops[at], Opcode::Call1 { fun, .. } if fun == locked));
        assert_eq!(jump_targets(b, at + 1), vec![ret]);
        assert!(matches!(b.ops[at + 2], Opcode::Call1 { fun, .. } if fun == dirty));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_skill_result(&mut again);
        assert!(write(&again) == patched);
    }
}
