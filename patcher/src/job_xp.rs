// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A profession keeps its experience when the unit switches to another one.
//
// Vanilla already remembers each profession's *level*: `st.Unit._removeTrait`
// writes `jobsLevel[tid] = t.level` for a trait with `props.hasLevel`, and
// `_addTrait` reads it back into the new `UnitTrait`. Only `UnitTrait.xp`, the
// progress inside the current level, is lost (the JobSelector confirm says
// "will lose all experience gained as <job> <level>"). This pass stores it in
// the same networked, saved `jobsLevel` map (`Map<String, Int>`) under an extra
// key `tid + "#xp"` (a new String constant global), so the hxbit schema does
// not change:
//
//   _removeTrait, after `jobsLevel.set(tid, t.level)`:
//       jobsLevel.set(tid + "#xp", t.xp);
//   _addTrait, right before its own 	 = getTrait(tid, &true):
//       var had = getTrait(tid, null);          // lookup only, never creates
//   _addTrait, after `jobsLevel.set(tid, t.level)`:
//       var v = jobsLevel.get(tid + "#xp");
//       if (had == null && v != null && v > 0 && t.xp == 0) { t.xp = v; jobsLevel.set(tid + "#xp", 0); }
//
// Both run where vanilla already marked `jobsLevel` changed. Every reader of
// the map looks a job id up (`get(tid)`) or copies the whole map, so the extra
// keys are inert for them and for an unpatched game. Both functions run only on
// the host (`game.isAuth`; clients go through `netRemoveTrait`/`netAddTrait`),
// and `t.xp` / `jobsLevel` reach clients through the existing sync: a client's
// build does not matter. Only a trait `_addTrait` just created gets the xp, and
// the stored value is consumed, so re-adding a trait the unit already has never
// re-applies stale xp. The max-level reset in `addTraitXP` is untouched.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use hlbc::types::RefGlobal;

const SUFFIX: &str = "#xp";

/// One `jobsLevel.map.set(tid, dyn(t.level))` site.
struct Site {
    fi: usize,
    /// Op index of the `Call3 set` (the new block goes right after it).
    at: usize,
    map: Reg,
    trait_reg: Reg,
    dyn_t: RefType,
}

