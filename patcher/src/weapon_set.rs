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
// Offhand vs mixed sets: `ItemSlot.autoUnequip` (ItemSlot.hx:692-694), when
// `is2HWeaponUser()` (true if EITHER set holds a two-handed weapon) and the new
// item is LeftHand/Shield (torch, shield), clears every slot that allows
// "Weapon", so both sets. Vanilla never mixes 1H and 2H across sets, so that
// was the same as clearing the 2H sets. Before that loop's `push` we insert a
// copy of the loop above's "slot.get() is TwoHands" test (ItemSlot.hx:691), so
// only sets holding a 2H weapon are cleared and a 1H set keeps its weapon.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    sites: Vec<usize>,
    unequip: Unequip,
    getter: Getter,
}

struct Unequip {
    fi: usize,
    at: usize,
    ops: Vec<Opcode>,
    /// autoUnequip's "item is TwoHands" ops: reads `item`, ends in JEq (not 2H)
    item: Reg,
    is_2h: Vec<Opcode>,
    types: Vec<(Reg, RefType)>,
}

struct Getter {
    fi: usize,
    left: RefField,
}

fn is_str(code: &Bytecode, g: RefGlobal, value: &str) -> bool {
    code.constants.iter().flatten().any(|c| {
        c.global == g
            && matches!(c.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value))
    })
}

/// In `autoUnequip`, the "LeftHand/Shield on a 2H user" loop pushes every
/// slot whose `allowed` matches "Weapon". Plan: before that push, a copy of the
/// 2H test of the first loop (`get()`, null -> skip, `type.props.flags & TwoHands`
/// == 0 -> skip), with the slot register swapped in.
fn plan_unequip(code: &Bytecode) -> Result<Unequip> {
    let f = crate::diag::static_fn(code, "ui.comp.$ItemSlot", "autoUnequip")?;
    let fi = fun_index(code, f.findex)?;
    let ops = &f.ops;
    let weapon = ops
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            matches!(w, [Opcode::GetGlobal { dst, global }, Opcode::Call2 { arg1, .. }]
                if dst == arg1 && is_str(code, *global, "Weapon"))
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let [wi] = weapon[..] else {
        bail!(
            "autoUnequip: {} \"Weapon\" slot matches, want 1",
            weapon.len()
        )
    };
    // JNull found -> skip; NullCheck list; push(list, slot); JAlways loop
    let (j, slot) = (wi..ops.len().saturating_sub(3))
        .find_map(|j| match &ops[j..j + 4] {
            [Opcode::JNull { offset: 2, .. }, Opcode::NullCheck { reg: l }, Opcode::Call2 { arg0, arg1, .. }, Opcode::JAlways { .. }]
                if l == arg0 => Some((j, *arg1)),
            _ => None,
        })
        .context("autoUnequip: weapon-slot push not found")?;
    // Field get = s.get; NullCheck get; CallClosure it = get(); JNull it; ... JEq (not 2H)
    let k = (0..wi)
        .find(|&k| {
            matches!(&ops[k..k + 4],
                [Opcode::Field { dst: g, .. }, Opcode::NullCheck { reg: g2 },
                 Opcode::CallClosure { dst: it, fun, args }, Opcode::JNull { reg: it2, .. }]
                if g == g2 && g == fun && args.is_empty() && it == it2)
        })
        .context("autoUnequip: 2H test not found")?;
    let e = (k..wi)
        .find(|&e| matches!(ops[e], Opcode::JEq { .. }))
        .context("autoUnequip: 2H test end not found")?;
    let mut new: Vec<Opcode> = ops[k..=e].to_vec();
    let n = new.len();
    let Opcode::Field { obj, .. } = &mut new[0] else {
        unreachable!()
    };
    if n != 18 || f.regs[obj.0 as usize] != f.regs[slot.0 as usize] {
        bail!("autoUnequip: 2H test shape changed ({n} ops)");
    }
    *obj = slot;
    // both exits land on the JAlways after the push: original j + 3, shifted by n
    let jumps = (k..=e).filter(|&i| !jump_targets(f, i).is_empty()).count();
    if jumps != 2 {
        bail!("autoUnequip: 2H test has {jumps} jumps, want 2");
    }
    for (m, op) in new.iter_mut().enumerate() {
        if let Opcode::JNull { offset, .. } | Opcode::JEq { offset, .. } = op {
            *offset = (n + 1 - m) as i32;
        }
    }
    // the "item is TwoHands" part (after `JNull it; NullCheck it`), reused by the getter
    let Opcode::JNull { reg: item, .. } = ops[k + 3] else {
        unreachable!()
    };
    let is_2h = ops[k + 5..=e].to_vec();
    let mut types = Vec::new();
    for op in &is_2h {
        let mut op = op.clone();
        for r in regs_mut(&mut op).context("autoUnequip: unexpected op in 2H test")? {
            types.push((*r, f.regs[r.0 as usize]));
        }
    }
    Ok(Unequip {
        fi,
        at: j + 1,
        ops: new,
        item,
        is_2h,
        types,
    })
}

