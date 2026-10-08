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

// Champions and bosses can drop their worn gear too.
//
// genLoot skips the whole worn-gear roll for a class with the NoEquipDrop flag
// (Debrief.hx:518):
//
//   Int a = 1; Int b = 4; Shl a = a << b; And x = x & a; Int a = 0; JNotEq x != a -> skip
//
// cdb unitClass flags: IsChampion bit 3, NoEquipDrop bit 4, ArenaChampion
// bit 19. The NoEquipDrop classes are the named champions and bosses
// (humanoids with legendary gear), the arena champions, and creatures:
// ghosts, The Beast, sea snake, rats / molerats / rat nests and Remastered's
// workmen. The roll now runs for a NoEquipDrop class that is a champion but
// not an arena champion. With x = flags & (bit3 | bit4 | bit19) the possible
// values (every ArenaChampion class is also IsChampion + NoEquipDrop, vanilla
// and Remastered cdb) map through `x ^ 8` as:
//
//   0 (plain) -> 8, 8 (champion) -> 0, 24 (boss) -> 16: roll;
//   16 (creature) -> 24, arena champion -> 16 + bit19:     skip.
//
// So the same six ops become
//
//   Int a = MASK; And x = x & a; Int a = 8; Xor x = x ^ a; Int a = 16; JSGt x > a -> skip
//
// Arena champions keep the flag: vanilla arena masters give their gear as
// rewards (A1Arena, E1Arena, IR1Arena prefab dialog gains), so dropping it
// would duplicate a reward. The IsChampion creatures (H2TheBeast, Ecila,
// Erymanthe, CrawlerChampion) have no class equipment and roll for nothing,
// as ordinary animals already do. A boss's unique items are kept out by the
// item data the candidate closure reads (`disableLoot`). ForceDropWeapon, the
// drop chance and the pity counter stay vanilla: a boss that already dropped
// its weapon has used the battle's guaranteed drop, so its worn gear rolls
// the normal chance.

const CHAMPION_MASK: i32 = (1 << 3) | (1 << 4) | (1 << 19);

/// Function index and index of the `Int a = 1` that starts the NoEquipDrop test in genLoot.
fn champion_site(code: &Bytecode) -> Result<(usize, usize)> {
    let gen = method(code, obj_type(code, "battle.Debrief")?, "genLoot")?;
    let fi = fun_index(code, gen.findex)?;
    let int = |o: &Opcode| match o {
        Opcode::Int { dst, ptr } => Some((*dst, code.ints[ptr.0])),
        _ => None,
    };
    let ops = &gen.ops;
    let mut found = Vec::new();
    for k in 0..ops.len().saturating_sub(5) {
        let (Some((a, 1)), Some((b, 4))) = (int(&ops[k]), int(&ops[k + 1])) else {
            continue;
        };
        let Opcode::Shl { dst, a: sa, b: sb } = ops[k + 2] else { continue };
        let Opcode::And { dst: x, a: xa, b: xb } = ops[k + 3] else { continue };
        if (dst, sa, sb) != (a, a, b) || xa != x || xb != a {
            continue;
        }
        if int(&ops[k + 4]) == Some((a, 0))
            && matches!(ops[k + 5], Opcode::JNotEq { a: ja, b: jb, .. } if ja == x && jb == a)
        {
            found.push(k);
        }
    }
    match found[..] {
        [k] => Ok((fi, k)),
        [] if ops.iter().any(|o| int(o).is_some_and(|(_, v)| v == CHAMPION_MASK)) => {
            bail!("already applied")
        }
        _ => bail!("genLoot: expected one NoEquipDrop test, found {}", found.len()),
    }
}

/// Lets champions / bosses (not arena champions) drop worn gear, or leaves `code` untouched and logs why.
pub(crate) fn patch_champion_gear(code: &mut Bytecode) {
    let (fi, k) = match champion_site(code) {
        Ok(s) => s,
        Err(e) => return crate::skipped(format!("champion gear skipped: {e:#}")),
    };
    let mask = int_const(code, CHAMPION_MASK);
    let eight = int_const(code, 8);
    let sixteen = int_const(code, 16);
    let f = &mut code.functions[fi];
    let Opcode::Int { dst: a, .. } = f.ops[k] else { unreachable!() };
    let Opcode::And { dst: x, .. } = f.ops[k + 3] else { unreachable!() };
    let Opcode::JNotEq { offset, .. } = f.ops[k + 5] else { unreachable!() };
    f.ops.splice(k..k + 6, [
        Opcode::Int { dst: a, ptr: mask },
        Opcode::And { dst: x, a: x, b: a },
        Opcode::Int { dst: a, ptr: eight },
        Opcode::Xor { dst: x, a: x, b: a },
        Opcode::Int { dst: a, ptr: sixteen },
        Opcode::JSGt { a: x, b: a, offset },
    ]);
    eprintln!(
        "patched champion gear fn@{}: champions and bosses can drop worn gear",
        f.findex.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    #[test]
    fn champion_gear_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let (fi, k) = champion_site(&orig).expect("site");
        let mut code = read(&image);
        patch_champion_gear(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        assert_eq!(a.regs, b.regs);
        for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
            let changed = format!("{x:?}") != format!("{y:?}");
            assert_eq!(changed, (k..k + 6).contains(&i), "op {i}");
        }
        let v = |o: &Opcode| match o {
            Opcode::Int { ptr, .. } => back.ints[ptr.0],
            _ => panic!("not Int: {o:?}"),
        };
        assert_eq!((v(&b.ops[k]), v(&b.ops[k + 2]), v(&b.ops[k + 4])), (CHAMPION_MASK, 8, 16));
        let (Opcode::JNotEq { offset: o1, .. }, Opcode::JSGt { offset: o2, .. }) =
            (&a.ops[k + 5], &b.ops[k + 5])
        else {
            panic!("skip jump");
        };
        assert_eq!(o1, o2);
        // The skip decision for every flag combination the cdb has.
        let skip = |flags: i32| ((flags & CHAMPION_MASK) ^ 8) > 16;
        let (champ, ned, arena) = (1 << 3, 1 << 4, 1 << 19);
        assert!(!skip(0) && !skip(champ) && !skip(champ | ned) && !skip(1 << 7));
        assert!(skip(ned) && skip(ned | champ | arena) && skip(ned | 1 << 13));

        let mut again = read(&patched);
        assert!(champion_site(&again).is_err());
        patch_champion_gear(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

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
