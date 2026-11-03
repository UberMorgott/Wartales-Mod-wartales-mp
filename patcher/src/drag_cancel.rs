// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A UI element drag (the battle skill bar's "reorder skills in battle" drag)
// can no longer stay stuck to the cursor, and Escape or a right click cancels it.
//
// `ui.comp.Element.onDrag` (Element.hx:296-322) gives the element a push
// handler that starts a per-frame `waitUntil` loop: once the mouse is 20 px
// away it calls `BaseUI.startDrag(el)` (a copy of the skill icon follows the
// cursor; while `ui.dragElement` is set every skill click is ignored), and it
// ends the drag only when `el.pushedDrag` drops, which only a release on the
// element's own interactive (or a release outside it) does. `Element.onRemove`
// cancels a started drag, not a push still inside the 20 px: a skill bar
// rebuilt in that moment (`SkillBar.setUnit` -> `rebuild`, which a co-op guest
// runs whenever a sync marks its battle UI dirty: another player's action, a
// round reset, a skill result) removes the pushed slot with its interactive, so
// no release can reach it any more; the next mouse move starts the drag on the
// removed slot and nothing ever ends it. The battle is softlocked.
//
// The loop now starts with
//
//   if (!el.allocated || Key.isPressed(ESCAPE) || Key.isPressed(MOUSE_RIGHT)) {
//       el.pushedDrag = false; el.stopDrag(true); return true;
//   }
//
// `stopDrag(true)` is the vanilla cancel (`onStopDrag(true)`: the skill bar
// drops its edit; `endDrag` clears `ui.dragElement` only when it is `el`), and
// returning true ends the loop. Local UI only, nothing is sent.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

/// hxd.Key codes.
const ESCAPE: i32 = 27;
const MOUSE_RIGHT: i32 = 1;

