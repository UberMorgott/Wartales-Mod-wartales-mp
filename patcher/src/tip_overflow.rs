// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Tooltip helper panels never run off screen, and are not shown twice.
//
// A tooltip's side column of keyword helpers ("Poison", "Vigilance", ...) is the
// child flow with class `elt-clamped` (ItemTip.itemHelper, a skill / status
// tip's #helperList), a vertical h2d.Flow of ui.comp.TipHelper panels. Each
// frame `ui.comp.TipContent.sync` (TipContent.hx:162-216) finds it once
// (`clampedObj`), makes it absolute, puts it right of the tip when it fits, else
// left (`x = -width`), and when its bottom passes the screen:
//
//   if (gamepad) { overflow = Scroll; maxHeight = screenH - y - 1; }
//   else clampedObj.y -= this.y + clampedObj.y + height + 1 - screenH;
//
// The mouse branch only moves the column up, so a column taller than the screen
// (a weapon with several skills, each listing its statuses) spills over both
// edges. And every skill adds its own helpers (ItemTip.addSkillHelpers), so a
// status two skills share is listed twice.
//
// Patch, in TipContent.sync:
//
//   * right after the search, on every path (the op after the `clampedObjSearched`
//     jump target): `tipFix(this, scene)`, an appended function that
//       - hides (visible = false) a TipHelper whose title (first h2d.Text child)
//         equals an earlier visible TipHelper's in the same column;
//       - sets the column `multiline = true`, `maxHeight = screenH - 20` (unless the
//         gamepad branch already scrolls it with its own maxHeight) and
//         `horizontalSpacing >= 5`: h2d.Flow's vertical multiline layout starts
//         a new column when the next child would pass maxHeight, so the column
//         is no taller than the screen (bar a single helper taller than that) and
//         the existing shift-up keeps it inside. The width the side test (right
//         if it fits, else left) measures right after includes the extra
//         columns. Setters run only when the value differs (no reflow churn).
//     All of it runs inside a try/catch that swallows anything, so a tooltip can
//     never throw from here (sync runs every frame).
//   * in the "left of the tip" branch, after `clampedObj.x = -width`:
//     `if (this.x - width < 0) this.x = width;`, so a wide column placed left
//     still starts on screen (the vanilla right clamp further down still wins on
//     a screen too narrow for both).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use hlbc::types::{RefGlobal, ValBool};

/// Screen height the helper column leaves free (half above, half below).
const MARGIN: i32 = 20;
/// Below this cap (a tiny or not yet sized scene) the column is left alone.
const MIN_CAP: i32 = 100;
/// Gap between helper columns.
const COL_GAP: i32 = 5;

struct T {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    dyn_: RefType,
    str_: RefType,
}

