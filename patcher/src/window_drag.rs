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
//        it on a plain h2d.Flow): `mpDragClamp(win, null)`, then frameFlow gets the
//        window's offsets again (it may be created after init).
//   P. `GameInventory` constructor (end): `addChild(chestInventory)` (the chest
//      panel, created first, becomes the last child: drawn above the inventory
//      panel, which a row resize or a drag can put under it; absolute, so the
//      flow layout is the same), `mpDragPanel(chestInventory,
//      "GameInventory#chest")`, `mpDragPanel(inventory, "GameInventory#inv")`.
//      (GameInventory's own onAfterReflow is not usable: vanilla showInventory
//      restores InventoryContent's saved handler onto it.)
//
// Shared appended functions (reusable through `api()`, e.g. for more
// inventory panels keyed "AllInv#<slot>"):
//   mpDragBegin(obj, follow, key)   on push: a second push on the same object
//       within DOUBLE_S resets it (offset 0 on obj and follow, then the
//       `mpWinBase:<key>` offsets if any; saved); otherwise starts a scene capture (with an onCancel: a capture
//       taken over / stopped by other code drops the drag and saves). Moves set
//       obj's (and follow's) offsets in its parent flow to mouse - start, the
//       mouse clamped to the scene, and shift x/y by the change at once: no
//       getProperties (its needReflow cascades a reflow up every parent flow,
//       the whole HUD for a panel), no storage write. Release / release-outside
//       stops the capture and saves. The capture consumes the drag's mouse
//       events (this checkEvents lets them on to the scene unless cleared:
//       moves hovered the world, the release clicked a town); a release
//       without any move passes on as a click.
//   Windows: on each windowRoot reflow of a modal (draggable) window, default-cursor interactives of the
//       title row (top HEADER_PX of the window) get their onPush wrapped (own
//       handler, then the window's drag push), so a push on the title starts
//       the drag; hover / move stay with them (tooltips work); buttons
//       (cursor: button) stay as they are.
//   mpDragRestore(obj, follow, key) applies the saved offset, if any.
//   Absolute children count as placed by their flow (moved, clamped) on an
//       axis with an align: Flow.hx:1779-1806 adds the offsets there (the chest
//       panel is `position: absolute; align: bottom left; offset-y: -410`).
//   mpDragClamp(obj, key)  for an onAfterReflow: a visible, flow-placed `obj`
//       with a non-zero offset in its parent flow is pushed back so at least
//       KEEP_PX of it stays inside the scene horizontally and its top edge
//       stays within [0, height - KEEP_PX] (header reachable). Position: parent
//       absX/absY (local x/y before its first sync) + obj.x/y; width: the
//       flow's calculatedWidth for it. Covers restore at a smaller resolution
//       and a window resize. A panel (key not null) at its `mpWinBase:<key>`
//       spot (vanilla placement) is left alone; while its parent still has
//       needReflow set (its onAfterReflow then runs inside the parent's
//       reflow, before the parent places it: stale x/y) it only sets its own
//       needReflow, so it reflows (and clamps) on its sync right after; a
//       clamped panel is saved (pinned). Windows pass a null key.
//   mpDragPanel(panel, key)  sets `panel.onAfterReflow = panelLate(panel, key)`:
//       once the panel's dom has no style refresh pending (domkit styles a new
//       element on the next sync; that first pass writes the CSS offsets), its
//       offsets are stored as `mpWinBase:<key>` (the double-push reset target;
//       removed at 0,0), the saved offset is restored, a panel with an
//       InventoryContent gets the row-resize handle and saved size (see
//       "row resize") and onAfterReflow becomes `mpDragClamp(panel, key)` plus
//       the rows kept built. A panel with such a base gets every saved offset
//       pinned as inline dom attributes (offset-x / offset-y): a style refresh
//       (the chest Element's hover) re-applies the CSS rules, which threw the
//       chest back to `offset-y: -410`. A release after a move clears the
//       double-push state, so quick successive drags all drag. It then puts an interactive on its first child with
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
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::{EnumConstruct, RefEnumConstruct, RefGlobal, RefString, ValBool};

const HEADER_PX: f64 = 70.0;
const WIDE: f64 = 0.9;
const KEEP_PX: f64 = 48.0;
const DOUBLE_S: f64 = 0.35;
const LOG_CAP: i32 = 20;
pub(crate) const PREFIX: &str = "mpWinPos:";
const BASE_PREFIX: &str = "mpWinBase:";
pub(crate) const KEY_CHEST: &str = "GameInventory#chest";
pub(crate) const KEY_INV: &str = "GameInventory#inv";
const HEADER_CLASS: &str = "title";
const HEAD_MARK: &str = "mpDragHead";
pub(crate) const SIZE_PREFIX: &str = "mpWinSize:";
/// Inventory grid row pitch (ui.comp.Inventory.INV_SPACING) and the scroll
/// area's height beyond whole rows (vanilla 330 px = 6 rows + 12).
const ROW_PX: i32 = 53;
const ROW_PAD: i32 = 12;
const MIN_ROWS: i32 = 2;
/// The resize handle: square side and colour (ARGB).
const HANDLE_PX: f64 = 14.0;
const HANDLE_ARGB: i32 = 0xA0C8A060u32 as i32;
const S_ERR: &str = "mp: drag: ";
const N_BEGIN: &str = "mpDragBegin";
const N_RESTORE: &str = "mpDragRestore";
const N_CLAMP: &str = "mpDragClamp";
const N_PANEL: &str = "mpDragPanel";
const N_WIN_INSTALL: &str = "mpWinInstall";
/// Functions appended by `api()` (report, save, restore, save base, restore
/// base, rs content, rs apply, rs read, rs move, event, cancel, begin, clamp,
/// panel push, rs push, rs reflow, rs install, panel late, panel, win push,
/// head push, head, win reflow, win install).
#[cfg(test)]
pub(crate) const API_FNS: usize = 24;

/// The shared drag functions other passes call (findexes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DragApi {
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
    cursor_t: RefType,
    ha_t: RefType,
    va_t: RefType,
    /// domkit.CssValue and Properties.setAttribute's result enum.
    cssv_t: RefType,
    attr_res_t: RefType,
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
    h_align: RefField,
    v_align: RefField,
    need_style: RefField,
    p_calc_w: RefField,
    sc_w: RefField,
    sc_h: RefField,
    kind: RefField,
    button: RefField,
    propagate: RefField,
    cursor: RefField,
    /// h2d.Object.name: marks a header interactive whose onPush is wrapped.
    name: RefField,
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
    /// Flow.needReflow: cleared at the end of reflow (Flow.hx:1856), before
    /// onAfterReflow; still set while the flow measures (reflows) its children.
    need_reflow: RefField,
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
    is_of_type: RefFun,
    set_attr: RefFun,
    inter_cls: RefGlobal,
    // enum indexes
    /// CssValue.VInt(i32)
    css_vint: usize,
    ev_push: i32,
    ev_release: i32,
    ev_move: i32,
    ev_release_out: i32,
    modal_none: i32,
    cursor_default: i32,
    dbg_file: usize,
    rs: RsCtx,
}

/// What the row resize of inventory panels needs.
struct RsCtx {
    /// ui.comp.InventoryContent (the scroll area) and its class global.
    cont_t: RefType,
    cont_cls: RefGlobal,
    /// ui.comp.Inventory (the slot grid) and `InventoryContent.getInventory`.
    inv_t: RefType,
    get_inv: RefFun,
    vis_h: RefField,
    base_h: RefField,
    force_update: RefFun,
    /// InventoryContent.contentChanged (needScroll class) / scrollReset.
    content_changed: RefFun,
    scroll_reset: RefFun,
    /// Flow.set_maxHeight and its Null<Int>.
    set_max_h: RefFun,
    nint_t: RefType,
    calc_h: RefField,
    /// `new h2d.Interactive(w, h, parent, shape)`; shape's type.
    inter_ctor: RefFun,
    shape_t: RefType,
    bg_color: RefField,
    /// The FlowAlign.Left / Bottom singletons (globals).
    fa_left: RefGlobal,
    fa_bottom: RefGlobal,
}

/// The global holding the parameterless construct `idx` of enum `t`: the
/// enum init stores each one as `Int k; GetArray; SafeCast r; SetGlobal g = r`.
/// Haxe code compares such values by identity, so a MakeEnum copy would not do.
fn enum_const(code: &Bytecode, t: RefType, idx: i32) -> Result<RefGlobal> {
    for f in &code.functions {
        for w in f.ops.windows(4) {
            if let [Opcode::Int { ptr, .. }, Opcode::GetArray { .. }, Opcode::SafeCast { dst, .. }, Opcode::SetGlobal { global, src }] =
                w
            {
                if dst == src && code.globals[global.0] == t && code.ints[ptr.0] == idx {
                    return Ok(*global);
                }
            }
        }
    }
    bail!("enum type {}: no global for construct {idx}", t.0)
}

/// Base types for rs_ctx: flow, interactive, object, i32, f64, void, h-align, v-align.
struct RsBase {
    flow_t: RefType,
    inter_t: RefType,
    obj_t: RefType,
    i32_t: RefType,
    f64_t: RefType,
    void_t: RefType,
    ha_t: RefType,
    va_t: RefType,
}

fn rs_ctx(code: &Bytecode, c: &RsBase) -> Result<RsCtx> {
    let cont_t = obj_type(code, "ui.comp.InventoryContent")?;
    let inv_t = obj_type(code, "ui.comp.Inventory")?;
    if !is_sub(code, cont_t, c.flow_t) {
        bail!("InventoryContent is not an h2d.Flow");
    }
    let cont_cls = obj(code, cont_t)?
        .global
        .0
        .checked_sub(1)
        .filter(|&g| g < code.globals.len())
        .map(RefGlobal)
        .context("InventoryContent: no class global")?;
    let get_inv = method(code, cont_t, "getInventory")?.findex;
    want_sig(
        code,
        get_inv,
        "InventoryContent.getInventory",
        &[cont_t],
        inv_t,
    )?;
    let vis_h = typed(code, inv_t, "visibleHeight", c.i32_t)?;
    let base_h = typed(code, inv_t, "baseHeight", c.i32_t)?;
    let force_update = method(code, inv_t, "forceUpdate")?.findex;
    want_sig(
        code,
        force_update,
        "Inventory.forceUpdate",
        &[inv_t],
        c.void_t,
    )?;
    let content_changed = method(code, cont_t, "contentChanged")?.findex;
    want_sig(
        code,
        content_changed,
        "InventoryContent.contentChanged",
        &[cont_t, c.obj_t],
        c.void_t,
    )?;
    let scroll_reset = method(code, cont_t, "scrollReset")?.findex;
    want_sig(
        code,
        scroll_reset,
        "InventoryContent.scrollReset",
        &[cont_t],
        c.void_t,
    )?;
    let set_max_h = proto(code, c.flow_t, "set_maxHeight")?;
    let (sa, nint_t) = sig(code, set_max_h)?;
    if sa != [c.flow_t, nint_t] || !matches!(code.types[nint_t.0], Type::Null(t) if t == c.i32_t) {
        bail!("Flow.set_maxHeight: unexpected signature");
    }
    let calc_h = typed(code, c.flow_t, "calculatedHeight", c.f64_t)?;
    let inter_ctor = method(code, c.inter_t, "__constructor__")?.findex;
    let (ia, ir) = sig(code, inter_ctor)?;
    if ia.len() != 5 || ia[..4] != [c.inter_t, c.f64_t, c.f64_t, c.obj_t] || ir != c.void_t {
        bail!("Interactive constructor: unexpected signature");
    }
    let bg_color = typed(code, c.inter_t, "backgroundColor", nint_t)?;
    let construct = |t: RefType, name: &str| -> Result<RefGlobal> {
        enum_const(code, t, enum_index(code, t, name)?)
    };
    Ok(RsCtx {
        cont_t,
        cont_cls,
        inv_t,
        get_inv,
        vis_h,
        base_h,
        force_update,
        content_changed,
        scroll_reset,
        set_max_h,
        nint_t,
        calc_h,
        inter_ctor,
        shape_t: ia[4],
        bg_color,
        fa_left: construct(c.ha_t, "Left")?,
        fa_bottom: construct(c.va_t, "Bottom")?,
    })
}

