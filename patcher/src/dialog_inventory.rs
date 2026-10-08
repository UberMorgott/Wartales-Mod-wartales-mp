// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A dialog shown to a co-op player who did not start it hides that player's
// inventory panels, as it does for the player who started it.
//
// The HUD (GameInventory: inventory and chest panels) draws above dialog
// windows. Vanilla hides the inventory only on the machine that starts the
// dialog (`Controller.playConfessionSetup` ends with `ui.showInventory(false)`,
// on the host). A shared dialog (`ui.win.Dialog`, replicated) reaches every
// other machine through `Dialog.onAlive`, which only opens the inventory for a
// trade dialog (`if (showInventory) openInventory(null)`), so a client with its
// inventory open saw the panels drawn over the confession dialog.
//
// `onAlive` now calls `openInventory(showInventory)` always (same op count:
// the JFalse and the Null become `Ref arg = &flag` and a Label; the optional
// Bool is an hl ref, as vanilla passes it elsewhere): false hides
// the panels and keeps the previous state in `prevInventoryDisplay`, restored
// by the vanilla `Window.onRemove` when the dialog closes; true (null before)
// opens it as before.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// onAlive op index of `GetThis flag = this.showInventory`.
    at: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    let win_t = obj_type(code, "ui.Window")?;
    let alive = method(code, dlg_t, "onAlive")?;
    let open = method(code, win_t, "openInventory")?.findex;
    let (show_f, _) = field(code, dlg_t, "showInventory")?;
    let o = &alive.ops;
    let at = o
        .iter()
        .position(|op| matches!(op, Opcode::GetThis { field, .. } if *field == show_f))
        .context("Dialog.onAlive: no showInventory read")?;
    match (o.get(at), o.get(at + 1), o.get(at + 2), o.get(at + 3)) {
        (
            Some(Opcode::GetThis { dst: flag, .. }),
            Some(Opcode::JFalse { cond, offset: 2 }),
            Some(Opcode::Null { dst: arg }),
            Some(Opcode::Call2 {
                fun,
                arg0: Reg(0),
                arg1,
                ..
            }),
        ) if cond == flag && arg == arg1 && *fun == open => {
            if !matches!(code.types[alive.regs[flag.0 as usize].0], Type::Bool) {
                bail!("Dialog.onAlive: showInventory is not a Bool");
            }
            if !matches!(code.types[alive.regs[arg.0 as usize].0], Type::Ref(t) if t == alive.regs[flag.0 as usize])
            {
                bail!("Dialog.onAlive: openInventory's argument is not a ref<Bool>");
            }
        }
        (_, Some(Opcode::Ref { .. }), Some(Opcode::Call2 { .. }), Some(Opcode::Label)) => {
            bail!("already applied")
        }
        _ => bail!("Dialog.onAlive: no `if (showInventory) openInventory(null)` at op {at}"),
    }
    // Nothing jumps into the block (its own JFalse lands right after it).
    if (0..o.len()).any(|i| {
        jump_targets(alive, i)
            .iter()
            .any(|t| (at + 1..=at + 3).contains(t))
    }) {
        bail!("Dialog.onAlive: a jump lands inside the block");
    }
    Ok(Plan {
        fi: fun_index(code, alive.findex)?,
        at,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let (Opcode::GetThis { dst: flag, .. }, Opcode::Call2 { dst, fun, arg1, .. }) =
        (f.ops[p.at].clone(), f.ops[p.at + 3].clone())
    else {
        unreachable!("validated by plan");
    };
    f.ops[p.at + 1] = Opcode::Ref {
        dst: arg1,
        src: flag,
    };
    f.ops[p.at + 2] = Opcode::Call2 {
        dst,
        fun,
        arg0: Reg(0),
        arg1,
    };
    f.ops[p.at + 3] = Opcode::Label;
    eprintln!(
        "patched dialog inventory fn@{}: a shared dialog hides the inventory on every machine",
        f.findex.0
    );
}

/// Makes a replicated dialog hide the local inventory panels, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_dialog_inventory(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("dialog inventory skipped: {e:#}")),
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
        let (fi, at) = (p.fi, p.at);
        let mut code = read(&image);
        patch_dialog_inventory(&mut code);
        let once = write(&code);
        let back = read(&once);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i}");
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        assert_eq!(a.regs, b.regs);
        let Opcode::GetThis { dst: flag, .. } = b.ops[at] else {
            panic!("flag read moved")
        };
        let Opcode::Call2 { fun, arg1, .. } = a.ops[at + 3] else {
            unreachable!()
        };
        assert!(matches!(b.ops[at + 1], Opcode::Ref { dst, src } if dst == arg1 && src == flag));
        assert!(
            matches!(b.ops[at + 2], Opcode::Call2 { fun: f, arg0: Reg(0), arg1: x, .. } if f == fun && x == arg1)
        );
        assert!(matches!(b.ops[at + 3], Opcode::Label));
        for i in (0..a.ops.len()).filter(|i| !(at + 1..=at + 3).contains(i)) {
            assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[i]));
        }
        check_types(&back, b, at..at + 4);
        check_flow(b);
        // Idempotent.
        let mut again = read(&once);
        assert!(plan(&again).is_err());
        patch_dialog_inventory(&mut again);
        assert!(write(&again) == once);
    }
}
