// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Battle camera: pan while rotating, finer wheel zoom, opening zoom kept.
//
// 1. `battle.Battle.updateCamera` switches on `dragMode` (enum battle.DragMode:
//    None, Rotate, Pan, DragUnit). Case None/DragUnit is the keyboard + screen-edge
//    pan block; case Rotate only rotates and then jumps to the end of the switch,
//    so holding a pan key does nothing while the right button rotates. The
//    Rotate case now ends with a jump back to the pan block (a Label in front of
//    it, as the JIT wants for backward jumps). The block re-checks lock / pause /
//    focus and leaves through its own jump to the switch end. In the pan block,
//    the edge-scroll part is skipped when `dragMode == Rotate`
//    (`Int t = 1; JEq variant, t -> skip`): the mouse reaches the screen edge
//    while dragging to rotate, which must not pan.
// 2. `onOverlayEvent` wheel zoom: `targetDistance *= delta > 0 ? 1.25 : 0.8`
//    becomes 1.15 / 0.87 (finer steps for the wider Remastered zoom range).
// 3. `initUI` opens the battle at `distance = targetDistance = CameraMaxDistance`.
//    Remastered raises that constant to 60, so the opening value is clamped to
//    40 (vanilla max): `if (40 < d) d = 40`.
//
// Each part is validated against the vanilla shape before editing; a mismatch
// skips that part (logged).

use super::*;

const ZOOM_OUT: (f64, f64) = (1.25, 1.15);
const ZOOM_IN: (f64, f64) = (0.8, 0.87);
const OPEN_DISTANCE: f64 = 40.0;

/// Ops of the dragMode switch in updateCamera.
struct Pan {
    fi: usize,
    /// First op of the None/DragUnit case (keyboard + edge pan).
    pan: usize,
    /// The Rotate case's closing `JAlways -> end`.
    rot_end: usize,
    /// `Int t = 3` of the edge-scroll `dragMode == DragUnit` test.
    edge: usize,
    /// Where the edge-scroll part ends (target of the `JFalse` in front of it).
    skip: usize,
}

fn drag_switch(code: &Bytecode) -> Result<Pan> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let f = method(code, battle_t, "updateCamera")?;
    let fi = fun_index(code, f.findex)?;
    let (drag, drag_t) = field(code, battle_t, "dragMode")?;
    match &code.types[drag_t.0] {
        Type::Enum { constructs, .. }
            if constructs.len() == 4 && s(code, constructs[1].name) == "DragRotateCamera" => {}
        Type::Enum { constructs, .. } => bail!(
            "battle.DragMode is not (_, DragRotateCamera, _, _): {:?}",
            constructs
                .iter()
                .map(|c| s(code, c.name))
                .collect::<Vec<_>>()
        ),
        _ => bail!("dragMode is not an enum"),
    }
    let o = &f.ops;
    let reads_drag = |i: usize| {
        i + 2 < o.len()
            && matches!(o[i], Opcode::GetThis { field, .. } if field == drag)
            && matches!(o[i + 1], Opcode::NullCheck { .. })
            && matches!(o[i + 2], Opcode::EnumIndex { .. })
    };
    let switches: Vec<usize> = (3..o.len())
        .filter(|&i| matches!(o[i], Opcode::Switch { .. }) && reads_drag(i - 3))
        .collect();
    let [sw] = switches[..] else {
        bail!(
            "updateCamera: {} dragMode switches (want 1)",
            switches.len()
        );
    };
    let Opcode::Switch { offsets, .. } = &o[sw] else {
        unreachable!()
    };
    if offsets.len() != 3 {
        bail!("updateCamera: dragMode switch has {} cases", offsets.len());
    }
    let t = jump_targets(f, sw);
    let (pan, rot, drag_pan, end) = (t[0], t[1], t[2], t[3]);
    if !(sw < pan && pan < rot && rot < drag_pan && drag_pan < end) {
        bail!("updateCamera: dragMode cases out of order");
    }
    if matches!(o[pan], Opcode::Label) {
        bail!("updateCamera: pan block already has a Label (applied)");
    }
    let Opcode::JFalse { .. } = o[pan] else {
        bail!("updateCamera: pan block does not start with the lock test");
    };
    let exit = jump_targets(f, pan)[0];
    if exit + 1 != rot
        || !matches!(o[exit], Opcode::JAlways { .. })
        || jump_targets(f, exit) != [end]
    {
        bail!("updateCamera: pan block does not leave through a jump to the switch end");
    }
    let rot_end = drag_pan - 1;
    if !matches!(o[rot_end], Opcode::JAlways { .. }) || jump_targets(f, rot_end) != [end] {
        bail!("updateCamera: Rotate case does not end with a jump to the switch end");
    }
    // `JFalse edgeOn -> skip; dragMode variant; Int t = 3; JNotEq` (margin 40 / 10 px).
    let edges: Vec<usize> = (pan + 1..exit)
        .filter(|&i| {
            reads_drag(i)
                && matches!(o[i - 1], Opcode::JFalse { .. })
                && matches!(o[i + 3], Opcode::Int { ptr, .. } if code.ints[ptr.0] == 3)
                && matches!(o[i + 4], Opcode::JNotEq { .. })
        })
        .collect();
    let [e] = edges[..] else {
        bail!(
            "updateCamera: {} edge-scroll dragMode tests (want 1)",
            edges.len()
        );
    };
    let skip = jump_targets(f, e - 1)[0];
    if !(e < skip && skip < exit) {
        bail!("updateCamera: edge-scroll skip target out of the pan block");
    }
    Ok(Pan {
        fi,
        pan,
        rot_end,
        edge: e + 3,
        skip,
    })
}