fn want_sig(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("{what}: unexpected signature");
    }
    Ok(())
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
    // Absolute children with an align are still placed by the flow, offsets included (Flow.hx:1779-1806).
    let (h_align, ha_t) = field(code, fprops_t, "horizontalAlign")?;
    let (v_align, va_t) = field(code, fprops_t, "verticalAlign")?;
    let need_style = typed(code, dk_t, "needStyleRefresh", bool_t)?;
    let p_calc_w = typed(code, fprops_t, "calculatedWidth", i32_t)?;
    let sc_w = typed(code, scene_t, "width", i32_t)?;
    let sc_h = typed(code, scene_t, "height", i32_t)?;
    let (kind, kind_t) = field(code, ev_t, "kind")?;
    let button = typed(code, ev_t, "button", i32_t)?;
    let propagate = typed(code, ev_t, "propagate", bool_t)?;
    let (modal, modal_t) = field(code, win_t, "modal")?;
    let frame_flow = typed(code, win_t, "frameFlow", flow_t)?;
    let window_root = typed(code, win_t, "windowRoot", flow_t)?;
    let (on_push, push_t) = field(code, inter_t, "onPush")?;
    let (cursor, cursor_t) = field(code, inter_t, "cursor")?;
    let name = typed(code, obj_t, "name", str_t)?;
    let cursor_default = enum_index(code, cursor_t, "Default")?;
    let is_of_type = static_fn(code, "$Std", "isOfType")?.findex;
    want_sig(code, is_of_type, "Std.isOfType", &[dyn_t, dyn_t], bool_t)?;
    // HL stores an object's class global 1-based (0 = none).
    let inter_cls = obj(code, inter_t)?
        .global
        .0
        .checked_sub(1)
        .filter(|&g| g < code.globals.len())
        .map(RefGlobal)
        .context("h2d.Interactive: no class global")?;
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
    let need_reflow = typed(code, flow_t, "needReflow", bool_t)?;
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
    let set_attr = method(code, dk_t, "setAttribute")?.findex;
    let (sa, attr_res_t) = sig(code, set_attr)?;
    if sa.len() != 3 || sa[0] != dk_t || sa[1] != str_t {
        bail!("Properties.setAttribute: unexpected signature");
    }
    let cssv_t = sa[2];
    let css_vint = match &code.types[cssv_t.0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|k| s(code, k.name) == "VInt" && k.params == [i32_t])
            .context("CssValue.VInt(Int) not found")?,
        _ => bail!("setAttribute's value is not an enum"),
    };
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

    let ev_push = enum_index(code, kind_t, "EPush")?;
    let ev_release = enum_index(code, kind_t, "ERelease")?;
    let ev_move = enum_index(code, kind_t, "EMove")?;
    let ev_release_out = enum_index(code, kind_t, "EReleaseOutside")?;
    let modal_none = enum_index(code, modal_t, "None")?;
    let rs = rs_ctx(
        code,
        &RsBase {
            flow_t,
            inter_t,
            obj_t,
            i32_t,
            f64_t,
            void_t,
            ha_t,
            va_t,
        },
    )?;
    Ok(Ctx {
        rs,
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
        cursor_t,
        ha_t,
        va_t,
        cssv_t,
        attr_res_t,
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
        h_align,
        v_align,
        need_style,
        p_calc_w,
        sc_w,
        sc_h,
        kind,
        button,
        propagate,
        cursor,
        name,
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
        need_reflow,
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
        is_of_type,
        set_attr,
        inter_cls,
        css_vint,
        ev_push,
        ev_release,
        ev_move,
        ev_release_out,
        modal_none,
        cursor_default,
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
    /// The current drag has moved (its release is consumed).
    moved: RefGlobal,
    logs: RefGlobal,
    prefix: RefGlobal,
    /// "mpWinBase:": a panel's styled (CSS) offsets, what a double push resets to.
    base: RefGlobal,
    err: RefGlobal,
    title: RefGlobal,
    /// "offset-x" / "offset-y": the inline attributes a pin sets.
    attr_x: RefGlobal,
    attr_y: RefGlobal,
    /// The name given to a header interactive once its onPush is wrapped.
    head_mark: RefGlobal,
    /// The capture is a row resize (not a move): its scroll area, start rows,
    /// current rows, max rows, start mouse y; "mpWinSize:".
    rs_on: RefGlobal,
    rs_cont: RefGlobal,
    rs_rows0: RefGlobal,
    rs_rows: RefGlobal,
    rs_max: RefGlobal,
    rs_my0: RefGlobal,
    size: RefGlobal,
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

pub(crate) fn name_fn(code: &mut Bytecode, f: RefFun, name: &str) {
    let n = string_ref(code, name);
    let i = code.functions.iter().position(|x| x.findex == f).unwrap();
    code.functions[i].name = n;
}

pub(crate) fn find_named(code: &Bytecode, name: &str) -> Option<RefFun> {
    code.functions
        .iter()
        .find(|f| f.name != RefString(0) && s(code, f.name) == name)
        .map(|f| f.findex)
}

/// The shared drag functions, appended on first use (found by name afterwards).
pub(crate) fn api(code: &mut Bytecode) -> Result<DragApi> {
    if let (Some(panel), Some(win_install)) =
        (find_named(code, N_PANEL), find_named(code, N_WIN_INSTALL))
    {
        return Ok(DragApi { panel, win_install });
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
        obj: add_global(code, c.obj_t),
        follow: add_global(code, c.obj_t),
        key: add_global(code, c.str_t),
        scene: add_global(code, c.scene_t),
        dx: add_global(code, c.f64_t),
        dy: add_global(code, c.f64_t),
        last: add_global(code, c.obj_t),
        last_t: add_global(code, c.f64_t),
        moved: add_global(code, c.bool_t),
        logs: add_global(code, c.i32_t),
        prefix: str_global(code, c.str_t, PREFIX),
        base: str_global(code, c.str_t, BASE_PREFIX),
        err: str_global(code, c.str_t, S_ERR),
        title: str_global(code, c.str_t, HEADER_CLASS),
        attr_x: str_global(code, c.str_t, "offset-x"),
        attr_y: str_global(code, c.str_t, "offset-y"),
        head_mark: str_global(code, c.str_t, HEAD_MARK),
        rs_on: add_global(code, c.bool_t),
        rs_cont: add_global(code, c.rs.cont_t),
        rs_rows0: add_global(code, c.i32_t),
        rs_rows: add_global(code, c.i32_t),
        rs_max: add_global(code, c.i32_t),
        rs_my0: add_global(code, c.f64_t),
        size: str_global(code, c.str_t, SIZE_PREFIX),
    };
    let dispose_fi = game_dispose_fi(code)?;
    let report = add_report(code, c, &g)?;
    let save = add_save(
        code,
        c,
        g.prefix,
        report,
        Some((g.base, g.attr_x, g.attr_y)),
    )?;
    let restore = add_restore(code, c, g.prefix, report)?;
    let save_base = add_save(code, c, g.base, report, None)?;
    let restore_base = add_restore(code, c, g.base, report)?;
    let rs_content = add_rs_content(code, c)?;
    let rs_apply = add_rs_apply(code, c)?;
    let rs_read = add_rs_read(code, c, &g)?;
    let rs_move = add_rs_move(code, c, &g, report, rs_apply)?;
    let event = add_event(code, c, &g, report, save, rs_move)?;
    let cancel = add_cancel(code, c, &g, report, save)?;
    let begin = add_begin(code, c, &g, report, save, restore_base, event, cancel)?;
    let clamp = add_clamp(code, c, &g, report, save)?;
    let (panel_push, cap_t) = add_panel_push(code, c, begin)?;
    let rs = RsFns {
        content: rs_content,
        apply: rs_apply,
        read: rs_read,
    };
    let rs_push = add_rs_push(code, c, &g, report, begin, rs.content, cap_t)?;
    let rs_reflow = add_rs_reflow(code, c, report, clamp, &rs, cap_t)?;
    let rs_install = add_rs_install(code, c, report, &rs, rs_push, cap_t)?;
    let late = add_panel_late(
        code,
        c,
        report,
        save_base,
        restore,
        save,
        (rs_install, rs_reflow),
        cap_t,
    )?;
    let panel = add_panel(code, c, &g, report, late, panel_push, cap_t)?;
    let win_push = add_win_push(code, c, report, begin)?;
    let (head_push, head_t) = add_head_push(code, c, win_push)?;
    let head = add_head(code, c, &g, head_push, head_t)?;
    let win_reflow = add_win_reflow(code, c, report, clamp, head)?;
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
    // Nothing of a drag (nor the last pushed object, kept for a double push)
    // outlives its game. Last: an error above leaves Game.dispose untouched.
    forget_on_game_dispose(
        code,
        dispose_fi,
        &[g.obj, g.follow, g.scene, g.last, g.rs_cont],
    );
    Ok(DragApi { panel, win_install })
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

/// `full = prefix + key`
fn full_key(a: &mut Asm, c: &Ctx, prefix: RefGlobal, full: Reg, key: Reg) {
    a.op(Opcode::GetGlobal {
        dst: full,
        global: prefix,
    });
    a.op(Opcode::Call2 {
        dst: full,
        fun: c.str_add,
        arg0: full,
        arg1: key,
    });
}

/// The names a pin sets: `(mpWinBase: prefix, "offset-x", "offset-y")`.
type Pin = (RefGlobal, RefGlobal, RefGlobal);

/// `save(obj, key)`: stores obj's offsets in its parent flow (removes the key at 0,0).
///
/// With `pin` (the position save, not the base one): a panel with a CSS offset
/// (a `mpWinBase:<key>` entry: the chest's `offset-y: -410`) also gets its
/// offsets as inline dom attributes (`dom.setAttribute("offset-x"/"offset-y",
/// VInt)`). domkit re-applies every matching CSS rule on each style refresh of
/// the element (CssStyle.applyStyleRec@4064 calls handler.apply for all of
/// them; the chest is a ui.comp.Element whose over / out toggles dom.hover =
/// a refresh, fn@31634 / @31635 -> set_hover@1640), which put `offset-y: -410`
/// back and threw the dragged chest back to its styled spot. Inline values
/// are applied after the rules, so the dragged spot survives a refresh.
fn add_save(
    code: &mut Bytecode,
    c: &Ctx,
    prefix: RefGlobal,
    report: RefFun,
    pin: Option<Pin>,
) -> Result<RefFun> {
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
    let (dm, nm, cv, res) = (r.r(c.dk_t), r.r(c.str_t), r.r(c.cssv_t), r.r(c.attr_res_t));
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
    if let Some((base, nx, ny)) = pin {
        full_key(&mut a, c, base, full, key);
        a.op(Opcode::Null { dst: d });
        a.op(Opcode::Call2 {
            dst: d,
            fun: c.get_ud,
            arg0: full,
            arg1: d,
        });
        a.jmp(Opcode::JNull { reg: d, offset: 0 }, "store");
        a.op(Opcode::Field {
            dst: dm,
            obj,
            field: c.dom,
        });
        a.jmp(Opcode::JNull { reg: dm, offset: 0 }, "store");
        for (name, val) in [(nx, ox), (ny, oy)] {
            a.op(Opcode::GetGlobal {
                dst: nm,
                global: name,
            });
            a.op(Opcode::MakeEnum {
                dst: cv,
                construct: RefEnumConstruct(c.css_vint),
                args: vec![val],
            });
            a.op(Opcode::Call3 {
                dst: res,
                fun: c.set_attr,
                arg0: dm,
                arg1: nm,
                arg2: cv,
            });
        }
        a.label("store");
    }
    full_key(&mut a, c, prefix, full, key);
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
fn add_restore(code: &mut Bytecode, c: &Ctx, prefix: RefGlobal, report: RefFun) -> Result<RefFun> {
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
    full_key(&mut a, c, prefix, full, key);
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

// ---------- row resize (inventory panels) ----------
//
// A small handle (HANDLE_PX square, h2d.Interactive with a backgroundColor)
// sits at a panel's bottom-left corner (absolute child, align left bottom).
// Its push starts the shared drag capture in resize mode (`g.rs_on`): a move
// sets the panel's InventoryContent (the scroll area) to `rows` whole grid
// rows, rows = start + round(mouse dy / ROW_PX), clamped to [MIN_ROWS, as
// many as fit below the panel on screen]: max height = rows * ROW_PX +
// ROW_PAD (Flow.set_maxHeight only; no CSS sets it on
// inventory-content). Width is untouched. Enough grid rows are built
// (Inventory.visibleHeight / baseHeight >= rows, forceUpdate). The three
// panels (chest, inventory, co-op AllInv) are all bottom-anchored (chest:
// absolute align bottom; inventory: in the bottom-aligned .windows flow;
// AllInv: in a bottom-aligned box), so the top would move up: the panel's
// offsetY grows by the added height, the top stays and the panel grows
// downward. The rows are saved as `mpWinSize:<key>` at each change, the
// position (offset) on release as for a move. On install (panel styled) a
// saved size is applied; the panel's reflow handler (clamp, then the rows
// kept built: vanilla showInventory resets visibleHeight to 6) replaces the
// plain clamp.

/// The functions shared by the resize parts.
struct RsFns {
    /// `(panel) -> InventoryContent`: the first direct child that is one, or null.
    content: RefFun,
    /// `(content, rows, full)`: full: viewport height; always: rows built.
    apply: RefFun,
    /// `(key) -> rows` saved for key, 0 if none.
    read: RefFun,
}

fn add_rs_content(code: &mut Bytecode, c: &Ctx) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![c.flow_t]);
    let panel = Reg(0);
    let (ct, ch, n, i, raw, d, co, b, cls) = (
        r.r(c.rs.cont_t),
        r.r(c.arr_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.bool_t),
        r.r(c.dyn_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: ct });
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
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: c.rs.cont_cls,
    });
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
    a.op(Opcode::UnsafeCast { dst: co, src: d });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: co, offset: 0 }, "loop");
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.is_of_type,
        arg0: co,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::UnsafeCast { dst: ct, src: co });
    a.label("out");
    a.op(Opcode::Ret { ret: ct });
    push_fn(
        code,
        vec![c.flow_t],
        c.rs.cont_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

fn add_rs_apply(code: &mut Bytecode, c: &Ctx) -> Result<RefFun> {
    let (k_row, k_pad) = (int_const(code, ROW_PX), int_const(code, ROW_PAD));
    let mut r = Regs(vec![c.rs.cont_t, c.i32_t, c.bool_t]);
    let (ct, rows, full) = (Reg(0), Reg(1), Reg(2));
    let (v, k, px, nb, nr, inv, vh) = (
        r.r(c.void_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.rs.nint_t),
        r.r(c.rs.nint_t),
        r.r(c.rs.inv_t),
        r.r(c.i32_t),
    );
    let mut a = Asm::new();
    a.jmp(
        Opcode::JFalse {
            cond: full,
            offset: 0,
        },
        "grid",
    );
    a.op(Opcode::Int { dst: k, ptr: k_row });
    a.op(Opcode::Mul {
        dst: px,
        a: rows,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: k_pad });
    a.op(Opcode::Add {
        dst: px,
        a: px,
        b: k,
    });
    a.op(Opcode::ToDyn { dst: nb, src: px });
    // maxHeight only, as vanilla: a minHeight on this horizontal,
    // single-line flow is its line height (Flow.hx:1344), so the line, and
    // the content, were exactly the viewport (no scroll range) and the grid
    // sat valign Bottom in it (rows above the header, unreachable).
    a.op(Opcode::Call2 {
        dst: nr,
        fun: c.rs.set_max_h,
        arg0: ct,
        arg1: nb,
    });
    a.label("grid");
    a.op(Opcode::Call1 {
        dst: inv,
        fun: c.rs.get_inv,
        arg0: ct,
    });
    a.jmp(
        Opcode::JNull {
            reg: inv,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::SetField {
        obj: inv,
        field: c.rs.base_h,
        src: rows,
    });
    a.op(Opcode::Field {
        dst: vh,
        obj: inv,
        field: c.rs.vis_h,
    });
    a.jmp(
        Opcode::JSGte {
            a: vh,
            b: rows,
            offset: 0,
        },
        "built",
    );
    a.op(Opcode::SetField {
        obj: inv,
        field: c.rs.vis_h,
        src: rows,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.rs.force_update,
        arg0: inv,
    });
    a.label("built");
    // A new viewport: vanilla's own update for it. contentChanged sets the
    // `needScroll` class (scrollbar shown) from the new maxHeight (only
    // InventoryContent.contentChanged reads it), scrollReset puts the rows
    // back at the top (a scroll position kept from a taller or shorter
    // viewport left them scrolled out of sight).
    a.jmp(
        Opcode::JFalse {
            cond: full,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call2 {
        dst: v,
        fun: c.rs.content_changed,
        arg0: ct,
        arg1: inv,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.rs.scroll_reset,
        arg0: ct,
    });
    a.label("out");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![c.rs.cont_t, c.i32_t, c.bool_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

fn add_rs_read(code: &mut Bytecode, c: &Ctx, g: &Globals) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![c.str_t]);
    let key = Reg(0);
    let (rows, full, d, nd) = (r.r(c.i32_t), r.r(c.str_t), r.r(c.dyn_t), r.r(c.dyn_t));
    let mut a = Asm::new();
    a.op(Opcode::Int { dst: rows, ptr: k0 });
    full_key(&mut a, c, g.size, full, key);
    a.op(Opcode::Null { dst: nd });
    a.op(Opcode::Call2 {
        dst: d,
        fun: c.get_ud,
        arg0: full,
        arg1: nd,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "out");
    a.op(Opcode::SafeCast { dst: rows, src: d });
    a.label("out");
    a.op(Opcode::Ret { ret: rows });
    push_fn(code, vec![c.str_t], c.i32_t, r.0, a.finish(), c.dbg_file)
}

/// `rsMove()`: the capture's move in resize mode (see the section comment).
fn add_rs_move(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    apply: RefFun,
) -> Result<RefFun> {
    let (k0, k_half, k_row, k_min) = (
        int_const(code, 0),
        int_const(code, ROW_PX / 2),
        int_const(code, ROW_PX),
        int_const(code, MIN_ROWS),
    );
    let mut r = Regs(vec![]);
    let (v, exc, obj, p, fl, sc, my, m0) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.scene_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
    );
    let (d, k, z, rows, cur, lim, ct, b) = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.rs.cont_t),
        r.r(c.bool_t),
    );
    let (pr, oy, key, full, dd) = (
        r.r(c.fprops_t),
        r.r(c.i32_t),
        r.r(c.str_t),
        r.r(c.str_t),
        r.r(c.dyn_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.op(Opcode::GetGlobal {
        dst: obj,
        global: g.obj,
    });
    parent_flow(&mut a, c, obj, p, fl);
    a.op(Opcode::GetGlobal {
        dst: sc,
        global: g.scene,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    a.op(Opcode::GetGlobal {
        dst: ct,
        global: g.rs_cont,
    });
    a.jmp(Opcode::JNull { reg: ct, offset: 0 }, "out");
    // rows = rows0 + round((mouseY - y0) / ROW_PX), in [MIN_ROWS, max].
    a.op(Opcode::Call1 {
        dst: my,
        fun: c.mouse_y,
        arg0: sc,
    });
    a.op(Opcode::GetGlobal {
        dst: m0,
        global: g.rs_my0,
    });
    a.op(Opcode::Sub {
        dst: my,
        a: my,
        b: m0,
    });
    a.op(Opcode::ToInt { dst: d, src: my });
    a.op(Opcode::Int { dst: z, ptr: k0 });
    a.op(Opcode::Int {
        dst: k,
        ptr: k_half,
    });
    a.jmp(
        Opcode::JSGte {
            a: d,
            b: z,
            offset: 0,
        },
        "pos",
    );
    a.op(Opcode::Sub { dst: d, a: d, b: k });
    a.jmp(Opcode::JAlways { offset: 0 }, "div");
    a.label("pos");
    a.op(Opcode::Add { dst: d, a: d, b: k });
    a.label("div");
    a.op(Opcode::Int { dst: k, ptr: k_row });
    a.op(Opcode::SDiv { dst: d, a: d, b: k });
    a.op(Opcode::GetGlobal {
        dst: rows,
        global: g.rs_rows0,
    });
    a.op(Opcode::Add {
        dst: rows,
        a: rows,
        b: d,
    });
    a.op(Opcode::GetGlobal {
        dst: lim,
        global: g.rs_max,
    });
    a.jmp(
        Opcode::JSLte {
            a: rows,
            b: lim,
            offset: 0,
        },
        "c1",
    );
    a.op(Opcode::Mov {
        dst: rows,
        src: lim,
    });
    a.label("c1");
    a.op(Opcode::Int {
        dst: lim,
        ptr: k_min,
    });
    a.jmp(
        Opcode::JSGte {
            a: rows,
            b: lim,
            offset: 0,
        },
        "c2",
    );
    a.op(Opcode::Mov {
        dst: rows,
        src: lim,
    });
    a.label("c2");
    a.op(Opcode::GetGlobal {
        dst: cur,
        global: g.rs_rows,
    });
    a.jmp(
        Opcode::JEq {
            a: rows,
            b: cur,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::SetGlobal {
        global: g.rs_rows,
        src: rows,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: apply,
        arg0: ct,
        arg1: rows,
        arg2: b,
    });
    // Bottom-anchored panel: offsetY += the added height keeps its top.
    a.op(Opcode::Sub {
        dst: cur,
        a: rows,
        b: cur,
    });
    a.op(Opcode::Int { dst: k, ptr: k_row });
    a.op(Opcode::Mul {
        dst: cur,
        a: cur,
        b: k,
    });
    get_props(&mut a, c, pr, fl, obj, "out");
    a.op(Opcode::Field {
        dst: oy,
        obj: pr,
        field: c.off_y,
    });
    a.op(Opcode::Add {
        dst: oy,
        a: oy,
        b: cur,
    });
    a.op(Opcode::SetField {
        obj: pr,
        field: c.off_y,
        src: oy,
    });
    a.op(Opcode::GetGlobal {
        dst: key,
        global: g.key,
    });
    full_key(&mut a, c, g.size, full, key);
    a.op(Opcode::ToDyn { dst: dd, src: rows });
    a.op(Opcode::Call2 {
        dst: v,
        fun: c.set_ud,
        arg0: full,
        arg1: dd,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// Handle push `(cap(panel, key), hxd.Event)`: a left push starts the resize.
#[allow(clippy::too_many_arguments)]
fn add_rs_push(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    begin: RefFun,
    content: RefFun,
    cap_t: RefType,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let (f0, f_row) = (float_const(code, 0.0), float_const(code, ROW_PX as f64));
    let mut r = Regs(vec![cap_t, c.ev_t]);
    let (cx, e) = (Reg(0), Reg(1));
    let (v, exc, bt, z, panel, key, ct, sc) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.flow_t),
        r.r(c.str_t),
        r.r(c.rs.cont_t),
        r.r(c.scene_t),
    );
    let (fr, h, room, t, rows, mx, si, nobj, b) = (
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.obj_t),
        r.r(c.bool_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
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
        "out",
    );
    for (dst, i) in [(panel, 0), (key, 1)] {
        a.op(Opcode::EnumField {
            dst,
            value: cx,
            construct: RefEnumConstruct(0),
            field: RefField(i),
        });
    }
    a.jmp(
        Opcode::JNull {
            reg: panel,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: ct,
        fun: content,
        arg0: panel,
    });
    a.jmp(Opcode::JNull { reg: ct, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: sc,
        fun: c.get_scene,
        arg0: panel,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    // Start rows: the viewport's height in whole rows.
    a.op(Opcode::Float {
        dst: fr,
        ptr: f_row,
    });
    a.op(Opcode::Field {
        dst: h,
        obj: ct,
        field: c.rs.calc_h,
    });
    a.op(Opcode::SDiv {
        dst: h,
        a: h,
        b: fr,
    });
    a.op(Opcode::ToInt { dst: rows, src: h });
    // Max: what fits between the panel's bottom and the screen's (no less than now).
    a.op(Opcode::Field {
        dst: si,
        obj: sc,
        field: c.sc_h,
    });
    a.op(Opcode::ToSFloat { dst: room, src: si });
    for fl in [c.abs_y, c.rs.calc_h] {
        a.op(Opcode::Field {
            dst: t,
            obj: panel,
            field: fl,
        });
        a.op(Opcode::Sub {
            dst: room,
            a: room,
            b: t,
        });
    }
    a.op(Opcode::Float { dst: t, ptr: f0 });
    a.jmp(
        Opcode::JSGte {
            a: room,
            b: t,
            offset: 0,
        },
        "room",
    );
    a.op(Opcode::Mov { dst: room, src: t });
    a.label("room");
    a.op(Opcode::SDiv {
        dst: room,
        a: room,
        b: fr,
    });
    a.op(Opcode::ToInt { dst: mx, src: room });
    a.op(Opcode::Add {
        dst: mx,
        a: mx,
        b: rows,
    });
    for (gl, src) in [
        (g.rs_cont, ct),
        (g.rs_rows0, rows),
        (g.rs_rows, rows),
        (g.rs_max, mx),
    ] {
        a.op(Opcode::SetGlobal { global: gl, src });
    }
    a.op(Opcode::Call1 {
        dst: h,
        fun: c.mouse_y,
        arg0: sc,
    });
    a.op(Opcode::SetGlobal {
        global: g.rs_my0,
        src: h,
    });
    // Never a double push (that resets the position): the capture starts.
    a.op(Opcode::Null { dst: nobj });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: nobj,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: begin,
        arg0: panel,
        arg1: nobj,
        arg2: key,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.rs_on,
        src: b,
    });
    guard_close(&mut a, &gd, report);
    push_fn(
        code,
        vec![cap_t, c.ev_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `rsReflow(cap(panel, key))`, the panel's onAfterReflow once styled: the
/// clamp, then a saved size keeps its rows built.
fn add_rs_reflow(
    code: &mut Bytecode,
    c: &Ctx,
    report: RefFun,
    clamp: RefFun,
    rs: &RsFns,
    cap_t: RefType,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![cap_t]);
    let cx = Reg(0);
    let (v, exc, panel, key, rows, z, ct, b) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.flow_t),
        r.r(c.str_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.rs.cont_t),
        r.r(c.bool_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    for (dst, i) in [(panel, 0), (key, 1)] {
        a.op(Opcode::EnumField {
            dst,
            value: cx,
            construct: RefEnumConstruct(0),
            field: RefField(i),
        });
    }
    a.jmp(
        Opcode::JNull {
            reg: panel,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call2 {
        dst: v,
        fun: clamp,
        arg0: panel,
        arg1: key,
    });
    a.op(Opcode::Call1 {
        dst: rows,
        fun: rs.read,
        arg0: key,
    });
    a.op(Opcode::Int { dst: z, ptr: k0 });
    a.jmp(
        Opcode::JSLte {
            a: rows,
            b: z,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: ct,
        fun: rs.content,
        arg0: panel,
    });
    a.jmp(Opcode::JNull { reg: ct, offset: 0 }, "out");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: rs.apply,
        arg0: ct,
        arg1: rows,
        arg2: b,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![cap_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `rsInstall(panel, key)`: saved size applied, resize handle added (panels
/// with an InventoryContent child only).
fn add_rs_install(
    code: &mut Bytecode,
    c: &Ctx,
    report: RefFun,
    rs: &RsFns,
    rs_push: RefFun,
    cap_t: RefType,
) -> Result<RefFun> {
    let (k0, k_argb) = (int_const(code, 0), int_const(code, HANDLE_ARGB));
    let f_side = float_const(code, HANDLE_PX);
    let mut r = Regs(vec![c.flow_t, c.str_t]);
    let (panel, key) = (Reg(0), Reg(1));
    let (v, exc, ct, rows, z, b, it, w) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.rs.cont_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.bool_t),
        r.r(c.inter_t),
        r.r(c.f64_t),
    );
    let (sh, nb, pr, fa, fv, cx, cl) = (
        r.r(c.rs.shape_t),
        r.r(c.rs.nint_t),
        r.r(c.fprops_t),
        r.r(c.ha_t),
        r.r(c.va_t),
        r.r(cap_t),
        r.r(c.push_t),
    );
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
    a.op(Opcode::Call1 {
        dst: ct,
        fun: rs.content,
        arg0: panel,
    });
    a.jmp(Opcode::JNull { reg: ct, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: rows,
        fun: rs.read,
        arg0: key,
    });
    a.op(Opcode::Int { dst: z, ptr: k0 });
    a.jmp(
        Opcode::JSLte {
            a: rows,
            b: z,
            offset: 0,
        },
        "handle",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: rs.apply,
        arg0: ct,
        arg1: rows,
        arg2: b,
    });
    a.label("handle");
    a.op(Opcode::New { dst: it });
    a.op(Opcode::Float {
        dst: w,
        ptr: f_side,
    });
    a.op(Opcode::Null { dst: sh });
    a.op(Opcode::CallN {
        dst: v,
        fun: c.rs.inter_ctor,
        args: vec![it, w, w, panel, sh],
    });
    a.op(Opcode::Int {
        dst: z,
        ptr: k_argb,
    });
    a.op(Opcode::ToDyn { dst: nb, src: z });
    a.op(Opcode::SetField {
        obj: it,
        field: c.rs.bg_color,
        src: nb,
    });
    get_props(&mut a, c, pr, panel, it, "out");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetField {
        obj: pr,
        field: c.is_abs,
        src: b,
    });
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: c.rs.fa_left,
    });
    a.op(Opcode::SetField {
        obj: pr,
        field: c.h_align,
        src: fa,
    });
    a.op(Opcode::GetGlobal {
        dst: fv,
        global: c.rs.fa_bottom,
    });
    a.op(Opcode::SetField {
        obj: pr,
        field: c.v_align,
        src: fv,
    });
    a.op(Opcode::MakeEnum {
        dst: cx,
        construct: RefEnumConstruct(0),
        args: vec![panel, key],
    });
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: rs_push,
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

/// Registers of `forget_drag`.
struct DropRegs {
    obj: Reg,
    cont: Reg,
    off: Reg,
    sc: Reg,
    nsc: Reg,
}

fn drop_regs(r: &mut Regs, c: &Ctx) -> DropRegs {
    DropRegs {
        obj: r.r(c.obj_t),
        cont: r.r(c.rs.cont_t),
        off: r.r(c.bool_t),
        sc: r.r(c.scene_t),
        nsc: r.r(c.scene_t),
    }
}

/// The drag is over: every global that held it lets go (object, follower,
/// scene, resize area) and `d.sc` is the scene it had (or null).
fn forget_drag(a: &mut Asm, g: &Globals, d: &DropRegs) {
    a.op(Opcode::Null { dst: d.obj });
    a.op(Opcode::Null { dst: d.cont });
    a.op(Opcode::Null { dst: d.nsc });
    a.op(Opcode::Bool {
        dst: d.off,
        value: ValBool(false),
    });
    a.op(Opcode::GetGlobal {
        dst: d.sc,
        global: g.scene,
    });
    for (global, src) in [
        (g.obj, d.obj),
        (g.follow, d.obj),
        (g.scene, d.nsc),
        (g.rs_cont, d.cont),
        (g.rs_on, d.off),
    ] {
        a.op(Opcode::SetGlobal { global, src });
    }
}

/// `stop()`: forget_drag, then `if (scene != null) scene.stopCapture();` (its
/// onCancel finds no drag any more).
fn stop_drag(a: &mut Asm, c: &Ctx, g: &Globals, v: Reg, d: &DropRegs, end: &'static str) {
    forget_drag(a, g, d);
    a.jmp(
        Opcode::JNull {
            reg: d.sc,
            offset: 0,
        },
        end,
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.stop_capture,
        arg0: d.sc,
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

/// `pr = fl.properties[fl.getChildIndex(obj)]`, read without `getProperties`:
/// that one sets `needReflow`, and `set_needReflow` (Flow.hx:696) calls
/// `parentContainer.contentChanged`, so every ancestor flow (the whole HUD for a
/// panel) would reflow. Jumps to `out` when obj has no properties in `fl`.
#[allow(clippy::too_many_arguments)]
fn props_raw(
    a: &mut Asm,
    c: &Ctx,
    fl: Reg,
    obj: Reg,
    t: (Reg, Reg, Reg, Reg, Reg, Reg),
    pr: Reg,
    out: &'static str,
) {
    let (idx, n, z, ps, raw, d) = t;
    a.op(Opcode::Call2 {
        dst: idx,
        fun: c.child_index,
        arg0: fl,
        arg1: obj,
    });
    a.op(Opcode::Field {
        dst: ps,
        obj: fl,
        field: c.properties,
    });
    a.jmp(Opcode::JNull { reg: ps, offset: 0 }, out);
    a.op(Opcode::Field {
        dst: n,
        obj: ps,
        field: c.arr_len,
    });
    a.jmp(
        Opcode::JSLt {
            a: idx,
            b: z,
            offset: 0,
        },
        out,
    );
    a.jmp(
        Opcode::JSGte {
            a: idx,
            b: n,
            offset: 0,
        },
        out,
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: ps,
        field: c.arr_arr,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: idx,
    });
    a.op(Opcode::UnsafeCast { dst: pr, src: d });
    a.jmp(Opcode::JNull { reg: pr, offset: 0 }, out);
}

/// Moves `obj` to the offsets (ox, oy) right away: the flow offsets are set
/// (a later reflow keeps the spot) and obj.x/y shift by the change, so the
/// next sync draws it there without any reflow. Per axis, an absolute child
/// is placed by the flow (offset included) only when it has an align on that
/// axis (Flow.hx:1779-1806: the chest panel, `position: absolute; align:
/// bottom left`); an axis the flow does not place is left alone.
#[allow(clippy::too_many_arguments)]
fn shift(
    a: &mut Asm,
    c: &Ctx,
    pr: Reg,
    obj: Reg,
    (ox, oy): (Reg, Reg),
    (old, df, t, b): (Reg, Reg, Reg, Reg),
    (ha, va): (Reg, Reg),
    labels: [&'static str; 4],
) {
    let [dx, sx, dy, sy] = labels;
    for (off, pos, o, al, align, go, skip) in [
        (c.off_x, c.x, ox, ha, c.h_align, dx, sx),
        (c.off_y, c.y, oy, va, c.v_align, dy, sy),
    ] {
        a.op(Opcode::Field {
            dst: b,
            obj: pr,
            field: c.is_abs,
        });
        a.jmp(Opcode::JFalse { cond: b, offset: 0 }, go);
        a.op(Opcode::Field {
            dst: al,
            obj: pr,
            field: align,
        });
        a.jmp(Opcode::JNull { reg: al, offset: 0 }, skip);
        a.label(go);
        a.op(Opcode::Field {
            dst: old,
            obj: pr,
            field: off,
        });
        a.op(Opcode::SetField {
            obj: pr,
            field: off,
            src: o,
        });
        a.op(Opcode::Sub {
            dst: old,
            a: o,
            b: old,
        });
        a.op(Opcode::ToSFloat { dst: df, src: old });
        a.op(Opcode::Field {
            dst: t,
            obj,
            field: pos,
        });
        a.op(Opcode::Add {
            dst: t,
            a: t,
            b: df,
        });
        a.op(Opcode::SetField {
            obj,
            field: pos,
            src: t,
        });
        a.label(skip);
    }
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetField {
        obj,
        field: c.pos_changed,
        src: b,
    });
}

/// The scene capture callback `(hxd.Event) -> void`.
///
/// This SceneEvents.checkEvents sets `e.propagate = true` before calling the
/// capture (SceneEvents.hx:358) and emits the event to the scene afterwards
/// unless the callback clears it. Mouse events of a drag are consumed here:
/// otherwise every move also reaches what lies under the cursor (world hover:
/// raycast, accost cursor, `netOverEntity` RPC on a client) and the release
/// lands on the world / a town as a click of its own.
fn add_event(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
    rs_move: RefFun,
) -> Result<RefFun> {
    let (k0, k_push, k_move, k_rel, k_relo) = (
        int_const(code, 0),
        int_const(code, c.ev_push),
        int_const(code, c.ev_move),
        int_const(code, c.ev_release),
        int_const(code, c.ev_release_out),
    );
    let f0 = float_const(code, 0.0);
    let mut r = Regs(vec![c.ev_t]);
    let dr = drop_regs(&mut r, c);
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
    let (key, p, fl, pr, fp, fo, b) = (
        r.r(c.str_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.fprops_t),
        r.r(c.fprops_t),
        r.r(c.obj_t),
        r.r(c.bool_t),
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
    let raw = (
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.arr_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
    );
    let tmp = (r.r(c.i32_t), r.r(c.f64_t), r.r(c.f64_t), b);
    let al = (r.r(c.ha_t), r.r(c.va_t));
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Field {
        dst: kd,
        obj: e,
        field: c.kind,
    });
    a.jmp(Opcode::JNull { reg: kd, offset: 0 }, "out");
    a.op(Opcode::EnumIndex { dst: ki, value: kd });
    // Mouse buttons and moves stay with the drag; keys, wheel etc. pass on.
    for (kc, to) in [
        (k_push, "eat"),
        (k_move, "eat"),
        (k_rel, "eat"),
        (k_relo, "eat"),
    ] {
        a.op(Opcode::Int { dst: k, ptr: kc });
        a.jmp(
            Opcode::JEq {
                a: ki,
                b: k,
                offset: 0,
            },
            to,
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("eat");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetField {
        obj: e,
        field: c.propagate,
        src: b,
    });
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
    for (kc, to) in [(k_move, "move"), (k_rel, "release"), (k_relo, "release")] {
        a.op(Opcode::Int { dst: k, ptr: kc });
        a.jmp(
            Opcode::JEq {
                a: ki,
                b: k,
                offset: 0,
            },
            to,
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    // A capture without a drag object (should not happen): just stop it.
    a.label("stop");
    stop_drag(&mut a, c, g, v, &dr, "out");
    a.jmp(Opcode::JAlways { offset: 0 }, "out");

    a.label("release");
    // A push without a move is a plain click: its release goes on to the scene.
    // A drag that moved is no first click of a double push: the next push
    // (even within DOUBLE_S) starts a new drag instead of resetting.
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.moved,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "click");
    a.op(Opcode::Null { dst: nobj });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: nobj,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "rel");
    a.label("click");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetField {
        obj: e,
        field: c.propagate,
        src: b,
    });
    a.label("rel");
    stop_drag(&mut a, c, g, v, &dr, "save");
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

    // Move: position applied now; no reflow, nothing stored (save is on release).
    a.label("move");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.moved,
        src: b,
    });
    // A row resize (handle) changes the size, not the position.
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.rs_on,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "pos");
    a.op(Opcode::Call0 {
        dst: v,
        fun: rs_move,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("pos");
    a.op(Opcode::GetGlobal {
        dst: sc,
        global: g.scene,
    });
    a.jmp(Opcode::JNull { reg: sc, offset: 0 }, "out");
    parent_flow(&mut a, c, obj, p, fl);
    a.op(Opcode::Int {
        dst: raw.2,
        ptr: k0,
    });
    props_raw(&mut a, c, fl, obj, raw, pr, "out");
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
    shift(
        &mut a,
        c,
        pr,
        obj,
        (ox, oy),
        tmp,
        al,
        ["dx0", "sx0", "dy0", "sy0"],
    );
    a.label("follow");
    a.op(Opcode::GetGlobal {
        dst: fo,
        global: g.follow,
    });
    a.jmp(Opcode::JNull { reg: fo, offset: 0 }, "out");
    props_raw(&mut a, c, fl, fo, raw, fp, "out");
    shift(
        &mut a,
        c,
        fp,
        fo,
        (ox, oy),
        tmp,
        al,
        ["dx1", "sx1", "dy1", "sy1"],
    );

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
    stop_drag(&mut a, c, g, v, &dr, "end");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![c.ev_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// The capture's `onCancel` `() -> void`: SceneEvents calls it when another
/// startCapture replaces ours or anyone calls stopCapture (GameUI fn@41254,
/// MiniMap, ...). The drag state is dropped and the reached offset saved, so
/// a lost capture never leaves a drag half open. Our own release clears
/// `g.obj` before stopCapture, so this is then a no-op.
fn add_cancel(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![]);
    let (v, exc, obj, key) = (r.r(c.void_t), r.r(c.dyn_t), r.r(c.obj_t), r.r(c.str_t));
    let dr = drop_regs(&mut r, c);
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    a.op(Opcode::GetGlobal {
        dst: obj,
        global: g.obj,
    });
    a.jmp(
        Opcode::JNull {
            reg: obj,
            offset: 0,
        },
        "out",
    );
    forget_drag(&mut a, g, &dr);
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
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `begin(obj, follow, key)`: double push resets, a single push starts the drag.
fn add_begin(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
    restore_base: RefFun,
    event: RefFun,
    cancel: RefFun,
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
    let (cb, cn, ni, mv) = (r.r(c.push_t), r.r(c.cancel_t), r.r(c.nint_t), r.r(c.bool_t));
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

    // Double push: back to the layout position (the styled offsets a panel's
    // CSS gives it, else 0,0), saved key reset.
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
    a.op(Opcode::Call3 {
        dst: v,
        fun: restore_base,
        arg0: obj,
        arg1: follow,
        arg2: key,
    });
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
    // Capture first: a capture still open is cancelled by it (our cancel saves
    // and clears the old drag) before the globals below name the new one.
    a.op(Opcode::StaticClosure {
        dst: cb,
        fun: event,
    });
    a.op(Opcode::StaticClosure {
        dst: cn,
        fun: cancel,
    });
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Call4 {
        dst: v,
        fun: c.start_capture,
        arg0: sc,
        arg1: cb,
        arg2: cn,
        arg3: ni,
    });
    a.op(Opcode::Bool {
        dst: mv,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.moved,
        src: mv,
    });
    // A move drag (the resize push sets it again after this).
    a.op(Opcode::SetGlobal {
        global: g.rs_on,
        src: mv,
    });
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

/// `clamp(obj, key)`: keeps a dragged `obj` (child of a flow) on screen (see the header).
///
/// With a `key` (panels; null for windows): nothing at the panel's styled spot
/// (`mpWinBase:<key>`, vanilla placement); while the parent flow is mid-reflow
/// (its needReflow still set; a panel's onAfterReflow runs inside the parent's
/// reflow when the parent measures it, Flow.hx:1755, before it places it,
/// Flow.hx:1804-1806: stale x/y) only the panel's needReflow is set, so it
/// clamps on its own reflow right after; a clamped spot is saved (and so
/// pinned over the CSS offset).
fn add_clamp(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    save: RefFun,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let (k16, kh, km) = (
        int_const(code, 16),
        int_const(code, 32768),
        int_const(code, 65535),
    );
    let (f0, f_keep) = (float_const(code, 0.0), float_const(code, KEEP_PX));
    let mut r = Regs(vec![c.flow_t, c.str_t]);
    let (obj, key) = (Reg(0), Reg(1));
    let (full, bd, bi, pk, k) = (
        r.r(c.str_t),
        r.r(c.dyn_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
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
    let (ha, va) = (r.r(c.ha_t), r.r(c.va_t));
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
    a.jmp(Opcode::JNull { reg: fl, offset: 0 }, "out");
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
    // Absolute: only with an align does the flow place it (offsets included).
    fld(&mut a, b, pr, c.is_abs);
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "placed");
    fld(&mut a, ha, pr, c.h_align);
    a.jmp(Opcode::JNotNull { reg: ha, offset: 0 }, "placed");
    fld(&mut a, va, pr, c.v_align);
    a.jmp(Opcode::JNull { reg: va, offset: 0 }, "out");
    a.label("placed");
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
    // A panel at its styled spot is where vanilla puts it: left alone.
    a.jmp(Opcode::JNull { reg: key, offset: 0 }, "pos");
    full_key(&mut a, c, g.base, full, key);
    a.op(Opcode::Null { dst: bd });
    a.op(Opcode::Call2 {
        dst: bd,
        fun: c.get_ud,
        arg0: full,
        arg1: bd,
    });
    a.jmp(Opcode::JNull { reg: bd, offset: 0 }, "defer");
    a.op(Opcode::SafeCast { dst: bi, src: bd });
    a.op(Opcode::Int { dst: k, ptr: kh });
    a.op(Opcode::Add {
        dst: pk,
        a: ox,
        b: k,
    });
    a.op(Opcode::Add {
        dst: nx,
        a: oy,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: k16 });
    a.op(Opcode::Shl {
        dst: pk,
        a: pk,
        b: k,
    });
    a.op(Opcode::Int { dst: k, ptr: km });
    a.op(Opcode::And {
        dst: nx,
        a: nx,
        b: k,
    });
    a.op(Opcode::Or {
        dst: pk,
        a: pk,
        b: nx,
    });
    a.jmp(
        Opcode::JEq {
            a: pk,
            b: bi,
            offset: 0,
        },
        "out",
    );
    // A panel whose parent is still to place it (stale x/y): clamped on its
    // own reflow right after, in the same frame (the parent's sync reflows,
    // then syncs its children; set_needReflow does not reach the parent).
    a.label("defer");
    fld(&mut a, b, fl, c.need_reflow);
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "pos");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.set_need_reflow,
        arg0: obj,
        arg1: b,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("pos");
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
    // Kept: a style refresh (the chest's hover) re-applies the CSS offsets.
    a.jmp(Opcode::JNull { reg: key, offset: 0 }, "out");
    a.op(Opcode::Call2 {
        dst: v,
        fun: save,
        arg0: obj,
        arg1: key,
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

/// `panelLate(cap(panel, key))`, the panel's onAfterReflow until its style is
/// applied: domkit styles a new element later (Properties.hx:106 puts it on
/// the dirty list; CssStyle.hx:414-418 applies it on the next style sync), and
/// that first pass sets the CSS offsets (the chest panel: `offset-y: -410`),
/// overwriting a restore done in the constructor. So, once `panel.dom` has no
/// style refresh pending: the styled offsets are kept as the panel's base (the
/// double-push reset target), the saved offset is restored and saved (pinned
/// over the CSS offset), and onAfterReflow becomes the plain clamp.
fn add_panel_late(
    code: &mut Bytecode,
    c: &Ctx,
    report: RefFun,
    save_base: RefFun,
    restore: RefFun,
    save: RefFun,
    (rs_install, rs_reflow): (RefFun, RefFun),
    cap_t: RefType,
) -> Result<RefFun> {
    let mut r = Regs(vec![cap_t]);
    let cx = Reg(0);
    let (v, exc, b, nf, panel, key, dm, rc) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.bool_t),
        r.r(c.obj_t),
        r.r(c.flow_t),
        r.r(c.str_t),
        r.r(c.dk_t),
        r.r(c.reflow_t),
    );
    let gd = Guard { exc, v };
    let mut a = Asm::new();
    guard_open(&mut a, &gd);
    for (dst, i) in [(panel, 0), (key, 1)] {
        a.op(Opcode::EnumField {
            dst,
            value: cx,
            construct: RefEnumConstruct(0),
            field: RefField(i),
        });
    }
    a.jmp(
        Opcode::JNull {
            reg: panel,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: dm,
        obj: panel,
        field: c.dom,
    });
    a.jmp(Opcode::JNull { reg: dm, offset: 0 }, "styled");
    a.op(Opcode::Field {
        dst: b,
        obj: dm,
        field: c.need_style,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "out");
    a.label("styled");
    a.op(Opcode::Call2 {
        dst: v,
        fun: save_base,
        arg0: panel,
        arg1: key,
    });
    a.op(Opcode::Null { dst: nf });
    a.op(Opcode::Call3 {
        dst: v,
        fun: restore,
        arg0: panel,
        arg1: nf,
        arg2: key,
    });
    // Saved again: pins the restored spot over the CSS offset (see add_save).
    a.op(Opcode::Call2 {
        dst: v,
        fun: save,
        arg0: panel,
        arg1: key,
    });
    // Row resize: handle + saved size; the reflow handler clamps and keeps the rows built.
    a.op(Opcode::Call2 {
        dst: v,
        fun: rs_install,
        arg0: panel,
        arg1: key,
    });
    a.op(Opcode::InstanceClosure {
        dst: rc,
        fun: rs_reflow,
        obj: cx,
    });
    a.op(Opcode::SetField {
        obj: panel,
        field: c.on_after_reflow,
        src: rc,
    });
    guard_close(&mut a, &gd, report);
    push_fn(code, vec![cap_t], c.void_t, r.0, a.finish(), c.dbg_file)
}

/// `panel(panel, key)`: onAfterReflow = panelLate (run once now: restore at
/// once if already styled), then a drag interactive on the "title" header child.
fn add_panel(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    report: RefFun,
    late: RefFun,
    panel_push: RefFun,
    cap_t: RefType,
) -> Result<RefFun> {
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![c.flow_t, c.str_t]);
    let (panel, key) = (Reg(0), Reg(1));
    let (v, exc, b, cls, ch, raw, d) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.bool_t),
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
    a.op(Opcode::MakeEnum {
        dst: cx,
        construct: RefEnumConstruct(0),
        args: vec![panel, key],
    });
    a.op(Opcode::InstanceClosure {
        dst: rc,
        fun: late,
        obj: cx,
    });
    a.op(Opcode::SetField {
        obj: panel,
        field: c.on_after_reflow,
        src: rc,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: late,
        arg0: cx,
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

/// Header push wrapper `(cap(orig onPush, win), hxd.Event) -> void` and its
/// capture enum type: the element's own onPush, then `winPush(win, e)` (which
/// starts the drag only for a left push in the header band of a modal window).
/// Not trapped itself: the element's handler is vanilla code; winPush is trapped.
fn add_head_push(code: &mut Bytecode, c: &Ctx, win_push: RefFun) -> Result<(RefFun, RefType)> {
    code.types.push(Type::Enum {
        name: RefString(0),
        global: RefGlobal(0),
        constructs: vec![EnumConstruct {
            name: RefString(0),
            params: vec![c.push_t, c.win_t],
        }],
    });
    let cap_t = RefType(code.types.len() - 1);
    let mut r = Regs(vec![cap_t, c.ev_t]);
    let (cx, e) = (Reg(0), Reg(1));
    let (v, orig, win) = (r.r(c.void_t), r.r(c.push_t), r.r(c.win_t));
    let mut a = Asm::new();
    for (dst, i) in [(orig, 0), (win, 1)] {
        a.op(Opcode::EnumField {
            dst,
            value: cx,
            construct: RefEnumConstruct(0),
            field: RefField(i),
        });
    }
    a.jmp(
        Opcode::JNull {
            reg: orig,
            offset: 0,
        },
        "drag",
    );
    a.op(Opcode::CallClosure {
        dst: v,
        fun: orig,
        args: vec![e],
    });
    a.label("drag");
    a.op(Opcode::Call2 {
        dst: v,
        fun: win_push,
        arg0: win,
        arg1: e,
    });
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

/// `head(o, rel, win)`: pushes on header elements also start the window drag.
///
/// A window's title row (place name, icon, text with a tooltip, ...) often
/// has interactives of its own; they keep the push, so the window's
/// interactive (the drag start) only saw the thin margin above them. Every
/// h2d.Interactive below `o` whose top (`rel` + local y, relative to the
/// window) is within HEADER_PX and whose cursor is Default (not a button: X,
/// tabs, ... use `cursor: button`) gets its onPush wrapped (`headPush`: its
/// own handler, then `winPush(win, e)`) and is named HEAD_MARK (an unnamed
/// one only: the name marks it as done, so a later reflow never wraps twice).
/// Only the push changes: over / move / out stay with the element, so its
/// tooltip keeps working (propagateEvents, the earlier way, handed every
/// event on to the window interactive below, which took the hover: the unit
/// sheet's effect icons lost their tooltips). The window's own interactive is
/// left alone. Subtrees starting below the band are not walked. Runs from the
/// window's reflow (content built after init is covered); exceptions reach
/// the caller's trap.
fn add_head(
    code: &mut Bytecode,
    c: &Ctx,
    g: &Globals,
    head_push: RefFun,
    head_t: RefType,
) -> Result<RefFun> {
    let me = next_findex(code)?;
    let k0 = int_const(code, 0);
    let k_def = int_const(code, c.cursor_default);
    let f_head = float_const(code, HEADER_PX);
    let mut r = Regs(vec![c.obj_t, c.f64_t, c.win_t]);
    let (o, rel, win) = (Reg(0), Reg(1), Reg(2));
    let (skip, nm, orig, hc, clo) = (
        r.r(c.inter_t),
        r.r(c.str_t),
        r.r(c.push_t),
        r.r(head_t),
        r.r(c.push_t),
    );
    let (v, ch, n, i, raw, d, co, b) = (
        r.r(c.void_t),
        r.r(c.arr_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
        r.r(c.raw_t),
        r.r(c.dyn_t),
        r.r(c.obj_t),
        r.r(c.bool_t),
    );
    let (y, lim, cls, it, cur, ci, k) = (
        r.r(c.f64_t),
        r.r(c.f64_t),
        r.r(c.dyn_t),
        r.r(c.inter_t),
        r.r(c.cursor_t),
        r.r(c.i32_t),
        r.r(c.i32_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: skip,
        obj: win,
        field: c.interactive,
    });
    a.op(Opcode::Field {
        dst: ch,
        obj: o,
        field: c.children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: n,
        obj: ch,
        field: c.arr_len,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: f_head,
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
    a.op(Opcode::UnsafeCast { dst: co, src: d });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: co, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: b,
        obj: co,
        field: c.visible,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: y,
        obj: co,
        field: c.y,
    });
    a.op(Opcode::Add {
        dst: y,
        a: rel,
        b: y,
    });
    a.jmp(
        Opcode::JSGte {
            a: y,
            b: lim,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: c.inter_cls,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: c.is_of_type,
        arg0: co,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "down");
    a.op(Opcode::UnsafeCast { dst: it, src: co });
    a.jmp(
        Opcode::JEq {
            a: it,
            b: skip,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: cur,
        obj: it,
        field: c.cursor,
    });
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "down",
    );
    a.op(Opcode::EnumIndex {
        dst: ci,
        value: cur,
    });
    a.op(Opcode::Int { dst: k, ptr: k_def });
    a.jmp(
        Opcode::JNotEq {
            a: ci,
            b: k,
            offset: 0,
        },
        "loop",
    );
    // Unnamed only (the name marks it wrapped): onPush = headPush(orig, win).
    a.op(Opcode::Field {
        dst: nm,
        obj: it,
        field: c.name,
    });
    a.jmp(Opcode::JNotNull { reg: nm, offset: 0 }, "down");
    a.op(Opcode::GetGlobal {
        dst: nm,
        global: g.head_mark,
    });
    a.op(Opcode::SetField {
        obj: it,
        field: c.name,
        src: nm,
    });
    a.op(Opcode::Field {
        dst: orig,
        obj: it,
        field: c.on_push,
    });
    a.op(Opcode::MakeEnum {
        dst: hc,
        construct: RefEnumConstruct(0),
        args: vec![orig, win],
    });
    a.op(Opcode::InstanceClosure {
        dst: clo,
        fun: head_push,
        obj: hc,
    });
    a.op(Opcode::SetField {
        obj: it,
        field: c.on_push,
        src: clo,
    });
    a.label("down");
    a.op(Opcode::Call3 {
        dst: v,
        fun: me,
        arg0: co,
        arg1: y,
        arg2: win,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("out");
    a.op(Opcode::Ret { ret: v });
    let f = push_fn(
        code,
        vec![c.obj_t, c.f64_t, c.win_t],
        c.void_t,
        r.0,
        a.finish(),
        c.dbg_file,
    )?;
    debug_assert_eq!(f, me);
    Ok(f)
}

/// `winReflow(win)`: clamp windowRoot's children, then frameFlow follows the window.
fn add_win_reflow(
    code: &mut Bytecode,
    c: &Ctx,
    report: RefFun,
    clamp: RefFun,
    head: RefFun,
) -> Result<RefFun> {
    let f0 = float_const(code, 0.0);
    let k_none = int_const(code, c.modal_none);
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
    // No key: a window has no styled spot to keep nor a pin.
    let ns = r.r(c.str_t);
    a.op(Opcode::Null { dst: ns });
    a.op(Opcode::Call2 {
        dst: v,
        fun: clamp,
        arg0: win,
        arg1: ns,
    });
    // Title row: pushes on its non-button elements reach the drag. Only for
    // windows that can be dragged (modal): the HUD (GameUI is a ui.Window,
    // modal None) keeps its pushes where they were.
    let (zf, md, mi, kn) = (r.r(c.f64_t), r.r(c.modal_t), r.r(c.i32_t), r.r(c.i32_t));
    a.op(Opcode::Field {
        dst: md,
        obj: win,
        field: c.modal,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "nohead");
    a.op(Opcode::EnumIndex { dst: mi, value: md });
    a.op(Opcode::Int {
        dst: kn,
        ptr: k_none,
    });
    a.jmp(
        Opcode::JEq {
            a: mi,
            b: kn,
            offset: 0,
        },
        "nohead",
    );
    a.op(Opcode::Float { dst: zf, ptr: f0 });
    a.op(Opcode::Call3 {
        dst: v,
        fun: head,
        arg0: win,
        arg1: zf,
        arg2: win,
    });
    a.label("nohead");
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
/// The window goes to getClass's Dyn argument as is: a ToDyn box would carry
/// the register's static type (ui.Window), so every window shared one key.
fn class_key(a: &mut Asm, c: &Ctx, win: Reg, cl: Reg, name: Reg) {
    a.op(Opcode::Call1 {
        dst: cl,
        fun: c.get_class,
        arg0: win,
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
    let (cl, name, ff) = (r.r(c.cls_t), r.r(c.str_t), r.r(c.flow_t));
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
    class_key(&mut a, c, win, cl, name);
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
    let (v, exc, it, pc, cl, name, ff, root, rc) = (
        r.r(c.void_t),
        r.r(c.dyn_t),
        r.r(c.inter_t),
        r.r(c.push_t),
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
    class_key(&mut a, c, win, cl, name);
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
    let fi = fun_index(code, init)?;
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
    /// h2d.Object.addChild (Flow.addChildAt keeps the child's FlowProperties).
    add_child: RefFun,
}

fn panel_plan(code: &Bytecode) -> Result<PanelPlan> {
    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let ctor = method(code, gi_t, "__constructor__")?;
    let fi = fun_index(code, ctor.findex)?;
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
    let add_child = proto(code, obj_t, "addChild")?;
    Ok(PanelPlan {
        fi,
        ret,
        chest,
        inv,
        void_reg,
        add_child,
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
            Opcode::Call2 {
                dst: v,
                fun: p.add_child,
                arg0: Reg(0),
                arg1: e1,
            },
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

const PANEL_OPS: usize = 8;

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
        crate::skipped(format!("window drag (windows) skipped: {e:#}"));
    }
    if let Err(e) = &pp {
        crate::skipped(format!("window drag (inventory panels) skipped: {e:#}"));
    }
    if wp.is_err() && pp.is_err() {
        return;
    }
    let c = match ctx(code) {
        Ok(c) => c,
        Err(e) => {
            crate::skipped(format!("window drag skipped: {e:#}"));
            return;
        }
    };
    let api = match api(code) {
        Ok(a) => a,
        Err(e) => {
            crate::skipped(format!("window drag skipped: {e:#}"));
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
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted, write};
    use crate::asm::testutil::{same, traps_ok};

    /// Sites found, only Window.init and the GameInventory constructor change,
    /// API_FNS functions are appended, every new op type-checks, a second pass
    /// changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let wp = win_plan(&orig).expect("windows plan");
        let pp = panel_plan(&orig).expect("panels plan");
        let mut code = read(&image);
        patch_window_drag(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + API_FNS);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        // the drag state goes with the game: Game.dispose clears it
        let gd = crate::game_dispose_fi(&orig).unwrap();
        for i in 0..n {
            let want = i == wp.fi || i == pp.fi || i == gd;
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
            panel: back.functions[n + 18].findex,
            win_install: back.functions[n + 23].findex,
        };
        assert!(
            matches!(b.ops[end], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == api.win_install)
        );
        check_types(&back, b, end..end + 2);
        check_flow(b);

        // P: eight ops in front of the final Ret.
        let (a, b) = (&orig.functions[pp.fi], &back.functions[pp.fi]);
        shifted(a, b, pp.ret, PANEL_OPS);
        check_types(&back, b, pp.ret..pp.ret + PANEL_OPS);
        check_flow(b);
        let calls: Vec<RefFun> = b.ops[pp.ret..pp.ret + PANEL_OPS]
            .iter()
            .filter_map(|o| match o {
                Opcode::Call2 { fun, .. } => Some(*fun),
                _ => None,
            })
            .collect();
        assert_eq!(calls, [pp.add_child, api.panel, api.panel]);
        assert!(matches!(b.ops[pp.ret], Opcode::GetThis { field, .. } if field == pp.chest.0));
        assert!(matches!(b.ops[pp.ret + 1], Opcode::Call2 { arg0: Reg(0), .. }));
        let keys: Vec<&str> = b.ops[pp.ret..pp.ret + PANEL_OPS]
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
        assert_eq!(traps, 17);

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
        let Some(image) = game() else { return };
        let mut code = read(&image);
        let n = code.functions.len();
        let a = api(&mut code).expect("api");
        assert_eq!(code.functions.len(), n + API_FNS);
        let b = api(&mut code).expect("api again");
        assert_eq!(a, b);
        assert_eq!(code.functions.len(), n + API_FNS);
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
        let named = |n: &str| find_named(&code, n).expect("named");
        assert_eq!(
            sig_of(named(N_BEGIN)).0,
            ["h2d.Object", "h2d.Object", "String"]
        );
        assert_eq!(
            sig_of(named(N_RESTORE)).0,
            ["h2d.Object", "h2d.Object", "String"]
        );
        assert_eq!(sig_of(named(N_CLAMP)).0, ["h2d.Flow", "String"]);
        assert_eq!(sig_of(a.panel).0, ["h2d.Flow", "String"]);
    }

    /// A jump straight to the constructor's Ret (another pass's early exit, as
    /// chest_buttons adds) lands on the panel install, not past it.
    #[test]
    fn early_exits_reach_install() {
        let Some(image) = game() else { return };
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
        let Some(image) = game() else { return };
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
            orig.functions[pp.fi].ops.len() + PANEL_OPS
        );
    }

    // ---------- behaviour (testsim) ----------

    use crate::testsim::{Core, Sim, V};

    /// The appended functions by build() order.
    struct Fns {
        event: RefFun,
        cancel: RefFun,
        begin: RefFun,
        late: RefFun,
        rs_push: RefFun,
        rs_reflow: RefFun,
        rs_install: RefFun,
        head_push: RefFun,
        head: RefFun,
    }

    fn built(image: &[u8]) -> (Bytecode, usize) {
        let mut code = read(image);
        let n = code.functions.len();
        api(&mut code).expect("api");
        (code, n)
    }

    fn fns(code: &Bytecode, n: usize) -> Fns {
        let f = |k: usize| code.functions[n + k].findex;
        Fns {
            event: f(9),
            cancel: f(10),
            begin: f(11),
            late: f(17),
            rs_push: f(14),
            rs_reflow: f(15),
            rs_install: f(16),
            head_push: f(20),
            head: f(21),
        }
    }

    /// Mouse position comes from maps["in"]["mx"/"my"]; vanilla calls are logged.
    fn sim<'a>(code: &'a Bytecode, n: usize, c: &'a Ctx) -> Sim<'a> {
        let mut s = Sim::new(
            code,
            n,
            move |k: &mut Core, f: RefFun, a: &[V]| {
                let log = |k: &mut Core, what: &'static str| k.log.push((what, a.to_vec()));
                if f == c.child_index {
                    Some(V::I(0))
                } else if f == c.get_scene {
                    Some(k.map("in", "scene"))
                } else if f == c.mouse_x {
                    Some(k.map("in", "mx"))
                } else if f == c.mouse_y {
                    Some(k.map("in", "my"))
                } else if f == c.sys_time {
                    Some(k.map("in", "now"))
                } else if f == c.get_props {
                    log(k, "getProperties");
                    // The resize handle has props of its own (made by its constructor).
                    let own = k.key_get(&a[1], "props");
                    Some(if own != V::Null {
                        own
                    } else {
                        k.map("in", "props")
                    })
                } else if f == c.rs.inter_ctor {
                    log(k, "handle");
                    let pr = k.obj(&[]);
                    k.key_set(&a[0], "props".into(), pr);
                    Some(V::Null)
                } else if f == c.rs.get_inv {
                    Some(k.key_get(&a[0], "inv"))
                } else if f == c.rs.force_update {
                    log(k, "forceUpdate");
                    Some(V::Null)
                } else if f == c.rs.content_changed {
                    log(k, "contentChanged");
                    Some(V::Null)
                } else if f == c.rs.scroll_reset {
                    log(k, "scrollReset");
                    Some(V::Null)
                } else if f == c.rs.set_max_h {
                    log(k, "maxHeight");
                    Some(a[1].clone())
                } else if f == c.set_need_reflow {
                    log(k, "needReflow");
                    Some(V::B(true))
                } else if f == c.start_capture {
                    log(k, "startCapture");
                    Some(V::Null)
                } else if f == c.stop_capture {
                    log(k, "stopCapture");
                    Some(V::Null)
                } else if f == c.set_ud {
                    log(k, "setUserData");
                    if let V::S(key) = &a[0] {
                        k.put("ud", key, a[1].clone());
                    }
                    Some(V::Null)
                } else if f == c.get_ud {
                    let V::S(key) = &a[0] else {
                        panic!("getUserData key")
                    };
                    Some(k.map("ud", key))
                } else if f == c.is_of_type {
                    let cls = if a[1] == V::S("cont".into()) {
                        "cont"
                    } else {
                        "inter"
                    };
                    Some(V::B(k.key_get(&a[0], cls) == V::B(true)))
                } else if f == c.str_add {
                    let st = |v: &V| match v {
                        V::S(x) => x.clone(),
                        o => panic!("str {o:?}"),
                    };
                    Some(V::S(st(&a[0]) + &st(&a[1])))
                } else if f == c.set_attr {
                    // domkit: kept as an inline style and applied at once.
                    log(k, "setAttribute");
                    let V::S(name) = &a[1] else {
                        panic!("setAttribute name")
                    };
                    assert_eq!(k.key_get(&a[2], "idx"), V::I(c.css_vint as i32));
                    let n = k.key_get(&a[2], "e0");
                    k.put("inline", name, n.clone());
                    let pr = k.map("in", "props");
                    let off = if name == "offset-x" { c.off_x } else { c.off_y };
                    k.set(&pr, off, n);
                    Some(V::Null)
                } else if f == c.get_class {
                    Some(V::S("cls".into()))
                } else if f == c.class_name {
                    Some(V::S("Win".into()))
                } else if f == c.std_string || f == c.println {
                    panic!("drag code threw (report called)")
                } else {
                    None
                }
            },
            |_: &mut Core, field: usize, _: &[V]| panic!("virtual call #{field}"),
        );
        for (gl, v) in [
            (c.inter_cls, "inter"),
            (c.rs.cont_cls, "cont"),
            (c.rs.fa_left, "L"),
            (c.rs.fa_bottom, "B"),
        ] {
            s.c.globals.insert(gl.0, V::S(v.into()));
        }
        s
    }

    /// A panel (offset 0,0 at x 100 / y 50) in a flow, a 1920x1080 scene.
    fn scene(s: &mut Sim, c: &Ctx) -> (V, V) {
        let k = &mut s.c;
        let sc = k.obj(&[(c.sc_w, V::I(1920)), (c.sc_h, V::I(1080))]);
        let pr = k.obj(&[
            (c.off_x, V::I(0)),
            (c.off_y, V::I(0)),
            (c.is_abs, V::B(false)),
        ]);
        let props = k.arr(c.arr_len, c.arr_arr, vec![pr.clone()]);
        let fl = k.obj(&[(c.properties, props)]);
        let obj = k.obj(&[
            (c.parent, fl),
            (c.x, V::F(100.0)),
            (c.y, V::F(50.0)),
            (c.pos_changed, V::B(false)),
        ]);
        k.put("in", "scene", sc);
        k.put("in", "props", pr.clone());
        k.put("in", "now", V::F(10.0));
        (obj, pr)
    }

    fn mouse(s: &mut Sim, x: f64, y: f64) {
        s.c.put("in", "mx", V::F(x));
        s.c.put("in", "my", V::F(y));
    }

    fn event(s: &mut Sim, c: &Ctx, f: &Fns, kind: i32) -> V {
        let kd = s.c.enm(kind, vec![]);
        let e =
            s.c.obj(&[(c.kind, kd), (c.propagate, V::B(true)), (c.button, V::I(0))]);
        s.run(f.event, vec![e.clone()]);
        s.c.get(&e, c.propagate)
    }

    fn press(s: &mut Sim, f: &Fns, obj: &V) {
        s.run(f.begin, vec![obj.clone(), V::Null, V::S("k".into())]);
        let cap = s.c.take("startCapture");
        assert_eq!(cap.len(), 1);
        // The capture gets our onCancel (not null): a lost capture resets the drag.
        assert_eq!(cap[0][2], V::Fun(f.cancel));
        s.c.take("getProperties");
    }

    /// Moves apply the position at once (x/y and the flow offset), with no
    /// reflow and no storage write; the release saves once and consumes the
    /// event, so it never reaches the world under the cursor.
    #[test]
    fn move_is_immediate_and_release_saves() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, pr) = scene(&mut s, &c);
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);

        mouse(&mut s, 530.0, 320.0);
        assert_eq!(
            event(&mut s, &c, &f, c.ev_move),
            V::B(false),
            "move consumed"
        );
        assert_eq!(s.c.get(&obj, c.x), V::F(130.0));
        assert_eq!(s.c.get(&obj, c.y), V::F(70.0));
        assert_eq!(s.c.get(&obj, c.pos_changed), V::B(true));
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(20));
        mouse(&mut s, 540.0, 310.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(140.0));
        assert_eq!(s.c.get(&obj, c.y), V::F(60.0));
        for what in ["getProperties", "needReflow", "setUserData", "stopCapture"] {
            assert!(s.c.take(what).is_empty(), "{what} during move");
        }

        // Released over something else (a town): consumed, capture stopped, saved once.
        assert_eq!(
            event(&mut s, &c, &f, c.ev_release),
            V::B(false),
            "release consumed"
        );
        assert_eq!(s.c.take("stopCapture").len(), 1);
        let saved = s.c.take("setUserData");
        assert_eq!(saved.len(), 1);
        let packed = ((40 + 32768) << 16) | ((10 + 32768) & 0xFFFF);
        assert_eq!(saved[0][1], V::I(packed));

        // The drag is over and let go: no global keeps its scene (or a resize
        // area); a late move does not move anything.
        for (gi, v) in &s.c.globals {
            let t = code.globals[*gi];
            if t == c.scene_t || t == c.rs.cont_t {
                assert_eq!(*v, V::Null, "global {gi} still holds the drag");
            }
        }
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(140.0));
        assert!(s.c.take("stopCapture").is_empty());

        // A new push drags again.
        mouse(&mut s, 100.0, 100.0);
        s.c.put("in", "now", V::F(20.0));
        press(&mut s, &f, &obj);
        mouse(&mut s, 110.0, 100.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(150.0));
    }

    /// Capture taken away (another startCapture / a stopCapture): onCancel
    /// drops the drag and saves; later events of the dead drag do nothing.
    #[test]
    fn cancel_resets_the_drag() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, _) = scene(&mut s, &c);
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);
        mouse(&mut s, 520.0, 300.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(120.0));

        s.run(f.cancel, vec![]);
        assert_eq!(s.c.take("setUserData").len(), 1, "cancel saves");
        assert!(
            s.c.take("stopCapture").is_empty(),
            "cancel must not re-enter stopCapture"
        );
        s.run(f.cancel, vec![]);
        assert!(
            s.c.take("setUserData").is_empty(),
            "second cancel is a no-op"
        );

        mouse(&mut s, 900.0, 300.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(120.0));
        assert_eq!(event(&mut s, &c, &f, c.ev_release), V::B(false));
        assert!(s.c.take("setUserData").is_empty());

        // Dragging works again afterwards.
        mouse(&mut s, 0.0, 0.0);
        s.c.put("in", "now", V::F(30.0));
        press(&mut s, &f, &obj);
        mouse(&mut s, 5.0, 0.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&obj, c.x), V::F(125.0));
    }

    /// Push + release without a move is a click: the release goes on.
    #[test]
    fn click_without_move_passes_release() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, _) = scene(&mut s, &c);
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);
        assert_eq!(event(&mut s, &c, &f, c.ev_release), V::B(true));
        assert_eq!(s.c.take("stopCapture").len(), 1);
        // A key event during a drag is not swallowed either.
        s.c.put("in", "now", V::F(20.0));
        press(&mut s, &f, &obj);
        let wheel = (0..8)
            .find(|&k| ![c.ev_push, c.ev_move, c.ev_release, c.ev_release_out].contains(&k))
            .unwrap();
        assert_eq!(event(&mut s, &c, &f, wheel), V::B(true));
    }

    /// Title row: default-cursor interactives in the top band get their push
    /// wrapped (own handler, then the window drag) but never propagateEvents,
    /// so hover / move stay with them (their tooltips: the unit sheet's
    /// effect icons). Buttons, the window's own interactive, body elements and
    /// named interactives don't change; a second reflow does not wrap twice.
    #[test]
    fn header_elements_pass_push() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (win, _) = scene(&mut s, &c);
        let prop = typed(&code, c.inter_t, "propagateEvents", c.bool_t).unwrap();
        let k = &mut s.c;
        let def = c.cursor_default;
        let other = if def == 0 { 1 } else { 0 };
        let inter = |k: &mut Core, cur: i32, y: f64| {
            let e = k.enm(cur, vec![]);
            let it = k.obj(&[
                (c.cursor, e),
                (prop, V::B(false)),
                (c.visible, V::B(true)),
                (c.y, V::F(y)),
            ]);
            k.key_set(&it, "inter".into(), V::B(true));
            it
        };
        let own = inter(k, def, 0.0);
        let title_it = inter(k, def, 0.0);
        let button = inter(k, other, 5.0);
        let body_it = inter(k, def, 0.0);
        let deep_it = inter(k, def, 0.0);
        let title_kids = k.arr(c.arr_len, c.arr_arr, vec![title_it.clone(), button.clone()]);
        let title = k.obj(&[
            (c.children, title_kids),
            (c.visible, V::B(true)),
            (c.y, V::F(20.0)),
        ]);
        let body_kids = k.arr(c.arr_len, c.arr_arr, vec![body_it.clone()]);
        let body = k.obj(&[
            (c.children, body_kids),
            (c.visible, V::B(true)),
            (c.y, V::F(90.0)),
        ]);
        // A header child pushed below the band by its parent's y.
        let low_kids = k.arr(c.arr_len, c.arr_arr, vec![deep_it.clone()]);
        let low = k.obj(&[
            (c.children, low_kids),
            (c.visible, V::B(true)),
            (c.y, V::F(60.0)),
        ]);
        k.set(&deep_it, c.y, V::F(15.0));
        // A named interactive in the band (someone else's name): left alone.
        let named = inter(k, def, 0.0);
        k.set(&named, c.name, V::S("x".into()));
        let kids = k.arr(
            c.arr_len,
            c.arr_arr,
            vec![own.clone(), title, V::Null, body, low, named.clone()],
        );
        let modal = k.enm(if c.modal_none == 0 { 1 } else { 0 }, vec![]);
        for (fl, v) in [
            (c.children, kids),
            (c.interactive, own.clone()),
            (c.modal, modal),
            (c.abs_y, V::F(0.0)),
            (c.calc_w, V::F(400.0)),
            (c.frame_flow, V::Null),
        ] {
            k.set(&win, fl, v);
        }
        // The title element's own push handler (a stand-in vanilla call).
        let orig = V::Fun(c.set_need_reflow);
        for it in [&own, &title_it, &button, &body_it, &deep_it, &named] {
            k.set(it, c.on_push, orig.clone());
        }
        s.run(f.head, vec![win.clone(), V::F(0.0), win.clone()]);
        let wrapped = s.c.get(&title_it, c.on_push);
        let V::Clo(hf, cap) = wrapped.clone() else {
            panic!("title onPush not wrapped: {wrapped:?}")
        };
        assert_eq!(hf, f.head_push);
        assert_eq!(s.c.get(&title_it, c.name), V::S(HEAD_MARK.into()));
        for (it, what) in [
            (&own, "own"),
            (&title_it, "title"),
            (&button, "button"),
            (&body_it, "body"),
            (&deep_it, "deep"),
            (&named, "named"),
        ] {
            assert_eq!(
                s.c.get(it, prop),
                V::B(false),
                "{what}: hover must not pass on"
            );
            if what != "title" {
                assert_eq!(s.c.get(it, c.on_push), orig, "{what} untouched");
            }
        }

        // Next reflow: already wrapped, not again.
        s.run(f.head, vec![win.clone(), V::F(0.0), win.clone()]);
        assert_eq!(s.c.get(&title_it, c.on_push), wrapped);

        // A left push on the title: its own handler, then the drag starts.
        mouse(&mut s, 300.0, 20.0);
        s.c.take("needReflow");
        let kd = s.c.enm(c.ev_push, vec![]);
        let e = s.c.obj(&[
            (c.kind, kd),
            (c.propagate, V::B(false)),
            (c.button, V::I(0)),
        ]);
        assert_eq!(s.c.key_get(&cap, "e1"), win);
        s.run(hf, vec![*cap, e.clone()]);
        assert_eq!(s.c.take("needReflow").len(), 1, "own onPush ran");
        assert_eq!(s.c.take("startCapture").len(), 1, "drag started");
        assert_eq!(s.c.get(&e, c.propagate), V::B(false));
    }

    fn packed(x: i32, y: i32) -> V {
        V::I(((x + 32768) << 16) | ((y + 32768) & 0xFFFF))
    }

    /// The chest panel is `position: absolute; align: bottom left` (style.css
    /// game-inventory #chestInventory): the flow still places it, offsets
    /// included, so it drags. An absolute child without an align stays put.
    #[test]
    fn absolute_aligned_panel_drags() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, pr) = scene(&mut s, &c);
        let (al_h, al_v) = (s.c.enm(0, vec![]), s.c.enm(2, vec![]));
        s.c.set(&pr, c.is_abs, V::B(true));
        s.c.set(&pr, c.h_align, al_h);
        s.c.set(&pr, c.v_align, al_v);
        s.c.set(&pr, c.off_y, V::I(-410));
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);
        mouse(&mut s, 530.0, 320.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-390));
        assert_eq!(s.c.get(&obj, c.x), V::F(130.0));
        assert_eq!(s.c.get(&obj, c.y), V::F(70.0));
        event(&mut s, &c, &f, c.ev_release);
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(30, -390));

        // Absolute, no align: the flow does not place it, nothing moves.
        s.c.set(&pr, c.h_align, V::Null);
        s.c.set(&pr, c.v_align, V::Null);
        s.c.put("in", "now", V::F(20.0));
        mouse(&mut s, 0.0, 0.0);
        press(&mut s, &f, &obj);
        mouse(&mut s, 50.0, 50.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&obj, c.x), V::F(130.0));
    }

    /// Chest panel life cycle: built in the constructor before its style is
    /// applied (restore waits), styled (CSS offset-y -410 becomes the base,
    /// the saved spot is restored, onAfterReflow becomes the clamp), then a
    /// double push (as when clicking the header twice) goes back to the styled
    /// spot, not to offset 0,0 (bottom left, under the inventory panel, which
    /// looked like a chest that never reopens).
    #[test]
    fn panel_restores_after_style_and_resets_to_base() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, pr) = scene(&mut s, &c);
        let (al_h, al_v) = (s.c.enm(0, vec![]), s.c.enm(2, vec![]));
        s.c.set(&pr, c.is_abs, V::B(true));
        s.c.set(&pr, c.h_align, al_h);
        s.c.set(&pr, c.v_align, al_v);
        let dom = s.c.obj(&[(c.need_style, V::B(true))]);
        s.c.set(&obj, c.dom, dom.clone());
        s.c.put("ud", "mpWinPos:k", packed(30, -300));
        let cap = s.c.enm(0, vec![obj.clone(), V::S("k".into())]);

        // Style pending: nothing restored, the handler stays.
        s.run(f.late, vec![cap.clone()]);
        assert!(s.c.take("getProperties").is_empty());
        assert!(s.c.take("setUserData").is_empty());
        assert_eq!(s.c.get(&obj, c.on_after_reflow), V::Null);

        // Styled: CSS offset kept as base, saved spot restored, clamp installed.
        s.c.set(&pr, c.off_y, V::I(-410));
        s.c.set(&dom, c.need_style, V::B(false));
        s.run(f.late, vec![cap.clone()]);
        assert_eq!(s.c.map("ud", "mpWinBase:k"), packed(0, -410));
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-300));
        assert_eq!(
            s.c.get(&obj, c.on_after_reflow),
            V::Clo(f.rs_reflow, Box::new(cap.clone()))
        );

        // Double push: back to the styled spot, saved as such.
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);
        event(&mut s, &c, &f, c.ev_release);
        s.c.put("in", "now", V::F(10.1));
        s.run(f.begin, vec![obj.clone(), V::Null, V::S("k".into())]);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(0));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-410));
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(0, -410));

        // A panel without a CSS offset: base removed, reset to 0,0.
        s.c.set(&pr, c.off_y, V::I(0));
        s.run(f.late, vec![cap]);
        assert_eq!(s.c.map("ud", "mpWinBase:k"), V::Null);
        s.c.set(&pr, c.off_x, V::I(70));
        s.c.put("in", "now", V::F(30.0));
        press(&mut s, &f, &obj);
        event(&mut s, &c, &f, c.ev_release);
        s.c.put("in", "now", V::F(30.1));
        s.run(f.begin, vec![obj.clone(), V::Null, V::S("k".into())]);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(0));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(0));
    }

    /// domkit style refresh of the chest panel (its hover toggles): every
    /// matching CSS rule is applied again (`offset-y: -410`), then the inline
    /// attributes on top.
    fn restyle(s: &mut Sim, c: &Ctx, pr: &V) {
        s.c.set(pr, c.off_x, V::I(0));
        s.c.set(pr, c.off_y, V::I(-410));
        for (name, off) in [("offset-x", c.off_x), ("offset-y", c.off_y)] {
            let v = s.c.map("inline", name);
            if v != V::Null {
                s.c.set(pr, off, v);
            }
        }
    }

    /// Chest panel: a style refresh after (or between) drags keeps the dragged
    /// spot (it threw the chest back to the CSS one), two drags in quick
    /// succession both move it (the second was taken for a double push), and
    /// a double click still resets to the styled spot, which then sticks too.
    #[test]
    fn chest_drag_survives_restyle() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, pr) = scene(&mut s, &c);
        let (al_h, al_v) = (s.c.enm(0, vec![]), s.c.enm(2, vec![]));
        s.c.set(&pr, c.is_abs, V::B(true));
        s.c.set(&pr, c.h_align, al_h);
        s.c.set(&pr, c.v_align, al_v);
        s.c.set(&pr, c.off_y, V::I(-410));
        let dom = s.c.obj(&[(c.need_style, V::B(false))]);
        s.c.set(&obj, c.dom, dom);
        let cap = s.c.enm(0, vec![obj.clone(), V::S("k".into())]);
        s.run(f.late, vec![cap]);
        assert_eq!(s.c.map("ud", "mpWinBase:k"), packed(0, -410));
        restyle(&mut s, &c, &pr);
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-410));

        // First drag, then a refresh: the dragged spot stays.
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &obj);
        mouse(&mut s, 530.0, 320.0);
        event(&mut s, &c, &f, c.ev_move);
        event(&mut s, &c, &f, c.ev_release);
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(30, -390));
        s.c.take("setAttribute");
        restyle(&mut s, &c, &pr);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-390));

        // Second drag 0.1 s later: a drag again, not a reset.
        s.c.put("in", "now", V::F(10.1));
        press(&mut s, &f, &obj);
        mouse(&mut s, 540.0, 330.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(40));
        assert!(s.c.take("setAttribute").is_empty(), "no pin per move");
        event(&mut s, &c, &f, c.ev_release);
        restyle(&mut s, &c, &pr);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(40));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-380));
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(40, -380));

        // Double click: back to the styled spot, which sticks as well.
        s.c.put("in", "now", V::F(20.0));
        press(&mut s, &f, &obj);
        assert_eq!(event(&mut s, &c, &f, c.ev_release), V::B(true));
        s.c.put("in", "now", V::F(20.1));
        s.run(f.begin, vec![obj.clone(), V::Null, V::S("k".into())]);
        restyle(&mut s, &c, &pr);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(0));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-410));

        // A window (no base entry) is never pinned.
        s.c.take("setAttribute");
        let (win, _) = scene(&mut s, &c);
        s.c.put("in", "now", V::F(40.0));
        s.run(f.begin, vec![win.clone(), V::Null, V::S("w".into())]);
        mouse(&mut s, 600.0, 300.0);
        event(&mut s, &c, &f, c.ev_move);
        event(&mut s, &c, &f, c.ev_release);
        assert!(s.c.take("setAttribute").is_empty());
    }

    /// Issue #4: the chest panel at its styled spot (never dragged) is left
    /// where vanilla puts it, even when its stale position looks off screen;
    /// no clamp while the parent flow is mid-reflow (its needReflow still
    /// set: it places the panel afterwards), the panel's own reflow is asked
    /// for instead (clamped then, placed); a real clamp is saved and pinned,
    /// so the hover's style refresh keeps it. Windows (no key) clamp as before.
    #[test]
    fn panel_clamp_keeps_vanilla_spot_and_pins() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (obj, pr) = scene(&mut s, &c);
        let (al_h, al_v) = (s.c.enm(0, vec![]), s.c.enm(2, vec![]));
        s.c.set(&pr, c.is_abs, V::B(true));
        s.c.set(&pr, c.h_align, al_h);
        s.c.set(&pr, c.v_align, al_v);
        s.c.set(&pr, c.off_y, V::I(-410));
        s.c.set(&pr, c.p_calc_w, V::I(300));
        s.c.set(&obj, c.visible, V::B(true));
        let dom = s.c.obj(&[(c.need_style, V::B(false))]);
        s.c.set(&obj, c.dom, dom);
        let cap = s.c.enm(0, vec![obj.clone(), V::S("k".into())]);
        s.run(f.late, vec![cap.clone()]);
        assert_eq!(s.c.map("ud", "mpWinBase:k"), packed(0, -410));
        // The parent seen above the screen top (stale, mid-layout values).
        let fl = s.c.get(&obj, c.parent);
        s.c.set(&fl, c.abs_x, V::F(0.0));
        s.c.set(&fl, c.abs_y, V::F(-500.0));
        s.c.take("setUserData");
        s.c.take("needReflow");

        // Styled spot: untouched, nothing saved.
        s.run(f.rs_reflow, vec![cap.clone()]);
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-410));
        assert!(s.c.take("needReflow").is_empty());
        assert!(s.c.take("setUserData").is_empty());

        // A dragged spot while the parent is mid-reflow: untouched too.
        s.c.set(&pr, c.off_x, V::I(30));
        s.c.set(&pr, c.off_y, V::I(-300));
        s.c.set(&fl, c.need_reflow, V::B(true));
        s.run(f.rs_reflow, vec![cap.clone()]);
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-300));
        let nr = s.c.take("needReflow");
        assert_eq!(nr.len(), 1, "the panel reflows again once placed");
        assert_eq!(nr[0][0], obj);
        assert!(s.c.take("setUserData").is_empty());

        // Placed (parent reflow done): clamped down to the top edge, saved, pinned.
        s.c.set(&fl, c.need_reflow, V::B(false));
        s.run(f.rs_reflow, vec![cap.clone()]);
        assert_eq!(s.c.get(&pr, c.off_y), V::I(150));
        assert_eq!(s.c.take("needReflow").len(), 1);
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(30, 150));
        restyle(&mut s, &c, &pr);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(150), "pinned over the CSS offset");

        // A window: clamped whatever its parent's needReflow, never saved.
        let clamp = find_named(&code, N_CLAMP).unwrap();
        let (win, wpr) = scene(&mut s, &c);
        s.c.set(&win, c.visible, V::B(true));
        s.c.set(&wpr, c.off_y, V::I(-300));
        s.c.set(&wpr, c.p_calc_w, V::I(300));
        let wfl = s.c.get(&win, c.parent);
        for (fd, v) in [
            (c.abs_x, V::F(0.0)),
            (c.abs_y, V::F(-500.0)),
            (c.need_reflow, V::B(true)),
        ] {
            s.c.set(&wfl, fd, v);
        }
        s.c.take("setUserData");
        s.run(clamp, vec![win, V::Null]);
        assert_eq!(s.c.get(&wpr, c.off_y), V::I(150));
        assert!(s.c.take("setUserData").is_empty());
    }

    /// An inventory panel (scroll area 330 px = 6 rows, grid built for 6)
    /// whose top is at y 500 and 400 px tall on a 1080 px screen: room for 3
    /// more rows below it. Returns (panel, props, content, grid).
    fn rs_panel(s: &mut Sim, c: &Ctx) -> (V, V, V, V) {
        let (panel, pr) = scene(s, c);
        let k = &mut s.c;
        let inv = k.obj(&[(c.rs.vis_h, V::I(6)), (c.rs.base_h, V::I(6))]);
        let cont = k.obj(&[(c.rs.calc_h, V::F(330.0))]);
        k.key_set(&cont, "cont".into(), V::B(true));
        k.key_set(&cont, "inv".into(), inv.clone());
        let kids = k.arr(c.arr_len, c.arr_arr, vec![V::Null, cont.clone()]);
        for (fl, v) in [
            (c.children, kids),
            (c.abs_y, V::F(500.0)),
            (c.rs.calc_h, V::F(400.0)),
        ] {
            k.set(&panel, fl, v);
        }
        (panel, pr, cont, inv)
    }

    fn rs_height(rows: i32) -> V {
        V::I(rows * ROW_PX + ROW_PAD)
    }

    /// Row resize: the handle sits bottom-left; dragging it snaps the scroll
    /// area to whole rows, clamps to [MIN_ROWS, what fits on screen], builds
    /// enough grid rows, keeps the top (offsetY += added height: the panels
    /// are bottom-anchored, so they grow downward) and saves the rows; a
    /// header drag afterwards moves (not resizes), and its double-click reset
    /// keeps the size; a new panel gets the saved size back.
    #[test]
    fn resize_snaps_clamps_and_persists() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (panel, pr, cont, inv) = rs_panel(&mut s, &c);
        let key = V::S("k".into());

        // Install: no saved size, a handle at the bottom-left corner.
        s.run(f.rs_install, vec![panel.clone(), key.clone()]);
        assert!(s.c.take("maxHeight").is_empty(), "no size to restore");
        let made = s.c.take("handle");
        assert_eq!(made.len(), 1);
        let it = made[0][0].clone();
        assert_eq!(made[0][3], panel, "the handle is the panel's child");
        assert_eq!(s.c.get(&it, c.rs.bg_color), V::I(HANDLE_ARGB), "visible");
        let hp = s.c.key_get(&it, "props");
        assert_eq!(s.c.get(&hp, c.is_abs), V::B(true));
        assert_eq!(s.c.get(&hp, c.h_align), V::S("L".into()));
        assert_eq!(s.c.get(&hp, c.v_align), V::S("B".into()));
        let push = s.c.get(&it, c.on_push);
        let V::Clo(pf, cap) = push else {
            panic!("handle onPush: {push:?}")
        };
        assert_eq!(pf, f.rs_push);

        // Push the handle at y 600.
        mouse(&mut s, 20.0, 600.0);
        let kd = s.c.enm(c.ev_push, vec![]);
        let e = s.c.obj(&[(c.kind, kd), (c.button, V::I(0))]);
        s.run(f.rs_push, vec![(*cap).clone(), e]);
        assert_eq!(
            s.c.take("startCapture").len(),
            1,
            "the drag capture runs it"
        );
        s.c.take("getProperties");

        // +80 px: rounds to 2 rows -> 8; the top kept (offsetY +106).
        mouse(&mut s, 20.0, 680.0);
        assert_eq!(
            event(&mut s, &c, &f, c.ev_move),
            V::B(false),
            "move consumed"
        );
        let mh = s.c.take("maxHeight");
        assert_eq!(mh.len(), 1);
        assert_eq!(mh[0][0], cont);
        assert_eq!(mh[0][1], rs_height(8));
        assert!(s.c.take("minHeight").is_empty(), "no minHeight (line height)");
        assert_eq!(s.c.get(&pr, c.off_y), V::I(2 * ROW_PX));
        assert_eq!(s.c.get(&pr, c.off_x), V::I(0), "width / x unchanged");
        assert_eq!(s.c.get(&inv, c.rs.vis_h), V::I(8));
        assert_eq!(s.c.get(&inv, c.rs.base_h), V::I(8));
        assert_eq!(s.c.take("forceUpdate").len(), 1, "rows built");
        assert_eq!(s.c.map("ud", "mpWinSize:k"), V::I(8));
        // The new viewport goes through vanilla: needScroll from the new
        // maxHeight (after the rows are built), scroll back at the top.
        let cc = s.c.take("contentChanged");
        assert_eq!(cc.len(), 1);
        assert_eq!((cc[0][0].clone(), cc[0][1].clone()), (cont.clone(), inv.clone()));
        assert_eq!(s.c.take("scrollReset"), vec![vec![cont.clone()]]);
        // Within the same row: nothing changes.
        mouse(&mut s, 25.0, 690.0);
        s.c.take("setUserData");
        event(&mut s, &c, &f, c.ev_move);
        assert!(s.c.take("maxHeight").is_empty() && s.c.take("setUserData").is_empty());
        // +30 px: 7 rows; built rows stay, offset follows.
        mouse(&mut s, 20.0, 630.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.take("maxHeight")[0][1], rs_height(7));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(ROW_PX));
        assert_eq!(s.c.get(&inv, c.rs.vis_h), V::I(8));
        assert!(s.c.take("forceUpdate").is_empty());
        // Far down: clamped to 6 + 3 rows (fits on screen).
        mouse(&mut s, 20.0, 1600.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.take("maxHeight")[0][1], rs_height(9));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(3 * ROW_PX));
        // Far up: clamped to MIN_ROWS.
        mouse(&mut s, 20.0, -400.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.take("maxHeight")[0][1], rs_height(MIN_ROWS));
        assert_eq!(s.c.get(&pr, c.off_y), V::I((MIN_ROWS - 6) * ROW_PX));
        assert_eq!(s.c.map("ud", "mpWinSize:k"), V::I(MIN_ROWS));
        // Release: the position (offset) saved as for a move.
        assert_eq!(event(&mut s, &c, &f, c.ev_release), V::B(false));
        assert_eq!(
            s.c.map("ud", "mpWinPos:k"),
            packed(0, (MIN_ROWS - 6) * ROW_PX)
        );

        // A header drag now moves the panel; the size stays.
        s.c.put("in", "now", V::F(20.0));
        mouse(&mut s, 500.0, 300.0);
        press(&mut s, &f, &panel);
        mouse(&mut s, 530.0, 300.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert!(s.c.take("maxHeight").is_empty());
        event(&mut s, &c, &f, c.ev_release);
        // Double click on the header: position reset, size kept.
        s.c.put("in", "now", V::F(30.0));
        press(&mut s, &f, &panel);
        event(&mut s, &c, &f, c.ev_release);
        s.c.put("in", "now", V::F(30.1));
        s.run(f.begin, vec![panel.clone(), V::Null, key.clone()]);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(0));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(0));
        assert_eq!(s.c.map("ud", "mpWinSize:k"), V::I(MIN_ROWS));

        // Reopened (a new panel): the saved size comes back; its reflow
        // rebuilds rows that vanilla dropped (showInventory: 6, scroll: fewer).
        let (p2, _, c2, inv2) = rs_panel(&mut s, &c);
        s.c.set(&inv2, c.rs.vis_h, V::I(1));
        s.c.take("contentChanged");
        s.c.take("scrollReset");
        s.run(f.rs_install, vec![p2.clone(), key.clone()]);
        let mh = s.c.take("maxHeight");
        assert_eq!(
            (mh[0][0].clone(), mh[0][1].clone()),
            (c2.clone(), rs_height(MIN_ROWS))
        );
        assert_eq!(s.c.get(&inv2, c.rs.vis_h), V::I(MIN_ROWS));
        assert_eq!(s.c.take("contentChanged")[0][0], c2, "restored viewport");
        assert_eq!(s.c.take("scrollReset").len(), 1);
        s.c.set(&inv2, c.rs.vis_h, V::I(1));
        s.c.take("forceUpdate");
        let cap2 = s.c.enm(0, vec![p2.clone(), key.clone()]);
        s.run(f.rs_reflow, vec![cap2.clone()]);
        assert_eq!(s.c.get(&inv2, c.rs.vis_h), V::I(MIN_ROWS));
        assert_eq!(s.c.take("forceUpdate").len(), 1);
        assert!(
            s.c.take("maxHeight").is_empty(),
            "reflow never resets the height (no reflow loop)"
        );
        assert!(
            s.c.take("contentChanged").is_empty() && s.c.take("scrollReset").is_empty(),
            "nor marks the content changed or resets the scroll"
        );
        s.run(f.rs_reflow, vec![cap2]);
        assert!(
            s.c.take("forceUpdate").is_empty(),
            "rows already built: no rebuild"
        );
    }

    /// The chest (CSS offset-y -410, dragged to 30,-300): a resize grows it
    /// downward from its dragged spot, and the release pins the new offset
    /// so a style refresh keeps it.
    #[test]
    fn chest_resize_keeps_drag_offset() {
        let Some(image) = game() else { return };
        let (code, n) = built(&image);
        let c = ctx(&code).unwrap();
        let f = fns(&code, n);
        let mut s = sim(&code, n, &c);
        let (panel, pr, _, _) = rs_panel(&mut s, &c);
        let (al_h, al_v) = (s.c.enm(0, vec![]), s.c.enm(2, vec![]));
        s.c.set(&pr, c.is_abs, V::B(true));
        s.c.set(&pr, c.h_align, al_h);
        s.c.set(&pr, c.v_align, al_v);
        s.c.set(&pr, c.off_y, V::I(-410));
        let dom = s.c.obj(&[(c.need_style, V::B(false))]);
        s.c.set(&panel, c.dom, dom);
        s.c.put("ud", "mpWinPos:k", packed(30, -300));
        let cap = s.c.enm(0, vec![panel.clone(), V::S("k".into())]);
        s.run(f.late, vec![cap.clone()]);
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-300));
        let it = s.c.take("handle")[0][0].clone();
        let V::Clo(_, hcap) = s.c.get(&it, c.on_push) else {
            panic!("no handle push")
        };
        mouse(&mut s, 20.0, 600.0);
        let kd = s.c.enm(c.ev_push, vec![]);
        let e = s.c.obj(&[(c.kind, kd), (c.button, V::I(0))]);
        s.run(f.rs_push, vec![*hcap, e]);
        mouse(&mut s, 20.0, 653.0);
        event(&mut s, &c, &f, c.ev_move);
        assert_eq!(s.c.get(&pr, c.off_x), V::I(30));
        assert_eq!(s.c.get(&pr, c.off_y), V::I(-300 + ROW_PX), "grows downward");
        event(&mut s, &c, &f, c.ev_release);
        assert_eq!(s.c.map("ud", "mpWinPos:k"), packed(30, -300 + ROW_PX));
        restyle(&mut s, &c, &pr);
        assert_eq!(
            s.c.get(&pr, c.off_y),
            V::I(-300 + ROW_PX),
            "pinned over the CSS offset"
        );
        assert_eq!(s.c.map("ud", "mpWinSize:k"), V::I(7));
    }
}