struct Plan {
    fi: usize,
    /// Insert the tipFix call here (the op after the search join).
    hook_at: usize,
    scene: Reg,
    /// Insert the left clamp here (after `clampedObj.x = -width`).
    left_at: usize,
    /// The `-width` register of that store.
    neg_w: Reg,
    t: T,
    tc_t: RefType,
    scene_t: RefType,
    obj_t: RefType,
    flow_t: RefType,
    text_t: RefType,
    arr_t: RefType,
    nint_t: RefType,
    dbg_file: usize,
    tc_x: RefField,
    tc_pos_changed: RefField,
    clamped: RefField,
    o_children: RefField,
    o_visible: RefField,
    a_len: RefField,
    a_arr: (RefField, RefType),
    scene_h: RefField,
    f_multiline: RefField,
    f_max_h: RefField,
    f_hspace: RefField,
    f_overflow: RefField,
    overflow_t: RefType,
    /// The FlowOverflow value sync's gamepad branch sets (Scroll).
    scroll_g: RefGlobal,
    t_text: RefField,
    helper_g: (RefGlobal, RefType),
    text_g: (RefGlobal, RefType),
    check: RefFun,
    compare: RefFun,
    /// Prototype slot of h2d.Object.set_visible.
    set_visible: RefField,
    set_multiline: RefFun,
    set_max_h: RefFun,
    set_hspace: RefFun,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let g = &code.functions[fun_index(code, f)?];
    let t = g.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

/// The class global (`$Name` object) of class `name`, HL stores it 1-based.
fn class_global(code: &Bytecode, name: &str) -> Result<(RefGlobal, RefType)> {
    let o = obj(code, obj_type(code, name)?)?;
    let g = RefGlobal(
        o.global
            .0
            .checked_sub(1)
            .with_context(|| format!("{name}: no class global"))?,
    );
    let t = *code
        .globals
        .get(g.0)
        .with_context(|| format!("{name}: class global out of range"))?;
    let (pkg, cls) = name.rsplit_once('.').unwrap_or(("", name));
    let want = if pkg.is_empty() {
        format!("${cls}")
    } else {
        format!("{pkg}.${cls}")
    };
    if obj(code, t).ok().map(|o| s(code, o.name)) != Some(want.as_str()) {
        bail!("{name}: class global is not {want}");
    }
    Ok((g, t))
}

/// `field` of `t`, which must have type `want`.
fn typed_field(code: &Bytecode, t: RefType, name: &str, want: RefType) -> Result<RefField> {
    let (f, ft) = field(code, t, name)?;
    if ft != want {
        bail!("field {name}: unexpected type");
    }
    Ok(f)
}

/// Whether `op` writes register `r` (every opcode names its destination `dst`).
fn writes(op: &Opcode, r: Reg) -> bool {
    let d = format!("{op:?}");
    d.contains(&format!("dst: Reg({}),", r.0)) || d.contains(&format!("dst: Reg({}) }}", r.0))
}

fn is_target(f: &Function, at: usize) -> bool {
    (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&at))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let tc_t = obj_type(code, "ui.comp.TipContent")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let scene_t = obj_type(code, "h2d.Scene")?;
    let text_t = obj_type(code, "h2d.Text")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let str_t = obj_type(code, "String")?;
    let t = T {
        void: prim_type(code, "void", |t| matches!(t, Type::Void))?,
        bool_: prim_type(code, "bool", |t| matches!(t, Type::Bool))?,
        i32_: prim_type(code, "i32", |t| matches!(t, Type::I32))?,
        f64_: prim_type(code, "f64", |t| matches!(t, Type::F64))?,
        dyn_: prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?,
        str_: str_t,
    };

    let sync = method(code, tc_t, "sync")?;
    let fi = fun_index(code, sync.findex)?;
    let clamped = typed_field(code, tc_t, "clampedObj", flow_t)?;
    let searched = typed_field(code, tc_t, "clampedObjSearched", t.bool_)?;
    let tc_x = typed_field(code, tc_t, "x", t.f64_)?;
    let tc_pos_changed = typed_field(code, tc_t, "posChanged", t.bool_)?;
    let flow_x = typed_field(code, flow_t, "x", t.f64_)?;

    // The search runs once: `if (!clampedObjSearched) {...}` joins at T, and T + 1
    // is the `if (clampedObj != null)` placement test every path reaches.
    let joins: Vec<usize> = (0..sync.ops.len().saturating_sub(1))
        .filter_map(|k| match (&sync.ops[k], &sync.ops[k + 1]) {
            (Opcode::GetThis { dst, field }, Opcode::JTrue { cond, offset })
                if *field == searched && cond == dst && *offset >= 0 =>
            {
                Some(k + 2 + *offset as usize)
            }
            _ => None,
        })
        .collect();
    let [join] = joins[..] else {
        bail!(
            "TipContent.sync: expected one clampedObjSearched test, found {}",
            joins.len()
        );
    };
    let hook_at = join + 1;
    match sync.ops.get(hook_at..hook_at + 2) {
        Some([Opcode::GetThis { dst, field }, Opcode::JNull { reg, .. }])
            if *field == clamped && reg == dst => {}
        Some([Opcode::Call2 { .. }, ..]) => bail!("TipContent.sync: already applied"),
        _ => bail!("TipContent.sync: no clampedObj test after the search"),
    }
    if is_target(sync, hook_at) {
        bail!("TipContent.sync: the hook point is a jump target");
    }

