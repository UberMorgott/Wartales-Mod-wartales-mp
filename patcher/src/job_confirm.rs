// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Switching a unit's profession applies at once, without the confirm window.
//
// `ui.win.JobSelector.chooseJob(j)` (UnitInfo.hx:655) is the only path into the
// switch (mouse and gamepad both reach it through the selector's click closure):
//
//   if (currentJob == null || currentUnit.getTraitXp(currentJob.id) == 0) {
//       if (currentJob != null) currentUnit.removeTrait(currentJob.id);
//       onSelect(j.inf.id);
//   } else if (currentJob.id != j.inf.id) {
//       var id = currentJob.id, u = currentUnit;
//       game.ui.confirm(Text("unit_switch_job" ...), function() {
//           u.removeTrait(id);
//           onSelect(j.inf.id);
//       });
//   }
//   if (parent != null) parent.<close>(this);
//
// The vanilla no-confirm branch (no job yet, or no xp in it) is not reused: it
// would also re-take the *same* job when clicked again, which the second branch
// rules out. Instead the pass keeps the second branch and runs its "yes" closure
// in place of the window:
//
//   _confirmImpl(ui, text, yes, null)   ->   yes()
//
// and the two ops that only build the window's text closure (New + its
// constructor) become `JAlways +0`. The op count and every jump stay the same.
// `yes` is the exact closure the Confirm window would run, so the host path
// (`removeTrait` -> `_removeTrait`) and the client path (`netRemoveTrait` RPC),
// the add-trait callback and the window close are unchanged. `_confirmImpl`'s
// `confirmHook` is only set by the tutorial click trigger, not by this screen.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// `New` of the text closure; the constructor call follows it.
    text_at: usize,
    /// The `_confirmImpl` call.
    call_at: usize,
    dst: Reg,
    yes: Reg,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn reads(op: &Opcode, r: Reg) -> bool {
    // Debug text lists every register an op names; enough to prove a register is unused.
    let s = format!("{op:?}");
    s.contains(&format!("Reg({})", r.0))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let sel_t = obj_type(code, "ui.win.JobSelector")?;
    let f = method(code, sel_t, "chooseJob")?;
    let fi = fun_index(code, f.findex)?;
    let confirm = method(code, obj_type(code, "ui.GameUI")?, "_confirmImpl")?.findex;
    let remove = method(code, obj_type(code, "st.Unit")?, "removeTrait")?.findex;

    // The "yes" closure: removeTrait(unit, id) on two captured values, then a
    // one-argument closure call (the selector's onSelect(j.inf.id)).
    let is_yes = |fun: RefFun| {
        fun_index(code, fun).is_ok_and(|g| {
            let ops = &code.functions[g].ops;
            let captured = |r: Reg, before: usize| {
                ops[..before]
                    .iter()
                    .any(|o| matches!(o, Opcode::EnumField { dst, value: Reg(0), .. } if *dst == r))
            };
            let rm = ops
                .iter()
                .position(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == remove));
            rm.is_some_and(|i| {
                let Opcode::Call2 { arg0, arg1, .. } = ops[i] else {
                    return false;
                };
                captured(arg0, i)
                    && captured(arg1, i)
                    && ops[i..]
                        .iter()
                        .any(|o| matches!(o, Opcode::CallClosure { args, .. } if args.len() == 1))
            })
        })
    };
    let yes_sites: Vec<(usize, Reg)> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::InstanceClosure { dst, fun, .. } if is_yes(*fun) => Some((i, *dst)),
            _ => None,
        })
        .collect();
    let [(yes_at, yes)] = yes_sites[..] else {
        bail!(
            "chooseJob: expected one removeTrait closure, found {}",
            yes_sites.len()
        );
    };

    let calls: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::Call4 { fun, .. } if fun == confirm))
        .collect();
    let [call_at] = calls[..] else {
        if f.ops[yes_at..]
            .iter()
            .any(|o| matches!(o, Opcode::CallClosure { fun, args, .. } if *fun == yes && args.is_empty()))
        {
            bail!("chooseJob: already applied");
        }
        bail!(
            "chooseJob: expected one _confirmImpl call, found {}",
            calls.len()
        );
    };
    let Opcode::Call4 {
        dst,
        arg0: _,
        arg1: text,
        arg2,
        arg3: no,
        ..
    } = f.ops[call_at]
    else {
        unreachable!()
    };
    if arg2 != yes || yes_at > call_at {
        bail!("chooseJob: _confirmImpl does not take the removeTrait closure");
    }
    // No "no" callback is lost: the 4th argument is a Null set just before the call.
    if !f.ops[yes_at..call_at]
        .iter()
        .any(|o| matches!(o, Opcode::Null { dst } if *dst == no))
    {
        bail!("chooseJob: _confirmImpl option argument is not null");
    }
    match f.regs[yes.0 as usize].as_fun(code) {
        Some(ft) if ft.args.is_empty() && ft.ret == f.regs[dst.0 as usize] => {}
        _ => bail!("chooseJob: yes closure is not a no-arg function returning the call's type"),
    }
    let text_t = obj(code, f.regs[text.0 as usize])?;
    if !s(code, text_t.name).starts_with("hxbit.closure.") {
        bail!("chooseJob: confirm text is not an hxbit closure");
    }

    // The text closure is built by New + its constructor and read only by the call.
    let users: Vec<usize> = (0..f.ops.len())
        .filter(|&i| reads(&f.ops[i], text))
        .collect();
    let [text_at, ctor_at, at] = users[..] else {
        bail!("chooseJob: text closure register has unexpected uses {users:?}");
    };
    // Exactly the text closure's own constructor, whose (Void) result nothing reads.
    let ctor = method(code, f.regs[text.0 as usize], "__constructor__")?.findex;
    let ctor_ok = matches!(f.ops[ctor_at], Opcode::Call3 { dst, fun, arg0, .. }
        if arg0 == text && fun == ctor && matches!(code.types[f.regs[dst.0 as usize].0], Type::Void));
    if !matches!(f.ops[text_at], Opcode::New { dst } if dst == text)
        || ctor_at != text_at + 1
        || !ctor_ok
        || at != call_at
    {
        bail!("chooseJob: text closure is not New + constructor right before use");
    }
    Ok(Plan {
        fi,
        text_at,
        call_at,
        dst,
        yes,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    f.ops[p.text_at] = Opcode::JAlways { offset: 0 };
    f.ops[p.text_at + 1] = Opcode::JAlways { offset: 0 };
    f.ops[p.call_at] = Opcode::CallClosure {
        dst: p.dst,
        fun: p.yes,
        args: vec![],
    };
    eprintln!(
        "patched job confirm fn@{}: profession switch applies without the confirm window",
        f.findex.0
    );
}

/// Switches professions without the confirm window, or leaves `code` untouched and logs why.
pub(crate) fn patch_job_confirm(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("job confirm skipped: {e:#}"),
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
        let (fi, text_at, call_at, dst, yes) = (p.fi, p.text_at, p.call_at, p.dst, p.yes);
        let mut code = read(&image);
        patch_job_confirm(&mut code);
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
            let want = if i == text_at || i == text_at + 1 {
                format!("{:?}", Opcode::JAlways { offset: 0 })
            } else if i == call_at {
                format!(
                    "{:?}",
                    Opcode::CallClosure {
                        dst,
                        fun: yes,
                        args: vec![]
                    }
                )
            } else {
                assert_eq!(jump_targets(b, i), jump_targets(a, i), "op {i}");
                format!("{:?}", a.ops[i])
            };
            assert_eq!(format!("{:?}", b.ops[i]), want, "op {i}");
        }
        let confirm = method(&back, obj_type(&back, "ui.GameUI").unwrap(), "_confirmImpl")
            .unwrap()
            .findex;
        assert!(!b
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call4 { fun, .. } if *fun == confirm)));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_job_confirm(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