struct Plan {
    remove: Site,
    add: Site,
    map_set: RefFun,
    map_get: RefFun,
    str_add: RefFun,
    set_xp: RefFun,
    xp_f: RefField,
    str_t: RefType,
    i32_t: RefType,
    void_t: RefType,
    any_t: RefType,
    null_i32_t: RefType,
    get_trait: RefFun,
    lookup_at: usize,
    trait_t: RefType,
    ref_bool_t: RefType,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

/// The last op before `before` that writes `reg`.
fn last_write(f: &Function, reg: Reg, before: usize) -> Option<&Opcode> {
    f.ops[..before].iter().rev().find(|op| match op {
        Opcode::GetThis { dst, .. }
        | Opcode::Field { dst, .. }
        | Opcode::SafeCast { dst, .. }
        | Opcode::UnsafeCast { dst, .. }
        | Opcode::ToDyn { dst, .. }
        | Opcode::Mov { dst, .. } => *dst == reg,
        _ => false,
    })
}

/// Finds the single `jobsLevel.map.set(arg1, level)` in unit method `name`:
/// `Field lvl = t.level; ...; Field mv = jl.map; SafeCast m = mv; NullCheck m; ToDyn d = lvl; Call3 set(m, r1, d)`
/// with `jl` loaded by `GetThis jobsLevel` and marked changed in between
/// (`Field o = jl.obj; ...; CallMethod o.<0>(o, jl.bit)`, the hxbit MapData mark).
#[allow(clippy::too_many_arguments)] // one call site; the refs are the matcher's inputs
fn find_site(
    code: &Bytecode,
    unit_t: RefType,
    name: &str,
    jobs_f: RefField,
    map_f: RefField,
    mark_f: RefField,
    level_f: RefField,
    trait_t: RefType,
) -> Result<(Site, RefFun)> {
    let f = method(code, unit_t, name)?;
    let fi = fun_index(code, f.findex)?;
    let ops = &f.ops;
    let mut sites = vec![];
    for i in 4..ops.len() {
        let Opcode::Call3 {
            fun,
            arg0: m,
            arg1: Reg(1),
            arg2: d,
            ..
        } = ops[i]
        else {
            continue;
        };
        let Opcode::ToDyn { dst, src: lvl } = ops[i - 1] else {
            continue;
        };
        if dst != d || !matches!(ops[i - 2], Opcode::NullCheck { reg } if reg == m) {
            continue;
        }
        let Opcode::SafeCast { dst: sm, src: mv } = ops[i - 3] else {
            continue;
        };
        let Opcode::Field {
            dst: fm,
            obj: jl,
            field,
        } = ops[i - 4]
        else {
            continue;
        };
        if sm != m || fm != mv || field != map_f {
            continue;
        }
        let Some(g) = (0..i - 4).rev().find(|&k| match ops[k] {
            Opcode::GetThis { dst, .. } | Opcode::Field { dst, .. } | Opcode::Mov { dst, .. } => {
                dst == jl
            }
            _ => false,
        }) else {
            continue;
        };
        if !matches!(ops[g], Opcode::GetThis { field, .. } if field == jobs_f) {
            continue;
        }
        let marked = (g..i - 4).any(|k| match &ops[k] {
            Opcode::CallMethod { field: RefField(0), args, .. } if args.len() == 2 => (g..k).any(|j| {
                matches!(ops[j], Opcode::Field { dst, obj, field } if dst == args[0] && obj == jl && field == mark_f)
            }),
            _ => false,
        });
        if !marked {
            continue;
        }
        let Some(Opcode::Field {
            obj: t, field: lf, ..
        }) = last_write(f, lvl, i - 1)
        else {
            continue;
        };
        if *lf != level_f || f.regs[t.0 as usize] != trait_t {
            continue;
        }
        sites.push((
            Site {
                fi,
                at: i,
                map: m,
                trait_reg: *t,
                dyn_t: f.regs[d.0 as usize],
            },
            fun,
        ));
    }
    if sites.len() != 1 {
        bail!(
            "{name}: expected one jobsLevel.set(tid, t.level), found {}",
            sites.len()
        );
    }
    Ok(sites.pop().unwrap())
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "st.Unit")?;
    let trait_t = obj_type(code, "st.UnitTrait")?;
    let str_t = obj_type(code, "String")?;
    let (jobs_f, jobs_t) = field(code, unit_t, "jobsLevel")?;
    let (map_f, _) = field(code, jobs_t, "map")?;
    let (mark_f, _) = field(code, jobs_t, "obj")?;
    let (level_f, i32_t) = field(code, trait_t, "level")?;
    let (xp_f, xp_t) = field(code, trait_t, "xp")?;
    let (_, kind_t) = field(code, trait_t, "kind")?;
    if xp_t != i32_t || !matches!(code.types[i32_t.0], Type::I32) {
        bail!("UnitTrait.xp/level are not both i32");
    }
    if kind_t != str_t {
        bail!("UnitTrait.kind is not a String");
    }
    let set_xp = method(code, trait_t, "set_xp")?;
    if fun_args(code, set_xp) != [trait_t, i32_t]
        || set_xp.t.as_fun(code).map(|t| t.ret) != Some(i32_t)
    {
        bail!("unexpected UnitTrait.set_xp signature");
    }
    let set_xp = set_xp.findex;