struct Plan {
    fi: usize,
    /// `el = <loop env>.0` as the loop reads it before `startDrag`.
    get_el: Opcode,
    el_t: RefType,
    allocated: RefField,
    pushed: RefField,
    stop: RefFun,
    ref_t: RefType,
    pressed: RefFun,
    bool_t: RefType,
    i32_t: RefType,
    void_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let el_t = obj_type(code, "ui.comp.Element")?;
    let ui_t = obj_type(code, "ui.BaseUI")?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let start = method(code, ui_t, "startDrag")?.findex;
    let stop = method(code, el_t, "stopDrag")?.findex;
    let (stop_args, _) = sig(code, stop)?;
    let ref_t = match stop_args[..] {
        [a, r] if a == el_t && matches!(code.types[r.0], Type::Ref(t) if t == bool_t) => r,
        _ => bail!("Element.stopDrag: unexpected signature"),
    };
    let allocated = typed(code, el_t, "allocated", bool_t)?;
    let pushed = typed(code, el_t, "pushedDrag", bool_t)?;
    let pressed: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "isPressed"
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args[..] == [i32_t] && t.ret == bool_t)
        })
        .map(|f| f.findex)
        .collect();
    let [pressed] = pressed[..] else {
        bail!("expected one Key.isPressed(i32), found {}", pressed.len());
    };

    // The drag loop: the only caller of BaseUI.startDrag.
    let loops: Vec<usize> = (0..code.functions.len())
        .filter(|&i| calls(&code.functions[i], start))
        .collect();
    let [fi] = loops[..] else {
        bail!(
            "expected one BaseUI.startDrag caller, found {}",
            loops.len()
        );
    };
    let f = &code.functions[fi];
    let ret = f.t.as_fun(code).context("drag loop type")?.ret;
    if ret != bool_t || !calls(f, stop) {
        bail!("drag loop: not a `-> Bool` waitUntil step calling stopDrag");
    }
    if f.ops.iter().any(
        |o| matches!(o, Opcode::Field { field, obj, .. } if *field == allocated && f.regs[obj.0 as usize] == el_t),
    ) {
        bail!("drag loop: already applied");
    }
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
        bail!("drag loop: a jump targets op 0");
    }
    let at = f
        .ops
        .iter()
        .position(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == start))
        .unwrap();
    let Opcode::Call2 { arg1: el, .. } = f.ops[at] else {
        unreachable!()
    };
    let get_el = f.ops[..at]
        .iter()
        .rev()
        .find_map(|o| match o {
            Opcode::EnumField {
                dst, value: Reg(0), ..
            } if *dst == el => Some(o.clone()),
            _ => None,
        })
        .context("drag loop: startDrag's element is not read from the loop env")?;
    if f.regs[el.0 as usize] != el_t {
        bail!("drag loop: startDrag's element is not a ui.comp.Element");
    }
    Ok(Plan {
        fi,
        get_el,
        el_t,
        allocated,
        pushed,
        stop,
        ref_t,
        pressed,
        bool_t,
        i32_t,
        void_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let (esc, right) = (int_const(code, ESCAPE), int_const(code, MOUSE_RIGHT));
    let f = &mut code.functions[p.fi];
    let el = new_reg(f, p.el_t);
    let b = new_reg(f, p.bool_t);
    let k = new_reg(f, p.i32_t);
    let rb = new_reg(f, p.ref_t);
    let v = new_reg(f, p.void_t);
    let Opcode::EnumField {
        value,
        construct,
        field,
        ..
    } = p.get_el
    else {
        unreachable!()
    };
    let t = hlbc::types::ValBool(true);
    let mut ops = vec![
        Opcode::EnumField {
            dst: el,
            value,
            construct,
            field,
        },
        Opcode::Field {
            dst: b,
            obj: el,
            field: p.allocated,
        },
        Opcode::JFalse { cond: b, offset: 0 }, // 2 -> CANCEL
        Opcode::Int { dst: k, ptr: esc },
        Opcode::Call1 {
            dst: b,
            fun: p.pressed,
            arg0: k,
        },
        Opcode::JTrue { cond: b, offset: 0 }, // 5 -> CANCEL
        Opcode::Int { dst: k, ptr: right },
        Opcode::Call1 {
            dst: b,
            fun: p.pressed,
            arg0: k,
        },
        Opcode::JFalse { cond: b, offset: 0 }, // 8 -> vanilla op 0
        Opcode::Bool {
            dst: b,
            value: hlbc::types::ValBool(false),
        }, // 9 CANCEL
        Opcode::SetField {
            obj: el,
            field: p.pushed,
            src: b,
        },
        Opcode::Bool { dst: b, value: t },
        Opcode::Ref { dst: rb, src: b },
        Opcode::Call2 {
            dst: v,
            fun: p.stop,
            arg0: el,
            arg1: rb,
        },
        Opcode::Bool { dst: b, value: t },
        Opcode::Ret { ret: b },
    ];
    let n = ops.len();
    resolve_jumps(&mut ops, &[(2, 9), (5, 9), (8, n)]);
    insert_ops(f, 0, ops);
    eprintln!(
        "patched drag cancel fn@{}: a drag whose element left the scene, Escape or a right click cancels it",
        f.findex.0
    );
}

/// Cancels a UI drag whose element was removed, or on Escape / a right click,
/// or leaves `code` untouched and logs why.
pub(crate) fn patch_drag_cancel(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("drag cancel skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, shifted, write};
    use crate::testsim::{Sim, V};

    /// Inserted ops at the loop's start.
    const N: usize = 16;
    /// Stands in for the drag's per-frame update closure.
    const UPDATE: RefFun = RefFun(usize::MAX);

    /// One frame of the drag loop `fi`: the element is in the scene or not,
    /// the drag already started or not, the mouse 30 px away, `keys` pressed
    /// this frame. Returns (loop done, startDrag calls, stopDrag args, pushedDrag after).
    fn frame(
        code: &Bytecode,
        fi: usize,
        in_scene: bool,
        started: bool,
        keys: &[i32],
    ) -> (bool, usize, Vec<V>, V) {
        let f = &code.functions[fi];
        let el_t = obj_type(code, "ui.comp.Element").unwrap();
        let fld = |t: RefType, n: &str| field(code, t, n).unwrap().0;
        let pt_t = obj_type(code, "h2d.col.PointImpl").unwrap();
        let (x, y) = (fld(pt_t, "x"), fld(pt_t, "y"));
        // The env's `dragging` flag array: the type of env field 4.
        let env_t = match &code.types[f.regs[0].0] {
            Type::Enum { constructs, .. } => constructs[0].params.clone(),
            o => panic!("env {o:?}"),
        };
        let arr_t = env_t[4];
        let keys = keys.to_vec();
        let mut s = Sim::new(
            code,
            code.functions.len(),
            move |c, fun, a| {
                if fun == UPDATE {
                    return Some(V::Null);
                }
                Some(match fname(code, fun) {
                    "get_current" => c.map("in", "ui"),
                    "mousePos" => c.obj(&[(x, V::F(30.0)), (y, V::F(0.0))]),
                    "isPressed" => match a[0] {
                        V::I(k) => V::B(keys.contains(&k)),
                        ref o => panic!("isPressed({o:?})"),
                    },
                    "startDrag" => {
                        c.log.push(("start", a.to_vec()));
                        V::Null
                    }
                    "stopDrag" => {
                        c.log.push(("stop", a.to_vec()));
                        V::Null
                    }
                    "toggleClass" => V::Null,
                    n => panic!("unexpected call {n}@{}", fun.0),
                })
            },
            // startingDrag (no result), canDrag (true: the skill bar is editable)
            |_, _, _| V::B(true),
        );
        let ui = s.c.obj(&[]);
        s.c.put("in", "ui", ui);
        let dom = s.c.obj(&[]);
        let el = s.c.obj(&[
            (fld(el_t, "allocated"), V::B(in_scene)),
            (fld(el_t, "pushedDrag"), V::B(true)),
            (fld(el_t, "dom"), dom),
        ]);
        let start = s.c.obj(&[(x, V::F(0.0)), (y, V::F(0.0))]);
        let delta = s.c.obj(&[(x, V::F(0.0)), (y, V::F(0.0))]);
        let dragging = s.c.arr(
            fld(arr_t, "length"),
            fld(arr_t, "array"),
            vec![V::B(started)],
        );
        let env =
            s.c.enm(0, vec![el.clone(), start, delta, V::Fun(UPDATE), dragging]);
        let done = s.run(f.findex, vec![env, V::F(0.016)]);
        let starts = s.c.take("start").len();
        let stops = s.c.take("stop");
        assert!(stops.len() <= 1, "{stops:?}");
        let stop = stops
            .into_iter()
            .next()
            .map(|a| a[1..].to_vec())
            .unwrap_or_default();
        (
            done == V::B(true),
            starts,
            stop,
            s.c.get(&el, fld(el_t, "pushedDrag")),
        )
    }

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, stop, pressed) = (p.fi, p.stop, p.pressed);
        let mut code = read(&image);
        patch_drag_cancel(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        shifted(a, b, 0, N);
        assert_eq!(jump_targets(b, 2), vec![9]);
        assert_eq!(jump_targets(b, 5), vec![9]);
        assert_eq!(jump_targets(b, 8), vec![N]);
        assert!(matches!(b.ops[4], Opcode::Call1 { fun, .. } if fun == pressed));
        assert!(matches!(b.ops[13], Opcode::Call2 { fun, .. } if fun == stop));
        assert!(matches!(b.ops[N - 1], Opcode::Ret { .. }));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_drag_cancel(&mut again);
        assert!(write(&again) == patched);
    }

    /// The softlock: the pushed slot is removed (bar rebuilt) before the
    /// mouse got 20 px away. Vanilla then starts the drag on the removed slot
    /// and keeps it (pushedDrag can never drop); patched, it is cancelled
    /// before it starts. Escape / right click cancel a running drag; a plain
    /// frame still runs the vanilla loop.
    #[test]
    fn removed_slot_and_cancel_keys_end_the_drag() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let fi = plan(&orig).expect("plan").fi;
        let mut code = read(&image);
        patch_drag_cancel(&mut code);
        let canc = vec![V::B(true)];

        // vanilla: the removed slot's drag starts and the loop goes on
        assert_eq!(
            frame(&orig, fi, false, false, &[]),
            (false, 1, vec![], V::B(true))
        );
        // patched: cancelled, never started, loop over
        assert_eq!(
            frame(&code, fi, false, false, &[]),
            (true, 0, canc.clone(), V::B(false))
        );
        assert_eq!(
            frame(&code, fi, false, true, &[]),
            (true, 0, canc.clone(), V::B(false))
        );
        for k in [ESCAPE, MOUSE_RIGHT] {
            assert_eq!(
                frame(&code, fi, true, true, &[k]),
                (true, 0, canc.clone(), V::B(false))
            );
        }
        // a live drag with no key: unchanged
        assert_eq!(
            frame(&code, fi, true, true, &[]),
            frame(&orig, fi, true, true, &[])
        );
        assert_eq!(
            frame(&code, fi, true, true, &[]),
            (false, 0, vec![], V::B(true))
        );
        assert_eq!(
            frame(&code, fi, true, false, &[]),
            (false, 1, vec![], V::B(true))
        );
    }
}
