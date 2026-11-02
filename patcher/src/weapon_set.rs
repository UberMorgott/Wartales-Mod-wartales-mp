// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The second weapon set (weapon-swap perk) takes any weapon the class can use.
//
// `st.Unit.getEquipSlots` (Unit.hx:3235-3237) builds both weapon slots as
//
//   addSlot(secondaryWeapon != null ? [secondaryWeapon.inf.type] : weapons,
//           ..., identicItemType = secondaryWeapon != null, "Weapon");
//   addSlot(weapon != null ? [weapon.inf.type] : weapons,
//           ..., identicItemType = weapon != null, "SecondaryWeapon");
//
// where `weapons` is the class's `inf.weaponTypes` minus Shield. So a filled
// set pins the other set to its exact item type (axe + axe). Every equip path
// reads these slots: the inventory highlight and drop (`getTargetEquipSlot`),
// `autoUnequip`, `localEquipSlot` on host and guest, and the load-time
// `recoverInvalidEquips`. The four `JNull` on `this.secondaryWeapon` /
// `this.weapon` become `JAlways` to the same target, so both slots always take
// the class list without the exact-type flag (the shape vanilla uses while the
// other set is empty). The class check itself (`hasCantEquipReasons`) stays.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    sites: Vec<usize>,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "st.Unit")?;
    let f = method(code, unit_t, "getEquipSlots")?;
    let fi = fun_index(code, f.findex)?;
    let (weapon, _) = field(code, unit_t, "weapon")?;
    let (secondary, _) = field(code, unit_t, "secondaryWeapon")?;
    let mut sites = Vec::new();
    for (i, w) in f.ops.windows(2).enumerate() {
        if let [Opcode::GetThis { dst, field }, Opcode::JNull { reg, .. }] = w {
            if dst == reg && (*field == weapon || *field == secondary) {
                sites.push(i + 1);
            }
        }
    }
    match sites.len() {
        4 => Ok(Plan { fi, sites }),
        0 => bail!("getEquipSlots: already applied or changed"),
        n => bail!("getEquipSlots: {n} weapon null checks, want 4"),
    }
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    for &i in &p.sites {
        let Opcode::JNull { offset, .. } = f.ops[i] else { unreachable!() };
        f.ops[i] = Opcode::JAlways { offset };
    }
    eprintln!(
        "patched weapon sets fn@{}: second set takes any class weapon",
        f.findex.0
    );
}

/// Lets either weapon set hold any weapon type of the unit's class, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_weapon_set(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("weapon set skipped: {e:#}")),
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
        let (fi, sites) = (p.fi, p.sites.clone());        let mut code = read(&image);
        patch_weapon_set(&mut code);
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
        for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
            if sites.contains(&i) {
                let (Opcode::JNull { offset: o1, .. }, Opcode::JAlways { offset: o2 }) = (x, y)
                else {
                    panic!("op {i}: {x:?} -> {y:?}");
                };
                assert_eq!(o1, o2);
            } else {
                assert_eq!(format!("{x:?}"), format!("{y:?}"), "op {i}");
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_weapon_set(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