    let (remove, map_set) = find_site(
        code,
        unit_t,
        "_removeTrait",
        jobs_f,
        map_f,
        mark_f,
        level_f,
        trait_t,
    )?;
    let (add, add_set) = find_site(
        code,
        unit_t,
        "_addTrait",
        jobs_f,
        map_f,
        mark_f,
        level_f,
        trait_t,
    )?;
    if add_set != map_set {
        bail!("_addTrait and _removeTrait use different map setters");
    }
    for s in [&remove, &add] {
        let f = &code.functions[s.fi];
        if fun_args(code, f).get(1) != Some(&str_t) {
            bail!("fn@{}: tid is not a String argument", f.findex.0);
        }
        if f.ops.iter().any(|op| matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(SUFFIX)))
        {
            bail!("fn@{}: already applied", f.findex.0);
        }
    }

    // `lvl = jobsLevel.map.get(tid)` in _addTrait: Call2 get(m, r1); UnsafeCast nl = dyn.
    let af = &code.functions[add.fi];
    let gets: Vec<(RefFun, RefType, RefType)> = (0..add.at)
        .filter_map(|i| match (&af.ops[i], af.ops.get(i + 1)) {
            (
                Opcode::Call2 {
                    dst,
                    fun,
                    arg1: Reg(1),
                    ..
                },
                Some(Opcode::UnsafeCast { dst: nl, src }),
            ) if src == dst => Some((*fun, af.regs[dst.0 as usize], af.regs[nl.0 as usize])),
            _ => None,
        })
        .collect();
    let [(map_get, any_t, null_i32_t)] = gets[..] else {
        bail!(
            "_addTrait: expected one jobsLevel.get(tid), found {}",
            gets.len()
        );
    };
    if !matches!(code.types[null_i32_t.0], Type::Null(t) if t == i32_t) {
        bail!("_addTrait: jobsLevel value is not Null<i32>");
    }
    let get_f = &code.functions[fun_index(code, map_get)?];
    let set_f = &code.functions[fun_index(code, map_set)?];
    let map_t = af.regs[add.map.0 as usize];
    if fun_args(code, get_f) != [map_t, str_t] || get_f.t.as_fun(code).map(|t| t.ret) != Some(any_t)
    {
        bail!("unexpected map get signature");
    }
    let set_args = fun_args(code, set_f);
    if !matches!(code.types[any_t.0], Type::Dyn) {
        bail!("_addTrait: jobsLevel.get does not return Dyn");
    }
    if set_args != [map_t, str_t, any_t]
        || set_f
            .t
            .as_fun(code)
            .map(|t| matches!(code.types[t.ret.0], Type::Void))
            != Some(true)
    {
        bail!("unexpected map set signature");
    }
    if code.functions[remove.fi].regs[remove.map.0 as usize] != map_t {
        bail!("_removeTrait: jobsLevel map register type differs");
    }

    // String concatenation `String.__add__(a, b)`.
    let adds: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "__add__"
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == [str_t, str_t] && t.ret == str_t)
        })
        .map(|f| f.findex)
        .collect();
    let [str_add] = adds[..] else {
        bail!("expected one String.__add__, found {}", adds.len());
    };
    // `getTrait(tid, create: ref<bool>)`: a null `create` is a plain lookup.
    let gt = method(code, unit_t, "getTrait")?;
    let gt_args = fun_args(code, gt);
    if gt_args.len() != 3
        || gt_args[1] != str_t
        || !matches!(code.types[gt_args[2].0], Type::Ref(b) if matches!(code.types[b.0], Type::Bool))
        || gt.t.as_fun(code).map(|t| t.ret) != Some(trait_t)
    {
        bail!("unexpected getTrait signature");
    }
    let (get_trait, ref_bool_t) = (gt.findex, gt_args[2]);
    // Vanilla's own `t = getTrait(tid, &true)`: every path to the set passes it,
    // so a lookup placed right before it always runs first.
    let calls: Vec<usize> = (0..add.at)
        .filter(|&i| {
            matches!(af.ops[i], Opcode::Call3 { fun, arg0: Reg(0), arg1: Reg(1), dst, .. }
                if fun == get_trait && dst == add.trait_reg)
        })
        .collect();
    let [lookup_at] = calls[..] else {
        bail!(
            "_addTrait: expected one getTrait(tid, create) into t, found {}",
            calls.len()
        );
    };
    if (0..af.ops.len()).any(|i| jump_targets(af, i).contains(&lookup_at)) {
        bail!("_addTrait: a jump targets the getTrait call");
    }
    if (lookup_at..add.at).any(|i| {
        jump_targets(af, i).iter().any(|&t| t <= lookup_at)
            || matches!(af.ops[i], Opcode::Ret { .. })
    }) {
        bail!("_addTrait: the getTrait call does not lead straight to the set");
    }
    if (0..af.ops.len())
        .filter(|&i| i < lookup_at || i > add.at)
        .any(|i| {
            jump_targets(af, i)
                .iter()
                .any(|&t| t > lookup_at && t <= add.at)
        })
    {
        bail!("_addTrait: a jump bypasses the getTrait call");
    }
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    Ok(Plan {
        remove,
        add,
        map_set,
        map_get,
        str_add,
        set_xp,
        xp_f,
        str_t,
        i32_t,
        void_t,
        any_t,
        null_i32_t,
        get_trait,
        lookup_at,
        trait_t,
        ref_bool_t,
    })
}

/// The text of global `g` when it is a String constant (`[bytes string index, length int index]`).
fn const_str(code: &Bytecode, g: RefGlobal) -> Option<&str> {
    let c = code.constants.as_ref()?.iter().find(|c| c.global == g)?;
    let [si, _] = c.fields[..] else {
        return None;
    };
    code.strings.get(si).map(|x| x.as_str())
}

