// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: the tavern's daily report on a client shows every value as a loss
// (vanilla bug, Tavern.hx:734 / :770).
//
// `st.player.Tavern.doService` (the host's nightly tavern simulation) builds
// the day's `TavernResumeData`, links it to the previous day and publishes it:
//
//   var resumeData = sim.computeRest();
//   ...
//   if (history.length > 0) resumeData.previous = history[history.length - 1];
//   history.push(resumeData);             // :734, networkSetBitCond(history)
//   sim.computeRestEmployees();
//   for (r in sim.computeTavernRank()) { notify("NotifyTavernRank"); rewards; notify(...) }
//   resumeData.done();                    // :770, counters <- progress counters
//
// `done()` fills `counters` (silver, prestige, clients, satisfaction, ... as the
// running totals of the "Tavern" progress counters); the report shows each value
// as `data.get(id) - data.previous.get(id)` (TavernResumeValue, compare: true).
// `history` is a synced hxbit ArrayProxy and TavernResumeData a plain
// Serializable: the first flush that carries the new entry sends it whole, and
// later flushes send only its uid -- a client never sees a later change to it.
// Every RPC flushes pending properties first (NetworkHost.beforeRPC ->
// flushProps). Between the push and `done()` the host sends RPCs whenever the
// rank changes (Controller.notifyCustom for NotifyTavernRank / NotifyPathTitle /
// NotifyGainDialog, Inventory.netAddItem for the reward item) or an employee
// RPC fires in computeRestEmployees. On those nights the client gets the entry
// with empty counters: every `get` is 0, every delta is `-previous`, so the whole
// report is red and the tavern looks reset; the host's own copy is filled in
// place and is fine. The broken copy stays until the client reloads (a fullSync
// re-sends the host's complete entry).
//
// The fix: publish the entry once it is complete. The `history.push` and its
// `networkSetBitCond` move from :734 to right after `resumeData.done()`:
//   - op `Call2 push(history.array, resumeData)` at :734 becomes a JAlways over
//     the rest of the push block (the history reads in front of it are harmless);
//   - a copy of the push block (reads `this.history` again, pushes, marks the
//     bit), on fresh registers, is inserted after `done()`.
// Nothing between the two points reads `history` (checked here for doService;
// computeRestEmployees / computeTavernRank never touch it), and `previous` is
// still set before the push. Host behaviour is unchanged except for the order.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// First op of the push block: `GetThis history`.
    start: usize,
    /// `Call2 push(array, resume)` inside the block.
    push: usize,
    /// One past the block's last op (`CallMethod networkSetBitCond`).
    end: usize,
    /// `Call1 done(resume)`.
    done: usize,
}

const BLOCK: usize = 12;