    // `var scene = getScene();` once, and never overwritten.
    let get_scene = method(code, obj_t, "getScene")?.findex;
    let scenes: Vec<(usize, Reg)> = (0..hook_at)
        .filter_map(|k| match sync.ops[k] {
            Opcode::Call1 { dst, fun, arg0 } if fun == get_scene && arg0 == Reg(0) => {
                Some((k, dst))
            }
            _ => None,
        })
        .collect();
    let [(scene_at, scene)] = scenes[..] else {
        bail!(
            "TipContent.sync: expected one getScene(this), found {}",
            scenes.len()
        );
    };
    if sync.regs[scene.0 as usize] != scene_t {
        bail!("TipContent.sync: getScene result is not an h2d.Scene");
    }
    // ... and the hook sees that value: no other write in between, and no jump
    // from before it lands past it.
    if (scene_at + 1..hook_at).any(|k| writes(&sync.ops[k], scene)) {
        bail!("TipContent.sync: the scene register is reused");
    }
    if (0..scene_at).any(|k| {
        jump_targets(sync, k)
            .iter()
            .any(|&t| t > scene_at && t <= hook_at)
    }) {
        bail!("TipContent.sync: a jump skips getScene");
    }

    // The left placement: `clampedObj.x = -width` (Neg + SetField x on the flow).
    let lefts: Vec<(usize, Reg)> = (0..sync.ops.len().saturating_sub(1))
        .filter_map(|k| match (&sync.ops[k], &sync.ops[k + 1]) {
            (Opcode::Neg { dst, .. }, Opcode::SetField { obj: o, field, src })
                if src == dst && *field == flow_x && sync.regs[o.0 as usize] == flow_t =>
            {
                Some((k + 2, *dst))
            }
            _ => None,
        })
        .collect();
    let [(left_at, neg_w)] = lefts[..] else {
        bail!(
            "TipContent.sync: expected one clampedObj.x = -width, found {}",
            lefts.len()
        );
    };
    if left_at <= hook_at || left_at >= sync.ops.len() || is_target(sync, left_at) {
        bail!("TipContent.sync: unexpected left placement");
    }
    if sync.regs[neg_w.0 as usize] != t.f64_ {
        bail!("TipContent.sync: the left offset is not a float");
    }

    let o_children = typed_field(code, obj_t, "children", arr_t)?;
    let o_visible = typed_field(code, obj_t, "visible", t.bool_)?;
    let a_len = typed_field(code, arr_t, "length", t.i32_)?;
    let a_arr = field(code, arr_t, "array")?;
    if !matches!(code.types[a_arr.1 .0], Type::Array) {
        bail!("ArrayObj.array is not a native array");
    }
    let scene_h = typed_field(code, scene_t, "height", t.i32_)?;
    let f_multiline = typed_field(code, flow_t, "multiline", t.bool_)?;
    let (f_max_h, nint_t) = field(code, flow_t, "maxHeight")?;
    if !matches!(code.types[nint_t.0], Type::Null(i) if i == t.i32_) {
        bail!("Flow.maxHeight is not Null<Int>");
    }
    let f_hspace = typed_field(code, flow_t, "horizontalSpacing", t.i32_)?;
    let (f_overflow, overflow_t) = field(code, flow_t, "overflow")?;
    // sync's gamepad branch: `clampedObj.overflow = Scroll` (GetGlobal + set_overflow).
    let set_overflow = proto(code, flow_t, "set_overflow")?;
    let scrolls: Vec<RefGlobal> = (1..sync.ops.len())
        .filter_map(|k| match (&sync.ops[k - 1], &sync.ops[k]) {
            (
                Opcode::GetGlobal { dst, global },
                Opcode::Call2 {
                    fun, arg0, arg1, ..
                },
            ) if *fun == set_overflow
                && arg1 == dst
                && sync.regs[arg0.0 as usize] == flow_t
                && code.globals.get(global.0) == Some(&overflow_t) =>
            {
                Some(*global)
            }
            _ => None,
        })
        .collect();
    let [scroll_g] = scrolls[..] else {
        bail!(
            "TipContent.sync: expected one clampedObj.overflow store, found {}",
            scrolls.len()
        );
    };
    let t_text = typed_field(code, text_t, "text", str_t)?;