/// A global holding the String constant `value`, the way the Haxe compiler emits
/// string literals (a `String` object initialised from the constants table),
/// appended when missing.
pub(crate) fn str_global(code: &mut Bytecode, str_t: RefType, value: &'static str) -> RefGlobal {
    let found = code.constants.iter().flatten().find(|c| {
        code.globals.get(c.global.0) == Some(&str_t)
            && matches!(c.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value))
    });
    if let Some(c) = found {
        return c.global;
    }
    let si = match code.strings.iter().position(|v| v.as_str() == value) {
        Some(i) => i,
        None => {
            code.strings.push(hlbc::Str::from_static(value));
            code.strings.len() - 1
        }
    };
    let len = int_const(code, value.encode_utf16().count() as i32);
    code.globals.push(str_t);
    let g = RefGlobal(code.globals.len() - 1);
    let consts = code.constants.get_or_insert_with(Vec::new);
    consts.push(hlbc::types::ConstantDef {
        global: g,
        fields: vec![si, len.0],
    });
    let ci = consts.len() - 1;
    code.globals_initializers.insert(g, ci);
    g
}

fn apply(code: &mut Bytecode, p: Plan) {
    let sfx = str_global(code, p.str_t, SUFFIX);
    let zero_c = int_const(code, 0);

    // ---- _removeTrait: jobsLevel.set(tid + "#xp", t.xp)
    let f = &mut code.functions[p.remove.fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (rs, rk, rx, rd, rv) = (
        reg(p.str_t),
        reg(p.str_t),
        reg(p.i32_t),
        reg(p.remove.dyn_t),
        reg(p.void_t),
    );
    let at = p.remove.at + 1;
    insert_ops(
        f,
        at,
        vec![
            Opcode::GetGlobal {
                dst: rs,
                global: sfx,
            },
            Opcode::Call2 {
                dst: rk,
                fun: p.str_add,
                arg0: Reg(1),
                arg1: rs,
            },
            Opcode::Field {
                dst: rx,
                obj: p.remove.trait_reg,
                field: p.xp_f,
            },
            Opcode::ToDyn { dst: rd, src: rx },
            Opcode::Call3 {
                dst: rv,
                fun: p.map_set,
                arg0: p.remove.map,
                arg1: rk,
                arg2: rd,
            },
        ],
    );
    eprintln!(
        "patched job xp fn@{}: store t.xp as jobsLevel[tid+\"{SUFFIX}\"] at op {at}",
        f.findex.0
    );

    // ---- _addTrait: restore and consume the stored xp
    let f = &mut code.functions[p.add.fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (rs, rk, ra, rn, rv, rz, rx, rd, rvoid, had, rnull) = (
        reg(p.str_t),
        reg(p.str_t),
        reg(p.any_t),
        reg(p.null_i32_t),
        reg(p.i32_t),
        reg(p.i32_t),
        reg(p.i32_t),
        reg(p.add.dyn_t),
        reg(p.void_t),
        reg(p.trait_t),
        reg(p.ref_bool_t),
    );
    let t = p.add.trait_reg;
    let mut ops = vec![
        Opcode::JNotNull {
            reg: had,
            offset: 0,
        }, // 0 -> END (the unit already had this trait)
        Opcode::GetGlobal {
            dst: rs,
            global: sfx,
        },
        Opcode::Call2 {
            dst: rk,
            fun: p.str_add,
            arg0: Reg(1),
            arg1: rs,
        },
        Opcode::Call2 {
            dst: ra,
            fun: p.map_get,
            arg0: p.add.map,
            arg1: rk,
        },
        Opcode::UnsafeCast { dst: rn, src: ra },
        Opcode::JNull { reg: rn, offset: 0 }, // 5 -> END
        Opcode::SafeCast { dst: rv, src: rn },
        Opcode::Int {
            dst: rz,
            ptr: zero_c,
        },
        Opcode::JSGte {
            a: rz,
            b: rv,
            offset: 0,
        }, // 8 -> END (stored xp <= 0)
        Opcode::Field {
            dst: rx,
            obj: t,
            field: p.xp_f,
        },
        Opcode::JNotEq {
            a: rx,
            b: rz,
            offset: 0,
        }, // 10 -> END (trait already has progress)
        Opcode::Call2 {
            dst: rx,
            fun: p.set_xp,
            arg0: t,
            arg1: rv,
        },
        Opcode::ToDyn { dst: rd, src: rz },
        Opcode::Call3 {
            dst: rvoid,
            fun: p.map_set,
            arg0: p.add.map,
            arg1: rk,
            arg2: rd,
        },
    ];
    let end = ops.len();
    // resolve_jumps does not take JNotNull.
    if let Opcode::JNotNull { offset, .. } = &mut ops[0] {
        *offset = end as i32 - 1;
    }
    resolve_jumps(&mut ops, &[(5, end), (8, end), (10, end)]);
    let at = p.add.at + 1;
    // Later edit first: the lookup sits before the block and shifts it by 2.
    insert_ops(f, at, ops);
    insert_ops(
        f,
        p.lookup_at,
        vec![
            Opcode::Null { dst: rnull },
            Opcode::Call3 {
                dst: had,
                fun: p.get_trait,
                arg0: Reg(0),
                arg1: Reg(1),
                arg2: rnull,
            },
        ],
    );
    eprintln!(
        "patched job xp fn@{}: getTrait lookup at op {}, restore t.xp of a new trait from jobsLevel[tid+\"{SUFFIX}\"] at op {}",
        f.findex.0,
        p.lookup_at,
        at + 2
    );
}

/// Keeps a profession's xp across job switches, or leaves `code` untouched and logs why.
pub(crate) fn patch_job_xp(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("job xp skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// only _removeTrait and _addTrait change, each gains its block right after
    /// the vanilla `jobsLevel.set(tid, t.level)`, jumps stay in range, and a
    /// second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (rfi, rat, afi, aat) = (p.remove.fi, p.remove.at, p.add.fi, p.add.at);
        let mut code = read(&image);
        patch_job_xp(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        assert_eq!(back.strings.len(), orig.strings.len() + 1);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.strings.last().unwrap().as_str(), SUFFIX);
        assert_eq!(back.globals.len(), orig.globals.len() + 1);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        let (oc, bc) = (
            orig.constants.as_ref().unwrap(),
            back.constants.as_ref().unwrap(),
        );
        assert_eq!(bc.len(), oc.len() + 1);
        let new_c = bc.last().unwrap();
        assert_eq!(new_c.global.0, orig.globals.len());
        assert_eq!(back.ints[new_c.fields[1]], 3);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = ops(&a.ops) == ops(&b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != rfi && i != afi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        let lk = p.lookup_at;
        // (function, lookup op, lookup length, vanilla set op, block length, suffix op in block)
        for (fi, la, pre, at, n, g) in [(rfi, 0, 0, rat, 5, 0), (afi, lk, 2, aat, 14, 1)] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(b.ops.len(), a.ops.len() + pre + n);
            // Original ops keep their order around the new ops; jumps keep their targets.
            let m = |i: usize| match i {
                i if i < la => i,
                i if i <= at => i + pre,
                i => i + pre + n,
            };
            for i in 0..a.ops.len() {
                let ta = jump_targets(a, i);
                if ta.is_empty() {
                    assert_eq!(ops(&b.ops[m(i)..=m(i)]), ops(&a.ops[i..=i]), "op {i}");
                } else {
                    assert_eq!(
                        std::mem::discriminant(&b.ops[m(i)]),
                        std::mem::discriminant(&a.ops[i])
                    );
                    let tb: Vec<usize> = ta.into_iter().map(m).collect();
                    assert_eq!(jump_targets(b, m(i)), tb, "op {i}");
                }
            }
            let s = at + pre + 1;
            assert!(
                matches!(b.ops[s + g], Opcode::GetGlobal { global, .. } if const_str(&back, global) == Some(SUFFIX))
            );
            for i in 0..b.ops.len() {
                for t in jump_targets(b, i) {
                    assert!(
                        t < b.ops.len(),
                        "fn@{} op {i} jumps out of range",
                        b.findex.0
                    );
                    // Nothing outside the block jumps into it.
                    if !(s..s + n).contains(&i) {
                        assert!(!(s + 1..s + n).contains(&t), "op {i} jumps into the block");
                    }
                }
            }
        }
        // _addTrait: `had = getTrait(tid, null)` right before vanilla's getTrait; the four exits
        // land on the original op after the vanilla set; set_xp only past them.
        let b = &back.functions[afi];
        assert!(matches!(b.ops[lk + 2], Opcode::Call3 { fun, .. } if fun == p.get_trait));
        let Opcode::Call3 {
            fun,
            arg0: Reg(0),
            arg1: Reg(1),
            arg2,
            dst: had,
        } = b.ops[lk + 1]
        else {
            panic!("no getTrait lookup");
        };
        assert_eq!(fun, p.get_trait);
        assert!(matches!(b.ops[lk], Opcode::Null { dst } if dst == arg2));
        let s = aat + 3;
        assert!(matches!(b.ops[s], Opcode::JNotNull { reg, .. } if reg == had));
        for j in [s, s + 5, s + 8, s + 10] {
            assert_eq!(jump_targets(b, j), vec![s + 14]);
        }
        assert!(matches!(b.ops[s + 11], Opcode::Call2 { fun, .. } if fun == p.set_xp));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_job_xp(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
