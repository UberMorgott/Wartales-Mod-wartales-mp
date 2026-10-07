// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A censer's item tooltip no longer throws (and with it the inventory grid no
// longer stalls).
//
// The static `getHelpers(sk, lvl, u, stats, parent)` of SkillIcon.hx (line 327),
// called by the SkillTip constructor,
// adds, for the Purge and Inhalation skills, the helper of the censer the unit
// holds:
//
//   if (sk.id == "Purge" || sk.id == "Inhalation") {
//       var p = u.getCurrentEquippedWeapon(true)?.inf?.props?.passiveSkill;
//       if (p != null) createHelper("skill", p.name, formatSkillDesc(..));
//   }
//
// `u` is dereferenced without a null test (every other use of `u` in getHelpers
// is guarded or passed on to null-safe code). An item tooltip has no unit
// whenever the item is not shown for the current unit (chest, shop, party or
// another player's inventory, the stack held on the cursor): ItemTip passes
// `stats == null` to addSkillHelpers, SkillTip gets `u == null`, and getHelpers
// throws "Null access .getCurrentEquippedWeapon". Vanilla never reached it: no
// vanilla item grants Purge. Censers that grant the Purge weapon skill (the
// Remastered data) do, so their tooltip throws on every build. ui.comp.Slot.redraw
// calls redrawTip before it stores the new icon (currentIcoItem), so the throw
// leaves the slot out of date, Slot.sync schedules the redraw again the next
// frame, and it throws again: no tooltip, the moved stack never gets its icon in
// the new cell, and one exception per frame (the game's log prints the repeat
// once).
//
// Patch: the `NullCheck u` in front of that call becomes `JNull u -> <end of the
// block>`, the same target the `sk.id` test jumps to when the skill is neither
// Purge nor Inhalation. Without a unit there is no held censer, so no helper, as
// for any other skill. One op replaced; op count, registers and every other
// jump unchanged.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The `NullCheck u` op in front of `u.getCurrentEquippedWeapon(true)`.
    at: usize,
    unit: Reg,
    /// Absolute op index the block's `sk.id` test skips to.
    end: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    // Static SkillTip.getHelpers(sk, lvl, u: st.Unit, stats: st.UnitStats, parent: h2d.Object).
    let unit_t = obj_type(code, "st.Unit")?;
    let stats_t = obj_type(code, "st.UnitStats")?;
    let hits: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| {
            let a = fun_args(code, f);
            s(code, f.name) == "getHelpers" && a.len() == 5 && a[2] == unit_t && a[3] == stats_t
        })
        .collect();
    let [f] = hits[..] else {
        bail!(
            "{} getHelpers(sk, lvl, st.Unit, st.UnitStats, parent) functions, want 1",
            hits.len()
        );
    };
    let fi = fun_index(code, f.findex)?;
    let unit = Reg(2);
    let weapon = method(code, unit_t, "getCurrentEquippedWeapon")?.findex;

    let calls: Vec<usize> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Call2 { fun, arg0, .. } if *fun == weapon && *arg0 == unit => Some(i),
            _ => None,
        })
        .collect();
    let [c] = calls[..] else {
        bail!(
            "getHelpers: {} u.getCurrentEquippedWeapon calls, want 1",
            calls.len()
        );
    };
    if c < 3 {
        bail!("getHelpers: getCurrentEquippedWeapon call too early");
    }
    let at = c - 2;
    if !matches!(f.ops[c - 1], Opcode::Bool { .. }) {
        bail!("getHelpers: no Bool argument before getCurrentEquippedWeapon");
    }
    // The `sk.id != "Inhalation"` test right before the block gives its end.
    let Opcode::JNotEq { offset, .. } = f.ops[at - 1] else {
        bail!("getHelpers: no sk.id test before the held censer block");
    };
    let end = (at as i64 + offset as i64) as usize;
    if end <= c || end >= f.ops.len() {
        bail!("getHelpers: held censer block end {end} out of range");
    }
    match f.ops[at] {
        Opcode::NullCheck { reg } if reg == unit => {}
        Opcode::JNull { reg, offset }
            if reg == unit && at as i64 + 1 + offset as i64 == end as i64 =>
        {
            bail!("getHelpers: already applied");
        }
        _ => bail!("getHelpers: no NullCheck u before getCurrentEquippedWeapon"),
    }
    // Both strings the block tests for, read just before it.
    let names: Vec<&str> = f.ops[at.saturating_sub(6)..at]
        .iter()
        .filter_map(|op| match op {
            Opcode::GetGlobal { global, .. } => crate::job_xp::const_str(code, *global),
            _ => None,
        })
        .collect();
    if names != ["Purge", "Inhalation"] {
        bail!("getHelpers: held censer block tests {names:?}");
    }
    Ok(Plan { fi, at, unit, end })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    f.ops[p.at] = Opcode::JNull {
        reg: p.unit,
        offset: (p.end - p.at - 1) as i32,
    };
    eprintln!(
        "patched censer tip fn@{}: no held-censer helper without a unit (op {} -> {})",
        f.findex.0, p.at, p.end
    );
}

/// Makes Purge / Inhalation skill helpers safe without a unit, or leaves `code` untouched and logs why.
pub(crate) fn patch_censer_tip(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("censer tip skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, end) = (p.fi, p.at, p.end);
        let mut code = read(&image);
        patch_censer_tip(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(a.regs, b.regs);
        assert_eq!(a.ops.len(), b.ops.len());
        for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
            if i != at {
                assert_eq!(format!("{x:?}"), format!("{y:?}"), "op {i}");
            }
        }
        assert!(matches!(a.ops[at], Opcode::NullCheck { reg: Reg(2) }));
        let Opcode::JNull {
            reg: Reg(2),
            offset,
        } = b.ops[at]
        else {
            panic!("no JNull u at {at}");
        };
        assert_eq!(at as i64 + 1 + offset as i64, end as i64);
        // The skipped-to op is where the non-Purge/Inhalation test lands too.
        let Opcode::JNotEq { offset: o, .. } = b.ops[at - 1] else {
            panic!("no sk.id test");
        };
        assert_eq!(at as i64 + o as i64, end as i64);
        // Every getCurrentEquippedWeapon(u) call is now behind the null test.
        let weapon = method(
            &back,
            obj_type(&back, "st.Unit").unwrap(),
            "getCurrentEquippedWeapon",
        )
        .unwrap()
        .findex;
        assert!(b.ops[at..end]
            .iter()
            .any(|op| matches!(op, Opcode::Call2 { fun, arg0: Reg(2), .. } if *fun == weapon)));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_censer_tip(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let ser = |c: &Bytecode| {
            let mut v = Vec::new();
            c.serialize(&mut v).expect("write");
            v
        };
        let base = ser(&orig);
        // No null check in front of the call, a different block end, other tested strings.
        let edits: [fn(&mut Function, usize); 3] = [
            |f, at| f.ops[at] = Opcode::Nop,
            |f, at| {
                if let Opcode::JNotEq { offset, .. } = &mut f.ops[at - 1] {
                    *offset = 1;
                }
            },
            |f, at| f.ops[at - 2] = f.ops[at - 5].clone(),
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code.functions[p.fi], p.at);
            let before = ser(&code);
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_censer_tip(&mut code);
            assert!(ser(&code) == before, "edit {k} changed the code");
            assert!(before != base, "edit {k} was a no-op");
        }
    }
}
