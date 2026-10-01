// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Diagnostic for the new-game customize screen: the 4th human slot (front
// right model) cannot be hovered or clicked, in vanilla too. The 3D side (hit
// capsules, prefab, ray cast) checks out, so a 2D element over that spot is the
// suspect. This pass prints, never changes, what the 2D scene hit-test returns
// at the cursor and what the event system currently hovers.
//
// `CustomizeScreen.update`, right after its `super.update(dt)`, calls a new
// function `probe(screen)`:
//
//   try {
//     if (Sys.time() - gLast < GAP) return;                 // at most 4 lines/s
//     var s2d = this.windowRoot.getScene();   (null -> return)
//     var ev = s2d.events;                     (null -> return)
//     var hit = s2d.getInteractive(s2d.mouseX, s2d.mouseY); // top visible h2d.Interactive
//     over = ev.overList (n, first OVER entries), push = ev.pushList (n, first entry)
//     if all of that is unchanged since the last printed line: return;
//     gLast = now;                     // the state itself is stored after the println
//     Sys.println("mp: slot4 hit: m=<window x,y> v=<scene x,y> of <W>x<H>
//         2d=<Std.string(hit)> abs=<x,y> wh=<w>x<h> prop=<b> cancel=<b>
//         < <parent>[hidden] < ... (5 levels) | over=<n> ; <entry> ; ... push=<n> <entry>");
//   } catch (_) {}
//
// `Std.string` of an h2d object is `name(full.ClassName)` (h2d.Object.toString)
// and of a 3D one `ShortClass(name)` (h3d.scene.Object.toString), so the over
// list tells whether the 3D interactive (s3d) got the hover. Every read is
// null-guarded (native arrays too: the JIT's GetArray does not check) and the
// whole probe runs in a Trap: it never throws into the game.
// Printed through Sys.println -> hl_sys_print, which the shim copies into
// shim.log as "game:" lines (see diag.rs).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs, Snap};
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::RefGlobal;

const TAG: &str = "mp: slot4 hit:";
/// Minimum seconds between two lines.
const GAP: f64 = 0.25;
/// Parent levels printed above the hit.
const CHAIN: i32 = 5;
/// Over-list entries printed.
const OVER: i32 = 3;

struct Ty {
    cs: RefType,
    obj: RefType,
    inter: RefType,
    scene: RefType,
    events: RefType,
    arr: RefType,
    native_arr: RefType,
    root: RefType,
    str_: RefType,
    dyn_: RefType,
    void: RefType,
    i32_: RefType,
    bool_: RefType,
    f64_: RefType,
}

struct Fl {
    window_root: RefField,
    events: RefField,
    scene_w: RefField,
    scene_h: RefField,
    ev_mx: RefField,
    ev_my: RefField,
    over_list: RefField,
    push_list: RefField,
    arr_len: RefField,
    arr_array: RefField,
    parent: RefField,
    visible: RefField,
    abs_x: RefField,
    abs_y: RefField,
    width: RefField,
    height: RefField,
    propagate: RefField,
    cancel: RefField,
}

struct Fns {
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    sys_time: RefFun,
    get_scene: RefFun,
    mouse_x: RefFun,
    mouse_y: RefFun,
    get_interactive: RefFun,
}

struct Plan {
    update_fi: usize,
    dbg_file: usize,
    t: Ty,
    f: Fl,
    fns: Fns,
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let fun = code
        .functions
        .iter()
        .find(|g| g.findex == f)
        .with_context(|| format!("fn@{} not found", f.0))?;
    let t = fun.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

fn is_sub(code: &Bytecode, t: RefType, of: RefType) -> bool {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if c == of {
            return true;
        }
        cur = code.types[c.0].get_type_obj().and_then(|o| o.super_);
    }
    false
}

fn native(code: &Bytecode, name: &str, ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .natives
        .iter()
        .filter(|n| {
            s(code, n.name) == name
                && n.t
                    .as_fun(code)
                    .is_some_and(|t| t.args.is_empty() && t.ret == ret)
        })
        .map(|n| n.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one native {name}, found {}", hits.len()),
    }
}

fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|v| v.as_str() == TAG) {
        bail!("already applied");
    }
    let str_ = obj_type(code, "String")?;
    let dyn_ = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_ = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_ = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let f64_ = prim_type(code, "F64", |t| matches!(t, Type::F64))?;
    let cs = obj_type(code, "ui.win.CustomizeScreen")?;
    let obj = obj_type(code, "h2d.Object")?;
    let inter = obj_type(code, "h2d.Interactive")?;
    let scene = obj_type(code, "h2d.Scene")?;
    let events = obj_type(code, "hxd.SceneEvents")?;
    let arr = obj_type(code, "hl.types.ArrayObj")?;

    let println = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_] {
        bail!("Sys.println does not take one Dyn");
    }
    let std_string = static_fn(code, "$Std", "string")?.findex;
    if sig(code, std_string)? != (vec![dyn_], str_) {
        bail!("Std.string is not Dyn -> String");
    }
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_, str_], str_) {
        bail!("String.__add__ is not (String, String) -> String");
    }
    let sys_time = native(code, "sys_time", f64_)?;

    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let (window_root, root) = field(code, cs, "windowRoot")?;
    if !is_sub(code, root, obj) {
        bail!("CustomizeScreen.windowRoot is not an h2d.Object");
    }
    let (arr_array, native_arr) = field(code, arr, "array")?;
    if !matches!(code.types[native_arr.0], Type::Array) {
        bail!("ArrayObj.array is not a native array");
    }
    let f = Fl {
        window_root,
        events: typed(scene, "events", events)?,
        scene_w: typed(scene, "width", i32_)?,
        scene_h: typed(scene, "height", i32_)?,
        ev_mx: typed(events, "mouseX", f64_)?,
        ev_my: typed(events, "mouseY", f64_)?,
        over_list: typed(events, "overList", arr)?,
        push_list: typed(events, "pushList", arr)?,
        arr_len: typed(arr, "length", i32_)?,
        arr_array,
        parent: typed(obj, "parent", obj)?,
        visible: typed(obj, "visible", bool_)?,
        abs_x: typed(inter, "absX", f64_)?,
        abs_y: typed(inter, "absY", f64_)?,
        width: typed(inter, "width", f64_)?,
        height: typed(inter, "height", f64_)?,
        propagate: typed(inter, "propagateEvents", bool_)?,
        cancel: typed(inter, "cancelEvents", bool_)?,
    };

    let get_scene = proto(code, obj, "getScene")?;
    if sig(code, get_scene)? != (vec![obj], scene) {
        bail!("h2d.Object.getScene has an unexpected signature");
    }
    let mouse_x = proto(code, scene, "get_mouseX")?;
    let mouse_y = proto(code, scene, "get_mouseY")?;
    for m in [mouse_x, mouse_y] {
        if sig(code, m)? != (vec![scene], f64_) {
            bail!("h2d.Scene.get_mouseX/Y has an unexpected signature");
        }
    }
    let get_interactive = proto(code, scene, "getInteractive")?;
    if sig(code, get_interactive)? != (vec![scene, f64_, f64_], inter) {
        bail!("h2d.Scene.getInteractive has an unexpected signature");
    }

    // CustomizeScreen.update starts with `super.update(dt)`; the probe goes right after.
    let update = proto(code, cs, "update")?;
    let update_fi = code
        .functions
        .iter()
        .position(|g| g.findex == update)
        .context("CustomizeScreen.update not found")?;
    let u = &code.functions[update_fi];
    match u.ops.first() {
        Some(Opcode::Call2 {
            fun,
            arg0: Reg(0),
            arg1: Reg(1),
            ..
        }) if code
            .functions
            .iter()
            .find(|g| g.findex == *fun)
            .is_some_and(|g| s(code, g.name) == "update") => {}
        _ => bail!("CustomizeScreen.update does not start with super.update(dt)"),
    }
    if u.regs.first() != Some(&cs) || u.ops.len() < 2 {
        bail!("CustomizeScreen.update has an unexpected shape");
    }
    if (0..u.ops.len()).any(|i| jump_targets(u, i).contains(&1)) {
        bail!("CustomizeScreen.update: op 1 is a jump target");
    }
    let dbg_file = debug_file(code, "src/ui/win/CustomizeScreen.hx")?;

    Ok(Plan {
        update_fi,
        dbg_file,
        t: Ty {
            cs,
            obj,
            inter,
            scene,
            events,
            arr,
            native_arr,
            root,
            str_,
            dyn_,
            void,
            i32_,
            bool_,
            f64_,
        },
        f,
        fns: Fns {
            println: println.findex,
            std_string,
            str_add,
            sys_time,
            get_scene,
            mouse_x,
            mouse_y,
            get_interactive,
        },
    })
}

