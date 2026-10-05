// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Draggable popup windows and inventory panels, positions kept across reopen
// and restarts. Local UI only: nothing is sent over the network.
//
// Vanilla already has a window drag (Window.hx:525-575), installed by
// `ui.Window.init` only when the virtual `canDragWindow()` returns true (just
// `ui.win.ItemMap`). Its closures keep the offset in the window's
// `h2d.FlowProperties` inside `windowRoot` (offsetX/offsetY, applied after
// layout, so reflow / rebuild keep it) and mirror it on `frameFlow`.
//
// The pass:
//   W. `Window.init`: the `canDragWindow()` test (`JFalse -> Ret`) now jumps to
//      an appended `mpWinInstall(this)` instead of returning, so ItemMap keeps
//      its vanilla whole-body drag and every other window gets:
//      - `interactive.onPush = mpWinPush(this, _)`: left button only; ignored
//        unless the window is modal (`modal` is not `WindowModalMode.None`:
//        HUDs, title / loading screens and notifications stay put), the push is
//        in the top HEADER_PX band (`mouseY - absY`), and the window is
//        narrower than WIDE * scene width; then `mpDragBegin(win,
//        win.frameFlow, Type.getClassName(Type.getClass(win)))`.
//      - the saved offset of its class is applied (`mpDragRestore`).
//      - `windowRoot.onAfterReflow = mpWinReflow(this)` (no vanilla code sets
//        it on a plain h2d.Flow): `mpDragClamp(win)`, then frameFlow gets the
//        window's offsets again (it may be created after init).
//   P. `GameInventory` constructor (end): `mpDragPanel(chestInventory,
//      "GameInventory#chest")`, `mpDragPanel(inventory, "GameInventory#inv")`.
//      (GameInventory's own onAfterReflow is not usable: vanilla showInventory
//      restores InventoryContent's saved handler onto it.)
//
// Shared appended functions (reusable through `api()`, e.g. for more
// inventory panels keyed "AllInv#<slot>"):
//   mpDragBegin(obj, follow, key)   on push: a second push on the same object
//       within DOUBLE_S resets it (offset 0 on obj and follow, saved key
//       removed); otherwise starts a scene capture. Moves set obj's (and
//       follow's) offsets in its parent flow to mouse - start, the mouse clamped
//       to the scene; release / release-outside stops the capture and saves.
//   mpDragRestore(obj, follow, key) applies the saved offset, if any.
//   mpDragClamp(obj)  for an onAfterReflow: a visible, non-absolute `obj`
//       with a non-zero offset in its parent flow is pushed back so at least
//       KEEP_PX of it stays inside the scene horizontally and its top edge
//       stays within [0, height - KEEP_PX] (header reachable). Position: parent
//       absX/absY (local x/y before its first sync) + obj.x/y; width: the
//       flow's calculatedWidth for it. Covers restore at a smaller resolution
//       and a window resize.
//   mpDragPanel(panel, key)  restores the panel, sets `panel.onAfterReflow =
//       mpDragClamp(panel)`, then puts an interactive on its first child with
//       dom class "title" (the header; skipped when that child already has
//       one) whose push starts `mpDragBegin(panel, null, key)`. Header buttons
//       sit above that interactive and keep their clicks. The caller must own
//       the panel's onAfterReflow.
// Storage: `mpman.Storage.setUserData("mpWinPos:" + key, packed)` with
// packed = ((dx + 32768) << 16) | ((dy + 32768) & 0xFFFF) (one Int per key;
// removed when the offset is 0,0). Every appended function runs under a trap;
// an exception is logged ("mp: drag: ...", LOG_CAP lines per run) and the
// drag state is dropped.
//
// Validated before editing; each part is skipped (logged) on mismatch.

use super::asm::{push_fn, string_ref, Asm, Regs, Snap};
use super::diag::{index_of, static_fn};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{EnumConstruct, RefEnumConstruct, RefGlobal, RefString, ValBool};

const HEADER_PX: f64 = 70.0;
const WIDE: f64 = 0.9;
const KEEP_PX: f64 = 48.0;
const DOUBLE_S: f64 = 0.35;
const LOG_CAP: i32 = 20;
pub(crate) const PREFIX: &str = "mpWinPos:";
pub(crate) const KEY_CHEST: &str = "GameInventory#chest";
pub(crate) const KEY_INV: &str = "GameInventory#inv";
const HEADER_CLASS: &str = "title";
const S_ERR: &str = "mp: drag: ";
const N_BEGIN: &str = "mpDragBegin";
const N_RESTORE: &str = "mpDragRestore";
const N_CLAMP: &str = "mpDragClamp";
const N_PANEL: &str = "mpDragPanel";
const N_WIN_INSTALL: &str = "mpWinInstall";

/// The shared drag functions (findexes).
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // begin / restore: for later passes (more inventory panels)
pub(crate) struct DragApi {
    /// `(h2d.Object obj, h2d.Object follow, String key) -> void`
    pub(crate) begin: RefFun,
    /// `(h2d.Object obj, h2d.Object follow, String key) -> void`
    pub(crate) restore: RefFun,
    /// `(h2d.Flow obj) -> void`, bound to obj as an `onAfterReflow`.
    pub(crate) clamp: RefFun,
    /// `(h2d.Flow panel, String key) -> void`
    pub(crate) panel: RefFun,
    /// `(ui.Window win) -> void`
    win_install: RefFun,
}

struct Ctx {
    void_t: RefType,
    bool_t: RefType,
    i32_t: RefType,
    f64_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    obj_t: RefType,
    flow_t: RefType,
    fprops_t: RefType,
    scene_t: RefType,
    ev_t: RefType,
    win_t: RefType,
    arr_t: RefType,
    raw_t: RefType,
    dk_t: RefType,
    cls_t: RefType,
    push_t: RefType,
    reflow_t: RefType,
    cancel_t: RefType,
    nint_t: RefType,
    kind_t: RefType,
    modal_t: RefType,
    inter_t: RefType,
    // fields
    parent: RefField,
    children: RefField,
    x: RefField,
    y: RefField,
    abs_x: RefField,
    abs_y: RefField,
    visible: RefField,
    pos_changed: RefField,
    dom: RefField,
    properties: RefField,
    interactive: RefField,
    calc_w: RefField,
    on_after_reflow: RefField,
    off_x: RefField,
    off_y: RefField,
    is_abs: RefField,
    p_calc_w: RefField,
    sc_w: RefField,
    sc_h: RefField,
    kind: RefField,
    button: RefField,
    modal: RefField,
    frame_flow: RefField,
    window_root: RefField,
    on_push: RefField,
    arr_len: RefField,
    arr_arr: RefField,
    // functions
    get_props: RefFun,
    get_scene: RefFun,
    child_index: RefFun,
    mouse_x: RefFun,
    mouse_y: RefFun,
    start_capture: RefFun,
    stop_capture: RefFun,
    set_need_reflow: RefFun,
    set_enable_inter: RefFun,
    has_class: RefFun,
    get_class: RefFun,
    class_name: RefFun,
    str_add: RefFun,
    println: RefFun,
    std_string: RefFun,
    get_ud: RefFun,
    set_ud: RefFun,
    sys_time: RefFun,
    // enum indexes
    ev_release: i32,
    ev_move: i32,
    ev_release_out: i32,
    modal_none: i32,
    dbg_file: usize,
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let t = match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => n.t,
        None => code.functions[index_of(code, f)?].t,
    };
    let t = t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

fn want_sig(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("{what}: unexpected signature");
    }
    Ok(())
}

fn typed(code: &Bytecode, t: RefType, name: &str, want: RefType) -> Result<RefField> {
    let (f, ft) = field(code, t, name)?;
    if ft != want {
        bail!("field {name} has an unexpected type");
    }
    Ok(f)
}

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

fn native(code: &Bytecode, name: &str) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .natives
        .iter()
        .filter(|n| s(code, n.name) == name)
        .map(|n| n.findex)
        .collect();
    let [f] = hits[..] else {
        bail!("native {name}: {} matches (want 1)", hits.len());
    };
    Ok(f)
}