fn named(code: &Bytecode, f: RefFun) -> Option<&str> {
    code.functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| s(code, g.name))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let tavern_t = obj_type(code, "st.player.Tavern")?;
    let resume_t = obj_type(code, "st.player.tavern.TavernResumeData")?;
    let history = field(code, tavern_t, "history")?.0;
    let previous = field(code, resume_t, "previous")?.0;
    let done_f = method(code, resume_t, "done")?.findex;
    let service = method(code, tavern_t, "doService")?;
    let fi = fun_index(code, service.findex)?;
    let o = &service.ops;
    let rt = |r: &Reg| service.regs[r.0 as usize];

    // The single `resumeData.done()`; its argument is the day's entry.
    let dones: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::Call1 { fun, .. } if fun == done_f))
        .collect();
    let [done] = dones[..] else {
        bail!("Tavern.doService: {} done() calls (want 1)", dones.len());
    };
    let Opcode::Call1 { arg0: resume, .. } = o[done] else {
        unreachable!()
    };
    if rt(&resume) != resume_t {
        bail!("Tavern.doService: done() is not called on a TavernResumeData");
    }

    // `resume.previous = ...` then the push block right behind it.
    let prevs: Vec<usize> = (0..done)
        .filter(|&i| {
            matches!(o[i], Opcode::SetField { obj, field, .. } if obj == resume && field == previous)
        })
        .collect();
    let [prev] = prevs[..] else {
        bail!("Tavern.doService: {} previous stores (want 1)", prevs.len());
    };
    let start = prev + 1;
    let end = start + BLOCK;
    if end > done {
        bail!("Tavern.doService: no room for the push block before done()");
    }
    let b = &o[start..end];
    // GetThis h = history; NullCheck h; Field x = h.array; SafeCast a = x; NullCheck a;
    // Call2 _ = push(a, resume); Field m = h.obj; JNull m -> end; Field m = h.obj;
    // NullCheck m; Field bit = h.bit; CallMethod networkSetBitCond(m, bit)
    let ok = match b {
        [Opcode::GetThis { dst: h, field: f0 }, Opcode::NullCheck { reg: h1 }, Opcode::Field {
            dst: x, obj: h2, ..
        }, Opcode::SafeCast { dst: a, src: x1 }, Opcode::NullCheck { reg: a1 }, Opcode::Call2 {
            fun,
            arg0: a2,
            arg1: r,
            ..
        }, Opcode::Field {
            dst: m, obj: h3, ..
        }, Opcode::JNull { reg: m1, offset: 4 }, Opcode::Field {
            dst: m2, obj: h4, ..
        }, Opcode::NullCheck { reg: m3 }, Opcode::Field { obj: h5, .. }, Opcode::CallMethod { args, .. }] => {
            *f0 == history
                && [h1, h2, h3, h4, h5].iter().all(|v| *v == h)
                && x1 == x
                && a1 == a
                && a2 == a
                && *r == resume
                && named(code, *fun) == Some("push")
                && m1 == m
                && m2 == m
                && m3 == m
                && args.first() == Some(m)
        }
        _ => false,
    };
    if !ok {
        bail!("Tavern.doService: op {start}.. is not the history push block (or already moved)");
    }
    let push = start + 5;

    // Between the block and done(): nothing reads history, nothing returns, no jump
    // leaves the stretch past done() + 1 or enters it from outside.
    for i in end..done {
        if matches!(o[i], Opcode::GetThis { field, .. } if field == history) {
            bail!("Tavern.doService: op {i} reads history before done()");
        }
        if matches!(
            o[i],
            Opcode::Ret { .. } | Opcode::Throw { .. } | Opcode::Rethrow { .. }
        ) {
            bail!("Tavern.doService: op {i} leaves before done()");
        }
        if jump_targets(service, i)
            .iter()
            .any(|&t| t < end || t > done)
        {
            bail!("Tavern.doService: op {i} jumps out of the stretch before done()");
        }
    }
    for i in (0..o.len()).filter(|&i| i < end || i > done) {
        if jump_targets(service, i)
            .iter()
            .any(|&t| t > end && t <= done + 1)
        {
            bail!("Tavern.doService: op {i} jumps into the stretch before done()");
        }
    }
    Ok(Plan {
        fi,
        start,
        push,
        end,
        done,
    })
}

/// `op` with every register mapped through `m` (only the op kinds of the block).
fn remap(op: &Opcode, m: &mut impl FnMut(Reg) -> Reg) -> Opcode {
    match op.clone() {
        Opcode::GetThis { dst, field } => Opcode::GetThis { dst: m(dst), field },
        Opcode::NullCheck { reg } => Opcode::NullCheck { reg: m(reg) },
        Opcode::Field { dst, obj, field } => Opcode::Field {
            dst: m(dst),
            obj: m(obj),
            field,
        },
        Opcode::SafeCast { dst, src } => Opcode::SafeCast {
            dst: m(dst),
            src: m(src),
        },
        Opcode::Call2 {
            dst,
            fun,
            arg0,
            arg1,
        } => Opcode::Call2 {
            dst: m(dst),
            fun,
            arg0: m(arg0),
            arg1: m(arg1),
        },
        Opcode::JNull { reg, offset } => Opcode::JNull {
            reg: m(reg),
            offset,
        },
        Opcode::CallMethod { dst, field, args } => Opcode::CallMethod {
            dst: m(dst),
            field,
            args: args.into_iter().map(&mut *m).collect(),
        },
        o => unreachable!("unexpected op in the push block: {o:?}"),
    }
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let Opcode::Call1 { arg0: resume, .. } = f.ops[p.done] else {
        unreachable!()
    };
    // Fresh registers for every register the block touches except the entry.
    let mut used: Vec<Reg> = vec![];
    let mut note = |r: Reg| {
        if r != resume && !used.contains(&r) {
            used.push(r);
        }
    };
    for op in &f.ops[p.start..p.end] {
        let _ = remap(op, &mut |r| {
            note(r);
            r
        });
    }
    let base = f.regs.len() as u32;
    for r in &used {
        f.regs.push(f.regs[r.0 as usize]);
    }
    let mut map = |r: Reg| {
        used.iter()
            .position(|u| *u == r)
            .map_or(r, |k| Reg(base + k as u32))
    };
    let copy: Vec<Opcode> = f.ops[p.start..p.end]
        .iter()
        .map(|o| remap(o, &mut map))
        .collect();
    // Skip the original push and bit: JAlways from the push to the block's end.
    f.ops[p.push] = Opcode::JAlways {
        offset: (p.end - p.push - 1) as i32,
    };
    let line = f.debug_info.as_ref().map(|d| d[p.push]);
    insert_ops(f, p.done + 1, copy);
    if let (Some(dbg), Some(line)) = (&mut f.debug_info, line) {
        for d in &mut dbg[p.done + 1..p.done + 1 + BLOCK] {
            *d = line;
        }
    }
    eprintln!(
        "patched tavern resume fn@{} ops {}..{} -> after op {}: the day's report is synced once done() filled it",
        f.findex.0, p.start, p.end, p.done
    );
}