    let m = |o: RefType, name: &str, want: (Vec<RefType>, RefType)| -> Result<RefFun> {
        let f = method(code, o, name)?.findex;
        if sig(code, f)? != want {
            bail!("unexpected {name} signature");
        }
        Ok(f)
    };
    // set_visible is virtual (ui.comp.LayerElement, a TipHelper base, overrides
    // it): called through its prototype slot, as the game does.
    let vis = obj(code, obj_t)?
        .protos
        .iter()
        .find(|p| s(code, p.name) == "set_visible")
        .context("proto set_visible not found on h2d.Object")?;
    if sig(code, vis.findex)? != (vec![obj_t, t.bool_], t.bool_) {
        bail!("unexpected set_visible signature");
    }
    let set_visible = RefField(usize::try_from(vis.pindex).context("set_visible: no slot")?);
    let helper_t = obj_type(code, "ui.comp.TipHelper")?;
    let mut cur = Some(helper_t);
    while let Some(c) = cur.filter(|&c| c != flow_t) {
        let o = obj(code, c)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == "set_visible") {
            if p.pindex != vis.pindex || sig(code, p.findex)?.1 != t.bool_ {
                bail!("{}.set_visible: unexpected override", s(code, o.name));
            }
        }
        cur = o.super_;
    }
    if cur.is_none() {
        bail!("ui.comp.TipHelper is not an h2d.Flow");
    }
    let set_multiline = m(flow_t, "set_multiline", (vec![flow_t, t.bool_], t.bool_))?;
    let set_max_h = m(flow_t, "set_maxHeight", (vec![flow_t, nint_t], nint_t))?;
    let set_hspace = m(
        flow_t,
        "set_horizontalSpacing",
        (vec![flow_t, t.i32_], t.i32_),
    )?;
    let check = m(
        obj_type(code, "hl.BaseType")?,
        "check",
        (vec![obj_type(code, "hl.BaseType")?, t.dyn_], t.bool_),
    )?;
    let compare = method(code, str_t, "__compare")?.findex;
    match sig(code, compare)? {
        (a, r)
            if r == t.i32_
                && a.len() == 2
                && a[0] == str_t
                && (a[1] == str_t || a[1] == t.dyn_) => {}
        _ => bail!("unexpected String.__compare signature"),
    }

    Ok(Plan {
        fi,
        hook_at,
        scene,
        left_at,
        neg_w,
        tc_t,
        scene_t,
        obj_t,
        flow_t,
        text_t,
        arr_t,
        nint_t,
        dbg_file: debug_file(code, "src/ui/comp/TipContent.hx")?,
        tc_x,
        tc_pos_changed,
        clamped,
        o_children,
        o_visible,
        a_len,
        a_arr,
        scene_h,
        f_multiline,
        f_max_h,
        f_hspace,
        f_overflow,
        overflow_t,
        scroll_g,
        t_text,
        helper_g: class_global(code, "ui.comp.TipHelper")?,
        text_g: class_global(code, "h2d.Text")?,
        check,
        compare,
        set_visible,
        set_multiline,
        set_max_h,
        set_hspace,
        t,
    })
}

