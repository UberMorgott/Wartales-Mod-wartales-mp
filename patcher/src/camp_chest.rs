// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The co-op shared camp chest exists before the host first makes camp.
//
// GameInventory.update shows the chest button (and lets #chestInventory open)
// only while `state.camp.getTool("Chest")` exists. Vanilla adds that tool in
// the Camp constructor when the game is co-op at that moment, and in
// Camp.enter on the host: `if (isAuth) { ... if (game.get_isCoopGame() &&
// !hasTool("Chest", true)) addTool(Data.item.byId.get("Chest")); }`. A camp
// made with one player (solo save, players joined later) has no chest on the
// world map until the host makes camp.
//
// The pass appends `mpCampChest(camp)` = Camp.enter's isAuth test (ops 1-4)
// and its chest block (ops 15-32) copied verbatim, and GameInventory.update
// calls it where its multi branch has the camp (before `getTool("Chest")`).
// addTool replicates the tool to the clients; once it exists, hasTool returns.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

/// Camp.enter ops: the isAuth test, then the chest block (both inclusive).
const AUTH: std::ops::RangeInclusive<usize> = 1..=4;
const BLOCK: std::ops::RangeInclusive<usize> = 15..=32;

struct Plan {
    enter_fi: usize,
    upd_fi: usize,
    /// update's op index and camp register for the call.
    at: usize,
    camp: Reg,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let camp_t = obj_type(code, "st.player.Camp")?;
    let enter = method(code, camp_t, "enter")?;
    let o = &enter.ops;
    let (
        Some(Opcode::GetThis { dst: g, .. }),
        Some(Opcode::Field { dst: b, obj, .. }),
        Some(Opcode::JFalse { cond, .. }),
    ) = (o.get(1), o.get(3), o.get(4))
    else {
        bail!("Camp.enter: no isAuth test at ops 1-4");
    };
    if obj != g || cond != b {
        bail!("Camp.enter: unexpected isAuth test");
    }
    let calls = |name: &str, i: usize| matches!(o.get(i), Some(Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } | Opcode::Call3 { fun, .. }) if fname(code, *fun) == name);
    if !(calls("get_isCoopGame", 17) && calls("hasTool", 22) && calls("addTool", 32))
        || !matches!(o.get(23), Some(Opcode::JTrue { .. }))
    {
        bail!("Camp.enter: no hasTool(\"Chest\") -> addTool block at ops 15-32");
    }
    // The block's jumps all land just past it (op 33).
    for i in BLOCK {
        if let Some(t) = jump_targets(enter, i).first() {
            if *t != BLOCK.end() + 1 {
                bail!("Camp.enter: block jump {i} -> {t}");
            }
        }
    }
    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let upd = method(code, gi_t, "update")?;
    let at = upd
        .ops
        .iter()
        .position(|op| matches!(op, Opcode::Call3 { fun, .. } if fname(code, *fun) == "getTool"))
        .context("GameInventory.update: no getTool")?;
    let Some(Opcode::Call3 { arg0: camp, .. }) = upd.ops.get(at) else {
        unreachable!()
    };
    if upd.regs[camp.0 as usize] != camp_t {
        bail!("GameInventory.update: getTool not on the camp");
    }
    // The call goes right after the camp's NullCheck, before getTool's args.
    let at = upd.ops[..at]
        .iter()
        .rposition(|op| matches!(op, Opcode::NullCheck { reg } if reg == camp))
        .context("GameInventory.update: no camp NullCheck")?
        + 1;
    if matches!(upd.ops.get(at), Some(Opcode::Call1 { .. })) {
        bail!("already applied");
    }
    if (0..upd.ops.len()).any(|i| jump_targets(upd, i).contains(&at)) {
        bail!("GameInventory.update: a jump targets op {at}");
    }
    Ok(Plan {
        enter_fi: fun_index(code, enter.findex)?,
        upd_fi: fun_index(code, upd.findex)?,
        at,
        camp: *camp,
    })
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let enter = &code.functions[p.enter_fi];
    let (regs, t) = (enter.regs.clone(), enter.t);
    let mut ops: Vec<Opcode> = enter.ops[AUTH].to_vec();
    ops.extend_from_slice(&enter.ops[BLOCK]);
    let end = ops.len();
    // isAuth false -> Ret (the block's own jumps already land on it).
    if let Opcode::JFalse { offset, .. } = &mut ops[AUTH.count() - 1] {
        *offset = (end - AUTH.count()) as i32;
    }
    let void_t = regs[1];
    if !matches!(code.types[void_t.0], Type::Void) {
        bail!("Camp.enter: reg1 is not void");
    }
    ops.push(Opcode::Ret { ret: Reg(1) });
    let dbg = enter.debug_info.as_ref().map(|d| {
        let mut v: Vec<_> = d[AUTH].to_vec();
        v.extend_from_slice(&d[BLOCK]);
        v.push(d[BLOCK.end() + 1]);
        v
    });
    let findex = next_findex(code)?;
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t,
        findex,
        regs,
        ops,
        debug_info: dbg,
        assigns: Some(vec![]),
        parent: None,
    });
    let upd = &mut code.functions[p.upd_fi];
    let v = new_reg(upd, void_t);
    insert_ops(
        upd,
        p.at,
        vec![Opcode::Call1 {
            dst: v,
            fun: findex,
            arg0: p.camp,
        }],
    );
    eprintln!(
        "patched camp chest fn@{}: a co-op host's camp always has the shared chest",
        upd.findex.0
    );
    Ok(())
}

/// Gives a co-op host's camp the shared chest tool when it lacks it, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_camp_chest(code: &mut Bytecode) {
    let r = plan(code).and_then(|p| apply(code, p));
    if let Err(e) = r {
        crate::skipped(format!("camp chest skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at) = (p.upd_fi, p.at);
        let mut code = read(&image);
        patch_camp_chest(&mut code);
        let once = write(&code);
        let back = read(&once);
        let f = back.functions.last().unwrap();
        check_types(&back, f, 0..f.ops.len());
        check_flow(f);
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        assert!(
            matches!(b.ops[at], Opcode::Call1 { fun, arg0, .. } if fun == f.findex && arg0 == p.camp)
        );
        let e = &orig.functions[p.enter_fi];
        assert_eq!(
            format!("{:?}", &f.ops[4..22]),
            format!("{:?}", &e.ops[BLOCK])
        );
        // Idempotent.
        let mut again = read(&once);
        assert!(plan(&again).is_err());
        patch_camp_chest(&mut again);
        assert!(write(&again) == once);
    }
}