/// Publishes the tavern's daily report to clients only once it is complete, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_tavern_resume(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("tavern resume skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, write, HLBOOT};

    /// doService gains one moved block; every other function is untouched; a
    /// second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, start, push, end, done) = (p.fi, p.start, p.push, p.end, p.done);
        let mut code = read(&image);
        patch_tavern_resume(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());
        // Same as the original with BLOCK ops inserted after done(), except the push.
        let mut expect = a.clone();
        expect.ops[push] = Opcode::JAlways {
            offset: (end - push - 1) as i32,
        };
        crate::asm::testutil::shifted(&expect, b, done + 1, BLOCK);
        // The copy is the block on fresh registers; the entry register is kept.
        let Opcode::Call1 { arg0: resume, .. } = a.ops[done] else {
            panic!()
        };
        for k in 0..BLOCK {
            let (x, y) = (&a.ops[start + k], &b.ops[done + 1 + k]);
            assert_eq!(
                std::mem::discriminant(x),
                std::mem::discriminant(y),
                "copy op {k}"
            );
        }
        match &b.ops[done + 1 + 5] {
            Opcode::Call2 { arg1, arg0, .. } => {
                assert_eq!(*arg1, resume);
                assert!(arg0.0 as usize >= a.regs.len());
            }
            o => panic!("copy push: {o:?}"),
        }
        assert!(b.regs.len() > a.regs.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_tavern_resume(&mut again);
        assert!(write(&again) == patched);
    }

    /// A doService that already reads history between the push and done() is refused.
    #[test]
    fn refuses_history_read_before_done() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        let f = &mut code.functions[p.fi];
        // Put a copy of the block's `GetThis history` into the stretch.
        f.ops[p.end] = f.ops[p.start].clone();
        let before = format!("{:?}", code.functions[p.fi].ops);
        assert!(plan(&code).is_err());
        patch_tavern_resume(&mut code);
        assert_eq!(format!("{:?}", code.functions[p.fi].ops), before);
    }

    /// computeRestEmployees / computeTavernRank (and their direct callees) never
    /// read Tavern.history, so the late push changes nothing they see.
    #[test]
    fn simulation_never_reads_history() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let code = read(&image);
        let tavern_t = obj_type(&code, "st.player.Tavern").unwrap();
        let sim_t = obj_type(&code, "st.player.tavern.TavernSimulation").unwrap();
        let history = field(&code, tavern_t, "history").unwrap().0;
        let by_findex: std::collections::HashMap<usize, usize> = code
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| (f.findex.0, i))
            .collect();
        let reads = |f: &Function| {
            f.ops.iter().any(|o| match o {
                Opcode::GetThis { field, .. } => f.regs[0] == tavern_t && *field == history,
                Opcode::Field { obj, field, .. } => {
                    f.regs[obj.0 as usize] == tavern_t && *field == history
                }
                _ => false,
            })
        };
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<(usize, usize)> = ["computeRestEmployees", "computeTavernRank"]
            .iter()
            .map(|n| (by_findex[&method(&code, sim_t, n).unwrap().findex.0], 0))
            .collect();
        while let Some((i, depth)) = stack.pop() {
            if !seen.insert(i) || depth > 6 {
                continue;
            }
            let f = &code.functions[i];
            assert!(
                !reads(f),
                "fn@{} {} reads history",
                f.findex.0,
                s(&code, f.name)
            );
            for o in &f.ops {
                let callee = match o {
                    Opcode::Call0 { fun, .. }
                    | Opcode::Call1 { fun, .. }
                    | Opcode::Call2 { fun, .. }
                    | Opcode::Call3 { fun, .. }
                    | Opcode::Call4 { fun, .. }
                    | Opcode::CallN { fun, .. } => by_findex.get(&fun.0).copied(),
                    _ => None,
                };
                if let Some(c) = callee {
                    stack.push((c, depth + 1));
                }
            }
        }
        eprintln!("checked {} functions", seen.len());
    }
}
