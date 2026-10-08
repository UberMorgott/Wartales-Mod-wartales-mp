// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Clicks and hover only reach what a scrolling flow shows (chest panel bug 3:
// dragging the panel by its header picked up an item slot scrolled up under
// the header).
//
// `h2d.Flow.drawRec` clips a flow with `overflow` Scroll or Hidden to its own
// box (`Mask.maskWith(ctx, this, ceil(calculatedWidth), ceil(calculatedHeight))`),
// but the scene's hit test (`h2d.Scene.handleEvent`, `getInteractive`) only
// checks an interactive's own rectangle and that it and its parents are
// `visible`: a slot scrolled out of an inventory's scroll area is not drawn
// yet still takes the click, and it wins over the header drawn below it.
// Vanilla shares the gap (its header has no action there); the panel drag
// made it visible.
//
// Now both hit tests also require the point to lie inside the drawn box of
// every Scroll / Hidden flow above the interactive (the rectangle maskWith
// clips to, from the flow's absX/absY and matrix). Nothing else changes:
// release-outside, out, focus and capture are dispatched as before.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::window_drag::name_fn;
use hlbc::types::ValBool;

const NAME: &str = "mpInsideScrollClip";

/// One hit test's candidate loop: `if (visible) <hit> else continue;`.
struct Site {
    fi: usize,
    /// `JTrue visible -> at`, followed by the `continue` jump.
    test: usize,
    /// The visible flag, the candidate interactive, the point (scene space).
    cond: Reg,
    cand: Reg,
    x: Reg,
    y: Reg,
}

struct Plan {
    handle: Site,
    get: Site,
    obj_t: RefType,
    f64_t: RefType,
    bool_t: RefType,
    i32_t: RefType,
    dyn_t: RefType,
    flow_t: RefType,
    flow_cls: RefGlobal,
    is_of_type: RefFun,
    parent: RefField,
    overflow: RefField,
    ov_t: RefType,
    ov_scroll: i32,
    ov_hidden: i32,
    abs_x: RefField,
    abs_y: RefField,
    mat: [RefField; 4],
    calc_w: RefField,
    calc_h: RefField,
    dbg_file: usize,
}

/// Inserted ops per site.
const N: usize = 2;

fn enum_index(code: &Bytecode, t: RefType, name: &str) -> Result<i32> {
    match &code.types[t.0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|c| s(code, c.name) == name && c.params.is_empty())
            .map(|i| i as i32)
            .with_context(|| format!("enum construct {name} not found")),
        _ => bail!("type {} is not an enum", t.0),
    }
}