/// `tipKey(o: h2d.Object) -> String`: the text of the first h2d.Text child of a
/// visible ui.comp.TipHelper (its title), else null.
fn add_key(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let t = &p.t;
    let mut r = Regs(vec![p.obj_t]);
    let (b, ch, n, arr, i, d, tx, st, hg, tg) = (
        r.r(t.bool_),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(p.a_arr.1),
        r.r(t.i32_),
        r.r(t.dyn_),
        r.r(p.text_t),
        r.r(t.str_),
        r.r(p.helper_g.1),
        r.r(p.text_g.1),
    );
    let o = Reg(0);
    let mut a = Asm::new();
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "none");
    a.op(Opcode::GetGlobal {
        dst: hg,
        global: p.helper_g.0,
    });
    // An h2d.Object goes to BaseType.check's Dyn argument as is.
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: hg,
        arg1: o,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "none");
    a.op(Opcode::Field {
        dst: b,
        obj: o,
        field: p.o_visible,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "none");
    a.op(Opcode::Field {
        dst: ch,
        obj: o,
        field: p.o_children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "none");
    a.op(Opcode::Field {
        dst: n,
        obj: ch,
        field: p.a_len,
    });
    a.op(Opcode::Field {
        dst: arr,
        obj: ch,
        field: p.a_arr.0,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("loop");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "none",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "loop");
    a.op(Opcode::GetGlobal {
        dst: tg,
        global: p.text_g.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: tg,
        arg1: d,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::UnsafeCast { dst: tx, src: d });
    a.op(Opcode::Field {
        dst: st,
        obj: tx,
        field: p.t_text,
    });
    a.op(Opcode::Ret { ret: st });
    a.label("none");
    a.op(Opcode::Null { dst: st });
    a.op(Opcode::Ret { ret: st });
    push_fn(code, vec![p.obj_t], t.str_, r.0, a.finish(), p.dbg_file)
}

/// `tipFix(tc: TipContent, scene: h2d.Scene) -> Void`; see the module comment.
fn add_fix(code: &mut Bytecode, p: &Plan, key: RefFun) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let i_margin = int_const(code, MARGIN);
    let i_min = int_const(code, MIN_CAP);
    let i_gap = int_const(code, COL_GAP);
    let t = &p.t;
    let mut r = Regs(vec![p.tc_t, p.scene_t]);
    let (exc, f, ch, n, arr, i, j, lim, d, c, c2, k, k2, cmp, zero, b, v) = (
        r.r(t.dyn_),
        r.r(p.flow_t),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(p.a_arr.1),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.dyn_),
        r.r(p.obj_t),
        r.r(p.obj_t),
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.bool_),
        r.r(t.void),
    );
    let (h, m, cap, mh, cur, hs, ov, sc) = (
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.nint_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.overflow_t),
        r.r(p.overflow_t),
    );
    let (tc, scene) = (Reg(0), Reg(1));
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Field {
        dst: f,
        obj: tc,
        field: p.clamped,
    });
    a.jmp(Opcode::JNull { reg: f, offset: 0 }, "end");
    a.jmp(
        Opcode::JNull {
            reg: scene,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: zero, ptr: i0 });

    // Dedupe: hide a helper whose title an earlier visible helper already shows.
    a.op(Opcode::Field {
        dst: ch,
        obj: f,
        field: p.o_children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "wrap");
    a.op(Opcode::Field {
        dst: n,
        obj: ch,
        field: p.a_len,
    });
    a.op(Opcode::Field {
        dst: arr,
        obj: ch,
        field: p.a_arr.0,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("li");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "wrap",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: i,
    });
    a.op(Opcode::Mov { dst: lim, src: i });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: c, src: d });
    a.op(Opcode::Call1 {
        dst: k,
        fun: key,
        arg0: c,
    });
    a.jmp(Opcode::JNull { reg: k, offset: 0 }, "li");
    a.op(Opcode::Int { dst: j, ptr: i0 });
    a.loop_head("lj");
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: lim,
            offset: 0,
        },
        "li",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: arr,
        index: j,
    });
    a.op(Opcode::Incr { dst: j });
    a.op(Opcode::UnsafeCast { dst: c2, src: d });
    a.op(Opcode::Call1 {
        dst: k2,
        fun: key,
        arg0: c2,
    });
    a.jmp(Opcode::JNull { reg: k2, offset: 0 }, "lj");
    a.op(Opcode::Call2 {
        dst: cmp,
        fun: p.compare,
        arg0: k,
        arg1: k2,
    });
    a.jmp(
        Opcode::JNotEq {
            a: cmp,
            b: zero,
            offset: 0,
        },
        "lj",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::CallMethod {
        dst: b,
        field: p.set_visible,
        args: vec![c, b],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "li");

    // Wrap: never taller than the screen, extra columns side by side.
    a.label("wrap");
    a.op(Opcode::Field {
        dst: h,
        obj: scene,
        field: p.scene_h,
    });
    a.op(Opcode::Int {
        dst: m,
        ptr: i_margin,
    });
    a.op(Opcode::Sub {
        dst: cap,
        a: h,
        b: m,
    });
    a.op(Opcode::Int { dst: m, ptr: i_min });
    a.jmp(
        Opcode::JSLt {
            a: cap,
            b: m,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: f,
        field: p.f_multiline,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "mh");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_multiline,
        arg0: f,
        arg1: b,
    });
    // The gamepad branch scrolls with its own maxHeight: leave that one alone.
    a.label("mh");
    a.op(Opcode::Field {
        dst: ov,
        obj: f,
        field: p.f_overflow,
    });
    a.op(Opcode::GetGlobal {
        dst: sc,
        global: p.scroll_g,
    });
    a.jmp(
        Opcode::JEq {
            a: ov,
            b: sc,
            offset: 0,
        },
        "hs",
    );
    a.op(Opcode::Field {
        dst: mh,
        obj: f,
        field: p.f_max_h,
    });
    a.jmp(Opcode::JNull { reg: mh, offset: 0 }, "set");
    a.op(Opcode::SafeCast { dst: cur, src: mh });
    a.jmp(
        Opcode::JEq {
            a: cur,
            b: cap,
            offset: 0,
        },
        "hs",
    );
    a.label("set");
    a.op(Opcode::ToDyn { dst: mh, src: cap });
    a.op(Opcode::Call2 {
        dst: mh,
        fun: p.set_max_h,
        arg0: f,
        arg1: mh,
    });
    a.label("hs");
    a.op(Opcode::Field {
        dst: hs,
        obj: f,
        field: p.f_hspace,
    });
    a.op(Opcode::Int { dst: m, ptr: i_gap });
    a.jmp(
        Opcode::JSGte {
            a: hs,
            b: m,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call2 {
        dst: hs,
        fun: p.set_hspace,
        arg0: f,
        arg1: m,
    });
    a.label("end");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.tc_t, p.scene_t],
        t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// Ops inserted after `clampedObj.x = nw` (nw = -width), with `fx, fs, fz, fw`