fn apply_pan(code: &mut Bytecode, p: &Pan) {
    let one = int_const(code, 1);
    let f = &mut code.functions[p.fi];
    let (Opcode::Int { dst: t, .. }, Opcode::EnumIndex { dst: v, .. }) =
        (f.ops[p.edge].clone(), f.ops[p.edge - 1].clone())
    else {
        unreachable!()
    };
    // Guard first (it sits after the pan start), then the Label at the pan start.
    // The JEq lands at edge + 1 and jumps to the shifted skip (skip + 2).
    insert_ops(
        f,
        p.edge,
        vec![
            Opcode::Int { dst: t, ptr: one },
            Opcode::JEq {
                a: v,
                b: t,
                offset: (p.skip - p.edge) as i32,
            },
        ],
    );
    insert_ops(f, p.pan, vec![Opcode::Label]);
    let rot_end = p.rot_end + 3;
    let Opcode::JAlways { offset } = &mut f.ops[rot_end] else {
        unreachable!()
    };
    *offset = p.pan as i32 - rot_end as i32 - 1;
    eprintln!(
        "patched battle camera fn@{}: keyboard pan while rotating (op {} -> {}), no edge scroll during rotate",
        f.findex.0, p.rot_end, p.pan
    );
}

/// The wheel zoom factors in onOverlayEvent: `Float r = 1.25; JAlways +1; Float r = 0.8; Mul`.
fn wheel(code: &Bytecode) -> Result<(usize, usize)> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let f = method(code, battle_t, "onOverlayEvent")?;
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    let is =
        |i: usize, v: f64| matches!(o[i], Opcode::Float { ptr, .. } if code.floats[ptr.0] == v);
    let find = |hi: f64, lo: f64| -> Vec<usize> {
        (0..o.len().saturating_sub(3))
            .filter(|&i| {
                is(i, hi)
                    && matches!(o[i + 1], Opcode::JAlways { offset: 1 })
                    && is(i + 2, lo)
                    && matches!(o[i + 3], Opcode::Mul { .. })
            })
            .collect()
    };
    if !find(ZOOM_OUT.1, ZOOM_IN.1).is_empty() {
        bail!(
            "onOverlayEvent: wheel factors already {}/{} (applied)",
            ZOOM_OUT.1,
            ZOOM_IN.1
        );
    }
    let hits = find(ZOOM_OUT.0, ZOOM_IN.0);
    let [w] = hits[..] else {
        bail!(
            "onOverlayEvent: {} wheel zoom factor pairs (want 1)",
            hits.len()
        );
    };
    Ok((fi, w))
}

fn apply_wheel(code: &mut Bytecode, (fi, w): (usize, usize)) {
    let hi = float_const(code, ZOOM_OUT.1);
    let lo = float_const(code, ZOOM_IN.1);
    let f = &mut code.functions[fi];
    for (i, k) in [(w, hi), (w + 2, lo)] {
        if let Opcode::Float { ptr, .. } = &mut f.ops[i] {
            *ptr = k;
        }
    }
    eprintln!(
        "patched battle camera fn@{} op {w}: wheel zoom x{}/x{}",
        f.findex.0, ZOOM_OUT.1, ZOOM_IN.1
    );
}

