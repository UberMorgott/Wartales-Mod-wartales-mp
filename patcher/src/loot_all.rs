// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// "Take all" button on the post-battle loot screen in co-op.
//
// `ui.win.Debrief.init` (Debrief.hx:92) already builds a loot-all button, but
// only outside co-op:
//
//   if (!game.isMulti) {
//       lootBtn = createNew("button", .., ["LootAll"], {id: "lootBtn"}).obj;
//       lootBtn.onClick = lootAll;
//       lootBtn.enable = hasLoot();
//   }
//
// The `JTrue isMulti` that skips the block gets offset 0, so the button is
// built in co-op too (same icon "LootAll", same localized cdb tooltip, same
// place, enabled while the loot holds something that is not a corpse). The op
// count and every other jump stay the same.
//
// Why this is co-op safe as vanilla wrote it: the button is a networkable
// `ui.comp.Button` (its constructor sets `networkable = true`; Debrief only
// turns that off for its per-player repair / cure buttons). In the shared
// Debrief window a client's click is `Element.netButton` ->
// `Window.triggerClick` RPC, and the host runs `lootAll` itself
// (`Window._triggerClick` -> interactive click -> `onClick`):
//
//   for (it in debrief.loot.content) if (it != null && it.k.kind != "Corpse") {
//       PlayerInventory.addItem(game.getActionPlayer().inventory, it.k, it.count);
//       debrief.loot.useExactItem(it.k, it.count, null);
//   }
//   netRebuild();
//
// `getActionPlayer()` is the RPC's client during the call (the host itself for
// its own click), so the items go to the player who clicked; the host has
// authority over the loot and every player inventory, so all of it is one
// host-side step that replicates like a drag of each stack would. The loot is
// one shared pool (battle.Debrief.loot); two players clicking at once are
// serialized on the host, the second finds the pool empty. Player inventories
// have no size limit (`st.Inventory.maxSize` is only set for camp tools), so
// `addItem` never refuses and no item is lost; carry weight is the vanilla
// soft overload, as with a manual take.
//
// The button is not a wait-all-players button (only closeBtn is), so the
// coop_gates G1 click/push changes do not apply to it.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The `JTrue isMulti` that skips the loot button block.
    gate_at: usize,
}