fn ctx(code: &Bytecode) -> Result<Ctx> {
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let f64_t = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let fprops_t = obj_type(code, "h2d.FlowProperties")?;
    let scene_t = obj_type(code, "h2d.Scene")?;
    let ev_t = obj_type(code, "hxd.Event")?;
    let win_t = obj_type(code, "ui.Window")?;
    let inter_t = obj_type(code, "h2d.Interactive")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let (arr_arr, raw_t) = field(code, arr_t, "array")?;
    let arr_len = typed(code, arr_t, "length", i32_t)?;

    let (parent, parent_t) = field(code, obj_t, "parent")?;
    if parent_t != obj_t {
        bail!("h2d.Object.parent is not an h2d.Object");
    }
    let children = typed(code, obj_t, "children", arr_t)?;
    let x = typed(code, obj_t, "x", f64_t)?;
    let y = typed(code, obj_t, "y", f64_t)?;
    let abs_x = typed(code, obj_t, "absX", f64_t)?;
    let abs_y = typed(code, obj_t, "absY", f64_t)?;
    let visible = typed(code, obj_t, "visible", bool_t)?;
    let pos_changed = typed(code, obj_t, "posChanged", bool_t)?;
    let (dom, dk_t) = field(code, obj_t, "dom")?;
    let properties = typed(code, flow_t, "properties", arr_t)?;
    let interactive = typed(code, flow_t, "interactive", inter_t)?;
    let calc_w = typed(code, flow_t, "calculatedWidth", f64_t)?;
    let (on_after_reflow, reflow_t) = field(code, flow_t, "onAfterReflow")?;
    if !matches!(reflow_t.as_fun(code), Some(t) if t.args.is_empty() && t.ret == void_t) {
        bail!("Flow.onAfterReflow is not () -> void");
    }
    let off_x = typed(code, fprops_t, "offsetX", i32_t)?;
    let off_y = typed(code, fprops_t, "offsetY", i32_t)?;
    let is_abs = typed(code, fprops_t, "isAbsolute", bool_t)?;
    let p_calc_w = typed(code, fprops_t, "calculatedWidth", i32_t)?;
    let sc_w = typed(code, scene_t, "width", i32_t)?;
    let sc_h = typed(code, scene_t, "height", i32_t)?;
    let (kind, kind_t) = field(code, ev_t, "kind")?;
    let button = typed(code, ev_t, "button", i32_t)?;
    let (modal, modal_t) = field(code, win_t, "modal")?;
    let frame_flow = typed(code, win_t, "frameFlow", flow_t)?;
    let window_root = typed(code, win_t, "windowRoot", flow_t)?;
    let (on_push, push_t) = field(code, inter_t, "onPush")?;
    if !matches!(push_t.as_fun(code), Some(t) if t.args == [ev_t] && t.ret == void_t) {
        bail!("Interactive.onPush is not (hxd.Event) -> void");
    }

    let get_props = proto(code, flow_t, "getProperties")?;
    want_sig(
        code,
        get_props,
        "Flow.getProperties",
        &[flow_t, obj_t],
        fprops_t,
    )?;
    let get_scene = method(code, obj_t, "getScene")?.findex;
    want_sig(code, get_scene, "Object.getScene", &[obj_t], scene_t)?;
    let child_index = method(code, obj_t, "getChildIndex")?.findex;
    want_sig(
        code,
        child_index,
        "Object.getChildIndex",
        &[obj_t, obj_t],
        i32_t,
    )?;
    let mouse_x = method(code, scene_t, "get_mouseX")?.findex;
    let mouse_y = method(code, scene_t, "get_mouseY")?.findex;
    want_sig(code, mouse_x, "Scene.get_mouseX", &[scene_t], f64_t)?;
    want_sig(code, mouse_y, "Scene.get_mouseY", &[scene_t], f64_t)?;
    let start_capture = method(code, scene_t, "startCapture")?.findex;
    let (sa, sr) = sig(code, start_capture)?;
    if sa.len() != 4 || sa[0] != scene_t || sa[1] != push_t || sr != void_t {
        bail!("Scene.startCapture: unexpected signature");
    }
    let (cancel_t, nint_t) = (sa[2], sa[3]);
    let stop_capture = method(code, scene_t, "stopCapture")?.findex;
    want_sig(code, stop_capture, "Scene.stopCapture", &[scene_t], void_t)?;
    let set_need_reflow = method(code, flow_t, "set_needReflow")?.findex;
    want_sig(
        code,
        set_need_reflow,
        "Flow.set_needReflow",
        &[flow_t, bool_t],
        bool_t,
    )?;
    let set_enable_inter = method(code, flow_t, "set_enableInteractive")?.findex;
    want_sig(
        code,
        set_enable_inter,
        "Flow.set_enableInteractive",
        &[flow_t, bool_t],
        bool_t,
    )?;
    let has_class = method(code, dk_t, "hasClass")?.findex;
    want_sig(
        code,
        has_class,
        "Properties.hasClass",
        &[dk_t, str_t],
        bool_t,
    )?;
    let get_class = static_fn(code, "$Type", "getClass")?;
    let ga = fun_args(code, get_class);
    let cls_t = get_class.t.as_fun(code).context("getClass")?.ret;
    if ga != [dyn_t] {
        bail!("Type.getClass does not take one Dyn");
    }
    let get_class = get_class.findex;
    let class_name = static_fn(code, "$Type", "getClassName")?.findex;
    want_sig(code, class_name, "Type.getClassName", &[cls_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want_sig(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;
    let println = static_fn(code, "$Sys", "println")?.findex;
    want_sig(code, println, "Sys.println", &[dyn_t], void_t)?;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want_sig(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let get_ud = static_fn(code, "mpman.$Storage", "getUserData")?.findex;
    want_sig(code, get_ud, "Storage.getUserData", &[str_t, dyn_t], dyn_t)?;
    let set_ud = static_fn(code, "mpman.$Storage", "setUserData")?.findex;
    want_sig(code, set_ud, "Storage.setUserData", &[str_t, dyn_t], void_t)?;
    let sys_time = native(code, "sys_time")?;
    want_sig(code, sys_time, "sys_time", &[], f64_t)?;

    let ev_release = enum_index(code, kind_t, "ERelease")?;
    let ev_move = enum_index(code, kind_t, "EMove")?;
    let ev_release_out = enum_index(code, kind_t, "EReleaseOutside")?;
    let modal_none = enum_index(code, modal_t, "None")?;
    Ok(Ctx {
        void_t,
        bool_t,
        i32_t,
        f64_t,
        dyn_t,
        str_t,
        obj_t,
        flow_t,
        fprops_t,
        scene_t,
        ev_t,
        win_t,
        arr_t,
        raw_t,
        dk_t,
        cls_t,
        push_t,
        reflow_t,
        cancel_t,
        nint_t,
        kind_t,
        modal_t,
        inter_t,
        parent,
        children,
        x,
        y,
        abs_x,
        abs_y,
        visible,
        pos_changed,
        dom,
        properties,
        interactive,
        calc_w,
        on_after_reflow,
        off_x,
        off_y,
        is_abs,
        p_calc_w,
        sc_w,
        sc_h,
        kind,
        button,
        modal,
        frame_flow,
        window_root,
        on_push,
        arr_len,
        arr_arr,
        get_props,
        get_scene,
        child_index,
        mouse_x,
        mouse_y,
        start_capture,
        stop_capture,
        set_need_reflow,
        set_enable_inter,
        has_class,
        get_class,
        class_name,
        str_add,
        println,
        std_string,
        get_ud,
        set_ud,
        sys_time,
        ev_release,
        ev_move,
        ev_release_out,
        modal_none,
        dbg_file: debug_file(code, "src/ui/Window.hx")?,
    })
}

// ---------- appended functions ----------

struct Globals {
    obj: RefGlobal,
    follow: RefGlobal,
    key: RefGlobal,
    scene: RefGlobal,
    dx: RefGlobal,
    dy: RefGlobal,
    last: RefGlobal,
    last_t: RefGlobal,
    logs: RefGlobal,
    prefix: RefGlobal,
    err: RefGlobal,
    title: RefGlobal,
}

fn new_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

/// `Trap exc -> catch` ... `OUT: EndTrap; Ret v` / `catch: report(exc); Ret v`.
struct Guard {
    exc: Reg,
    v: Reg,
}

fn guard_open(a: &mut Asm, g: &Guard) {
    a.jmp(
        Opcode::Trap {
            exc: g.exc,
            offset: 0,
        },
        "catch",
    );
}

fn guard_close(a: &mut Asm, g: &Guard, report: RefFun) {
    a.label("out");
    a.op(Opcode::EndTrap { exc: g.exc });
    a.op(Opcode::Ret { ret: g.v });
    a.label("catch");
    a.op(Opcode::Call1 {
        dst: g.v,
        fun: report,
        arg0: g.exc,
    });
    a.op(Opcode::Ret { ret: g.v });
}

fn name_fn(code: &mut Bytecode, f: RefFun, name: &str) {
    let n = string_ref(code, name);
    let i = code.functions.iter().position(|x| x.findex == f).unwrap();
    code.functions[i].name = n;
}

fn find_named(code: &Bytecode, name: &str) -> Option<RefFun> {
    code.functions
        .iter()
        .find(|f| f.name != RefString(0) && s(code, f.name) == name)
        .map(|f| f.findex)
}

/// The shared drag functions, appended on first use (found by name afterwards).
pub(crate) fn api(code: &mut Bytecode) -> Result<DragApi> {
    if let (Some(begin), Some(restore), Some(clamp), Some(panel), Some(win_install)) = (
        find_named(code, N_BEGIN),
        find_named(code, N_RESTORE),
        find_named(code, N_CLAMP),
        find_named(code, N_PANEL),
        find_named(code, N_WIN_INSTALL),
    ) {
        return Ok(DragApi {
            begin,
            restore,
            clamp,
            panel,
            win_install,
        });
    }
    let c = ctx(code)?;
    let snap = Snap::take(code);
    let r = build(code, &c);
    if r.is_err() {
        snap.restore(code);
    }
    r
}

fn build(code: &mut Bytecode, c: &Ctx) -> Result<DragApi> {
    let g = Globals {
        obj: new_global(code, c.obj_t),
        follow: new_global(code, c.obj_t),
        key: new_global(code, c.str_t),
        scene: new_global(code, c.scene_t),
        dx: new_global(code, c.f64_t),
        dy: new_global(code, c.f64_t),
        last: new_global(code, c.obj_t),
        last_t: new_global(code, c.f64_t),
        logs: new_global(code, c.i32_t),
        prefix: str_global(code, c.str_t, PREFIX),
        err: str_global(code, c.str_t, S_ERR),
        title: str_global(code, c.str_t, HEADER_CLASS),
    };
    let report = add_report(code, c, &g)?;
    let save = add_save(code, c, &g, report)?;
    let restore = add_restore(code, c, &g, report)?;
    let event = add_event(code, c, &g, report, save)?;
    let begin = add_begin(code, c, &g, report, save, event)?;
    let clamp = add_clamp(code, c, report)?;
    let panel_push = add_panel_push(code, c, begin)?;
    let panel = add_panel(
        code,
        c,
        &g,
        report,
        restore,
        clamp,
        panel_push.0,
        panel_push.1,
    )?;
    let win_reflow = add_win_reflow(code, c, report, clamp)?;
    let win_push = add_win_push(code, c, report, begin)?;
    let win_install = add_win_install(code, c, report, restore, win_push, win_reflow)?;
    for (f, n) in [
        (begin, N_BEGIN),
        (restore, N_RESTORE),
        (clamp, N_CLAMP),
        (panel, N_PANEL),
        (win_install, N_WIN_INSTALL),
    ] {
        name_fn(code, f, n);
    }
    Ok(DragApi {
        begin,
        restore,
        clamp,
        panel,
        win_install,
    })
}

/// `report(exc)`: `if (logs < LOG_CAP) { logs++; Sys.println("mp: drag: " + Std.string(exc)); }`
fn add_report(code: &mut Bytecode, c: &Ctx, g: &Globals) -> Result<RefFun> {
    let cap = int_const(code, LOG_CAP);
    let mut r = Regs(vec![c.dyn_t]);
    let exc = Reg(0);
    let (v, n, k, s1, s2) = (
        r.r(c.void_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.str_t),
        r.r(c.str_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: n,
        global: g.logs,
    });
    a.op(Opcode::Int { dst: k, ptr: cap });
    a.jmp(
        Opcode::JSGte {
            a: n,
            b: k,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Incr { dst: n });
    a.op(Opcode::SetGlobal {
        global: g.logs,
        src: n,
    });
    a.op(Opcode::GetGlobal {
        dst: s1,
        global: g.err,
    });
    a.op(Opcode::Call1 {
        dst: s2,
        fun: c.std_string,
        arg0: exc,
    });
    a.op(Opcode::Call2 {
        dst: s1,
        fun: c.str_add,
        arg0: s1,
        arg1: s2,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.println,
        arg0: s1,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![c.dyn_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `flow = SafeCast(obj.parent)`; jumps to `out` when obj or parent is null.
fn parent_flow(a: &mut Asm, c: &Ctx, obj: Reg, p: Reg, fl: Reg) {
    a.jmp(
        Opcode::JNull {
            reg: obj,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: p,
        obj,
        field: c.parent,
    });
    a.jmp(Opcode::JNull { reg: p, offset: 0 }, "out");
    a.op(Opcode::SafeCast { dst: fl, src: p });
}

fn get_props(a: &mut Asm, c: &Ctx, dst: Reg, fl: Reg, obj: Reg, null_to: &'static str) {
    a.op(Opcode::Call2 {
        dst,
        fun: c.get_props,
        arg0: fl,
        arg1: obj,
    });
    a.jmp(
        Opcode::JNull {
            reg: dst,
            offset: 0,
        },
        null_to,
    );
}

fn set_offsets(a: &mut Asm, c: &Ctx, pr: Reg, ox: Reg, oy: Reg) {
    a.op(Opcode::SetField {
        obj: pr,
        field: c.off_x,
        src: ox,
    });
    a.op(Opcode::SetField {
        obj: pr,
        field: c.off_y,
        src: oy,
    });
}

/// `full = PREFIX + key`
fn full_key(a: &mut Asm, c: &Ctx, g: &Globals, full: Reg, key: Reg) {
    a.op(Opcode::GetGlobal {
        dst: full,
        global: g.prefix,
    });
    a.op(Opcode::Call2 {
        dst: full,
        fun: c.str_add,
        arg0: full,
        arg1: key,
    });
}

/// `save(obj, key)`: stores obj's offsets in its parent flow (removes the key at 0,0).
fn add_save(code: &mut Bytecode, c: &Ctx, g: &Globals, report: RefFun) -> Result<RefFun> {
    let (k0, k16, kh, km) = (
        int_const(code, 0),
        int_const(code, 16),
        int_const(code, 32768),
        int_const(code, 65535),
    );
    let mut r = Regs(vec![c.obj_t, c.str_t]);
    let (obj, key) = (Reg(0), Reg(1));
    let (v, exc, p, fl, pr) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.fprops_t),
    );
    let (ox, oy, k, full, d) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.str_t),
        r.r(c.dyn_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    parent_flow(&mut a, c, obj, p, fl);
    get_props(&mut a, c, pr, fl, obj, "out");
    a.op(Opcode::Field {
        dst: ox,
        obj: pr,
        field: c.off_x,
    });
    a.op(Opcode::Field {
        dst: oy,
        obj: pr,
        field: c.off_y,
    });
    full_key(&mut a, c, g, full, key);
    a.op(Opcode::Int { dst: k, ptr: k0 });
    a.jmp(
        Opcode::JNotEq {
            a: ox,
            b: k,
            offset: 0,
        },
        "pack",
    );
    a.jmp(
        Opcode::JNotEq {
            a: oy,
            b: k,
            offset: 0,
        },
        "pack",
    );
    a.op(Opcode::Null { dst: d });
    a.op(Opcode::Call2 {
        dst: v,
        fun: c.set_ud,
        arg0: full,
        arg1: d,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("pack");
    a.op(Opcode::Int { dst: k, ptr: kh });
    a.op(Opcode::Add {
        dst: ox,
        a: ox,
        b: k,
    });
    a.op(Opcode::Add {
        dst: oy,
        a: oy,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: k16 });
    a.op(Opcode::Shl {
        dst: ox,
        a: ox,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: km });
    a.op(Opcode::And {
        dst: oy,
        a: oy,
        b: k,
    });
    a.op(Opcode::Or {
        dst: ox,
        a: ox,
        b: oy,
    });
    a.op(Opcode::ToDyn { dst: d, src: ox });
    a.op(Opcode::Call2 {
        dst: v,
        fun: c.set_ud,
        arg0: full,
        arg1: d,
    });
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![c.obj_t, c.str_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `restore(obj, follow, key)`: applies the saved offset of `key`, if any.
fn add_restore(code: &mut Bytecode, c: &Ctx, g: &Globals, report: RefFun) -> Result<RefFun> {
    let (k16, kh, km) = (
        int_const(code, 16),
        int_const(code, 32768),
        int_const(code, 65535),
    );
    let mut r = Regs(vec![c.obj_t, c.obj_t, c.str_t]);
    let (obj, follow, key) = (Reg(0), Reg(1), Reg(2));
    let (v, exc, p, fl, pr, fp) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.fprops_t),
        r.r(c.fprops_t),
    );
    let (iv, ox, oy, k, full, d, nd) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.str_t),
        r.r(c.dyn_t),
        r.r(c.dyn_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.jmp(
        Opcode::JNull {
            reg: obj,
            offset: 0,
        },
        "out",
    );
    full_key(&mut a, c, g, full, key);
    a.op(Opcode::Null { dst: nd });
    a.op(Opcode::Call2 {
        dst: d,
        fun: c.get_ud,
        arg0: full,
        arg1: nd,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "out");
    a.op(Opcode::SafeCast { dst: iv, src: d });
    parent_flow(&mut a, c, obj, p, fl);
    get_props(&mut a, c, pr, fl, obj, "out");
    a.op(Opcode::Int { dst: k, ptr: k16 });
    a.op(Opcode::UShr {
        dst: ox,
        a: iv,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: km });
    a.op(Opcode::And {
        dst: oy,
        a: iv,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: kh });
    a.op(Opcode::Sub {
        dst: ox,
        a: ox,
        b: k,
    });
    a.op(Opcode::Sub {
        dst: oy,
        a: oy,
        b: k,
    });
    set_offsets(&mut a, c, pr, ox, oy);
    a.jmp(
        Opcode::JNull {
            reg: follow,
            offset: 0,
        },
        "out",
    );
    get_props(&mut a, c, fp, fl, follow, "out");
    set_offsets(&mut a, c, fp, ox, oy);
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![c.obj_t, c.obj_t, c.str_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `stop()`: `g.obj = null; if (g.scene != null) g.scene.stopCapture();`
fn stop_drag(a: &mut Asm, c: &Ctx, g: &Globals, v: Reg, nobj: Reg, sc: Reg, end: &'static str) {
    a.op(Opcode::Null { dst: nobj });
    a.op(Opcode::SetGlobal {
        global: g.obj,
        src: nobj,
    });
    a.op(Opcode::GetGlobal {
        dst: sc,
        global: g.scene,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, end);
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.stop_capture,
        arg0: sc,
    });
}

/// `v = clamp(mouse, 0, size)` on f64 registers.
fn clamp_mouse(a: &mut Asm, m: Reg, zero: Reg, size: Reg, l1: &'static str, l2: &'static str) {
    a.jmp(
        Opcode::JSGte {
            a: m,
            b: zero,
            offset: 0,
        },
        l1,
    );
    a.op(Opcode::Mov { dst: m, src: zero });
    a.label(l1);
    a.jmp(
        Opcode::JSLte {
            a: m,
            b: size,
            offset: 0,
        },
        l2,
    );
    a.op(Opcode::Mov { dst: m, src: size });
    a.label(l2);
}

/// The scene capture callback `(hxd.Event) -> void`.
fn add_event(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
) -> Result<RefFun> {
    let (k_move, k_rel, k_relo) = (
        int_const(code, c.ev_move),
        int_const(code, c.ev_release),
        int_const(code, c.ev_release_out),
    );
    let f0 = float_const(code, 0.0);
    let mut r = Regs(vec![c.ev_t]);
    let e = Reg(0);
    let (v, exc, obj, nobj, sc, kd, ki, k) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.obj_t),
        r.r(c.scene_t),
        r.r(c.kind_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
    let (key, p, fl, pr, fp, fo) = (
        r.r(c.str_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.fprops_t),
        r.r(c.fprops_t),
        r.r(c.obj_t),
    );
    let (mx, my, z, sz, dd, si, ox, oy) = (
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetGlobal {
        dst: obj,
        global: g.obj,
    });
    a.jmp(
        Opcode::JNull {
            reg: obj,
            offset: 0,
        },
        "stop",
    );
    a.op(Opcode::Field {
        dst: kd,
        obj: e,
        field: c.kind,
    });
    a.jmp(Opcode::JNull { reg: kd, offset: 0 }, "out");
    a.op(Opcode::EnumIndex { dst: ki, value: kd });
    a.op(Opcode::Int {
        dst: k,
        ptr: k_move,
    });
    a.jmp(
        Opcode::JEq {
            a: ki,
            b: k,
            offset: 0,
        },
        "move",
    );
    a.op(Opcode::Int { dst: k, ptr: k_rel });
    a.jmp(
        Opcode::JEq {
            a: ki,
            b: k,
            offset: 0,
        },
        "release",
    );
    a.op(Opcode::Int {
        dst: k,
        ptr: k_relo,
    });
    a.jmp(
        Opcode::JEq {
            a: ki,
            b: k,
            offset: 0,
        },
        "release",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    // A capture without a drag object (should not happen): just stop it.
    a.label("stop");
    stop_drag(&mut a, c, g, v, nobj, sc, "out");
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    a.label("release");
    stop_drag(&mut a, c, g, v, nobj, sc, "save");
    a.label("save");
    a.op(Opcode::GetGlobal {
        dst: key,
        global: g.key,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: save,
        arg0: obj,
        arg1: key,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    a.label("move");
    a.op(Opcode::GetGlobal {
        dst: sc,
        global: g.scene,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    parent_flow(&mut a, c, obj, p, fl);
    get_props(&mut a, c, pr, fl, obj, "out");
    a.op(Opcode::Float { dst: z, ptr: f0 });
    a.op(Opcode::Call1 {
        dst: mx,
        fun: c.mouse_x,
        arg0: sc,
    });
    a.op(Opcode::Field {
        dst: si,
        obj: sc,
        field: c.sc_w,
    });
    a.op(Opcode::ToSFloat { dst: sz, src: si });
    clamp_mouse(&mut a, mx, z, sz, "x1", "x2");
    a.op(Opcode::GetGlobal {
        dst: dd,
        global: g.dx,
    });
    a.op(Opcode::Sub {
        dst: mx,
        a: mx,
        b: dd,
    });
    a.op(Opcode::ToInt { dst: ox, src: mx });
    a.op(Opcode::Call1 {
        dst: my,
        fun: c.mouse_y,
        arg0: sc,
    });
    a.op(Opcode::Field {
        dst: si,
        obj: sc,
        field: c.sc_h,
    });
    a.op(Opcode::ToSFloat { dst: sz, src: si });
    clamp_mouse(&mut a, my, z, sz, "y1", "y2");
    a.op(Opcode::GetGlobal {
        dst: dd,
        global: g.dy,
    });
    a.op(Opcode::Sub {
        dst: my,
        a: my,
        b: dd,
    });
    a.op(Opcode::ToInt { dst: oy, src: my });
    set_offsets(&mut a, c, pr, ox, oy);
    a.op(Opcode::GetGlobal {
        dst: fo,
        global: g.follow,
    });
    a.jmp(Opcode::JNull { reg: fo, offset: 0 }, "out");
    get_props(&mut a, c, fp, fl, fo, "out");
    set_offsets(&mut a, c, fp, ox, oy);

    a.label("out");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    // An exception drops the drag (and its capture) after logging it.
    a.label("catch");
    a.op(Opcode::Call1 {
        dst: v,
        fun: report,
        arg0: exc,
    });
    stop_drag(&mut a, c, g, v, nobj, sc, "end");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![c.ev_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `begin(obj, follow, key)`: double push resets, a single push starts the drag.
fn add_begin(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
    event: RefFun,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let f_dbl = float_const(code, DOUBLE_S);
    let mut r = Regs(vec![c.obj_t, c.obj_t, c.str_t]);
    let (obj, follow, key) = (Reg(0), Reg(1), Reg(2));
    let (v, exc, p, fl, sc, pr, fp) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.scene_t),
        r.r(c.fprops_t),
        r.r(c.fprops_t),
    );
    let (now, lt, dt, lim, last, nobj, z, m, oi, of) = (
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.obj_t),
        r.r(c.obj_t),
        r.r(c.i32_t),
        r.r(c.f64_t),
        r.r(c.i32_t),
        r.r(c.f64_t),
    );
    let (cb, cn, ni) = (r.r(c.push_t), r.r(c.cancel_t), r.r(c.nint_t));
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    parent_flow(&mut a, c, obj, p, fl);
    a.op(Opcode::Call1 {
        dst: sc,
        fun: c.get_scene,
        arg0: obj,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    get_props(&mut a, c, pr, fl, obj, "out");
    a.op(Opcode::Call0 {
        dst: now,
        fun: c.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.last,
    });
    a.jmp(
        Opcode::JNotEq {
            a: last,
            b: obj,
            offset: 0,
        },
        "press",
    );
    a.op(Opcode::GetGlobal {
        dst: lt,
        global: g.last_t,
    });
    a.op(Opcode::Sub {
        dst: dt,
        a: now,
        b: lt,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: f_dbl,
    });
    a.jmp(
        Opcode::JSGte {
            a: dt,
            b: lim,
            offset: 0,
        },
        "press",
    );

    // Double push: back to the layout position, saved key removed.
    a.op(Opcode::Int { dst: z, ptr: k0 });
    set_offsets(&mut a, c, pr, z, z);
    a.jmp(
        Opcode::JNull {
            reg: follow,
            offset: 0,
        },
        "reset_save",
    );
    get_props(&mut a, c, fp, fl, follow, "reset_save");
    set_offsets(&mut a, c, fp, z, z);
    a.label("reset_save");
    a.op(Opcode::Null { dst: nobj });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: nobj,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: save,
        arg0: obj,
        arg1: key,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    a.label("press");
    for (gl, src) in [
        (g.last, obj),
        (g.last_t, now),
        (g.obj, obj),
        (g.follow, follow),
        (g.key, key),
        (g.scene, sc),
    ] {
        a.op(Opcode::SetGlobal { global: gl, src });
    }
    for (mouse, off, gl) in [(c.mouse_x, c.off_x, g.dx), (c.mouse_y, c.off_y, g.dy)] {
        a.op(Opcode::Call1 {
            dst: m,
            fun: mouse,
            arg0: sc,
        });
        a.op(Opcode::Field {
            dst: oi,
            obj: pr,
            field: off,
        });
        a.op(Opcode::ToSFloat { dst: of, src: oi });
        a.op(Opcode::Sub {
            dst: m,
            a: m,
            b: of,
        });
        a.op(Opcode::SetGlobal { global: gl, src: m });
    }
    a.op(Opcode::StaticClosure {
        dst: cb,
        fun: event,
    });
    a.op(Opcode::Null { dst: cn });
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Call4 {
        dst: v,
        fun: c.start_capture,
        arg0: sc,
        arg1: cb,
        arg2: cn,
        arg3: ni,
    });
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![c.obj_t, c.obj_t, c.str_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `clamp(obj)`: keeps a dragged `obj` (child of a flow) on screen (see the header).
fn add_clamp(code: &mut Bytecode, c: &Ctx, report: RefFun) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let (f0, f_keep) = (float_const(code, 0.0), float_const(code, KEEP_PX));
    let mut r = Regs(vec![c.flow_t]);
    let obj = Reg(0);
    let (v, exc, b, sc, p, fl, ps, raw, d, pr) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.bool_t),
        r.r(c.scene_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.arr_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
        r.r(c.fprops_t),
    );
    let (idx, n, z, ox, oy, nx, ny, si, wi) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
    let (fx, fy, wm, hm, keep, zf, ax, ay, w, sf, t) = (
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
    );
    let gd = Guard { exc, v };
    let fld =
        |a: &mut Asm, dst: Reg, obj: Reg, field: RefField| a.op(Opcode::Field { dst, obj, field });
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.jmp(
        Opcode::JNull {
            reg: obj,
            offset: 0,
        },
        "out",
    );
    fld(&mut a, b, obj, c.visible);
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    parent_flow(&mut a, c, obj, p, fl);
    // Its FlowProperties, read without getProperties (that would force a reflow).
    a.op(Opcode::Call2 {
        dst: idx,
        fun: c.child_index,
        arg0: fl,
        arg1: obj,
    });
    fld(&mut a, ps, fl, c.properties);
    a.jmp(Opcode::JNull { reg: ps, offset: 0 }, "out");
    fld(&mut a, n, ps, c.arr_len);
    a.op(Opcode::Int { dst: z, ptr: k0 });
    a.jmp(
        Opcode::JSLt {
            a: idx,
            b: z,
            offset: 0,
        },
        "out",
    );
    a.jmp(
        Opcode::JSGte {
            a: idx,
            b: n,
            offset: 0,
        },
        "out",
    );
    fld(&mut a, raw, ps, c.arr_arr);
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: idx,
    });
    a.op(Opcode::UnsafeCast { dst: pr, src: d });
    a.jmp(Opcode::JNull { reg: pr, offset: 0 }, "out");
    fld(&mut a, b, pr, c.is_abs);
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "out");
    fld(&mut a, ox, pr, c.off_x);
    fld(&mut a, oy, pr, c.off_y);
    a.jmp(
        Opcode::JNotEq {
            a: ox,
            b: z,
            offset: 0,
        },
        "go",
    );
    a.jmp(
        Opcode::JEq {
            a: oy,
            b: z,
            offset: 0,
        },
        "out",
    );
    a.label("go");
    // Origin of the parent: absX/absY once synced, else its local position (windowRoot: 0,0).
    fld(&mut a, b, fl, c.pos_changed);
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "abs");
    fld(&mut a, fx, fl, c.x);
    fld(&mut a, fy, fl, c.y);
    a.jmp(Opcode::JAlways { offset: 0 }, "origin");
    a.label("abs");
    fld(&mut a, fx, fl, c.abs_x);
    fld(&mut a, fy, fl, c.abs_y);
    a.label("origin");
    a.op(Opcode::Call1 {
        dst: sc,
        fun: c.get_scene,
        arg0: obj,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    a.op(Opcode::Float {
        dst: keep,
        ptr: f_keep,
    });
    a.op(Opcode::Float { dst: zf, ptr: f0 });
    fld(&mut a, wi, sc, c.sc_w);
    a.op(Opcode::ToSFloat { dst: wm, src: wi });
    a.op(Opcode::Sub {
        dst: wm,
        a: wm,
        b: keep,
    });
    fld(&mut a, wi, sc, c.sc_h);
    a.op(Opcode::ToSFloat { dst: hm, src: wi });
    a.op(Opcode::Sub {
        dst: hm,
        a: hm,
        b: keep,
    });
    fld(&mut a, t, obj, c.x);
    a.op(Opcode::Add {
        dst: ax,
        a: fx,
        b: t,
    });
    fld(&mut a, t, obj, c.y);
    a.op(Opcode::Add {
        dst: ay,
        a: fy,
        b: t,
    });
    fld(&mut a, wi, pr, c.p_calc_w);
    a.op(Opcode::ToSFloat { dst: w, src: wi });
    a.op(Opcode::Mov { dst: nx, src: ox });
    a.op(Opcode::Mov { dst: ny, src: oy });
    // Right: x <= W - KEEP.
    a.jmp(
        Opcode::JSLte {
            a: ax,
            b: wm,
            offset: 0,
        },
        "r1",
    );
    a.op(Opcode::Sub {
        dst: sf,
        a: ax,
        b: wm,
    });
    a.op(Opcode::ToInt { dst: si, src: sf });
    a.op(Opcode::Sub {
        dst: nx,
        a: nx,
        b: si,
    });
    a.label("r1");
    // Left: x + w >= KEEP.
    a.op(Opcode::Add {
        dst: sf,
        a: ax,
        b: w,
    });
    a.jmp(
        Opcode::JSGte {
            a: sf,
            b: keep,
            offset: 0,
        },
        "r2",
    );
    a.op(Opcode::Sub {
        dst: sf,
        a: keep,
        b: sf,
    });
    a.op(Opcode::ToInt { dst: si, src: sf });
    a.op(Opcode::Add {
        dst: nx,
        a: nx,
        b: si,
    });
    a.label("r2");
    // Top: y >= 0.
    a.jmp(
        Opcode::JSGte {
            a: ay,
            b: zf,
            offset: 0,
        },
        "r3",
    );
    a.op(Opcode::ToInt { dst: si, src: ay });
    a.op(Opcode::Sub {
        dst: ny,
        a: ny,
        b: si,
    });
    a.label("r3");
    // Bottom: y <= H - KEEP.
    a.jmp(
        Opcode::JSLte {
            a: ay,
            b: hm,
            offset: 0,
        },
        "r4",
    );
    a.op(Opcode::Sub {
        dst: sf,
        a: ay,
        b: hm,
    });
    a.op(Opcode::ToInt { dst: si, src: sf });
    a.op(Opcode::Sub {
        dst: ny,
        a: ny,
        b: si,
    });
    a.label("r4");
    a.jmp(
        Opcode::JNotEq {
            a: nx,
            b: ox,
            offset: 0,
        },
        "set",
    );
    a.jmp(
        Opcode::JEq {
            a: ny,
            b: oy,
            offset: 0,
        },
        "out",
    );
    a.label("set");
    set_offsets(&mut a, c, pr, nx, ny);
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.set_need_reflow,
        arg0: fl,
        arg1: b,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![c.flow_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// Panel push closure `(capture(panel, key), hxd.Event) -> void` and its capture enum type.
fn add_panel_push(code: &mut Bytecode, c: &Ctx, begin: RefFun) -> Result<(RefFun, RefType)> {
    let k0 = int_const(code, 0);
    code.types.push(Type::Enum {
        name: RefString(0),
        global: RefGlobal(0),
        constructs: vec![EnumConstruct {
            name: RefString(0),
            params: vec![c.flow_t, c.str_t],
        }],
    });
    let cap_t = RefType(code.types.len() - 1);
    let mut r = Regs(vec![cap_t, c.ev_t]);
    let (cx, e) = (Reg(0), Reg(1));
    let (v, bt, z, panel, key, nf) = (
        r.r(c.void_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.flow_t),
        r.r(c.str_t),
        r.r(c.obj_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: bt,
        obj: e,
        field: c.button,
    });
    a.op(Opcode::Int { dst: z, ptr: k0 });
    a.jmp(
        Opcode::JNotEq {
            a: bt,
            b: z,
            offset: 0,
        },
        "end",
    );
    for (dst, i) in [(panel, 0), (key, 1)] {
        a.op(Opcode::EnumField {
            dst,
            value: cx,
            construct: RefEnumConstruct(0),
            field: RefField(i),
        });
    }
    a.op(Opcode::Null { dst: nf });
    a.op(Opcode::Call3 {
        dst: v,
        fun: begin,
        arg0: panel,
        arg1: nf,
        arg2: key,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    let f = push_fn(
        code,
        vec![cap_t, c.ev_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )?;
    Ok((f, cap_t))
}

/// `panel(panel, key)`: restore, then a drag interactive on the "title" header child.
#[allow(clippy::too_many_arguments)]
fn add_panel(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    restore: RefFun,
    clamp: RefFun,
    panel_push: RefFun,
    cap_t: RefType,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![c.flow_t, c.str_t]);
    let (panel, key) = (Reg(0), Reg(1));
    let (v, exc, b, nf, cls, ch, raw, d) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.bool_t),
        r.r(c.obj_t),
        r.r(c.str_t),
        r.r(c.arr_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
    );
    let (n, i, ch_o, dm, hf, it, cx, cl) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.obj_t),
        r.r(c.dk_t),
        r.r(c.flow_t),
        r.r(c.inter_t),
        r.r(cap_t),
        r.r(c.push_t),
    );
    let rc = r.r(c.reflow_t);
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.jmp(
        Opcode::JNull {
            reg: panel,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Null { dst: nf });
    a.op(Opcode::Call3 {
        dst: v,
        fun: restore,
        arg0: panel,
        arg1: nf,
        arg2: key,
    });
    a.op(Opcode::InstanceClosure {
        dst: rc,
        fun: clamp,
        obj: panel,
    });
    a.op(Opcode::SetField {
        obj: panel,
        field: c.on_after_reflow,
        src: rc,
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: g.title,
    });
    a.op(Opcode::Field {
        dst: ch,
        obj: panel,
        field: c.children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: n,
        obj: ch,
        field: c.arr_len,
    });
    a.op(Opcode::Int { dst: i, ptr: k0 });
    a.loop_head("loop");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: ch,
        field: c.arr_arr,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::UnsafeCast { dst: ch_o, src: d });
    a.op(Opcode::Incr { dst: i });
    a.jmp(
        Opcode::JNull {
            reg: ch_o,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: dm,
        obj: ch_o,
        field: c.dom,
    });
    a.jmp(Opcode::JNull { reg: dm, offset: 0 }, "loop");
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.has_class,
        arg0: dm,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    // The header: a flow without an interactive of its own.
    a.op(Opcode::SafeCast { dst: hf, src: ch_o });
    a.op(Opcode::Field {
        dst: it,
        obj: hf,
        field: c.interactive,
    });
    a.jmp(Opcode::JNotNull { reg: it, offset: 0 }, "out");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.set_enable_inter,
        arg0: hf,
        arg1: b,
    });
    a.op(Opcode::Field {
        dst: it,
        obj: hf,
        field: c.interactive,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "out");
    a.op(Opcode::MakeEnum {
        dst: cx,
        construct: RefEnumConstruct(0),
        args: vec![panel, key],
    });
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: panel_push,
        obj: cx,
    });
    a.op(Opcode::SetField {
        obj: it,
        field: c.on_push,
        src: cl,
    });
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![c.flow_t, c.str_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `winReflow(win)`: clamp windowRoot's children, then frameFlow follows the window.
fn add_win_reflow(code: &mut Bytecode, c: &Ctx, report: RefFun, clamp: RefFun) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![c.win_t]);
    let win = Reg(0);
    let (v, exc, b, root, ff, ps, raw, d, wp, fp) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.bool_t),
        r.r(c.flow_t),
        r.r(c.flow_t),
        r.r(c.arr_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
        r.r(c.fprops_t),
        r.r(c.fprops_t),
    );
    let (i, j, n, z, x1, x2) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.op(Opcode::Field {
        dst: root,
        obj: win,
        field: c.window_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: clamp,
        arg0: win,
    });
    a.op(Opcode::Field {
        dst: ff,
        obj: win,
        field: c.frame_flow,
    });
    a.jmp(Opcode::JNull { reg: ff, offset: 0 }, "out");
    a.op(Opcode::Call2 {
        dst: i,
        fun: c.child_index,
        arg0: root,
        arg1: win,
    });
    a.op(Opcode::Call2 {
        dst: j,
        fun: c.child_index,
        arg0: root,
        arg1: ff,
    });
    a.op(Opcode::Field {
        dst: ps,
        obj: root,
        field: c.properties,
    });
    a.jmp(Opcode::JNull { reg: ps, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: n,
        obj: ps,
        field: c.arr_len,
    });
    a.op(Opcode::Int { dst: z, ptr: k0 });
    for idx in [i, j] {
        a.jmp(
            Opcode::JSLt {
                a: idx,
                b: z,
                offset: 0,
            },
            "out",
        );
        a.jmp(
            Opcode::JSGte {
                a: idx,
                b: n,
                offset: 0,
            },
            "out",
        );
    }
    a.op(Opcode::Field {
        dst: raw,
        obj: ps,
        field: c.arr_arr,
    });
    for (dst, idx) in [(wp, i), (fp, j)] {
        a.op(Opcode::GetArray {
            dst: d,
            array: raw,
            index: idx,
        });
        a.op(Opcode::UnsafeCast { dst, src: d });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "out",
        );
    }
    a.op(Opcode::Field {
        dst: x1,
        obj: wp,
        field: c.off_x,
    });
    a.op(Opcode::Field {
        dst: x2,
        obj: fp,
        field: c.off_x,
    });
    a.jmp(
        Opcode::JNotEq {
            a: x1,
            b: x2,
            offset: 0,
        },
        "set",
    );
    a.op(Opcode::Field {
        dst: x1,
        obj: wp,
        field: c.off_y,
    });
    a.op(Opcode::Field {
        dst: x2,
        obj: fp,
        field: c.off_y,
    });
    a.jmp(
        Opcode::JEq {
            a: x1,
            b: x2,
            offset: 0,
        },
        "out",
    );
    a.label("set");
    a.op(Opcode::Field {
        dst: x1,
        obj: wp,
        field: c.off_x,
    });
    a.op(Opcode::Field {
        dst: x2,
        obj: wp,
        field: c.off_y,
    });
    set_offsets(&mut a, c, fp, x1, x2);
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.set_need_reflow,
        arg0: root,
        arg1: b,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![c.win_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `name = Type.getClassName(Type.getClass(win))`; jumps to `out` on null.
fn class_key(a: &mut Asm, c: &Ctx, win: Reg, dw: Reg, cl: Reg, name: Reg) {
    a.op(Opcode::ToDyn { dst: dw, src: win });
    a.op(Opcode::Call1 {
        dst: cl,
        fun: c.get_class,
        arg0: dw,
    });
    a.jmp(Opcode::JNull { reg: cl, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: name,
        fun: c.class_name,
        arg0: cl,
    });
    a.jmp(
        Opcode::JNull {
            reg: name,
            offset: 0,
        },
        "out",
    );
}

/// `winPush(win, e)`: header-band push on a modal, not full-width window.
fn add_win_push(code: &mut Bytecode, c: &Ctx, report: RefFun, begin: RefFun) -> Result<RefFun> {
    let (k0, k_none) = (int_const(code, 0), int_const(code, c.modal_none));
    let (f_head, f_wide) = (float_const(code, HEADER_PX), float_const(code, WIDE));
    let mut r = Regs(vec![c.win_t, c.ev_t]);
    let (win, e) = (Reg(0), Reg(1));
    let (v, exc, bt, k, md, mi, sc, my, ay, lim) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.modal_t),
        r.r(c.i32_t),
        r.r(c.scene_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
    );
    let (dw, cl, name, ff) = (r.r(c.dyn_t), r.r(c.cls_t), r.r(c.str_t), r.r(c.flow_t));
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.op(Opcode::Field {
        dst: bt,
        obj: e,
        field: c.button,
    });
    a.op(Opcode::Int { dst: k, ptr: k0 });
    a.jmp(
        Opcode::JNotEq {
            a: bt,
            b: k,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: md,
        obj: win,
        field: c.modal,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "out");
    a.op(Opcode::EnumIndex { dst: mi, value: md });
    a.op(Opcode::Int {
        dst: k,
        ptr: k_none,
    });
    a.jmp(
        Opcode::JEq {
            a: mi,
            b: k,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: sc,
        fun: c.get_scene,
        arg0: win,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    // Header band only.
    a.op(Opcode::Call1 {
        dst: my,
        fun: c.mouse_y,
        arg0: sc,
    });
    a.op(Opcode::Field {
        dst: ay,
        obj: win,
        field: c.abs_y,
    });
    a.op(Opcode::Sub {
        dst: my,
        a: my,
        b: ay,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: f_head,
    });
    a.jmp(
        Opcode::JSGt {
            a: my,
            b: lim,
            offset: 0,
        },
        "out",
    );
    // Not a (nearly) full-width window.
    a.op(Opcode::Field {
        dst: k,
        obj: sc,
        field: c.sc_w,
    });
    a.op(Opcode::ToSFloat { dst: lim, src: k });
    a.op(Opcode::Float {
        dst: ay,
        ptr: f_wide,
    });
    a.op(Opcode::Mul {
        dst: lim,
        a: lim,
        b: ay,
    });
    a.op(Opcode::Field {
        dst: my,
        obj: win,
        field: c.calc_w,
    });
    a.jmp(
        Opcode::JSGte {
            a: my,
            b: lim,
            offset: 0,
        },
        "out",
    );
    class_key(&mut a, c, win, dw, cl, name);
    a.op(Opcode::Field {
        dst: ff,
        obj: win,
        field: c.frame_flow,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: begin,
        arg0: win,
        arg1: ff,
        arg2: name,
    });
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![c.win_t, c.ev_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `winInstall(win)`: onPush, saved offset, reflow clamp.
fn add_win_install(
    code: &mut Bytecode,
    c: &Ctx,
    report: RefFun,
    restore: RefFun,
    win_push: RefFun,
    win_reflow: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![c.win_t]);
    let win = Reg(0);
    let (v, exc, it, pc, dw, cl, name, ff, root, rc) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.inter_t),
        r.r(c.push_t),
        r.r(c.dyn_t),
        r.r(c.cls_t),
        r.r(c.str_t),
        r.r(c.flow_t),
        r.r(c.flow_t),
        r.r(c.reflow_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.op(Opcode::Field {
        dst: it,
        obj: win,
        field: c.interactive,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "restore");
    a.op(Opcode::InstanceClosure {
        dst: pc,
        fun: win_push,
        obj: win,
    });
    a.op(Opcode::SetField {
        obj: it,
        field: c.on_push,
        src: pc,
    });
    a.label("restore");
    class_key(&mut a, c, win, dw, cl, name);
    a.op(Opcode::Field {
        dst: ff,
        obj: win,
        field: c.frame_flow,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: restore,
        arg0: win,
        arg1: ff,
        arg2: name,
    });
    a.op(Opcode::Field {
        dst: root,
        obj: win,
        field: c.window_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::InstanceClosure {
        dst: rc,
        fun: win_reflow,
        obj: win,
    });
    a.op(Opcode::SetField {
        obj: root,
        field: c.on_after_reflow,
        src: rc,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![c.win_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

// ---------- sites ----------

/// W: `Window.init`'s `canDragWindow()` test.
struct WinPlan {
    fi: usize,
    /// The `JFalse` after the `canDragWindow()` call.
    at: usize,
}

fn win_plan(code: &Bytecode) -> Result<WinPlan> {
    let win_t = obj_type(code, "ui.Window")?;
    let init = proto(code, win_t, "init")?;
    let fi = index_of(code, init)?;
    let f = &code.functions[fi];
    let wo = obj(code, win_t)?;
    let can = wo
        .protos
        .iter()
        .find(|p| s(code, p.name) == "canDragWindow")
        .context("proto canDragWindow not found")?;
    let slot = RefField(usize::try_from(can.pindex).context("canDragWindow pindex")?);
    let o = &f.ops;
    let calls: Vec<usize> = (0..o.len())
        .filter(|&i| {
            matches!(&o[i], Opcode::CallThis { field, args, .. } if *field == slot && args.is_empty())
        })
        .collect();
    let [call] = calls[..] else {
        bail!(
            "Window.init: {} canDragWindow() calls (want 1)",
            calls.len()
        );
    };
    let Opcode::CallThis { dst, .. } = o[call] else {
        unreachable!()
    };
    let at = call + 1;
    if !matches!(o.get(at), Some(Opcode::JFalse { cond, .. }) if *cond == dst) {
        bail!("Window.init: canDragWindow() is not followed by JFalse on its result");
    }
    let last = o.len() - 1;
    if jump_targets(f, at) != [last] {
        bail!("Window.init: the canDragWindow test does not jump to the final op (applied?)");
    }
    if !matches!(o[last], Opcode::Ret { .. }) {
        bail!("Window.init: no final Ret");
    }
    if (0..o.len()).any(|i| i != at && jump_targets(f, i).contains(&last)) {
        bail!("Window.init: another op jumps to the final Ret");
    }
    // The vanilla drag install between the test and the Ret.
    let inter_t = obj_type(code, "h2d.Interactive")?;
    let on_push = field(code, inter_t, "onPush")?.0;
    let on_release = field(code, inter_t, "onRelease")?.0;
    let sets = |fl: RefField| {
        o[at + 1..last]
            .iter()
            .filter(|x| matches!(x, Opcode::SetField { field, .. } if *field == fl))
            .count()
    };
    if sets(on_push) != 1 || sets(on_release) != 1 {
        bail!("Window.init: the canDragWindow block does not install onPush / onRelease");
    }
    // windowRoot is a plain h2d.Flow: nobody else may own its onAfterReflow.
    let flow_t = obj_type(code, "h2d.Flow")?;
    if let Some(g) = reflow_setter(code, |t, _| t == flow_t, &[flow_t]) {
        bail!(
            "fn@{} sets onAfterReflow of a plain h2d.Flow (applied?)",
            g.0
        );
    }
    Ok(WinPlan { fi, at })
}

/// The first function that sets `onAfterReflow` through a register accepted by
/// `reg_ok(type, op that last wrote it)` or as `this` of a type in `this_types`.
/// h2d.Flow's constructor (it fills a null one with its empty default) is ignored.
fn reflow_setter(
    code: &Bytecode,
    reg_ok: impl Fn(RefType, Option<&Opcode>) -> bool,
    this_types: &[RefType],
) -> Option<RefFun> {
    let flow_t = obj_type(code, "h2d.Flow").ok()?;
    let oar = field(code, flow_t, "onAfterReflow").ok()?.0;
    for g in &code.functions {
        if s(code, g.name) == "__constructor__" && g.regs.first() == Some(&flow_t) {
            continue;
        }
        for (i, op) in g.ops.iter().enumerate() {
            let hit = match op {
                Opcode::SetField { obj, field, .. } if *field == oar => {
                    let src = g.ops[..i].iter().rev().find(|x| {
                        matches!(x, Opcode::Field { dst, .. } | Opcode::GetThis { dst, .. }
                            | Opcode::Mov { dst, .. } | Opcode::Call1 { dst, .. }
                            | Opcode::Call2 { dst, .. } | Opcode::GetGlobal { dst, .. } if dst == obj)
                    });
                    reg_ok(g.regs[obj.0 as usize], src)
                }
                Opcode::SetThis { field, .. } if *field == oar => {
                    g.regs.first().is_some_and(|t| this_types.contains(t))
                }
                _ => false,
            };
            if hit {
                return Some(g.findex);
            }
        }
    }
    None
}

fn win_apply(code: &mut Bytecode, p: &WinPlan, api: &DragApi) {
    let f = &mut code.functions[p.fi];
    let last = f.ops.len() - 1;
    let Opcode::Ret { ret } = f.ops[last] else {
        unreachable!()
    };
    let end = f.ops.len();
    insert_ops(
        f,
        end,
        vec![
            Opcode::Call1 {
                dst: ret,
                fun: api.win_install,
                arg0: Reg(0),
            },
            Opcode::Ret { ret },
        ],
    );
    if let Opcode::JFalse { offset, .. } = &mut f.ops[p.at] {
        *offset = (end - p.at - 1) as i32;
    }
    eprintln!(
        "patched window drag fn@{} op {}: windows without canDragWindow() get the header drag (fn@{})",
        f.findex.0, p.at, api.win_install.0
    );
}

/// P: the `GameInventory` constructor's final Ret.
struct PanelPlan {
    fi: usize,
    ret: usize,
    chest: (RefField, RefType),
    inv: (RefField, RefType),
    void_reg: Reg,
}

fn is_sub(code: &Bytecode, t: RefType, of: RefType) -> bool {
    let mut cur = Some(t);
    while let Some(x) = cur {
        if x == of {
            return true;
        }
        cur = code.types[x.0].get_type_obj().and_then(|o| o.super_);
    }
    false
}

fn panel_plan(code: &Bytecode) -> Result<PanelPlan> {
    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let ctor = method(code, gi_t, "__constructor__")?;
    let fi = index_of(code, ctor.findex)?;
    let f = &code.functions[fi];
    let chest = field(code, gi_t, "chestInventory")?;
    let inv = field(code, gi_t, "inventory")?;
    for (n, (_, t)) in [("chestInventory", chest), ("inventory", inv)] {
        if !is_sub(code, t, flow_t) {
            bail!("GameInventory.{n} is not an h2d.Flow");
        }
    }
    let o = &f.ops;
    let rets: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::Ret { .. }))
        .collect();
    if rets != [o.len() - 1] {
        bail!("GameInventory constructor: expected a single final Ret");
    }
    let ret = o.len() - 1;
    let Opcode::Ret { ret: void_reg } = o[ret] else {
        unreachable!()
    };
    if !f.regs[void_reg.0 as usize].is_void() {
        bail!("GameInventory constructor: the Ret is not void");
    }
    for fl in [chest.0, inv.0] {
        if !o
            .iter()
            .any(|x| matches!(x, Opcode::SetThis { field, .. } if *field == fl))
        {
            bail!("GameInventory constructor does not set its panels");
        }
    }
    // Already applied: the two panel calls sit right before the Ret.
    if ret >= 6
        && matches!(o[ret - 6], Opcode::GetThis { field, .. } if field == chest.0)
        && matches!(o[ret - 3], Opcode::GetThis { field, .. } if field == inv.0)
        && matches!(o[ret - 1], Opcode::Call2 { .. })
    {
        bail!("GameInventory constructor already calls the panel install (applied)");
    }
    // Nobody else owns the panels' onAfterReflow: no SetThis in their classes, no
    // SetField through a register just read from chestInventory / inventory.
    let mut this_types = vec![];
    for t in [chest.1, inv.1] {
        let mut cur = Some(t);
        while let Some(x) = cur {
            if x == obj_t {
                break;
            }
            if !this_types.contains(&x) {
                this_types.push(x);
            }
            cur = code.types[x.0].get_type_obj().and_then(|o| o.super_);
        }
    }
    let from_panel = |t: RefType, src: Option<&Opcode>| {
        (t == chest.1 || t == inv.1)
            && matches!(src, Some(Opcode::Field { field, .. } | Opcode::GetThis { field, .. })
                if *field == chest.0 || *field == inv.0)
    };
    if let Some(g) = reflow_setter(code, from_panel, &this_types) {
        bail!("fn@{} sets onAfterReflow of a panel class or field", g.0);
    }
    Ok(PanelPlan {
        fi,
        ret,
        chest,
        inv,
        void_reg,
    })
}

fn panel_apply(code: &mut Bytecode, p: &PanelPlan, api: &DragApi, c: &Ctx) {
    let k_chest = str_global(code, c.str_t, KEY_CHEST);
    let k_inv = str_global(code, c.str_t, KEY_INV);
    let f = &mut code.functions[p.fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (e1, e2, k) = (reg(p.chest.1), reg(p.inv.1), reg(c.str_t));
    let v = p.void_reg;
    // Ops that jump straight to the Ret (other passes add early exits) must run
    // the install too: they are retargeted onto the first inserted op.
    let jumpers: Vec<usize> = (0..p.ret)
        .filter(|&i| jump_targets(f, i).contains(&p.ret))
        .collect();
    insert_ops(
        f,
        p.ret,
        vec![
            Opcode::GetThis {
                dst: e1,
                field: p.chest.0,
            },
            Opcode::GetGlobal {
                dst: k,
                global: k_chest,
            },
            Opcode::Call2 {
                dst: v,
                fun: api.panel,
                arg0: e1,
                arg1: k,
            },
            Opcode::GetThis {
                dst: e2,
                field: p.inv.0,
            },
            Opcode::GetGlobal {
                dst: k,
                global: k_inv,
            },
            Opcode::Call2 {
                dst: v,
                fun: api.panel,
                arg0: e2,
                arg1: k,
            },
        ],
    );
    for &i in &jumpers {
        retarget(f, i, p.ret + PANEL_OPS, p.ret);
    }
    eprintln!(
        "patched window drag fn@{} op {}: chest / inventory panels drag by their header (fn@{}, {} early exits retargeted)",
        f.findex.0,
        p.ret,
        api.panel.0,
        jumpers.len()
    );
}

const PANEL_OPS: usize = 6;

/// Every jump offset of op `i` that lands on `from` lands on `to` instead.
fn retarget(f: &mut Function, i: usize, from: usize, to: usize) {
    let fix = |off: &mut i32| {
        if (i as i64 + 1 + *off as i64) as usize == from {
            *off = to as i32 - i as i32 - 1;
        }
    };
    match &mut f.ops[i] {
        Opcode::JTrue { offset, .. }
        | Opcode::JFalse { offset, .. }
        | Opcode::JNull { offset, .. }
        | Opcode::JNotNull { offset, .. }
        | Opcode::JSLt { offset, .. }
        | Opcode::JSGte { offset, .. }
        | Opcode::JSGt { offset, .. }
        | Opcode::JSLte { offset, .. }
        | Opcode::JULt { offset, .. }
        | Opcode::JUGte { offset, .. }
        | Opcode::JNotLt { offset, .. }
        | Opcode::JNotGte { offset, .. }
        | Opcode::JEq { offset, .. }
        | Opcode::JNotEq { offset, .. }
        | Opcode::JAlways { offset } => fix(offset),
        Opcode::Switch { offsets, end, .. } => {
            for o in offsets.iter_mut() {
                fix(o);
            }
            fix(end);
        }
        _ => {}
    }
}

/// Draggable windows and inventory panels, or leaves `code` untouched and logs why.
pub(crate) fn patch_window_drag(code: &mut Bytecode) {
    let wp = win_plan(code);
    let pp = panel_plan(code);
    if let Err(e) = &wp {
        eprintln!("window drag (windows) skipped: {e:#}");
    }
    if let Err(e) = &pp {
        eprintln!("window drag (inventory panels) skipped: {e:#}");
    }
    if wp.is_err() && pp.is_err() {
        return;
    }
    let c = match ctx(code) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("window drag skipped: {e:#}");
            return;
        }
    };
    let api = match api(code) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("window drag skipped: {e:#}");
            return;
        }
    };
    if let Ok(p) = &wp {
        win_apply(code, p, &api);
    }
    if let Ok(p) = &pp {
        panel_apply(code, p, &api, &c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, shifted, write, HLBOOT};

    fn same(a: &Function, b: &Function) -> bool {
        format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs
    }

    /// Each Trap is closed by an EndTrap on its register, nothing jumps out of
    /// the protected block, and the handler follows a Ret.
    fn traps_ok(f: &Function) -> usize {
        let n = f.ops.len();
        let mut traps = 0;
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Trap { exc, .. } = *op else {
                continue;
            };
            traps += 1;
            let [handler] = jump_targets(f, i)[..] else {
                unreachable!()
            };
            let end = (i + 1..n)
                .find(|&j| matches!(f.ops[j], Opcode::EndTrap { exc: e } if e == exc))
                .expect("EndTrap");
            assert!(handler > end);
            assert!(matches!(f.ops[handler - 1], Opcode::Ret { .. }));
            for j in i + 1..end {
                assert!(!matches!(
                    f.ops[j],
                    Opcode::Ret { .. } | Opcode::Trap { .. }
                ));
                for t in jump_targets(f, j) {
                    assert!(
                        t > i && t <= end,
                        "fn@{} op {j} leaves the trap",
                        f.findex.0
                    );
                }
            }
        }
        traps
    }

    /// Sites found, only Window.init and the GameInventory constructor change,
    /// eleven functions are appended, every new op type-checks, a second pass
    /// changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let wp = win_plan(&orig).expect("windows plan");
        let pp = panel_plan(&orig).expect("panels plan");
        let mut code = read(&image);
        patch_window_drag(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 11);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        for i in 0..n {
            let want = i == wp.fi || i == pp.fi;
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                want,
                "function #{i}"
            );
        }

        // W: the test now jumps to the appended install call; everything else as before.
        let (a, b) = (&orig.functions[wp.fi], &back.functions[wp.fi]);
        let end = a.ops.len();
        assert_eq!(b.ops.len(), end + 2);
        assert_eq!(jump_targets(b, wp.at), [end]);
        for i in 0..end {
            if i != wp.at {
                assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[i]));
            }
        }
        let api = DragApi {
            begin: back.functions[n + 4].findex,
            restore: back.functions[n + 2].findex,
            clamp: back.functions[n + 5].findex,
            panel: back.functions[n + 7].findex,
            win_install: back.functions[n + 10].findex,
        };
        assert!(
            matches!(b.ops[end], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == api.win_install)
        );
        check_types(&back, b, end..end + 2);
        check_flow(b);

        // P: six ops in front of the final Ret.
        let (a, b) = (&orig.functions[pp.fi], &back.functions[pp.fi]);
        shifted(a, b, pp.ret, 6);
        check_types(&back, b, pp.ret..pp.ret + 6);
        check_flow(b);
        let calls: Vec<RefFun> = b.ops[pp.ret..pp.ret + 6]
            .iter()
            .filter_map(|o| match o {
                Opcode::Call2 { fun, .. } => Some(*fun),
                _ => None,
            })
            .collect();
        assert_eq!(calls, [api.panel, api.panel]);
        let keys: Vec<&str> = b.ops[pp.ret..pp.ret + 6]
            .iter()
            .filter_map(|o| match o {
                Opcode::GetGlobal { global, .. } => crate::job_xp::const_str(&back, *global),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [KEY_CHEST, KEY_INV]);

        // Appended functions: well-formed, typed, trapped where they touch game objects.
        let mut traps = 0;
        for f in &back.functions[n..] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
            traps += traps_ok(f);
        }
        assert_eq!(traps, 9);

        // Idempotent: a second pass refuses both sites and leaves the image as is.
        let mut again = read(&patched);
        assert!(win_plan(&again).is_err());
        assert!(panel_plan(&again).is_err());
        patch_window_drag(&mut again);
        assert!(write(&again) == patched);
    }

    /// The shared functions are appended once and found again by name.
    #[test]
    fn api_is_shared() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut code = read(&image);
        let n = code.functions.len();
        let a = api(&mut code).expect("api");
        assert_eq!(code.functions.len(), n + 11);
        let b = api(&mut code).expect("api again");
        assert_eq!(a, b);
        assert_eq!(code.functions.len(), n + 11);
        let sig_of = |f: RefFun| {
            let (args, ret) = sig(&code, f).unwrap();
            let names: Vec<String> = args
                .iter()
                .map(|t| {
                    code.types[t.0]
                        .get_type_obj()
                        .map_or("?".into(), |o| s(&code, o.name).to_string())
                })
                .collect();
            (names, ret.is_void())
        };
        assert_eq!(sig_of(a.begin).0, ["h2d.Object", "h2d.Object", "String"]);
        assert_eq!(sig_of(a.restore).0, ["h2d.Object", "h2d.Object", "String"]);
        assert_eq!(sig_of(a.clamp).0, ["h2d.Flow"]);
        assert_eq!(sig_of(a.panel).0, ["h2d.Flow", "String"]);
    }

    /// A jump straight to the constructor's Ret (another pass's early exit, as
    /// chest_buttons adds) lands on the panel install, not past it.
    #[test]
    fn early_exits_reach_install() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut code = read(&image);
        let pp = panel_plan(&code).expect("panels plan");
        insert_ops(
            &mut code.functions[pp.fi],
            pp.ret,
            vec![Opcode::JAlways { offset: 0 }],
        );
        let pp = panel_plan(&code).expect("panels plan with an early exit");
        let exit = pp.ret - 1;
        assert_eq!(jump_targets(&code.functions[pp.fi], exit), [pp.ret]);
        patch_window_drag(&mut code);
        let f = &code.functions[pp.fi];
        assert_eq!(jump_targets(f, exit), [pp.ret]);
        assert!(matches!(f.ops[pp.ret], Opcode::GetThis { .. }));
        assert!(matches!(f.ops[pp.ret + PANEL_OPS], Opcode::Ret { .. }));
        check_flow(f);
    }

    /// A shape mismatch at either site skips that part and leaves it untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let wp = win_plan(&orig).expect("windows plan");
        let pp = panel_plan(&orig).expect("panels plan");

        // Both sites broken: nothing changes at all.
        let mut code = read(&image);
        code.functions[wp.fi].ops[wp.at] = Opcode::Label;
        let ret = pp.void_reg;
        code.functions[pp.fi].ops.insert(0, Opcode::Ret { ret });
        assert!(win_plan(&code).is_err());
        assert!(panel_plan(&code).is_err());
        let before = write(&code);
        patch_window_drag(&mut code);
        assert!(write(&code) == before);

        // Only the window site broken: the panels part still applies.
        let mut code = read(&image);
        code.functions[wp.fi].ops[wp.at] = Opcode::Label;
        let w_before = format!("{:?}", code.functions[wp.fi].ops);
        patch_window_drag(&mut code);
        assert_eq!(format!("{:?}", code.functions[wp.fi].ops), w_before);
        assert_eq!(
            code.functions[pp.fi].ops.len(),
            orig.functions[pp.fi].ops.len() + 6
        );
    }
}