/// `d = (CameraMaxDistance const).value` (a SafeCast) followed by
/// `state.camera.targetDistance = d; camera.distance = d` in initUI.
fn opening(code: &Bytecode) -> Result<(usize, usize)> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let f = method(code, battle_t, "initUI")?;
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    let set_named = |i: usize, src: Reg, name: &str| match o[i] {
        Opcode::SetField {
            obj,
            field,
            src: s2,
        } if s2 == src => field_name(code, f.regs[obj.0 as usize], field) == Some(name),
        _ => false,
    };
    let hits: Vec<usize> = (0..o.len().saturating_sub(2))
        .filter(|&i| match o[i] {
            Opcode::SafeCast { dst, .. } => {
                set_named(i + 1, dst, "targetDistance") && set_named(i + 2, dst, "distance")
            }
            _ => false,
        })
        .collect();
    let [c] = hits[..] else {
        bail!("initUI: {} opening distance stores (want 1)", hits.len());
    };
    let Opcode::SafeCast { dst, src } = o[c] else {
        unreachable!()
    };
    if !matches!(code.types[f.regs[dst.0 as usize].0], Type::F64) {
        bail!("initUI: opening distance is not f64");
    }
    // ... Call1 v = getConst("CameraMaxDistance"); NullCheck v; Field x = v.value; ... SafeCast d = x
    let from_const = (0..c).rev().find_map(|i| match o[i] {
        Opcode::Field { dst: x, obj, .. } if x == src => Some(obj),
        _ => None,
    });
    let named = from_const.is_some_and(|v| {
        (0..c).rev().any(|i| match o[i] {
            Opcode::Call1 { dst, arg0, .. } if dst == v => (0..i).rev().any(|j| {
                matches!(o[j], Opcode::GetGlobal { dst: a, global }
                    if a == arg0 && crate::job_xp::const_str(code, global) == Some("CameraMaxDistance"))
            }),
            _ => false,
        })
    });
    if !named {
        bail!("initUI: opening distance is not read from CameraMaxDistance");
    }
    if matches!(o.get(c + 1), Some(Opcode::Float { .. })) {
        bail!("initUI: opening distance already clamped (applied)");
    }
    Ok((fi, c))
}

fn apply_opening(code: &mut Bytecode, (fi, c): (usize, usize)) {
    let k = float_const(code, OPEN_DISTANCE);
    let f = &mut code.functions[fi];
    let Opcode::SafeCast { dst: d, .. } = f.ops[c] else {
        unreachable!()
    };
    let r = new_reg(f, f.regs[d.0 as usize]);
    insert_ops(
        f,
        c + 1,
        vec![
            Opcode::Float { dst: r, ptr: k },
            Opcode::JNotLt {
                a: r,
                b: d,
                offset: 1,
            },
            Opcode::Mov { dst: d, src: r },
        ],
    );
    eprintln!(
        "patched battle camera fn@{} op {c}: opening zoom at most {OPEN_DISTANCE}",
        f.findex.0
    );
}

