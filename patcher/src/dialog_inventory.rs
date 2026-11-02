// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a shared dialog never hides a player's inventory panels (owner
// decision 2026-11-02: the panels stay open and drawn over the dialog).
//
// Vanilla hides them on one machine only: `Controller.playConfessionSetup` (the
// host builds every confession, whoever talks) ends with
// `game.ui.showInventory(false)` and nothing restores it, so after any player's
// confession the host's inventory was closed. Replicated dialogs on the other
// machines (`Dialog.onAlive`) only open the inventory for a trade dialog and
// never hide it.
//
// `world.Ruins.onSwitched` hides them the same way on the first entry into a
// ruins, on every machine (owner rule: a scene setup never closes a window).
//
// In front of each of those calls: `if (this.game.get_isMulti())` skip it.
// Solo play keeps the vanilla hide.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

/// The `game.ui.showInventory(false)` sites that never restore, by class and
/// method: the host's confession start and the first entry into a ruins.
const SITES: [(&str, &str); 2] = [
    ("st.Controller", "playConfessionSetup"),
    ("world.Ruins", "onSwitched"),
];

struct Plan {
    fi: usize,
    /// Op index of `GetThis game` that starts `game.ui.showInventory(false)`.
    at: usize,
    game: (RefField, RefType),
    is_multi: RefFun,
    bool_t: RefType,
}

fn plan(code: &Bytecode, (cls, name): (&str, &str)) -> Result<Plan> {
    let this_t = obj_type(code, cls)?;
    let game_t = obj_type(code, "Game")?;
    let gui_t = obj_type(code, "ui.GameUI")?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let game = field(code, this_t, "game")?;
    if game.1 != game_t {
        bail!("{cls}.game is not a Game");
    }
    let ui = field(code, game_t, "ui")?.0;
    let show = method(code, gui_t, "showInventory")?.findex;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], bool_t) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let f = method(code, this_t, name)?;
    let o = &f.ops;
    // GetThis game; NullCheck; Field ui; NullCheck; Bool false; Call2 showInventory
    let sites: Vec<usize> = (0..o.len().saturating_sub(5))
        .filter(|&at| {
            matches!(o[at], Opcode::GetThis { field, .. } if field == game.0)
                && matches!(o[at + 1], Opcode::NullCheck { .. })
                && matches!(o[at + 2], Opcode::Field { field, .. } if field == ui)
                && matches!(o[at + 3], Opcode::NullCheck { .. })
                && matches!(o[at + 4], Opcode::Bool { value, .. } if !value.0)
                && matches!(o[at + 5], Opcode::Call2 { fun, .. } if fun == show)
        })
        .collect();
    let [at] = sites[..] else {
        bail!(
            "{cls}.{name}: {} `game.ui.showInventory(false)` sites (want 1)",
            sites.len()
        );
    };
    if at > 0 && matches!(o[at - 1], Opcode::JTrue { .. }) {
        bail!("{cls}.{name}: the co-op test is already in front (applied)");
    }
    if (0..o.len()).any(|i| {
        jump_targets(f, i)
            .iter()
            .any(|t| (at + 1..at + 6).contains(t))
    }) {
        bail!("{cls}.{name}: a jump lands inside the showInventory call");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        at,
        game,
        is_multi,
        bool_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let base = f.regs.len() as u32;
    let (g, b) = (Reg(base), Reg(base + 1));
    f.regs.extend([p.game.1, p.bool_t]);
    // JTrue (at + 3) skips the 6 vanilla ops of the call.
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::GetThis {
                dst: g,
                field: p.game.0,
            },
            Opcode::NullCheck { reg: g },
            Opcode::Call1 {
                dst: b,
                fun: p.is_multi,
                arg0: g,
            },
            Opcode::JTrue { cond: b, offset: 6 },
        ],
    );
    eprintln!(
        "patched dialog inventory fn@{} op {}: co-op keeps the inventory open here",
        f.findex.0, p.at
    );
}

/// Keeps the inventory panels open in co-op where vanilla hides them for good
/// (confession start, ruins entry); each site is validated and skipped alone.
pub(crate) fn patch_dialog_inventory(code: &mut Bytecode) {
    for site in SITES {
        match plan(code, site) {
            Ok(p) => apply(code, p),
            Err(e) => crate::skipped(format!("dialog inventory skipped: {e:#}")),
        }
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
        let plans: Vec<(usize, usize)> = SITES
            .iter()
            .map(|s| {
                let p = plan(&orig, *s).expect("plan");
                (p.fi, p.at)
            })
            .collect();
        let mut code = read(&image);
        patch_dialog_inventory(&mut code);
        let once = write(&code);
        let back = read(&once);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, plans.iter().all(|p| p.0 != i), "function #{i}");
        }
        for &(fi, at) in &plans {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(b.ops.len(), a.ops.len() + 4);
            let map = |i: usize| if i < at { i } else { i + 4 };
            for i in 0..a.ops.len() {
                let t: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
                assert_eq!(jump_targets(b, map(i)), t, "op {i} jumps");
                if t.is_empty() {
                    assert_eq!(
                        format!("{:?}", b.ops[map(i)]),
                        format!("{:?}", a.ops[i]),
                        "op {i}"
                    );
                }
            }
            assert_eq!(jump_targets(b, at + 3), [at + 10]);
            check_types(&back, b, at..at + 4);
            check_flow(b);
        }
        // Idempotent.
        let mut again = read(&once);
        assert!(SITES.iter().all(|s| plan(&again, *s).is_err()));
        patch_dialog_inventory(&mut again);
        assert!(write(&again) == once);
    }
}