fn calls(f: &Function, want: RefFun) -> bool {
    f.ops.iter().any(|o| {
        matches!(o,
            Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. }
            | Opcode::Call3 { fun, .. } | Opcode::Call4 { fun, .. } | Opcode::CallN { fun, .. }
            if *fun == want)
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let debrief_t = obj_type(code, "ui.win.Debrief")?;
    let init = method(code, debrief_t, "init")?;
    let fi = fun_index(code, init.findex)?;
    let loot_all = method(code, debrief_t, "lootAll")?;
    let has_loot = method(code, debrief_t, "hasLoot")?.findex;
    let game_t = obj_type(code, "Game")?;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    let action_player = method(code, game_t, "getActionPlayer")?.findex;
    let use_exact = proto(code, obj_type(code, "st.Inventory")?, "useExactItem")?;

    // lootAll must stay the host-side take: the clicking player's inventory, the
    // loot's own use. Anything else (a local-only take) would not be co-op safe.
    if !calls(loot_all, action_player) || !calls(loot_all, use_exact) {
        bail!("Debrief.lootAll: no getActionPlayer + useExactItem take");
    }

    let closures: Vec<usize> = (0..init.ops.len())
        .filter(|&i| matches!(init.ops[i], Opcode::InstanceClosure { fun, .. } if fun == loot_all.findex))
        .collect();
    let [click_at] = closures[..] else {
        bail!(
            "Debrief.init: expected one lootAll closure, found {}",
            closures.len()
        );
    };

    // The nearest `m = game.get_isMulti(); JTrue m` before the closure.
    let gate = (1..click_at).rev().find_map(|j| {
        let Opcode::JTrue { cond, offset } = init.ops[j] else {
            return None;
        };
        matches!(init.ops[j - 1], Opcode::Call1 { dst, fun, .. } if dst == cond && fun == is_multi)
            .then_some((j, offset))
    });
    let Some((gate_at, offset)) = gate else {
        bail!("Debrief.init: no isMulti test before the loot button");
    };
    if offset == 0 {
        bail!("Debrief.init: already applied");
    }
    let Ok(skip) = usize::try_from(offset) else {
        bail!("Debrief.init: isMulti test jumps backwards");
    };
    let end = gate_at + 1 + skip;
    if end <= click_at || end > init.ops.len() {
        bail!("Debrief.init: isMulti test does not skip the loot button");
    }
    let block = &init.ops[gate_at + 1..end];
    // The block is the loot button only: binds lootAll, enables on hasLoot,
    // keeps the Button's own networkable = true, and nothing jumps into it.
    if !block
        .iter()
        .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == has_loot))
    {
        bail!("Debrief.init: loot button block does not test hasLoot");
    }
    let lootbtn = field(code, debrief_t, "lootBtn")?.0;
    if !block
        .iter()
        .any(|o| matches!(o, Opcode::SetThis { field, .. } if *field == lootbtn))
    {
        bail!("Debrief.init: loot button block does not set lootBtn");
    }
    let sets_networkable = block.iter().any(|o| match o {
        Opcode::SetField {
            obj: r, field: fl, ..
        } => obj(code, init.regs[r.0 as usize])
            .ok()
            .and_then(|t| t.fields.get(fl.0))
            .is_some_and(|f| s(code, f.name) == "networkable"),
        _ => false,
    });
    if sets_networkable {
        bail!("Debrief.init: loot button is not networkable");
    }
    for i in 0..init.ops.len() {
        if i == gate_at {
            continue;
        }
        if jump_targets(init, i)
            .iter()
            .any(|&t| t > gate_at + 1 && t < end && !(gate_at < i && i < end))
        {
            bail!("Debrief.init: op {i} jumps into the loot button block");
        }
    }
    Ok(Plan { fi, gate_at })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let Opcode::JTrue { cond, .. } = f.ops[p.gate_at] else {
        unreachable!()
    };
    f.ops[p.gate_at] = Opcode::JTrue { cond, offset: 0 };
    eprintln!(
        "patched loot all fn@{}: post-battle Take all button in co-op",
        f.findex.0
    );
}

/// Shows the debrief's Take all button in co-op, or leaves `code` untouched and logs why.
pub(crate) fn patch_loot_all(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("loot all skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{read, HLBOOT};

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, gate_at) = (p.fi, p.gate_at);
        let mut code = read(&image);
        patch_loot_all(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
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
            if i == gate_at {
                let Opcode::JTrue { cond, .. } = a.ops[i] else {
                    panic!("gate is not JTrue");
                };
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", Opcode::JTrue { cond, offset: 0 })
                );
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
        patch_loot_all(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    /// A gate that jumps backwards, or a jump from outside into the button block,
    /// is refused and leaves the function as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (fi, gate_at) = (p.fi, p.gate_at);
        let backwards = |f: &mut Function| {
            let Opcode::JTrue { cond, .. } = f.ops[gate_at] else {
                panic!("gate is not JTrue");
            };
            f.ops[gate_at] = Opcode::JTrue { cond, offset: -1 };
        };
        let jump_in = |f: &mut Function| {
            f.ops[0] = Opcode::JAlways {
                offset: (gate_at + 5 - 1) as i32,
            };
        };
        for (what, mutate) in [
            ("backwards", &backwards as &dyn Fn(&mut Function)),
            ("jump in", &jump_in),
        ] {
            let mut code = read(&image);
            mutate(&mut code.functions[fi]);
            let before = format!("{:?}", code.functions[fi].ops);
            assert!(plan(&code).is_err(), "{what}");
            patch_loot_all(&mut code);
            assert_eq!(format!("{:?}", code.functions[fi].ops), before, "{what}");
        }
    }
}
