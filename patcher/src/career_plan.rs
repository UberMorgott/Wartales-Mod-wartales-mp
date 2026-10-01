// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op fix for the Career Plan extra attribute point.
//
// `UnitInfo.onBonusAttributeApply` (client UI) builds the confirm closure with
// `count = getAttributeUpCounts(unit).get(k) + n`: the level-up offer for `k`
// as the *clicking* machine sees it, plus the `n` extra points bought. The
// closure's `call` runs on the host (callClosure RPC) and applies `count`
// verbatim. On a client the offer can be stale at click time (an in-flight RPC
// or a sync already moved `usedAptitudePoints`), so `get(k)` is null, `count`
// becomes just `n`, and the UI's "+2" lands as "+1". The regular level-up
// closure does not have this problem: its host side re-reads the host's own
// offer and requires `aptitudePoints > 0`.
//
// This pass makes the Career Plan path do the same:
//
//   client  count = -1 - (n + (unit.usedAptitudePoints << 2))   (was: offer + n)
//   host    if (unit == null) return;
//           if (unit.aptitudePoints <= 0) skip the whole grant (no Influence spent);
//           if (count < 0) {
//               e = -1 - count; n = e & 3;
//               if (e >> 2 != unit.usedAptitudePoints) skip the whole grant;
//               base = 0;
//               m = getAttributeUpCounts(unit);    // before usedAptitudePoints++
//               if (m != null && attr != null) { v = m.get(attr); if (v != null) base = v; }
//               count = base + n;
//           }
//           ... original body ...
//
// The offer is a function of `usedAptitudePoints`, which every grant bumps. A
// request built against an older offer (a second confirm sent before the first
// one's sync arrived) carries the old count and is dropped whole: no Influence,
// no point, no attribute. It cannot be checked against the offer itself: Career
// Plan legitimately targets attributes outside the offer (UnitInfo.hx:1839-1852
// gives every upgradable attribute its +/- buttons with `count = offer ?? 0`,
// `n` capped at `2 - count`), so `base = 0` for such an attribute is correct.
// `n` is 0..2, so two bits hold it.
//
// `count >= 0` keeps the original behaviour, so a host running this image still
// accepts the old encoding. The reverse is NOT compatible: an unpatched host
// would apply a negative count. All players run the same winmm.dll build.
//
// Both halves are validated before anything is edited; on any mismatch the pass
// changes nothing and logs why, and the other patches still apply.

use super::*;

struct Plan {
    // client: UnitInfo.onBonusAttributeApply
    client_fi: usize,
    add_at: usize,
    cnt: Reg,
    extra: Reg,
    /// The client register holding `this.unit` (null-checked) at the add.
    client_unit: Reg,
    used_f: RefField,
    // host: the L2234 closure's `call`
    host_fi: usize,
    at: usize,
    skip: usize,
    ret_null: usize,
    unit: Reg,
    count: Reg,
    attr: Reg,
    get_counts: RefFun,
    map_get: RefFun,
    apt_f: RefField,
    i32_t: RefType,
    map_t: RefType,
    dyn_t: RefType,
    null_i32_t: RefType,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let ui_t = obj_type(code, "ui.win.UnitInfo")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let get_counts = method(code, unit_t, "getAttributeUpCounts")?.findex;
    let set_used = method(code, unit_t, "set_usedAptitudePoints")?.findex;
    let upgrade = method(code, unit_t, "upgradeAttribute")?.findex;
    let (apt_f, apt_t) = field(code, unit_t, "aptitudePoints")?;
    let game_t = obj_type(code, "Game")?;
    let (auth_f, _) = field(code, game_t, "isAuth")?;