fn site(
    code: &Bytecode,
    findex: RefFun,
    what: &str,
    inter_t: RefType,
    f64_t: RefType,
    visible: RefField,
    abs_x: RefField,
    abs_y: RefField,
) -> Result<Site> {
    let fi = fun_index(code, findex)?;
    let f = &code.functions[fi];
    // `JTrue visible -> +1; JAlways continue (backward)` right after the
    // parent `visible` walk.
    let tests: Vec<usize> = (0..f.ops.len().saturating_sub(1))
        .filter(|&i| {
            matches!(f.ops[i], Opcode::JTrue { offset: 1, .. })
                && matches!(f.ops[i + 1], Opcode::JAlways { offset } if offset < 0)
                && (i.saturating_sub(12)..i).any(
                    |k| matches!(f.ops[k], Opcode::Field { field, .. } if field == visible),
                )
        })
        .collect();
    let [test] = tests[..] else {
        bail!("{what}: expected one visible test, found {}", tests.len());
    };
    let Opcode::JTrue { cond, .. } = f.ops[test] else {
        unreachable!()
    };
    if matches!(f.ops.get(test + 2), Some(Opcode::Call3 { dst, .. }) if *dst == cond) {
        bail!("{what}: already applied");
    }
    // `Bool cond = true; Mov walk = cand` starts the walk.
    let cand = (0..test)
        .rev()
        .find_map(|k| match (&f.ops[k], f.ops.get(k + 1)) {
            (Opcode::Bool { dst, value }, Some(Opcode::Mov { src, .. }))
                if *dst == cond && value.0 =>
            {
                Some(*src)
            }
            _ => None,
        })
        .with_context(|| format!("{what}: no `visible = true; p = i` before the test"))?;
    // The point: `Sub d = X - cand.absX` (and Y).
    let point = |abs: RefField| {
        (0..test).find_map(|k| match (&f.ops[k], &f.ops[k + 1]) {
            (Opcode::Field { dst, obj, field }, Opcode::Sub { a, b, .. })
                if *obj == cand && *field == abs && b == dst =>
            {
                Some(*a)
            }
            _ => None,
        })
    };
    let x = point(abs_x).with_context(|| format!("{what}: no x - absX"))?;
    let y = point(abs_y).with_context(|| format!("{what}: no y - absY"))?;
    let reg_t = |r: Reg| f.regs[r.0 as usize];
    if reg_t(cand) != inter_t || reg_t(x) != f64_t || reg_t(y) != f64_t {
        bail!("{what}: hit-test registers have unexpected types");
    }
    if !matches!(code.types[reg_t(cond).0], Type::Bool) {
        bail!("{what}: the visible flag is not a Bool");
    }
    // Only the test enters the hit code after it.
    let entries: Vec<usize> = (0..f.ops.len())
        .filter(|&i| jump_targets(f, i).contains(&(test + 2)))
        .collect();
    if entries != [test] {
        bail!("{what}: other jumps enter the hit code");
    }
    Ok(Site {
        fi,
        test,
        cond,
        cand,
        x,
        y,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let f64_t = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let inter_t = obj_type(code, "h2d.Interactive")?;
    let scene_t = obj_type(code, "h2d.Scene")?;
    if !is_sub(code, flow_t, obj_t) || !is_sub(code, inter_t, obj_t) {
        bail!("h2d.Flow / h2d.Interactive are not h2d.Objects");
    }
    let parent = typed(code, obj_t, "parent", obj_t)?;
    let visible = typed(code, obj_t, "visible", bool_t)?;
    let abs_x = typed(code, obj_t, "absX", f64_t)?;
    let abs_y = typed(code, obj_t, "absY", f64_t)?;
    let mat = [
        typed(code, obj_t, "matA", f64_t)?,
        typed(code, obj_t, "matB", f64_t)?,
        typed(code, obj_t, "matC", f64_t)?,
        typed(code, obj_t, "matD", f64_t)?,
    ];
    let calc_w = typed(code, flow_t, "calculatedWidth", f64_t)?;
    let calc_h = typed(code, flow_t, "calculatedHeight", f64_t)?;
    let (overflow, ov_t) = field(code, flow_t, "overflow")?;
    let ov_scroll = enum_index(code, ov_t, "Scroll")?;
    let ov_hidden = enum_index(code, ov_t, "Hidden")?;
    let (flow_cls, _) = class_global(code, "h2d.Flow")?;
    let is_of_type = crate::diag::static_fn(code, "$Std", "isOfType")?.findex;
    if sig(code, is_of_type)? != (vec![dyn_t, dyn_t], bool_t) {
        bail!("Std.isOfType: unexpected signature");
    }
    let handle_f = method(code, scene_t, "handleEvent")?.findex;
    let get_f = method(code, scene_t, "getInteractive")?.findex;
    let (gargs, gret) = sig(code, get_f)?;
    if gargs != [scene_t, f64_t, f64_t] || gret != inter_t {
        bail!("Scene.getInteractive: unexpected signature");
    }
    let handle = site(
        code,
        handle_f,
        "Scene.handleEvent",
        inter_t,
        f64_t,
        visible,
        abs_x,
        abs_y,
    )?;
    let get = site(
        code,
        get_f,
        "Scene.getInteractive",
        inter_t,
        f64_t,
        visible,
        abs_x,
        abs_y,
    )?;
    let dbg_file = debug_file(code, "h2d/Scene.hx")?;
    Ok(Plan {
        handle,
        get,
        obj_t,
        f64_t,
        bool_t,
        i32_t,
        dyn_t,
        flow_t,
        flow_cls,
        is_of_type,
        parent,
        overflow,
        ov_t,
        ov_scroll,
        ov_hidden,
        abs_x,
        abs_y,
        mat,
        calc_w,
        calc_h,
        dbg_file,
    })
}

/// `(o: h2d.Object, x, y) -> Bool`: (x, y) lies inside the drawn box of every
/// Scroll / Hidden flow above `o` (Flow.drawRec + Mask.maskWith's rectangle).
fn add_inside(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let (k_scroll, k_hidden) = (int_const(code, p.ov_scroll), int_const(code, p.ov_hidden));
    let mut r = Regs(vec![p.obj_t, p.f64_t, p.f64_t]);
    let (o, x, y) = (Reg(0), Reg(1), Reg(2));
    let (cur, cls, b, fl, ov, idx, k) = (
        r.r(p.obj_t),
        r.r(p.dyn_t),
        r.r(p.bool_t),
        r.r(p.flow_t),
        r.r(p.ov_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let (x1, y1, x2, y2, w, h, m, t) = (
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: cur,
        obj: o,
        field: p.parent,
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.flow_cls,
    });
    a.loop_head("loop");
    a.jmp(Opcode::JNull { reg: cur, offset: 0 }, "inside");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.is_of_type,
        arg0: cur,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "next");
    a.op(Opcode::UnsafeCast { dst: fl, src: cur });
    a.op(Opcode::Field {
        dst: ov,
        obj: fl,
        field: p.overflow,
    });
    a.jmp(Opcode::JNull { reg: ov, offset: 0 }, "next");
    a.op(Opcode::EnumIndex { dst: idx, value: ov });
    a.op(Opcode::Int { dst: k, ptr: k_scroll });
    a.jmp(
        Opcode::JEq {
            a: idx,
            b: k,
            offset: 0,
        },
        "clip",
    );
    a.op(Opcode::Int { dst: k, ptr: k_hidden });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: k,
            offset: 0,
        },
        "next",
    );
    a.label("clip");
    // maskWith: (x1, y1) = abs pos, (x2, y2) = abs pos + mat * (w, h), sorted.
    a.op(Opcode::Field {
        dst: x1,
        obj: fl,
        field: p.abs_x,
    });
    a.op(Opcode::Field {
        dst: y1,
        obj: fl,
        field: p.abs_y,
    });
    a.op(Opcode::Field {
        dst: w,
        obj: fl,
        field: p.calc_w,
    });
    a.op(Opcode::Field {
        dst: h,
        obj: fl,
        field: p.calc_h,
    });
    for (dst, ma, mb, base) in [(x2, p.mat[0], p.mat[2], x1), (y2, p.mat[1], p.mat[3], y1)] {
        a.op(Opcode::Field {
            dst: m,
            obj: fl,
            field: ma,
        });
        a.op(Opcode::Mul { dst, a: w, b: m });
        a.op(Opcode::Field {
            dst: m,
            obj: fl,
            field: mb,
        });
        a.op(Opcode::Mul { dst: t, a: h, b: m });
        a.op(Opcode::Add { dst, a: dst, b: t });
        a.op(Opcode::Add {
            dst,
            a: dst,
            b: base,
        });
    }
    for (lo, hi, l) in [(x1, x2, "sx"), (y1, y2, "sy")] {
        a.jmp(
            Opcode::JSGte {
                a: hi,
                b: lo,
                offset: 0,
            },
            l,
        );
        a.op(Opcode::Mov { dst: t, src: lo });
        a.op(Opcode::Mov { dst: lo, src: hi });
        a.op(Opcode::Mov { dst: hi, src: t });
        a.label(l);
    }
    for (v, lo, hi) in [(x, x1, x2), (y, y1, y2)] {
        a.jmp(
            Opcode::JSLt {
                a: v,
                b: lo,
                offset: 0,
            },
            "outside",
        );
        a.jmp(
            Opcode::JSGte {
                a: v,
                b: hi,
                offset: 0,
            },
            "outside",
        );
    }
    a.label("next");
    a.op(Opcode::Field {
        dst: cur,
        obj: cur,
        field: p.parent,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("inside");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("outside");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    let f = push_fn(
        code,
        vec![p.obj_t, p.f64_t, p.f64_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )?;
    name_fn(code, f, NAME);
    Ok(f)
}

/// After the visible walk: `visible = inside(i, x, y); if (!visible) continue;`.
fn splice(code: &mut Bytecode, s: &Site, inside: RefFun) {
    let f = &mut code.functions[s.fi];
    let at = s.test + 2;
    let [cont] = jump_targets(f, s.test + 1)[..] else {
        unreachable!()
    };
    debug_assert!(cont < s.test);
    let ops = vec![
        Opcode::Call3 {
            dst: s.cond,
            fun: inside,
            arg0: s.cand,
            arg1: s.x,
            arg2: s.y,
        },
        Opcode::JFalse {
            cond: s.cond,
            offset: cont as i32 - (at as i32 + 1) - 1,
        },
    ];
    debug_assert_eq!(ops.len(), N);
    insert_ops(f, at, ops);
    // A visible candidate now enters the inserted check, not past it.
    f.ops[s.test] = Opcode::JTrue {
        cond: s.cond,
        offset: 1,
    };
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let inside = add_inside(code, &p)?;
    splice(code, &p.handle, inside);
    splice(code, &p.get, inside);
    eprintln!(
        "patched scroll hit fn@{} fn@{}: clicks only reach what a scrolling flow shows",
        code.functions[p.handle.fi].findex.0, code.functions[p.get.fi].findex.0
    );
    Ok(())
}

/// Makes the scene's hit tests respect Scroll / Hidden flow clipping, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_scroll_hit(code: &mut Bytecode) {
    let res = plan(code).and_then(|p| {
        let snap = crate::asm::Snap::take(code);
        apply(code, p).inspect_err(|_| snap.restore(code))
    });
    if let Err(e) = res {
        crate::skipped(format!("scroll hit skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let sites = [
            (p.handle.fi, p.handle.test, p.handle.cond),
            (p.get.fi, p.get.test, p.get.cond),
        ];
        let mut code = read(&image);
        patch_scroll_hit(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 1);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let touched = sites.iter().any(|s| s.0 == i);
            assert_eq!(same(a, b), !touched, "function #{i} (fn@{})", a.findex.0);
        }
        let inside = back.functions.last().unwrap().findex;
        for (fi, test, cond) in sites {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(b.regs, a.regs);
            assert_eq!(b.ops.len(), a.ops.len() + N);
            let at = test + 2;
            let map = |t: usize| if t < at { t } else { t + N };
            // Every vanilla jump keeps its target; the test enters the check.
            for i in (0..a.ops.len()).filter(|&i| i != test) {
                let want: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
                assert_eq!(jump_targets(b, map(i)), want, "fn@{} op {i}", a.findex.0);
            }
            assert_eq!(jump_targets(b, test), vec![at]);
            assert!(
                matches!(b.ops[at], Opcode::Call3 { dst, fun, .. } if dst == cond && fun == inside)
            );
            // Outside: the same `continue` as an invisible candidate.
            assert_eq!(jump_targets(b, at + 1), jump_targets(a, test + 1));
            check_flow(b);
            check_types(&back, b, 0..b.ops.len());
        }
        let nf = back.functions.last().unwrap();
        assert_eq!(nf.findex, inside);
        check_flow(nf);
        check_types(&back, nf, 0..nf.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_scroll_hit(&mut again);
        assert!(write(&again) == patched);
    }
}