/// new f64 registers and `b` a new bool register:
/// `if (this.x + nw < 0) { this.x = -nw; }`.
fn left_clamp(p: &Plan, zero: hlbc::types::RefFloat, regs: [Reg; 5]) -> Vec<Opcode> {
    let [fx, fs, fz, fw, b] = regs;
    vec![
        Opcode::GetThis {
            dst: fx,
            field: p.tc_x,
        },
        Opcode::Add {
            dst: fs,
            a: fx,
            b: p.neg_w,
        },
        Opcode::Float { dst: fz, ptr: zero },
        Opcode::JNotLt {
            a: fs,
            b: fz,
            offset: 4,
        },
        Opcode::Neg {
            dst: fw,
            src: p.neg_w,
        },
        Opcode::Bool {
            dst: b,
            value: ValBool(true),
        },
        Opcode::SetThis {
            field: p.tc_pos_changed,
            src: b,
        },
        Opcode::SetThis {
            field: p.tc_x,
            src: fw,
        },
    ]
}

struct Added {
    key: RefFun,
    fix: RefFun,
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<Added> {
    let key = add_key(code, p)?;
    let fix = add_fix(code, p, key)?;
    let zero = float_const(code, 0.0);
    let f = &mut code.functions[p.fi];
    let base = f.regs.len() as u32;
    f.regs
        .extend([p.t.f64_, p.t.f64_, p.t.f64_, p.t.f64_, p.t.bool_, p.t.void]);
    let r = |k: u32| Reg(base + k);
    // The later point first, so the hook index stays valid.
    insert_ops(
        f,
        p.left_at,
        left_clamp(p, zero, [r(0), r(1), r(2), r(3), r(4)]),
    );
    insert_ops(
        f,
        p.hook_at,
        vec![Opcode::Call2 {
            dst: r(5),
            fun: fix,
            arg0: Reg(0),
            arg1: p.scene,
        }],
    );
    Ok(Added { key, fix })
}

/// Keeps tooltip helper columns on screen and free of duplicates, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_tip_overflow(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tip overflow skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    let saved = code.functions[p.fi].clone();
    match apply(code, &p) {
        Ok(a) => eprintln!(
            "patched tip overflow fn@{}: helper columns wrap to the screen height, duplicates hidden (fix fn@{}, key fn@{})",
            code.functions[p.fi].findex.0, a.fix.0, a.key.0
        ),
        Err(e) => {
            snap.restore(code);
            code.functions[p.fi] = saved;
            eprintln!("tip overflow skipped: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, write, HLBOOT};

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        let added = apply(&mut code, &p).expect("apply");
        let patched = write(&code);
        let back = read(&patched);

        // Appended only: 2 functions (+ their types), no globals.
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 2);
        assert_eq!(&back.types[..orig.types.len()], &orig.types[..]);
        assert_eq!(back.globals, orig.globals);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != p.fi, "function #{i} (fn@{})", a.findex.0);
        }
        for (k, f) in back.functions[nf..].iter().enumerate() {
            assert_eq!(f.findex, [added.key, added.fix][k]);
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }
        // The fix runs inside one try/catch: every return after the Trap closes it.
        let fix = &back.functions[nf + 1];
        assert!(matches!(fix.ops[0], Opcode::Trap { .. }));
        let catch = jump_targets(fix, 0)[0];
        for (i, op) in fix.ops.iter().enumerate() {
            if matches!(op, Opcode::Ret { .. }) && i < catch {
                assert!(matches!(fix.ops[i - 1], Opcode::EndTrap { .. }), "op {i}");
            }
        }

