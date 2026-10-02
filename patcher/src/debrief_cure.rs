// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op post-battle screen rebuilt every frame (vanilla bug, Debrief.hx:631).
//
// `ui.win.Debrief.update` runs only in co-op. It recomputes whether the repair
// and cure buttons should be enabled and, when that differs from the button's
// current `enable`, calls `rebuild()` (Window.rebuild: removeChildren + init):
//
//   var n = 0; for (u in getUnitsToRepair()) for (s in u.getInjuries()) if (s.inf.props.injury == 1) n++;
//   var nbRemedy = countWithChest(me.inventory, "Remedy") + countWithChest(me.inventory, "EstantRemedy");
//   if ((n > 0 && n <= nbRemedy) != cureBtn.enable) { rebuild(); return; }
//
// `init` sets cureBtn.enable with the same formula over `cureTot`, which it
// counts over getUnitsToHeal(), as does the cure itself (`cureAll`). update's
// loop iterates getUnitsToRepair() instead (copied from the repair block above
// it). Whenever the two unit lists give a different answer -- e.g. an injured
// unit with intact armour and a remedy in the party, or a damaged-armour unit
// with an injury and no injured unit to heal -- init enables the button, the
// next update disagrees and rebuilds, init enables it again, and so on: the
// whole window is torn down and rebuilt every frame. Every frame the loot grid,
// the Take all button and every other interactive are new objects, so hover
// restarts (the cursor flips between its button / item / default shapes) and a
// click never lands (press and release hit different objects).
//
// The fix: update's cure loop reads getUnitsToHeal(), the list init and cureAll
// use. Same signature, one Call1 target changes; op count and jumps unchanged.
// update then agrees with init and only rebuilds when the inventories or the
// units really change, as vanilla intended.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The update op `Call1 getUnitsToRepair(this)` that feeds the cure count.
    at: usize,
    heal: RefFun,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn calls(f: &Function, want: RefFun) -> bool {
    f.ops.iter().any(|o| {
        matches!(o,
            Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. }
            | Opcode::Call3 { fun, .. } | Opcode::Call4 { fun, .. } | Opcode::CallN { fun, .. }
            if *fun == want)
    })
}

fn named(code: &Bytecode, f: RefFun) -> Option<&str> {
    code.functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| s(code, g.name))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let debrief_t = obj_type(code, "ui.win.Debrief")?;
    let update = method(code, debrief_t, "update")?;
    let fi = fun_index(code, update.findex)?;
    let repair = method(code, debrief_t, "getUnitsToRepair")?;
    let heal = method(code, debrief_t, "getUnitsToHeal")?;
    if repair.t != heal.t {
        bail!("getUnitsToRepair and getUnitsToHeal differ in type");
    }
    let (repair, heal) = (repair.findex, heal.findex);
    let cure_btn = field(code, debrief_t, "cureBtn")?.0;
    // init counts cureTot over getUnitsToHeal and cureAll heals exactly those units.
    let init = method(code, debrief_t, "init")?;
    let cure_all = method(code, debrief_t, "cureAll")?;
    if !calls(init, heal) || !calls(cure_all, heal) {
        bail!("Debrief.init / cureAll do not use getUnitsToHeal");
    }

    let o = &update.ops;
    let cure = o
        .iter()
        .position(|x| matches!(x, Opcode::GetThis { field, .. } if *field == cure_btn))
        .context("Debrief.update: no cureBtn block")?;
    let lists: Vec<(usize, RefFun)> = o[cure..]
        .iter()
        .enumerate()
        .filter_map(|(i, x)| match x {
            Opcode::Call1 {
                fun, arg0: Reg(0), ..
            } if *fun == repair || *fun == heal => Some((cure + i, *fun)),
            _ => None,
        })
        .collect();
    let [(at, fun)] = lists[..] else {
        bail!(
            "Debrief.update: {} unit lists in the cure block (want 1)",
            lists.len()
        );
    };
    if fun == heal {
        bail!("Debrief.update: already applied");
    }
    // The list feeds the injury count: getInjuries on its units, then the remedy
    // counts, then the enable test and rebuild.
    let tail = &o[at..];
    let has = |name: &str| {
        tail.iter().any(|x| match x {
            Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } => named(code, *fun) == Some(name),
            _ => false,
        })
    };
    if !has("getInjuries") || !has("countWithChest") || !has("rebuild") {
        bail!("Debrief.update: cure block is not the injury / remedy / rebuild test");
    }
    Ok(Plan { fi, at, heal })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let Opcode::Call1 { dst, arg0, .. } = f.ops[p.at] else {
        unreachable!()
    };
    f.ops[p.at] = Opcode::Call1 {
        dst,
        fun: p.heal,
        arg0,
    };
    eprintln!(
        "patched debrief cure fn@{} op {}: co-op cure-button check counts the units to heal",
        f.findex.0, p.at
    );
}

/// Stops the co-op Debrief window from rebuilding every frame, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_debrief_cure(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("debrief cure skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    fn write(code: &Bytecode) -> Vec<u8> {
        let mut v = Vec::new();
        code.serialize(&mut v).expect("write");
        v
    }

    /// Only the one Call1 target in Debrief.update changes; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, heal) = (p.fi, p.at, p.heal);
        let mut code = read(&image);
        patch_debrief_cure(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), a.ops.len());
        for i in 0..a.ops.len() {
            if i == at {
                let Opcode::Call1 { dst, arg0, .. } = a.ops[i] else {
                    panic!("site is not Call1");
                };
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", Opcode::Call1 { dst, fun: heal, arg0 })
                );
            } else {
                assert_eq!(format!("{:?}", b.ops[i]), format!("{:?}", a.ops[i]), "op {i}");
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_debrief_cure(&mut again);
        assert!(write(&again) == patched);
    }

    /// A cure block without the injury count is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (fi, at) = (p.fi, p.at);
        let mut code = read(&image);
        let injuries: Vec<usize> = (at..code.functions[fi].ops.len())
            .filter(|&i| {
                matches!(code.functions[fi].ops[i], Opcode::Call1 { fun, .. }
                    if named(&code, fun) == Some("getInjuries"))
            })
            .collect();
        assert!(!injuries.is_empty());
        for i in injuries {
            code.functions[fi].ops[i] = Opcode::Label;
        }
        let before = format!("{:?}", code.functions[fi].ops);
        assert!(plan(&code).is_err());
        patch_debrief_cure(&mut code);
        assert_eq!(format!("{:?}", code.functions[fi].ops), before);
    }
}