fn add_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

/// String pieces of the line, by label.
struct Labels {
    tag: RefGlobal,
    m: RefGlobal,
    comma: RefGlobal,
    v: RefGlobal,
    of: RefGlobal,
    x: RefGlobal,
    d2: RefGlobal,
    none: RefGlobal,
    abs: RefGlobal,
    wh: RefGlobal,
    prop: RefGlobal,
    cancel: RefGlobal,
    lt: RefGlobal,
    hidden: RefGlobal,
    over: RefGlobal,
    semi: RefGlobal,
    push: RefGlobal,
    sp: RefGlobal,
}

/// The probe `(CustomizeScreen) -> void`.
fn probe_fn(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let t = &p.t;
    let f = &p.f;
    let fns = &p.fns;
    let mut sg = |v: &'static str| str_global(code, t.str_, v);
    let l = Labels {
        tag: sg(TAG),
        m: sg(" m="),
        comma: sg(","),
        v: sg(" v="),
        of: sg(" of "),
        x: sg("x"),
        d2: sg(" 2d="),
        none: sg("none"),
        abs: sg(" abs="),
        wh: sg(" wh="),
        prop: sg(" prop="),
        cancel: sg(" cancel="),
        lt: sg(" < "),
        hidden: sg("[hidden]"),
        over: sg(" | over="),
        semi: sg(" ; "),
        push: sg(" push="),
        sp: sg(" "),
    };
    // The last printed state: hit, over count and entries, push count and first entry.
    let g_hit = add_global(code, t.inter);
    let g_over: Vec<RefGlobal> = (0..OVER).map(|_| add_global(code, t.dyn_)).collect();
    let g_over_n = add_global(code, t.i32_);
    let g_push = add_global(code, t.dyn_);
    let g_push_n = add_global(code, t.i32_);
    let g_last = add_global(code, t.f64_);
    let gap_c = float_const(code, GAP);
    let minus_one = int_const(code, -1);
    let zero = int_const(code, 0);
    let chain_c = int_const(code, CHAIN);
    let idx_c: Vec<_> = (0..OVER).map(|j| int_const(code, j)).collect();

    let mut r = Regs(vec![t.cs]);
    let exc = r.r(t.dyn_);
    let v = r.r(t.void);
    let (now, last, fv) = (r.r(t.f64_), r.r(t.f64_), r.r(t.f64_));
    let root = r.r(t.root);
    let scene = r.r(t.scene);
    let ev = r.r(t.events);
    let (mx, my) = (r.r(t.f64_), r.r(t.f64_));
    let hit = r.r(t.inter);
    let (over_l, push_l) = (r.r(t.arr), r.r(t.arr));
    let (n, pn) = (r.r(t.i32_), r.r(t.i32_));
    let over: Vec<Reg> = (0..OVER).map(|_| r.r(t.dyn_)).collect();
    let push0 = r.r(t.dyn_);
    let na = r.r(t.native_arr);
    let g_h = r.r(t.inter);
    let g_d = r.r(t.dyn_);
    let g_i = r.r(t.i32_);
    let acc = r.r(t.str_);
    let txt = r.r(t.str_);
    let d = r.r(t.dyn_);
    let iv = r.r(t.i32_);
    let b = r.r(t.bool_);
    let (k, lim) = (r.r(t.i32_), r.r(t.i32_));
    let (o, o2) = (r.r(t.obj), r.r(t.obj));

    let mut a = Asm::new();
    // acc += <label>
    let lab = |a: &mut Asm, g: RefGlobal| {
        a.op(Opcode::GetGlobal {
            dst: txt,
            global: g,
        });
        a.op(Opcode::Call2 {
            dst: acc,
            fun: fns.str_add,
            arg0: acc,
            arg1: txt,
        });
    };
    // acc += Std.string(<dyn-compatible reg>)
    let val = |a: &mut Asm, src: Reg| {
        a.op(Opcode::Call1 {
            dst: txt,
            fun: fns.std_string,
            arg0: src,
        });
        a.op(Opcode::Call2 {
            dst: acc,
            fun: fns.str_add,
            arg0: acc,
            arg1: txt,
        });
    };
    // acc += Std.string(<i32 or bool reg>), boxed
    let boxed = |a: &mut Asm, src: Reg| {
        a.op(Opcode::ToDyn { dst: d, src });
        val(a, d);
    };
    // acc += string of the integer part of <f64 reg>
    let float = |a: &mut Asm, src: Reg| {
        a.op(Opcode::ToInt { dst: iv, src });
        boxed(a, iv);
    };
    // acc += string of <obj>.<f64 field> (integer part)
    let ffield = |a: &mut Asm, obj: Reg, fld: RefField| {
        a.op(Opcode::Field {
            dst: fv,
            obj,
            field: fld,
        });
        float(a, fv);
    };

    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    // Throttle.
    a.op(Opcode::Call0 {
        dst: now,
        fun: fns.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g_last,
    });
    a.op(Opcode::Sub {
        dst: fv,
        a: now,
        b: last,
    });
    a.op(Opcode::Float {
        dst: last,
        ptr: gap_c,
    });
    a.jmp(
        Opcode::JSLt {
            a: fv,
            b: last,
            offset: 0,
        },
        "out",
    );
    // Scene, events, hit-test.
    a.op(Opcode::GetThis {
        dst: root,
        field: f.window_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: scene,
        fun: fns.get_scene,
        arg0: root,
    });
    a.jmp(
        Opcode::JNull {
            reg: scene,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: ev,
        obj: scene,
        field: f.events,
    });
    a.jmp(Opcode::JNull { reg: ev, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: mx,
        fun: fns.mouse_x,
        arg0: scene,
    });
    a.op(Opcode::Call1 {
        dst: my,
        fun: fns.mouse_y,
        arg0: scene,
    });
    a.op(Opcode::Call3 {
        dst: hit,
        fun: fns.get_interactive,
        arg0: scene,
        arg1: mx,
        arg2: my,
    });
    // Over list: count and its first OVER entries; push list: count and first entry.
    // The native array is null-checked too: GetArray reads memory unchecked.
    a.op(Opcode::Int {
        dst: n,
        ptr: minus_one,
    });
    a.op(Opcode::Int {
        dst: pn,
        ptr: minus_one,
    });
    for &o in over.iter().chain([&push0]) {
        a.op(Opcode::Null { dst: o });
    }
    a.op(Opcode::Field {
        dst: over_l,
        obj: ev,
        field: f.over_list,
    });
    a.jmp(
        Opcode::JNull {
            reg: over_l,
            offset: 0,
        },
        "no_over",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: over_l,
        field: f.arr_len,
    });
    a.op(Opcode::Field {
        dst: na,
        obj: over_l,
        field: f.arr_array,
    });
    a.jmp(Opcode::JNull { reg: na, offset: 0 }, "no_over");
    for (&o, &j) in over.iter().zip(&idx_c) {
        a.op(Opcode::Int { dst: k, ptr: j });
        a.jmp(
            Opcode::JSGte {
                a: k,
                b: n,
                offset: 0,
            },
            "no_over",
        );
        a.op(Opcode::GetArray {
            dst: o,
            array: na,
            index: k,
        });
    }
    a.label("no_over");
    a.op(Opcode::Field {
        dst: push_l,
        obj: ev,
        field: f.push_list,
    });
    a.jmp(
        Opcode::JNull {
            reg: push_l,
            offset: 0,
        },
        "no_push",
    );
    a.op(Opcode::Field {
        dst: pn,
        obj: push_l,
        field: f.arr_len,
    });
    a.op(Opcode::Field {
        dst: na,
        obj: push_l,
        field: f.arr_array,
    });
    a.jmp(Opcode::JNull { reg: na, offset: 0 }, "no_push");
    a.op(Opcode::Int { dst: k, ptr: zero });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: pn,
            offset: 0,
        },
        "no_push",
    );
    a.op(Opcode::GetArray {
        dst: push0,
        array: na,
        index: k,
    });
    a.label("no_push");
    // Unchanged since the last printed line: nothing to print.
    let mut state: Vec<(RefGlobal, Reg, Reg)> = vec![(g_hit, hit, g_h)];
    state.extend(g_over.iter().zip(&over).map(|(&g, &o)| (g, o, g_d)));
    state.extend([
        (g_over_n, n, g_i),
        (g_push, push0, g_d),
        (g_push_n, pn, g_i),
    ]);
    for &(global, cur, tmp) in &state {
        a.op(Opcode::GetGlobal { dst: tmp, global });
        a.jmp(
            Opcode::JNotEq {
                a: cur,
                b: tmp,
                offset: 0,
            },
            "changed",
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("changed");
    // Throttle from here; the state is stored only once the line is printed.
    a.op(Opcode::SetGlobal {
        global: g_last,
        src: now,
    });
    // The line.
    a.op(Opcode::GetGlobal {
        dst: acc,
        global: l.tag,
    });
    lab(&mut a, l.m);
    ffield(&mut a, ev, f.ev_mx);
    lab(&mut a, l.comma);
    ffield(&mut a, ev, f.ev_my);
    lab(&mut a, l.v);
    float(&mut a, mx);
    lab(&mut a, l.comma);
    float(&mut a, my);
    lab(&mut a, l.of);
    a.op(Opcode::Field {
        dst: iv,
        obj: scene,
        field: f.scene_w,
    });
    boxed(&mut a, iv);
    lab(&mut a, l.x);
    a.op(Opcode::Field {
        dst: iv,
        obj: scene,
        field: f.scene_h,
    });
    boxed(&mut a, iv);
    lab(&mut a, l.d2);
    a.jmp(
        Opcode::JNotNull {
            reg: hit,
            offset: 0,
        },
        "hit",
    );
    lab(&mut a, l.none);
    a.jmp(Opcode::JAlways { offset: 0 }, "hit_done");
    a.label("hit");
    val(&mut a, hit);
    lab(&mut a, l.abs);
    ffield(&mut a, hit, f.abs_x);
    lab(&mut a, l.comma);
    ffield(&mut a, hit, f.abs_y);
    lab(&mut a, l.wh);
    ffield(&mut a, hit, f.width);
    lab(&mut a, l.x);
    ffield(&mut a, hit, f.height);
    lab(&mut a, l.prop);
    a.op(Opcode::Field {
        dst: b,
        obj: hit,
        field: f.propagate,
    });
    boxed(&mut a, b);
    lab(&mut a, l.cancel);
    a.op(Opcode::Field {
        dst: b,
        obj: hit,
        field: f.cancel,
    });
    boxed(&mut a, b);
    // Parent chain: " < " + Std.string(p) + ("[hidden]" when !p.visible), CHAIN levels.
    a.op(Opcode::Mov { dst: o, src: hit });
    a.op(Opcode::Int { dst: k, ptr: zero });
    a.op(Opcode::Int {
        dst: lim,
        ptr: chain_c,
    });
    a.loop_head("chain");
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: lim,
            offset: 0,
        },
        "hit_done",
    );
    a.op(Opcode::Field {
        dst: o2,
        obj: o,
        field: f.parent,
    });
    a.jmp(Opcode::JNull { reg: o2, offset: 0 }, "hit_done");
    a.op(Opcode::Mov { dst: o, src: o2 });
    lab(&mut a, l.lt);
    val(&mut a, o);
    a.op(Opcode::Field {
        dst: b,
        obj: o,
        field: f.visible,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "shown");
    lab(&mut a, l.hidden);
    a.label("shown");
    a.op(Opcode::Incr { dst: k });
    a.jmp(Opcode::JAlways { offset: 0 }, "chain");
    a.label("hit_done");
    // Over list: count, then its first OVER entries (contiguous; the first null ends them).
    lab(&mut a, l.over);
    boxed(&mut a, n);
    for &o in &over {
        a.jmp(Opcode::JNull { reg: o, offset: 0 }, "over_done");
        lab(&mut a, l.semi);
        val(&mut a, o);
    }
    a.label("over_done");
    // Push list: count and first entry.
    lab(&mut a, l.push);
    boxed(&mut a, pn);
    a.jmp(
        Opcode::JNull {
            reg: push0,
            offset: 0,
        },
        "print",
    );
    lab(&mut a, l.sp);
    val(&mut a, push0);
    a.label("print");
    a.op(Opcode::Call1 {
        dst: v,
        fun: fns.println,
        arg0: acc,
    });
    for &(global, src, _) in &state {
        a.op(Opcode::SetGlobal { global, src });
    }
    a.label("out");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });

    push_fn(code, vec![t.cs], t.void, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let probe = probe_fn(code, &p)?;
    let f = &mut code.functions[p.update_fi];
    f.regs.push(p.t.void);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        1,
        vec![Opcode::Call1 {
            dst: v,
            fun: probe,
            arg0: Reg(0),
        }],
    );
    eprintln!(
        "patched slot4 diagnostic: CustomizeScreen.update fn@{} calls probe fn@{}",
        f.findex.0, probe.0
    );
    Ok(())
}

