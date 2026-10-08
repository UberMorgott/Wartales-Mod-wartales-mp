// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Enemies of a higher level can drop their worn equipment.
//
// `battle.Debrief.genLoot` picks the worn-gear drop from each dead enemy's
// active slots (weapon, secondaryWeapon, left, armor, helmet, trinket0..3)
// through one closure per slot (Debrief.hx:521-531, nine identical copies)
// that keeps an item only if:
//
//   if (!hasFeatureConst(item.props.feature) || item.props.disableLoot
//       || item.props.requireLevel != null
//          && state.getReferenceLevel(1.0) < item.props.requireLevel) return;
//   if (!item.isType(Armor)) { candidates.push(item); return; }
//   for (u in state.getUnits()) if (u.isPlayer() && u.canEquip(item)) { candidates.push(item); return; }
//
// `getReferenceLevel(1.0)` is the level of the highest unit of the army, and
// `Unit.canEquip` is `level >= item.requireLevel && !hasCantEquipReasons(item)`.
// So gear of a level above the squad never reaches the candidates. Two edits
// per closure, same op count, no new registers:
//
//   - the `JSGte refLevel >= requireLevel` becomes a `JAlways` to the same
//     target (the level is no reason to skip any more);
//   - for armor, `canEquip(item)` + `JFalse` becomes `hasCantEquipReasons(item)`
//     + `JTrue`: still "one of your units can wear it" (armor / helmet kind,
//     animal, item flag checks), without the unit's level.
//
// Everything else stays vanilla: feature / disableLoot, NoEquipDrop,
// ForceDropWeapon, drop chance and pity counter, quality, repair. The loot is
// generated on the host (the shared battle.Debrief.loot pool), so in co-op
// only the host's build matters; clients see the pool as usual.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Site {
    fi: usize,
    /// The `JSGte refLevel >= requireLevel` after `getReferenceLevel`.
    level_at: usize,
    /// The `Call2 canEquip(unit, item)` (a `JFalse` on its result follows).
    equip_at: usize,
}

struct Plan {
    sites: Vec<Site>,
    cant_equip: RefFun,
}

fn site(code: &Bytecode, fi: usize, ref_level: RefFun, can_equip: RefFun) -> Result<Site> {
    let f = &code.functions[fi];
    let ops = &f.ops;
    let at = |want: RefFun| -> Result<usize> {
        let v: Vec<usize> = (0..ops.len())
            .filter(|&i| matches!(ops[i], Opcode::Call2 { fun, .. } if fun == want))
            .collect();
        match v[..] {
            [i] => Ok(i),
            _ => bail!("fn@{}: expected one call to fn@{}", f.findex.0, want.0),
        }
    };
    let r = at(ref_level)?;
    let Opcode::Call2 { dst: lvl, .. } = ops[r] else { unreachable!() };
    let level_at = r + 2;
    if !matches!(
        (ops.get(r + 1), ops.get(level_at)),
        (Some(Opcode::SafeCast { .. }), Some(Opcode::JSGte { a, .. })) if *a == lvl
    ) {
        bail!("fn@{}: no level compare after getReferenceLevel", f.findex.0);
    }
    let equip_at = at(can_equip)?;
    let Opcode::Call2 { dst, .. } = ops[equip_at] else { unreachable!() };
    if !matches!(ops.get(equip_at + 1), Some(Opcode::JFalse { cond, .. }) if *cond == dst) {
        bail!("fn@{}: canEquip result is not a JFalse", f.findex.0);
    }
    Ok(Site {
        fi,
        level_at,
        equip_at,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "st.Unit")?;
    let can_equip = method(code, unit_t, "canEquip")?.findex;
    let cant_equip = method(code, unit_t, "hasCantEquipReasons")?.findex;
    if sig(code, can_equip)? != sig(code, cant_equip)? {
        bail!("canEquip / hasCantEquipReasons signatures differ");
    }
    let ref_level = method(code, obj_type(code, "st.GameState")?, "getReferenceLevel")?.findex;
    let debrief = debug_file(code, "src/battle/Debrief.hx")?;

    let in_debrief = |f: &Function| {
        matches!(f.debug_info.as_deref(), Some([(file, _), ..]) if *file == debrief)
            && calls(f, ref_level)
    };
    if code
        .functions
        .iter()
        .any(|f| in_debrief(f) && calls(f, cant_equip))
    {
        bail!("already applied");
    }
    let sites = (0..code.functions.len())
        .filter(|&i| in_debrief(&code.functions[i]) && calls(&code.functions[i], can_equip))
        .map(|i| site(code, i, ref_level, can_equip))
        .collect::<Result<Vec<_>>>()?;
    // One per worn slot: patching only some would leave a level filter behind.
    if sites.len() != 9 {
        bail!("expected 9 loot candidate closures, found {}", sites.len());
    }
    Ok(Plan { sites, cant_equip })
}

fn apply(code: &mut Bytecode, p: Plan) {
    for s in &p.sites {
        let f = &mut code.functions[s.fi];
        let Opcode::JSGte { offset, .. } = f.ops[s.level_at] else { unreachable!() };
        f.ops[s.level_at] = Opcode::JAlways { offset };
        let Opcode::Call2 { dst, arg0, arg1, .. } = f.ops[s.equip_at] else { unreachable!() };
        f.ops[s.equip_at] = Opcode::Call2 {
            dst,
            fun: p.cant_equip,
            arg0,
            arg1,
        };
        let Opcode::JFalse { cond, offset } = f.ops[s.equip_at + 1] else { unreachable!() };
        f.ops[s.equip_at + 1] = Opcode::JTrue { cond, offset };
    }
    let first = code.functions[p.sites[0].fi].findex.0;
    eprintln!(
        "patched loot level fn@{first} (+{} more): enemies of a higher level drop their worn gear",
        p.sites.len() - 1
    );
}

/// Lets enemy gear above the squad's level drop, or leaves `code` untouched and logs why.
pub(crate) fn patch_loot_level(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("loot level skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(p.sites.len(), 9, "one closure per worn slot");
        let cant = p.cant_equip;
        let mut code = read(&image);
        patch_loot_level(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            let site = p.sites.iter().find(|s| s.fi == i);
            assert_eq!(same, site.is_none(), "function #{i} (fn@{})", a.findex.0);
            let Some(s) = site else { continue };
            assert_eq!(a.ops.len(), b.ops.len());
            assert_eq!(a.regs, b.regs);
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                let changed = format!("{x:?}") != format!("{y:?}");
                assert_eq!(changed, [s.level_at, s.equip_at, s.equip_at + 1].contains(&k), "op {k}");
            }
            let (Opcode::JSGte { offset: o1, .. }, Opcode::JAlways { offset: o2 }) =
                (&a.ops[s.level_at], &b.ops[s.level_at])
            else {
                panic!("level jump");
            };
            assert_eq!(o1, o2);
            assert!(matches!(b.ops[s.equip_at], Opcode::Call2 { fun, .. } if fun == cant));
            let (Opcode::JFalse { cond: c1, offset: o1 }, Opcode::JTrue { cond: c2, offset: o2 }) =
                (&a.ops[s.equip_at + 1], &b.ops[s.equip_at + 1])
            else {
                panic!("equip jump");
            };
            assert_eq!((c1, o1), (c2, o2));
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_loot_level(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
