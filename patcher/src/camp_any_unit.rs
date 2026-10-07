// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: in camp any player may move any party unit, assign it to a camp tool
// (workshop, kitchen, guard, rest slot...) and send it between camp and reserve.
//
// Vanilla gates these camp actions on `st.Unit.isControllable()`, which outside
// battle is `isOwnedBy(game.me)`, i.e. only the unit's owner. The gates are
// client-side UI only:
//
//   G1 CampEntryEntity.canDrag (CampEntryEntity.hx:739)
//        case Unit(u): return u.isControllable();
//      the drag gate; one drag both moves a unit and assigns it (a drop on a
//      tool is GridData.moveEntry(..., assigned = Tool(t))).
//   G2 CampMode reserve thumbnail tip closure (CampMode.hx:115)
//        the "send to camp" action is added only if u.isControllable()
//   G3 CampMode reserve thumbnail click closure (CampMode.hx:137)
//        if (u.isControllable()) { camp.sendToCamp(u); syncReserve(); }
//   G4 Camp.removeUnit (Camp.hx:1338)
//        if (u.isControllable()) camp.sendToReserve(u, null);
//
// The host checks no ownership on the RPCs behind them (GridData.netMoveEntry,
// Camp.sendToCamp / sendToReserve: State.networkAllow accepts every RPC, the
// impls test only geometry / flags), so a non-owner's request is already
// accepted and replicated; only the local gates refuse it.
//
// The fix: at exactly those four call sites `isControllable(u)` becomes
// `isPlayer(u)` (owner != null): any party unit, whoever owns it. Both are
// `(st.Unit) -> bool`, so it is a one-op swap; registers, op count and jumps are
// unchanged. In single player `me` owns every party unit, so outside battle
// isControllable == isPlayer and nothing changes there. isControllable itself
// is untouched (battle control, item actions, choosers keep owner checks).
//
// Sites are found by the call's debug position (file:line); exactly one call per
// position and four in all are required, else the pass is skipped (logged).

use super::*;

/// The gated call sites: (debug file, line).
const SITES: [(&str, usize); 4] = [
    ("src/world/camp/CampEntryEntity.hx", 739),
    ("src/world/camp/CampMode.hx", 115),
    ("src/world/camp/CampMode.hx", 137),
    ("src/st/player/Camp.hx", 1338),
];

struct Plan {
    /// (function index, op index) per SITES entry, in order.
    sites: Vec<(usize, usize)>,
    is_player: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let unit_t = obj_type(code, "st.Unit")?;
    let ctrl = method(code, unit_t, "isControllable")?;
    let player = method(code, unit_t, "isPlayer")?;
    for f in [ctrl, player] {
        if fun_args(code, f) != [unit_t] || f.t.as_fun(code).map(|t| t.ret) != Some(bool_t) {
            bail!("st.Unit fn@{} is not (st.Unit) -> bool", f.findex.0);
        }
    }
    let is_ctrl = ctrl.findex;

    let files = SITES
        .iter()
        .map(|&(file, line)| Ok((debug_file(code, file)?, line)))
        .collect::<Result<Vec<_>>>()?;
    let mut hits: Vec<Vec<(usize, usize)>> = vec![Vec::new(); SITES.len()];
    for (fi, f) in code.functions.iter().enumerate() {
        let Some(dbg) = &f.debug_info else { continue };
        for (i, op) in f.ops.iter().enumerate() {
            if !matches!(op, Opcode::Call1 { fun, .. } if *fun == is_ctrl) {
                continue;
            }
            if let Some(k) = files.iter().position(|&p| dbg.get(i) == Some(&p)) {
                hits[k].push((fi, i));
            }
        }
    }
    let mut sites = Vec::new();
    for (k, h) in hits.iter().enumerate() {
        let (file, line) = SITES[k];
        let [site] = h[..] else {
            bail!(
                "{} isControllable calls at {file}:{line}, expected 1",
                h.len()
            );
        };
        sites.push(site);
    }

    // The two named sites must sit in the functions the brief names.
    let entry_t = obj_type(code, "world.camp.CampEntryEntity")?;
    let camp_t = obj_type(code, "st.player.Camp")?;
    for (k, owner_t, name) in [(0, entry_t, "canDrag"), (3, camp_t, "removeUnit")] {
        let f = method(code, owner_t, name)?;
        if code.functions[sites[k].0].findex != f.findex {
            bail!("{}:{} is not in {name}", SITES[k].0, SITES[k].1);
        }
    }
    Ok(Plan {
        sites,
        is_player: player.findex,
    })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    for (&(fi, i), (file, line)) in p.sites.iter().zip(SITES) {
        let f = &mut code.functions[fi];
        if let Opcode::Call1 { fun, .. } = &mut f.ops[i] {
            *fun = p.is_player;
        }
        eprintln!(
            "patched camp any unit fn@{} op {i} ({file}:{line}): isControllable -> isPlayer",
            f.findex.0
        );
    }
}

/// Lets any co-op player drag / assign / reserve any party unit in camp, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_camp_any_unit(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => eprintln!("camp any unit skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Exactly four functions change, each in one op (the Call1 target, now
    /// isPlayer); a second pass finds no site and is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(p.sites.len(), 4);
        let fis: Vec<usize> = p.sites.iter().map(|s| s.0).collect();
        let mut code = read(&image);
        patch_camp_any_unit(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops)
                && a.regs == b.regs
                && a.debug_info == b.debug_info;
            assert_eq!(same, !fis.contains(&i), "function #{i} (fn@{})", a.findex.0);
        }
        for &(fi, at) in &p.sites {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(a.ops.len(), b.ops.len());
            assert_eq!(a.regs, b.regs);
            for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                let differs = format!("{x:?}") != format!("{y:?}");
                assert_eq!(differs, i == at, "fn@{} op {i}", a.findex.0);
            }
            match (&a.ops[at], &b.ops[at]) {
                (
                    Opcode::Call1 {
                        dst: d0, arg0: r0, ..
                    },
                    Opcode::Call1 {
                        dst: d1,
                        fun,
                        arg0: r1,
                    },
                ) => {
                    assert_eq!((d0, r0), (d1, r1));
                    assert_eq!(*fun, p.is_player);
                }
                other => panic!("unexpected site ops {other:?}"),
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_camp_any_unit(&mut again);
        assert!(write(&again) == patched);
    }

    /// A missing site (here: G2 already gone) refuses the whole pass and leaves
    /// the image as it was.
    #[test]
    fn refuses_missing_site() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        let (fi, at) = p.sites[1];
        if let Opcode::Call1 { fun, .. } = &mut code.functions[fi].ops[at] {
            *fun = p.is_player;
        }
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_camp_any_unit(&mut code);
        assert!(write(&code) == before);
    }
}