/// Prints the customize screen's 2D hit-test at the cursor, or leaves `code` untouched and logs why.
pub(crate) fn patch_slot4_diag(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("slot4 diagnostic skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    let update_before = code.functions[p.update_fi].clone();
    let fi = p.update_fi;
    if let Err(e) = apply(code, p) {
        eprintln!("slot4 diagnostic skipped: {e:#}");
        code.functions[fi] = update_before;
        snap.restore(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Patches a copy of the installed game (skipped when absent): update only
    /// gains the probe call after super.update, the probe is well typed and
    /// trapped, the image round-trips, and a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (update_fi, println, cs) = (p.update_fi, p.fns.println, p.t.cs);
        let mut code = read(&image);
        patch_slot4_diag(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        let probe = &back.functions[nf];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == update_fi {
                shifted(a, b, 1, 1);
                assert!(matches!(
                    b.ops[1],
                    Opcode::Call1 { dst, fun, arg0: Reg(0) }
                        if fun == probe.findex && dst.0 as usize >= a.regs.len()
                ));
                check_types(&back, b, 0..b.ops.len());
                check_flow(b);
            } else {
                assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops), "fn#{i}");
                assert_eq!(a.regs, b.regs, "fn#{i}");
            }
        }
        check_types(&back, probe, 0..probe.ops.len());
        check_flow(probe);
        assert_eq!(probe.regs[0], cs);
        // Trapped from the first op; every Ret but the catch's one follows an EndTrap.
        let Opcode::Trap { offset, .. } = probe.ops[0] else {
            panic!("probe does not start with a Trap");
        };
        let catch = (1 + offset) as usize;
        assert_eq!(catch, probe.ops.len() - 1);
        for (i, op) in probe.ops.iter().enumerate() {
            if matches!(op, Opcode::Ret { .. }) && i != catch {
                assert!(matches!(probe.ops[i - 1], Opcode::EndTrap { .. }), "op {i}");
            }
        }
        assert_eq!(
            probe
                .ops
                .iter()
                .filter(|o| matches!(o, Opcode::EndTrap { .. }))
                .count(),
            1
        );
        // Prints once, through Sys.println.
        assert_eq!(
            probe
                .ops
                .iter()
                .filter(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == println))
                .count(),
            1
        );
        // Every GetArray reads a native array that was null-checked before (the JIT
        // reads it unchecked), at an index compared against the list length.
        for (i, op) in probe.ops.iter().enumerate() {
            if let Opcode::GetArray { array, index, .. } = op {
                assert!(probe.ops[..i]
                    .iter()
                    .any(|o| matches!(o, Opcode::JNull { reg, .. } if reg == array)));
                assert!(matches!(probe.ops[i - 1], Opcode::JSGte { a, .. } if a == *index));
            }
        }
        // Globals keep their types; the state is stored after the println, only
        // the throttle time before it.
        let print_at = probe
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == println))
            .unwrap();
        let stores: Vec<usize> = (0..probe.ops.len())
            .filter(|&i| matches!(probe.ops[i], Opcode::SetGlobal { .. }))
            .collect();
        assert_eq!(stores.len(), 1 + 1 + OVER as usize + 1 + 1 + 1);
        for &i in &stores {
            let Opcode::SetGlobal { global, src } = probe.ops[i] else {
                unreachable!()
            };
            assert_eq!(back.globals[global.0], probe.regs[src.0 as usize]);
            if i < print_at {
                assert!(matches!(back.types[back.globals[global.0].0], Type::F64));
            }
        }
        assert_eq!(stores.iter().filter(|&&i| i < print_at).count(), 1);
        // No String is boxed with ToDyn on its way to println / Std.string.
        for op in &probe.ops {
            if let Opcode::ToDyn { src, .. } = op {
                assert_ne!(probe.regs[src.0 as usize], p.t.str_);
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_slot4_diag(&mut again);
        assert!(write(&again) == patched);
    }
}