    // ---- client: `cnt = get(...) cast; ...; extra = extraPoints.n; cnt = cnt + extra`
    let client = method(code, ui_t, "onBonusAttributeApply")?;
    let client_fi = fun_index(code, client.findex)?;
    let ops = &client.ops;
    let mut sites = vec![];
    for i in 8..ops.len() {
        let Opcode::Add { dst, a, b } = ops[i] else {
            continue;
        };
        let w = &ops[i - 8..i];
        let ok = matches!(w[0], Opcode::Call1 { fun, .. } if fun == get_counts)
            && matches!(w[1], Opcode::NullCheck { .. })
            && matches!(w[2], Opcode::Call2 { .. })
            && matches!((&w[3], &w[4]), (Opcode::UnsafeCast { dst: n, .. }, Opcode::SafeCast { dst: c, src }) if c == &dst && src == n)
            && matches!(w[5], Opcode::GetThis { .. })
            && matches!(w[6], Opcode::NullCheck { .. })
            && matches!(w[7], Opcode::Field { dst: e, .. } if e == b)
            && a == dst;
        if ok {
            sites.push((i, dst, b));
        }
    }
    let [(add_at, cnt, extra)] = sites[..] else {
        bail!(
            "client: expected one `offer + n` site, found {}",
            sites.len()
        );
    };
    let Opcode::Call2 { fun: map_get, .. } = ops[add_at - 6] else {
        unreachable!()
    };
    // `this.unit`, null-checked right before getAttributeUpCounts and not rewritten up to the add.
    let Opcode::Call1 {
        arg0: client_unit, ..
    } = ops[add_at - 8]
    else {
        unreachable!()
    };
    if client.regs[client_unit.0 as usize] != unit_t
        || !matches!(ops[add_at - 9], Opcode::NullCheck { reg } if reg == client_unit)
        || ops[add_at - 7..add_at].iter().any(|op| match op {
            Opcode::Call2 { dst, .. }
            | Opcode::UnsafeCast { dst, .. }
            | Opcode::SafeCast { dst, .. }
            | Opcode::GetThis { dst, .. }
            | Opcode::Field { dst, .. } => *dst == client_unit,
            Opcode::NullCheck { .. } => false,
            _ => true,
        })
    {
        bail!("client: the unit register is not live and null-checked at the add");
    }
    let (used_f, used_t) = field(code, unit_t, "usedAptitudePoints")?;
    let i32_t = client.regs[cnt.0 as usize];
    if client.regs[extra.0 as usize] != i32_t || apt_t != i32_t || used_t != i32_t {
        bail!("client: count/n/aptitudePoints/usedAptitudePoints are not all i32");
    }
    if !matches!(code.types[i32_t.0], Type::I32) {
        bail!("client: count is not i32");
    }
    let dyn_t = match ops[add_at - 5] {
        Opcode::UnsafeCast { src, .. } => client.regs[src.0 as usize],
        _ => unreachable!(),
    };
    let null_i32_t = match ops[add_at - 5] {
        Opcode::UnsafeCast { dst, .. } => client.regs[dst.0 as usize],
        _ => unreachable!(),
    };
    if !matches!(code.types[null_i32_t.0], Type::Null(t) if t == i32_t) {
        bail!("client: offer cast is not Null<i32>");
    }
    for i in 0..ops.len() {
        if jump_targets(client, i).contains(&(add_at + 1)) {
            bail!("client: op {i} jumps right after the add");
        }
    }
    // The confirm closure constructor that receives `cnt`: CallN(ctor, [closure, unit, cnt, cost, attr]).
    let ctors: Vec<RefFun> = ops[add_at..]
        .iter()
        .filter_map(|op| match op {
            Opcode::CallN { fun, args, .. } if args.len() == 5 && args[2] == cnt => Some(*fun),
            _ => None,
        })
        .collect();
    let [ctor] = ctors[..] else {
        bail!(
            "client: expected one closure ctor taking count, found {}",
            ctors.len()
        );
    };
    let ctor_f = &code.functions[fun_index(code, ctor)?];
    let closure_t = *ctor_f.regs.first().context("closure ctor has no this")?;
    let host = method(code, closure_t, "call")?;
    let host_fi = fun_index(code, host.findex)?;

