// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: inspecting an NPC (a recruit's info sheet) locks only the inspecting
// player's own machine; nobody else waits for it.
//
// Vanilla `ent.p.Npc.inspect` (Npc.hx:252) runs on the inspecting machine only
// (netInspect__impl: `player == game.me`, also the dialog "inspect" choice):
//
//   game.me.lockedWith = this;      // set_lockedWith: hxbit setter, replicated
//   w = UnitInfo.create(getUnit(), null, Npc(this));
//   w.onClose = () -> { cb(); game.me.lockedWith = null; };
//
// The replicated lock made every other machine refuse while the sheet was open:
// Npc.talk (anyPlayerLocked), the dialog "waiting for" greying on the recruit
// (getPlayerLockedWith) and the place / tavern / camp leave.
//
// The fix: that set_lockedWith call becomes a plain `SetField me.lockedWith =
// this` (no network mark), so the lock exists on this machine only. Here it keeps
// every vanilla effect (this player's own talk / leave tests; force_leave closes
// the sheet on a pending leave, guard 3; dialog_recruit closes it before a
// recruit). The onClose still calls set_lockedWith(null): it marks and sends a
// null the others already have. A full sync (a player joining) or a mark already
// pending can still carry the lock for the sheet's lifetime, as vanilla always did.
//
// One op replaced; op count and registers unchanged. Validated before editing; a
// mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The Npc.inspect op `Call2 _ = set_lockedWith(me, this)`.
    at: usize,
    me: Reg,
    locked: RefField,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let npc_t = obj_type(code, "ent.p.Npc")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let (locked, locked_t) = field(code, bp_t, "lockedWith")?;
    if !is_sub(code, npc_t, locked_t) {
        bail!("ent.p.Npc is not a BasePlayer.lockedWith type");
    }
    let set_locked = method(code, bp_t, "set_lockedWith")?.findex;
    let f = method(code, npc_t, "inspect")?;
    if fun_args(code, f).first() != Some(&npc_t) {
        bail!("unexpected Npc.inspect signature");
    }
    let sites: Vec<(usize, Reg)> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call2 {
                fun, arg0, arg1, ..
            } if *fun == set_locked && *arg1 == Reg(0) => Some((i, *arg0)),
            _ => None,
        })
        .collect();
    let [(at, me)] = sites[..] else {
        bail!(
            "Npc.inspect: expected one set_lockedWith(me, this), found {}",
            sites.len()
        );
    };
    if f.regs[me.0 as usize] != bp_t {
        bail!("Npc.inspect: the locked player is not an ent.BasePlayer");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        at,
        me,
        locked,
    })
}

/// Makes a co-op NPC inspection lock only the inspecting machine, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_npc_inspect(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => {
            let f = &mut code.functions[p.fi];
            f.ops[p.at] = Opcode::SetField {
                obj: p.me,
                field: p.locked,
                src: Reg(0),
            };
            eprintln!(
                "patched npc inspect fn@{} op {}: the inspect lock stays on this machine",
                f.findex.0, p.at
            );
        }
        Err(e) => crate::skipped(format!("npc inspect skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only Npc.inspect's set_lockedWith op becomes a SetField of the same
    /// player and NPC; types check; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_npc_inspect(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != p.fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), a.ops.len());
        for i in 0..a.ops.len() {
            if i != p.at {
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", a.ops[i]),
                    "op {i}"
                );
            }
        }
        assert!(
            matches!(b.ops[p.at], Opcode::SetField { obj, field, src: Reg(0) } if obj == p.me && field == p.locked)
        );
        check_types(&back, b, p.at..p.at + 1);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_npc_inspect(&mut again);
        assert!(write(&again) == patched);
    }
}
