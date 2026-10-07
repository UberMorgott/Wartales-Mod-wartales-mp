// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a client closes its own modal window by clicking outside it (vanilla
// bug, Window.hx:295-300).
//
// `Window.setModal` makes `windowRoot` a full-screen interactive (the modal
// backdrop) whose onClick is:
//
//   if (modal != None && allowLeaveOnClickOutside(e) && tryClose()) {
//       if (game != null && !game.isAuth) triggerClose(); else close();
//   }
//
// `triggerClose` is the hxbit RPC stub of a shared window (one the host
// replicates, `__host` set): on a client it asks the host to close it. For a
// window that only exists on this machine (`__host == null`: UnitInfo, the
// character sheet, inventory sub-windows, every window a client opens for
// itself) the stub returns at once, so on a client a click anywhere outside a
// local modal window does nothing, while on the host the same click closes it.
// Escape already makes the distinction (Game.update, Game.hx:1725-1727:
// `if (w.__host == null || isAuth) w.close()`).
//
// The fix: in front of the client's `triggerClose()` call,
//   if (this.__host == null) { close(); return; }   // the vanilla else branch
// i.e. `Field h = this.__host; JNull h -> close()`. Shared windows keep the RPC.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The op `Call1 triggerClose(this)` of the backdrop click closure.
    at: usize,
    host: (RefField, RefType),
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let window_t = obj_type(code, "ui.Window")?;
    let set_modal = method(code, window_t, "setModal")?;
    let trigger_close = method(code, window_t, "triggerClose")?.findex;
    let host = field(code, window_t, "__host")?;
    let game = field(code, window_t, "game")?.0;
    let modal = field(code, window_t, "modal")?.0;
    // The backdrop click closure: the only instance closure setModal installs.
    let closures: Vec<RefFun> = set_modal
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::InstanceClosure {
                fun, obj: Reg(0), ..
            } => Some(*fun),
            _ => None,
        })
        .collect();
    let [closure] = closures[..] else {
        bail!(
            "Window.setModal: {} instance closures (want 1)",
            closures.len()
        );
    };
    let fi = fun_index(code, closure)?;
    let f = &code.functions[fi];
    if f.regs.first() != Some(&window_t) {
        bail!("backdrop closure is not bound to ui.Window");
    }
    let o = &f.ops;
    if !o
        .iter()
        .any(|x| matches!(x, Opcode::Field { obj: Reg(0), field, .. } if *field == modal))
    {
        bail!("backdrop closure does not read modal");
    }
    if o.iter()
        .any(|x| matches!(x, Opcode::Field { obj: Reg(0), field, .. } if *field == host.0))
    {
        bail!("backdrop closure already reads __host (applied)");
    }
    let calls: Vec<usize> = o
        .iter()
        .enumerate()
        .filter(
            |(_, x)| matches!(x, Opcode::Call1 { fun, arg0: Reg(0), .. } if *fun == trigger_close),
        )
        .map(|(i, _)| i)
        .collect();
    let [at] = calls[..] else {
        bail!(
            "backdrop closure: {} triggerClose calls (want 1)",
            calls.len()
        );
    };
    // ... JTrue isAuth -> close; Call1 triggerClose; JAlways -> Ret; close(); Ret
    if at < 4 || at + 3 >= o.len() {
        bail!("backdrop closure: triggerClose at an unexpected place");
    }
    let Opcode::JTrue { cond, .. } = o[at - 1] else {
        bail!("backdrop closure: no isAuth test before triggerClose");
    };
    if jump_targets(f, at - 1) != [at + 2] {
        bail!("backdrop closure: the isAuth test does not skip to close()");
    }
    if !matches!(o[at - 2], Opcode::Field { dst, .. } if dst == cond) {
        bail!("backdrop closure: the isAuth test does not read a field");
    }
    if !o[..at]
        .iter()
        .any(|x| matches!(x, Opcode::Field { obj: Reg(0), field, .. } if *field == game))
    {
        bail!("backdrop closure: no game read before triggerClose");
    }
    if jump_targets(f, at + 1) != [at + 3] || !matches!(o[at + 1], Opcode::JAlways { .. }) {
        bail!("backdrop closure: triggerClose is not followed by a jump to the end");
    }
    if !matches!(&o[at + 2], Opcode::CallMethod { args, .. } if args[..] == [Reg(0)]) {
        bail!("backdrop closure: the else branch is not this.close()");
    }
    if !matches!(o[at + 3], Opcode::Ret { .. }) {
        bail!("backdrop closure: no Ret after close()");
    }
    // close() is the target of the isAuth test only (and of the game == null test).
    for i in 0..o.len() {
        if i != at - 1 && jump_targets(f, i).contains(&at) {
            bail!("backdrop closure: op {i} jumps onto triggerClose");
        }
    }
    Ok(Plan { fi, at, host })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let h = Reg(f.regs.len() as u32);
    f.regs.push(p.host.1);
    // After the insert the old `close()` (at + 2) sits at at + 4; the JNull is at + 1.
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::Field {
                dst: h,
                obj: Reg(0),
                field: p.host.0,
            },
            Opcode::JNull { reg: h, offset: 2 },
        ],
    );
    eprintln!(
        "patched window close fn@{} op {}: a client closes its own (unshared) modal window on a click outside",
        f.findex.0, p.at
    );
}

/// Lets a co-op client close a local modal window by clicking outside it, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_window_close(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("window close skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{read, write, HLBOOT};

    /// Only the backdrop closure changes: two ops in front of triggerClose, one
    /// register; jumps still land on the same ops; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, host) = (p.fi, p.at, p.host);
        let mut code = read(&image);
        patch_window_close(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let h = Reg(a.regs.len() as u32);
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        assert_eq!(b.regs[a.regs.len()..], [host.1]);
        assert_eq!(b.ops.len(), a.ops.len() + 2);
        assert_eq!(
            format!("{:?}", b.ops[at]),
            format!(
                "{:?}",
                Opcode::Field {
                    dst: h,
                    obj: Reg(0),
                    field: host.0
                }
            )
        );
        assert_eq!(jump_targets(b, at + 1), [at + 4]);
        assert!(matches!(b.ops[at + 1], Opcode::JNull { reg, .. } if reg == h));
        let map = |i: usize| if i < at { i } else { i + 2 };
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

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_window_close(&mut again);
        assert!(write(&again) == patched);
    }

    /// A backdrop closure without the isAuth branch is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (fi, at) = (p.fi, p.at);
        let mut code = read(&image);
        code.functions[fi].ops[at - 1] = Opcode::Label;
        let before = format!("{:?}", code.functions[fi].ops);
        assert!(plan(&code).is_err());
        patch_window_close(&mut code);
        assert_eq!(format!("{:?}", code.functions[fi].ops), before);
    }
}