    // ---- host: `if (isAuth) { spend; usedAptitudePoints++; addAptitudePoints(-1); upgradeAttribute(unit, attr, &count) }`
    let hops = &host.ops;
    let auth: Vec<usize> = (1..hops.len())
        .filter(|&i| {
            matches!((&hops[i - 1], &hops[i]),
                (Opcode::Field { dst, field, .. }, Opcode::JFalse { cond, .. }) if *field == auth_f && cond == dst)
        })
        .collect();
    let [jf] = auth[..] else {
        bail!("host: expected one isAuth branch, found {}", auth.len());
    };
    let at = jf + 1;
    let [skip] = jump_targets(host, jf)[..] else {
        unreachable!()
    };
    let ups: Vec<usize> = (2..hops.len())
        .filter(|&i| matches!(hops[i], Opcode::Call4 { fun, .. } if fun == upgrade))
        .collect();
    let [up] = ups[..] else {
        bail!(
            "host: expected one upgradeAttribute call, found {}",
            ups.len()
        );
    };
    let (
        Opcode::Mov {
            dst: tmp,
            src: count,
        },
        Opcode::Ref { dst: r, src },
        Opcode::Call4 {
            arg0: unit,
            arg1: attr,
            arg2,
            ..
        },
    ) = (&hops[up - 3], &hops[up - 2], &hops[up])
    else {
        bail!("host: upgradeAttribute is not called with &count");
    };
    let (count, unit, attr) = (*count, *unit, *attr);
    if src != tmp || arg2 != r {
        bail!("host: upgradeAttribute count ref mismatch");
    }
    if !(at < up && up < skip) {
        bail!("host: isAuth branch does not enclose upgradeAttribute");
    }
    if !hops[at..up].iter().any(
        |op| matches!(op, Opcode::Call2 { fun, arg0, .. } if *fun == set_used && *arg0 == unit),
    ) {
        bail!("host: set_usedAptitudePoints not called before upgradeAttribute");
    }
    if !matches!(hops[skip], Opcode::NullCheck { reg } if reg == unit) {
        bail!("host: isAuth skip target is not `NullCheck unit`");
    }
    let n = hops.len();
    let ret_null = match (&hops[n - 2], &hops[n - 1]) {
        (Opcode::Null { dst }, Opcode::Ret { ret }) if dst == ret => n - 2,
        _ => bail!("host: does not end with `return null`"),
    };
    for i in 0..n {
        if jump_targets(host, i).contains(&at) {
            bail!("host: op {i} jumps to the isAuth body start");
        }
    }
    let regs = &host.regs;
    if regs[unit.0 as usize] != unit_t || regs[count.0 as usize] != i32_t {
        bail!("host: unit/count register types differ");
    }
    let get_f = &code.functions[fun_index(code, map_get)?];
    let get_args = fun_args(code, get_f);
    let counts_f = &code.functions[fun_index(code, get_counts)?];
    let map_t = counts_f
        .t
        .as_fun(code)
        .context("getAttributeUpCounts type")?
        .ret;
    if get_args.len() != 2 || get_args[0] != map_t || get_args[1] != regs[attr.0 as usize] {
        bail!("host: map.get(attr) signature mismatch");
    }
    if get_f.t.as_fun(code).map(|t| t.ret) != Some(dyn_t) {
        bail!("host: map.get does not return the client's Dyn type");
    }
    Ok(Plan {
        client_fi,
        add_at,
        cnt,
        extra,
        client_unit,
        used_f,
        host_fi,
        at,
        skip,
        ret_null,
        unit,
        count,
        attr,
        get_counts,
        map_get,
        apt_f,
        i32_t,
        map_t,
        dyn_t,
        null_i32_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let zero_c = int_const(code, 0);
    let two_c = int_const(code, 2);
    let three_c = int_const(code, 3);

    // ---- host
    let f = &mut code.functions[p.host_fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let apt = reg(p.i32_t);
    let zero = reg(p.i32_t);
    let extra = reg(p.i32_t);
    let base = reg(p.i32_t);
    let map = reg(p.map_t);
    let dynv = reg(p.dyn_t);
    let nl = reg(p.null_i32_t);
    let k = reg(p.i32_t);
    let used = reg(p.i32_t);
    let cur = reg(p.i32_t);
    let mut ops = vec![
        Opcode::JNull {
            reg: p.unit,
            offset: 0,
        }, // 0 -> RET
        Opcode::Field {
            dst: apt,
            obj: p.unit,
            field: p.apt_f,
        },
        Opcode::Int {
            dst: zero,
            ptr: zero_c,
        },
        Opcode::JSGte {
            a: zero,
            b: apt,
            offset: 0,
        }, // 3 -> SKIP
        Opcode::JSGte {
            a: p.count,
            b: zero,
            offset: 0,
        }, // 4 -> END (legacy encoding)
        Opcode::Neg {
            dst: extra,
            src: p.count,
        },
        Opcode::Decr { dst: extra }, // 6: e = -1 - count
        Opcode::Int { dst: k, ptr: two_c },
        Opcode::SShr {
            dst: used,
            a: extra,
            b: k,
        }, // 8: e >> 2
        Opcode::Field {
            dst: cur,
            obj: p.unit,
            field: p.used_f,
        },
        Opcode::JNotEq {
            a: used,
            b: cur,
            offset: 0,
        }, // 10 -> SKIP (stale offer)
        Opcode::Int {
            dst: k,
            ptr: three_c,
        },
        Opcode::And {
            dst: extra,
            a: extra,
            b: k,
        }, // 12: n = e & 3
        Opcode::Mov {
            dst: base,
            src: zero,
        },
        Opcode::Call1 {
            dst: map,
            fun: p.get_counts,
            arg0: p.unit,
        },
        Opcode::JNull {
            reg: map,
            offset: 0,
        }, // 15 -> ADD
        Opcode::JNull {
            reg: p.attr,
            offset: 0,
        }, // 16 -> ADD
        Opcode::Call2 {
            dst: dynv,
            fun: p.map_get,
            arg0: map,
            arg1: p.attr,
        },
        Opcode::UnsafeCast { dst: nl, src: dynv },
        Opcode::JNull { reg: nl, offset: 0 }, // 19 -> ADD
        Opcode::SafeCast { dst: base, src: nl },
        Opcode::Add {
            dst: p.count,
            a: base,
            b: extra,
        }, // 21 ADD
    ];
    let len = ops.len();
    // Block coordinates: block op j sits at `at + j`; original op t >= at moves to t + len.
    let outside = |t: usize| t - p.at + len;
    resolve_jumps(
        &mut ops,
        &[
            (0, outside(p.ret_null)),
            (3, outside(p.skip)),
            (4, len),
            (10, outside(p.skip)),
            (15, 21),
            (16, 21),
            (19, 21),
        ],
    );
    insert_ops(f, p.at, ops);
    eprintln!(
        "patched career-plan host fn@{}: {len} ops at {} (aptitude/offer guard -> op {}, re-derive count reg{} from host offer)",
        f.findex.0,
        p.at,
        p.skip + len,
        p.count.0
    );

    // ---- client: `cnt = cnt + n` -> `cnt = -1 - (n + (unit.usedAptitudePoints << 2))`
    let f = &mut code.functions[p.client_fi];
    f.regs.push(p.i32_t);
    let two = Reg((f.regs.len() - 1) as u32);
    f.ops[p.add_at] = Opcode::Field {
        dst: p.cnt,
        obj: p.client_unit,
        field: p.used_f,
    };
    insert_ops(
        f,
        p.add_at + 1,
        vec![
            Opcode::Int {
                dst: two,
                ptr: two_c,
            },
            Opcode::Shl {
                dst: p.cnt,
                a: p.cnt,
                b: two,
            },
            Opcode::Add {
                dst: p.cnt,
                a: p.cnt,
                b: p.extra,
            },
            Opcode::Neg {
                dst: p.cnt,
                src: p.cnt,
            },
            Opcode::Decr { dst: p.cnt },
        ],
    );
    eprintln!(
        "patched career-plan client fn@{}: count = -1 - (n + (usedAptitudePoints << 2)) at op {}",
        f.findex.0, p.add_at
    );
}

/// Applies the Career Plan fix, or leaves `code` untouched and logs why.
pub(crate) fn patch_career_plan(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("career-plan fix skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// only the two target functions change, the image round-trips, and the
    /// new op sequences are in place.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let mut out = Vec::new();
        orig.serialize(&mut out).expect("write");
        assert!(out == image, "unpatched round-trip is not byte-identical");

        let p = plan(&orig).expect("plan");
        let (cfi, hfi) = (p.client_fi, p.host_fi);
        let mut code = read(&image);
        patch_career_plan(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        if let Ok(dir) = std::env::var("CAREER_PLAN_OUT") {
            std::fs::write(dir, &patched).expect("save patched copy");
        }
        let back = read(&patched);

        // Everything except the two functions is untouched.
        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(back.ints, orig.ints);
        assert_eq!(back.types, orig.types);
        assert_eq!(back.strings, orig.strings);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same =
                ops(&a.ops) == ops(&b.ops) && a.regs == b.regs && a.debug_info == b.debug_info;
            assert_eq!(
                same,
                i != cfi && i != hfi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        let c = &back.functions[cfi];
        let oc = &orig.functions[cfi];
        let a = p.add_at;
        assert!(
            matches!(c.ops[a], Opcode::Field { dst, obj, field } if dst == p.cnt && obj == p.client_unit && field == p.used_f)
        );
        assert!(matches!(c.ops[a + 1], Opcode::Int { ptr, .. } if back.ints[ptr.0] == 2));
        assert!(
            matches!(c.ops[a + 2], Opcode::Shl { dst, a: x, .. } if dst == p.cnt && x == p.cnt)
        );
        assert!(
            matches!(c.ops[a + 3], Opcode::Add { dst, a: x, b } if dst == p.cnt && x == p.cnt && b == p.extra)
        );
        assert!(matches!(c.ops[a + 4], Opcode::Neg { dst, src } if dst == p.cnt && src == p.cnt));
        assert!(matches!(c.ops[a + 5], Opcode::Decr { dst } if dst == p.cnt));
        assert_eq!(c.ops.len(), oc.ops.len() + 5);
        assert_eq!(ops(&c.ops[..a]), ops(&oc.ops[..a]));
        assert_eq!(ops(&c.ops[a + 6..]), ops(&oc.ops[a + 1..]));

        let h = &back.functions[hfi];
        let oh = &orig.functions[hfi];
        let n = 22;
        assert_eq!(h.ops.len(), oh.ops.len() + n);
        assert_eq!(h.regs.len(), oh.regs.len() + 10);
        assert_eq!(ops(&h.ops[..p.at - 1]), ops(&oh.ops[..p.at - 1]));
        assert_eq!(ops(&h.ops[p.at + n..]), ops(&oh.ops[p.at..]));
        assert!(matches!(h.ops[p.at + n - 1], Opcode::Add { dst, .. } if dst == p.count));
        assert!(matches!(h.ops[p.at + 8], Opcode::SShr { .. }));
        assert!(
            matches!(h.ops[p.at + 9], Opcode::Field { obj, field, .. } if obj == p.unit && field == p.used_f)
        );
        assert!(matches!(h.ops[p.at + 12], Opcode::And { .. }));
        // Every jump lands inside the function; the aptitude and stale-offer
        // guards on the old skip target (before the Influence spend).
        for i in 0..h.ops.len() {
            for t in jump_targets(h, i) {
                assert!(t < h.ops.len(), "op {i} jumps out of range");
            }
        }
        assert_eq!(jump_targets(h, p.at + 3), vec![p.skip + n]);
        assert_eq!(jump_targets(h, p.at + 10), vec![p.skip + n]);
        assert_eq!(jump_targets(h, p.at + 4), vec![p.at + n]);
        for j in [15, 16, 19] {
            assert_eq!(jump_targets(h, p.at + j), vec![p.at + 21]);
        }
        assert_eq!(jump_targets(h, p.at), vec![p.ret_null + n]);
        assert_eq!(jump_targets(h, p.at - 1), vec![p.skip + n]);

        // A second pass finds nothing to patch and leaves the image alone.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_career_plan(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