        // sync: the original ops with the two blocks inserted, nothing else.
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        let mut want = a.clone();
        let base = a.regs.len() as u32;
        let r = |k: u32| Reg(base + k);
        let zero = back
            .floats
            .iter()
            .position(|&v| v == 0.0)
            .map(hlbc::types::RefFloat)
            .unwrap();
        insert_ops(
            &mut want,
            p.left_at,
            left_clamp(&p, zero, [r(0), r(1), r(2), r(3), r(4)]),
        );
        insert_ops(
            &mut want,
            p.hook_at,
            vec![Opcode::Call2 {
                dst: r(5),
                fun: added.fix,
                arg0: Reg(0),
                arg1: p.scene,
            }],
        );
        assert_eq!(format!("{:?}", b.ops), format!("{:?}", want.ops));
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        assert_eq!(b.regs.len(), a.regs.len() + 6);
        check_flow(b);
        check_types(&back, b, p.hook_at..p.hook_at + 1);
        let left = p.left_at + 1;
        check_types(&back, b, left..left + 8);
        // The left clamp skips to the op that followed the store.
        assert_eq!(jump_targets(b, left + 3), vec![left + 8]);
        assert_eq!(
            format!("{:?}", b.ops[left + 8]),
            format!("{:?}", a.ops[p.left_at])
        );
        // Every path past the search reaches the hook: the search joins on the op
        // right before it, and nothing jumps over it.
        let targets: Vec<usize> = (0..b.ops.len()).flat_map(|i| jump_targets(b, i)).collect();
        assert!(targets.iter().filter(|&&t| t == p.hook_at - 1).count() >= 2);
        assert!(!targets.contains(&p.hook_at));
        assert!(!targets.contains(&(p.hook_at + 1)));
        assert!(matches!(b.ops[p.hook_at - 1], Opcode::Float { .. }));

        // A second pass changes nothing.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_tip_overflow(&mut again);
        assert!(write(&again) == patched);
    }

    /// The whole pipeline still applies the pass.
    #[test]
    fn composes_with_other_passes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let out = crate::patch_image(&image).expect("patch_image");
        let b = read(&out);
        let err = plan(&b).err().expect("tip overflow was not applied");
        assert!(format!("{err:#}").contains("already applied"), "{err:#}");
    }

    /// A clampedObj search of another shape is refused and leaves sync as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        // Something else in place of the placement test.
        code.functions[p.fi].ops[p.hook_at + 1] = Opcode::JAlways { offset: 0 };
        let before = format!("{:?}", code.functions[p.fi].ops);
        let nf = code.functions.len();
        assert!(plan(&code).is_err());
        patch_tip_overflow(&mut code);
        assert_eq!(format!("{:?}", code.functions[p.fi].ops), before);
        assert_eq!(code.functions.len(), nf);
    }
}