/// Applies the three camera parts independently; each one that does not match
/// the vanilla shape is skipped and logged.
pub(crate) fn patch_battle_camera(code: &mut Bytecode) {
    match drag_switch(code) {
        Ok(p) => apply_pan(code, &p),
        Err(e) => eprintln!("battle camera pan skipped: {e:#}"),
    }
    match wheel(code) {
        Ok(p) => apply_wheel(code, p),
        Err(e) => eprintln!("battle camera wheel skipped: {e:#}"),
    }
    match opening(code) {
        Ok(p) => apply_opening(code, p),
        Err(e) => eprintln!("battle camera opening zoom skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, write};
    use crate::testsim::{Sim, V};

    /// Stubs for the vanilla calls of the three spans; inputs come from map "in".
    fn sim(code: &Bytecode) -> Sim<'_> {
        Sim::new(
            code,
            code.functions.len(),
            move |c, f, a| {
                Some(match fname(code, f) {
                    "isDown" => match &a[0] {
                        V::S(k) => c.map("keys", k),
                        o => panic!("isDown({o:?})"),
                    },
                    "get_mouseX" => c.map("in", "mx"),
                    "get_mouseY" => c.map("in", "my"),
                    "isPaused" => V::B(false),
                    "get_isFocused" => V::B(true),
                    "get_displayMode" => V::I(1),
                    "getInstance" => c.obj(&[]),
                    "getConst" => c.map("in", "const"),
                    n => panic!("unexpected call {n}@{}", f.0),
                })
            },
            |_, m, _| panic!("unexpected method call {m}"),
        )
    }

    fn f(v: &V) -> f64 {
        match v {
            V::F(x) => *x,
            o => panic!("not a float: {o:?}"),
        }
    }

    /// Runs the patched pan block of updateCamera (from its Label) with
    /// `dragMode` = `mode`, pan keys `keys` (0 left, 1 right, 2 up, 3 down) held
    /// and the mouse at (mx, my) on a 1920x1080 scene. Some((dx, dy)) when the
    /// camera target moves, None when the block exits without moving.
    fn pan_run(
        code: &Bytecode,
        p: &Pan,
        mode: i32,
        keys: &[usize],
        mx: f64,
        my: f64,
    ) -> Option<(f64, f64)> {
        let b = &code.functions[p.fi];
        let battle_t = obj_type(code, "battle.Battle").unwrap();
        let fld = |t: RefType, n: &str| field(code, t, n).unwrap();
        let (game_f, game_t) = fld(battle_t, "game");
        let (s2d_f, s2d_t) = fld(battle_t, "s2d");
        let (g, cls_t) = class_global(code, "Game").unwrap();
        let (prefs_f, prefs_t) = fld(cls_t, "PREFS");
        let slide = field_of_virtual(code, prefs_t, "battleCameraSlideBorder")
            .unwrap()
            .0;
        let exit = jump_targets(b, p.pan + 1)[0];
        let mv = (p.pan..exit)
            .find(|&i| matches!(b.ops[i], Opcode::New { .. }))
            .unwrap();
        let key_globals: Vec<RefGlobal> = (p.pan..exit)
            .filter_map(|i| match (&b.ops[i], &b.ops[i + 1]) {
                (Opcode::GetGlobal { global, .. }, Opcode::Call1 { fun, .. })
                    if fname(code, *fun) == "isDown" =>
                {
                    Some(*global)
                }
                _ => None,
            })
            .collect();
        assert_eq!(key_globals.len(), 4);

        let mut s = sim(code);
        for (i, g) in key_globals.iter().enumerate() {
            s.c.globals.insert(g.0, V::S(format!("k{i}")));
            s.c.put("keys", &format!("k{i}"), V::B(keys.contains(&i)));
        }
        s.c.put("in", "mx", V::F(mx));
        s.c.put("in", "my", V::F(my));
        let prefs = s.c.obj(&[(slide, V::B(true))]);
        let cls = s.c.obj(&[(prefs_f, prefs)]);
        s.c.globals.insert(g.0, cls);
        let ui = s.c.obj(&[]);
        let game = s.c.obj(&[(fld(game_t, "ui").0, ui)]);
        let scene = s.c.obj(&[
            (fld(s2d_t, "width").0, V::I(1920)),
            (fld(s2d_t, "height").0, V::I(1080)),
        ]);
        let drag = s.c.enm(mode, vec![]);
        let this = s.c.obj(&[
            (game_f, game),
            (s2d_f, scene),
            (fld(battle_t, "dragMode").0, drag),
            (fld(battle_t, "currentSkill").0, V::Null),
        ]);
        let mut r = vec![V::Null; b.regs.len()];
        r[0] = this;
        r[4] = V::B(true);
        let stop = s.span(b.findex, &mut r, p.pan, &[mv, exit]);
        (stop == mv).then(|| (f(&r[3]), f(&r[9])))
    }

    /// Runs the patched wheel branch of onOverlayEvent: returns the new targetDistance.
    fn wheel_run(code: &Bytecode, (fi, w): (usize, usize), dist: f64, delta: f64) -> f64 {
        let b = &code.functions[fi];
        let o = &b.ops;
        let (Opcode::GetThis { field: state_f, .. }, Opcode::Field { field: cam_f, .. }) =
            (&o[w - 9], &o[w - 7])
        else {
            panic!("wheel prologue")
        };
        let Opcode::Field { field: dist_f, .. } = o[w - 5] else {
            panic!("targetDistance")
        };
        let Opcode::Field {
            obj: ev,
            field: delta_f,
            ..
        } = o[w - 4]
        else {
            panic!("wheelDelta")
        };
        let Opcode::JNotLt { b: min, .. } = o[w + 8] else {
            panic!("min clamp")
        };
        let Opcode::JNotLt { a: max, .. } = o[w + 11] else {
            panic!("max clamp")
        };
        assert!(matches!(o[w + 15], Opcode::SetField { .. }));

        let mut s = sim(code);
        let cam = s.c.obj(&[(dist_f, V::F(dist))]);
        let state = s.c.obj(&[(*cam_f, cam.clone())]);
        let this = s.c.obj(&[(*state_f, state)]);
        let event = s.c.obj(&[(delta_f, V::F(delta))]);
        let mut r = vec![V::Null; b.regs.len()];
        r[0] = this;
        r[ev.0 as usize] = event;
        r[min.0 as usize] = V::F(12.0);
        r[max.0 as usize] = V::F(60.0);
        assert_eq!(s.span(b.findex, &mut r, w - 9, &[w + 16]), w + 16);
        f(&s.c.get(&cam, dist_f))
    }

    /// Runs the patched opening-distance code of initUI with CameraMaxDistance
    /// = `max`: returns (state.camera.targetDistance, camera.distance).
    fn opening_run(code: &Bytecode, (fi, c): (usize, usize), max: f64) -> (f64, f64) {
        let b = &code.functions[fi];
        let o = &b.ops;
        let start = (0..c)
            .rev()
            .find(|&i| {
                matches!(o[i], Opcode::GetGlobal { global, .. }
                    if crate::job_xp::const_str(code, global) == Some("CameraMaxDistance"))
            })
            .unwrap();
        let Opcode::SafeCast { src, .. } = o[c] else {
            panic!("SafeCast")
        };
        let value_f = (start..c)
            .find_map(|i| match o[i] {
                Opcode::Field { dst, field, .. } if dst == src => Some(field),
                _ => None,
            })
            .unwrap();
        let (Opcode::SetField { field: td_f, .. }, Opcode::SetField { field: d_f, .. }) =
            (&o[c + 4], &o[c + 5])
        else {
            panic!("stores")
        };
        let battle_t = obj_type(code, "battle.Battle").unwrap();
        let (state_f, state_t) = field(code, battle_t, "state").unwrap();
        let mut s = sim(code);
        let konst = s.c.obj(&[(value_f, V::F(max))]);
        s.c.put("in", "const", konst);
        let scam = s.c.obj(&[]);
        let state =
            s.c.obj(&[(field(code, state_t, "camera").unwrap().0, scam.clone())]);
        let cam = s.c.obj(&[]);
        let this = s.c.obj(&[
            (state_f, state),
            (field(code, battle_t, "camera").unwrap().0, cam.clone()),
        ]);
        let mut r = vec![V::Null; b.regs.len()];
        r[0] = this;
        assert_eq!(s.span(b.findex, &mut r, start, &[c + 6]), c + 6);
        (f(&s.c.get(&scam, *td_f)), f(&s.c.get(&cam, *d_f)))
    }

    /// Only the three camera functions change (+3, +0, +3 ops); jumps and types
    /// check out; a second pass refuses every part and leaves the image as is.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = drag_switch(&orig).expect("pan plan");
        let wp = wheel(&orig).expect("wheel plan");
        let op = opening(&orig).expect("opening plan");
        let mut code = read(&image);
        patch_battle_camera(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        let changed = [(p.fi, 3, 0), (wp.0, 0, 0), (op.0, 3, 1)];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            match changed.iter().find(|c| c.0 == i) {
                Some(&(_, ops, regs)) => {
                    assert!(!same(a, b), "fn@{} unchanged", a.findex.0);
                    assert_eq!(b.ops.len(), a.ops.len() + ops);
                    assert_eq!(b.regs.len(), a.regs.len() + regs);
                    assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
                    check_flow(b);
                }
                None => assert!(same(a, b), "function #{i} (fn@{})", a.findex.0),
            }
        }

        let b = &back.functions[p.fi];
        assert!(matches!(b.ops[p.pan], Opcode::Label));
        assert_eq!(jump_targets(b, p.rot_end + 3), [p.pan]);
        let sw = (0..p.pan)
            .rev()
            .find(|&i| matches!(b.ops[i], Opcode::Switch { .. }))
            .unwrap();
        assert_eq!(jump_targets(b, sw)[0], p.pan + 1);
        // After the Label (+1): Int at edge + 1, JEq at edge + 2 -> skip (+3).
        assert_eq!(jump_targets(b, p.edge + 2), [p.skip + 3]);
        check_types(&back, b, p.edge + 1..p.edge + 3);
        let (fi, w) = wp;
        let fl = |i: usize| match back.functions[fi].ops[i] {
            Opcode::Float { ptr, .. } => back.floats[ptr.0],
            _ => panic!("op {i} is not Float"),
        };
        assert_eq!((fl(w), fl(w + 2)), (ZOOM_OUT.1, ZOOM_IN.1));
        check_types(&back, &back.functions[op.0], op.1 + 1..op.1 + 4);

        let mut again = read(&patched);
        assert!(drag_switch(&again).is_err());
        assert!(wheel(&again).is_err());
        assert!(opening(&again).is_err());
        patch_battle_camera(&mut again);
        assert!(write(&again) == patched);
    }

    /// While rotating, a held pan key still moves the camera target; the screen
    /// edge does not. Outside rotation the edge scroll is unchanged.
    #[test]
    fn pans_with_keys_while_rotating() {
        let Some(image) = game() else { return };
        let p = drag_switch(&read(&image)).expect("plan");
        let mut code = read(&image);
        patch_battle_camera(&mut code);
        let (none, rotate, drag_unit) = (0, 1, 3);
        // Rotate: key left (and right edge) -> keys win; key down -> dy only.
        assert_eq!(
            pan_run(&code, &p, rotate, &[0], 1919.0, 540.0),
            Some((-1.0, 0.0))
        );
        assert_eq!(
            pan_run(&code, &p, rotate, &[3], 960.0, 540.0),
            Some((0.0, 1.0))
        );
        // Rotate, no key, mouse in the corner: no edge scroll.
        assert_eq!(pan_run(&code, &p, rotate, &[], 0.0, 0.0), None);
        assert_eq!(pan_run(&code, &p, rotate, &[], 960.0, 540.0), None);
        // None / DragUnit keep the vanilla edge scroll (10 px / 40 px margin).
        assert_eq!(pan_run(&code, &p, none, &[], 0.0, 0.0), Some((-1.0, -1.0)));
        assert_eq!(pan_run(&code, &p, none, &[], 30.0, 540.0), None);
        assert_eq!(
            pan_run(&code, &p, drag_unit, &[], 30.0, 1079.0),
            Some((-1.0, 1.0))
        );
        assert_eq!(
            pan_run(&code, &p, none, &[1], 960.0, 540.0),
            Some((1.0, 0.0))
        );
    }

    /// One notch scales the zoom by 1.15 / 0.87, clamped to [min, max].
    #[test]
    fn wheel_steps_are_finer() {
        let Some(image) = game() else { return };
        let wp = wheel(&read(&image)).expect("plan");
        let mut code = read(&image);
        patch_battle_camera(&mut code);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(wheel_run(&code, wp, 40.0, 1.0), 46.0));
        assert!(near(wheel_run(&code, wp, 40.0, -1.0), 34.8));
        assert!(near(wheel_run(&code, wp, 58.0, 1.0), 60.0));
        assert!(near(wheel_run(&code, wp, 13.0, -1.0), 12.0));
    }

    /// The battle opens at min(CameraMaxDistance, 40).
    #[test]
    fn opening_zoom_capped() {
        let Some(image) = game() else { return };
        let op = opening(&read(&image)).expect("plan");
        let mut code = read(&image);
        patch_battle_camera(&mut code);
        assert_eq!(opening_run(&code, op, 60.0), (40.0, 40.0));
        assert_eq!(opening_run(&code, op, 35.0), (35.0, 35.0));
        assert_eq!(opening_run(&code, op, 40.0), (40.0, 40.0));
    }

    /// A Rotate case without its closing jump is refused; the other parts still apply.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = drag_switch(&orig).expect("plan");
        let mut code = read(&image);
        code.functions[p.fi].ops[p.rot_end] = Opcode::Label;
        let before = format!("{:?}", code.functions[p.fi].ops);
        assert!(drag_switch(&code).is_err());
        patch_battle_camera(&mut code);
        assert_eq!(format!("{:?}", code.functions[p.fi].ops), before);
        assert!(wheel(&code).is_err() && opening(&code).is_err());
    }
}
