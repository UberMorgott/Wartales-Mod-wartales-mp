// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a guest joining (or rejoining) while a puzzle mini-game runs no longer
// loads forever.
//
// A coop activity is a replicated `ui.win.Activity` (coop_spectate.rs). On a
// full sync (drop-in join, reconnect) Game.start (Game.hx:807) runs initData of
// every received object before the guest's place view has built the Elements'
// 3D objects. Vanilla Activity.initData (Activity.hx:174-182) turns the
// activity camera of a CylinderPuzzle / NinePuzzle by the target's rotation:
//
//   var dir = target.obj.getRotationQuat().toMatrix().getDirection(); ...
//
// `target.obj` is still null there (or the target is in a place this guest
// never loads: a host that left the place with the puzzle open), so
// `Null access .getRotationQuat` throws out of Game.start and the guest stays
// on the loading screen. Vanilla never full-syncs a running activity (no
// mid-game join); a guest present at the start had its place built.
//
// The fix: the block's two NullChecks (`target`, `target.obj`) become JNull to
// the block's join (needUnit), so a missing target or 3D object skips only the
// camera offset. That offset is read by _start alone, which a full-sync guest
// never runs for an activity that has already started. Same op count. Validated
// before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The `NullCheck target` op; `NullCheck target.obj` is two ops later.
    at: usize,
    /// The block's join: where the non-puzzle branch jumps (needUnit).
    join: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let act_t = obj_type(code, "ui.win.Activity")?;
    let f = method(code, act_t, "initData")?;
    if fun_args(code, f) != [act_t] {
        bail!("Activity.initData: unexpected signature");
    }
    let fi = fun_index(code, f.findex)?;
    let (target, _) = field(code, act_t, "target")?;
    let o = &f.ops;
    // GetThis t = target; NullCheck t; Field x = t.obj; NullCheck x; Call1 getRotationQuat(x)
    let hits: Vec<usize> = (1..o.len().saturating_sub(4))
        .filter(|&i| {
            matches!(
                (&o[i - 1], &o[i], &o[i + 1], &o[i + 2], &o[i + 3]),
                (
                    Opcode::GetThis { dst: t, field },
                    Opcode::NullCheck { reg: t2 },
                    Opcode::Field { dst: x, obj: t3, .. },
                    Opcode::NullCheck { reg: x2 },
                    Opcode::Call1 { fun, arg0, .. },
                ) if *field == target && t == t2 && t == t3 && x == x2 && arg0 == x
                    && fname(code, *fun) == "getRotationQuat"
            )
        })
        .collect();
    let [at] = hits[..] else {
        if hits.is_empty()
            && o.windows(2).any(|w| {
                matches!(w, [Opcode::GetThis { field, .. }, Opcode::JNull { .. }] if *field == target)
            })
        {
            bail!("Activity.initData: already applied");
        }
        bail!(
            "Activity.initData: {} target rotation reads, want 1",
            hits.len()
        );
    };
    // the op before the block: `JAlways join` (the non-puzzle branch)
    let gt = at - 1;
    let join = match o.get(gt - 1) {
        Some(Opcode::JAlways { .. }) => jump_targets(f, gt - 1)[0],
        _ => bail!("Activity.initData: no branch around the camera block"),
    };
    if join <= at + 3
        || !matches!(o[join], Opcode::Call1 { fun, .. } if fname(code, fun) == "needUnit")
    {
        bail!("Activity.initData: the camera block does not end at needUnit");
    }
    Ok(Plan { fi, at, join })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    let f = &mut code.functions[p.fi];
    for i in [p.at, p.at + 2] {
        let Opcode::NullCheck { reg } = f.ops[i] else {
            unreachable!()
        };
        f.ops[i] = Opcode::JNull {
            reg,
            offset: (p.join - i - 1) as i32,
        };
    }
    eprintln!(
        "patched activity join fn@{}: puzzle camera offset skipped without a target 3D object (ops {} {})",
        f.findex.0,
        p.at,
        p.at + 2
    );
}

/// Lets a co-op guest join during a puzzle mini-game, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_activity_join(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => crate::skipped(format!("activity join skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only Activity.initData changes: the two NullChecks become JNull to
    /// needUnit, every other op and jump kept; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_activity_join(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != p.fi, "fn #{i}");
        }
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        for i in 0..a.ops.len() {
            if i == p.at || i == p.at + 2 {
                assert!(matches!(a.ops[i], Opcode::NullCheck { .. }));
                assert!(matches!(b.ops[i], Opcode::JNull { .. }));
                assert_eq!(jump_targets(b, i), [p.join], "op {i}");
            } else {
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", a.ops[i]),
                    "op {i}"
                );
            }
        }
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_activity_join(&mut again);
        assert!(write(&again) == patched);
    }
}