/// Registers of the ops the 2H test is made of; `None` for anything else.
fn regs_mut(op: &mut Opcode) -> Option<Vec<&mut Reg>> {
    Some(match op {
        Opcode::Field { dst, obj, .. } => vec![dst, obj],
        Opcode::Call1 { dst, arg0, .. } => vec![dst, arg0],
        Opcode::NullCheck { reg } => vec![reg],
        Opcode::SafeCast { dst, src } => vec![dst, src],
        Opcode::Int { dst, .. } => vec![dst],
        Opcode::Shl { dst, a, b } | Opcode::And { dst, a, b } => vec![dst, a, b],
        Opcode::JEq { a, b, .. } => vec![a, b],
        _ => return None,
    })
}

/// The LeftHand slot `getEquipSlots` builds for a 2H user (`left == null &&
/// is2HWeaponUser()`) reads back the active weapon, so the slot shows it and
/// `localEquipSlot` passes it as `prev` (its bar skill gets removed). With mixed
/// sets the active one can be 1H: the getter then returns `left` instead.
fn plan_getter(code: &Bytecode, unit_t: RefType, ges: &Function) -> Result<Getter> {
    let is2h = method(code, unit_t, "is2HWeaponUser")?.findex;
    let at = ges
        .ops
        .iter()
        .position(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == is2h))
        .context("getEquipSlots: is2HWeaponUser call not found")?;
    let fun = ges.ops[at..]
        .iter()
        .find_map(|o| match o {
            Opcode::InstanceClosure { fun, .. } => Some(*fun),
            _ => None,
        })
        .context("getEquipSlots: 2H offhand getter not found")?;
    let fi = fun_index(code, fun)?;
    let g = &code.functions[fi];
    let (weapon, _) = field(code, unit_t, "weapon")?;
    let (left, _) = field(code, unit_t, "left")?;
    let r1 = Reg(1);
    match &g.ops[..] {
        [.., Opcode::Ret { ret: a }, Opcode::Field {
            dst,
            obj: Reg(0),
            field,
        }, Opcode::Ret { ret: b }]
            if g.ops.len() == 14 && *a == r1 && *b == r1 && *dst == r1 && *field == weapon => {}
        _ => bail!("2H offhand getter fn@{}: shape changed", g.findex.0),
    }
    Ok(Getter { fi, left })
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
        4 => Ok(Plan {
            fi,
            sites,
            unequip: plan_unequip(code)?,
            getter: plan_getter(code, unit_t, f)?,
        }),
        0 => bail!("getEquipSlots: already applied or changed"),
        n => bail!("getEquipSlots: {n} weapon null checks, want 4"),
    }
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    for &i in &p.sites {
        let Opcode::JNull { offset, .. } = f.ops[i] else {
            unreachable!()
        };
        f.ops[i] = Opcode::JAlways { offset };
    }
    eprintln!(
        "patched weapon sets fn@{}: second set takes any class weapon",
        f.findex.0
    );
    let u = p.unequip;
    let f = &mut code.functions[u.fi];
    insert_ops(f, u.at, u.ops);
    eprintln!(
        "patched weapon sets fn@{}: offhand clears only 2H sets",
        f.findex.0
    );

    // getter tail: r = <active weapon>; if r == null || !2H(r) r = this.left; return r
    let g = &mut code.functions[p.getter.fi];
    let r1 = Reg(1);
    let mut map: Vec<(Reg, Reg)> = vec![(u.item, r1)];
    for (r, t) in u.types {
        if !map.iter().any(|(a, _)| *a == r) {
            map.push((r, new_reg(g, t)));
        }
    }
    let mut tail = vec![Opcode::JNull { reg: r1, offset: 0 }];
    for op in u.is_2h {
        let mut op = op;
        for r in regs_mut(&mut op).expect("checked in plan") {
            *r = map.iter().find(|(a, _)| a == r).expect("mapped").1;
        }
        tail.push(op);
    }
    tail.push(Opcode::Ret { ret: r1 });
    let n = tail.len(); // `left` load follows the Ret
    for (m, op) in tail.iter_mut().enumerate() {
        if let Opcode::JNull { offset, .. } | Opcode::JEq { offset, .. } = op {
            *offset = (n - 1 - m) as i32;
        }
    }
    tail.push(Opcode::Field {
        dst: r1,
        obj: Reg(0),
        field: p.getter.left,
    });
    tail.push(Opcode::Ret { ret: r1 });
    let last = g.ops.len() - 1;
    g.ops[last - 2] = Opcode::JAlways { offset: 1 }; // secondary path joins the tail
    g.ops.pop(); // weapon path's Ret: falls into the tail
    let line = g.debug_info.as_ref().map(|d| d[last]);
    g.ops.extend(tail);
    if let (Some(d), Some(l)) = (&mut g.debug_info, line) {
        d.resize(g.ops.len(), l);
    }
    eprintln!(
        "patched weapon sets fn@{}: 2H offhand slot shows only a 2H weapon",
        g.findex.0
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
        let (fi, sites) = (p.fi, p.sites.clone());
        let (ui, at, n) = (p.unequip.fi, p.unequip.at, p.unequip.ops.len());
        let gi = p.getter.fi;
        let mut code = read(&image);
        patch_weapon_set(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                ![fi, ui, gi].contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // 2H offhand getter: both weapon reads join one tail; the tail returns
        // the weapon only when it is TwoHands, else `left`.
        let (a, g) = (&orig.functions[gi], &back.functions[gi]);
        crate::asm::testutil::check_flow(g);
        crate::asm::testutil::check_types(&back, g, 0..g.ops.len());
        assert_eq!(format!("{:?}", &a.ops[..11]), format!("{:?}", &g.ops[..11]));
        assert_eq!(jump_targets(g, 11), vec![13]);
        let rets: Vec<usize> = (0..g.ops.len())
            .filter(|&i| matches!(g.ops[i], Opcode::Ret { .. }))
            .collect();
        let end = g.ops.len() - 1;
        assert_eq!(rets, vec![end - 2, end]);
        assert_eq!(jump_targets(g, 13), vec![end - 1]);
        assert!(matches!(g.ops[end - 3], Opcode::JEq { .. }));
        assert_eq!(jump_targets(g, end - 3), vec![end - 1]);
        assert!(matches!(g.ops[end - 1], Opcode::Field { .. }));

        // autoUnequip: 2H test inserted right before the weapon-slot push,
        // both exits skip the push and land on the loop's back jump.
        let (a, b) = (&orig.functions[ui], &back.functions[ui]);
        crate::asm::testutil::shifted(a, b, at, n);
        crate::asm::testutil::check_flow(b);
        crate::asm::testutil::check_types(&back, b, at..at + n);
        let skip = at + 2 + n; // original JAlways after the push
        assert!(matches!(b.ops[skip], Opcode::JAlways { .. }));
        assert!(matches!(b.ops[skip - 1], Opcode::Call2 { .. }));
        let exits: Vec<usize> = (at..at + n).flat_map(|i| jump_targets(b, i)).collect();
        assert_eq!(exits, vec![skip, skip]);
        assert!(matches!(b.ops[at], Opcode::Field { .. }));
        assert!(matches!(b.ops[at + 2], Opcode::CallClosure { .. }));
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
