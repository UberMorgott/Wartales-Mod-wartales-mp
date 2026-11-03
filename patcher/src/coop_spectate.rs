// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op spectating: a shared (data.cdb `props.coop`) mini-game no longer takes
// over the other players' screens. They stay in their scene with their own
// camera and input and watch the player's worker do it there.
//
// Vanilla: the element script (`ElementApi.a_startActivity`) starts a coop
// activity with `player = game.me` on the host, so `ui.win.Activity` lives on
// the host with `persist = CurrentMode` (`Controller.startActivity__impl`) and
// is replicated to every client (`State.updateReplication`). Its `setUnit` and
// `start` RPCs reach every machine (`networkRPC` cases 2 / 3 forward and run
// the impl), and `start__impl` -> `_start` has no owner test: on every machine
// it hides the inventory (`ui.showInventory(false)`), sets
// `mode.lockCamera`, `globalUI.padCursor.locked`, saves the camera, fades and
// moves the camera to the activity camera, and from the onReady closure
// creates the mini-game window (`miniGameToCall()`), `netSharing(true)`,
// `mode.setWindow(win)` and `addHelpIcon()` (an RPC: every machine's call adds
// an icon everywhere). The end (`onActionDone`) fades through
// `ctrl.netFade(coop)`, which also fades every other player. So every player
// got the window, the zoom and the input lock. The windows themselves are
// built for spectators: they gate input on `unit.owner == game.me` and play
// the worker's anims / props / effects from their replicated state, and on the
// host (also a spectator when a client plays) their logic stays authoritative.
// shim.log (client) shows it: `activity start id=Mine auth=false` followed by
// cancel / doResult with no `done` (a client watching the host mine).
//
// The patch keeps that window alive on a spectator but out of sight, and keeps
// the spectator's camera, inventory and input. A spectator is a machine where
// `spect(act)`: the activity is replicated (`__host != null`: coop only), its
// unit is set and `unit.owner != game.me` (the player the windows give the
// input to). Unit-less activities (BoardPuzzle, a solved NinePuzzle) keep the
// vanilla shared window: every player plays those.
//   _start: entry `g = spectPre(this)`: a spectator -> g, `changeCamera =
//     false` (no fade, no camera move, onReady runs at once), the chooseUnit
//     full-screen wait Interactive (added on every client before the start)
//     removed. The vanilla writes a spectator must not get are skipped, not
//     undone: `prevInventory = s ? spectInv(this) : ui.showInventory(false)`
//     for every spectator (s = spect(this); spectInv: the panel's current
//     visibility, so the end's showInventory(prevInventory) keeps it, and a
//     client spectator, which never runs that end, keeps its panel;
//     showInventory(false) would also drop an item held in the hand), and
//     `if (!g)` before `mode.lockCamera = true`, `padCursor.locked = true` and
//     `this.cameraSave = ...` (so _cancel restores no camera). The rest of
//     _start (start cost on the host, GC entry, trait level, the start sound)
//     runs as is.
//     The bard's song (Sing): the unit's unitView is a child of the stage
//     (`target.obj`), which only the skipped fade-in shows (and hides its
//     HandTool), so a watching spectator's spectPre runs that start half
//     itself (`spectStage`): the bard stands and sings on the tavern stage
//     while the spectator keeps its camera and input.
//   _cancel__impl (every machine's end): `spectEnd(this)` first: on a
//     spectator of Sing the end half of the vanilla stage pair
//     (onActionDone's closure, which a spectator does not run, hides the
//     stage and shows its HandTool again).
//     Exception (CAMERA_ACTIVITIES: LockPick, NinePuzzle, CylinderPuzzle):
//     seen only through the activity camera (the lock in front of it, the
//     puzzle board), so an idle watcher gets the vanilla view (camera move,
//     window) but never lockCamera / padCursor.locked: it keeps its input and
//     can pan away; a watcher busy with its own minigame or lock
//     (mode.lockCamera, me.lockedWith) watches from its own camera instead.
//   onReady closure: showTutorial(..., cb) -> spectTut: the host playing in co-op gets
//     the mini-game window at once and the tutorial over it (no gap in which the
//     minigame has no window: force_leave sees it); a spectator gets no
//     tutorial; cb (the window closure) runs at once on the host and not at
//     all on a client spectator: the mini-game windows are built for the
//     host and the acting player only. On a client spectator their
//     constructor / netSharing threw out of _start (before the old restore, so the
//     camera and input stayed locked): GatherAction `Can't register ...
//     without ownership` (networkAllow(Ownership) needs player == me),
//     FishingAction `keepOnGathered on a not allowed object`, LockPick
//     `Null access .pinNumber` (lockData is set on the host only).
//   window closure: setWindow(win) -> spectWin: a spectator's window is made
//     invisible (windowRoot and the window: no draw, no Interactive events,
//     not the mode's current window) but stays in the windows list, updated
//     and networked; addHelpIcon() -> spectHelp: not called by a spectator.
//   addHelpIcon__impl: returns at once on a spectator (the player's icon RPC).
//   onActionDone's end fade: netFade(coop) -> spectFade: no replicated fade
//     when spectators are not in the activity camera (unit + owner set).
//   Step-out (a watcher of a camera activity is never trapped by watching):
//     spectPre removes the chooseUnit full-screen wait Interactive for every
//     spectator (a camera watcher kept it: no click reached its screen);
//     initInteractive of LockPick / NinePuzzle / CylinderPuzzle ends in
//     spectInt: a watcher's full-window input catcher passes every event on
//     (propagateEvents). Game.update's Escape loop calls spectEsc(w) per window
//     before vanilla: a watched copy steps out (spectOut: hidden, not the
//     current window, an idle watcher's camera restored by _cancel__impl's own
//     restore block, cameraSave dropped; this machine only, nothing is sent),
//     Escape used up; a hidden ActivityWindow (spectWin's copy, or stepped out
//     of) is skipped (vanilla: a client's Escape did nothing on it, the host's
//     ran the other player's minigame escape). The window's Leave button
//     (stopActivity, refused for a watcher: `not allowed object`) steps out too.
// shim.log: `mp: spectate <activity id> 1` on each watching spectator, `0` on a
// spectator of a camera activity, `<id> out` on a step-out.
// Not covered: the host's unit choice (a shared ChooseUnit, chooseUnit's
// full-screen wait Interactive on clients) before the start: which player
// starts it is known on the host only (`conds`, not synced).
// All players need this build. Validated before editing; a mismatch skips the
// pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::{call_of, closure_passed_to, static_fn};
use super::job_xp::str_global;
use super::ping_cell::vproto;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

/// Activities whose mini-game is only seen through the activity camera (the
/// lock, the ruins puzzle boards): their spectators keep the vanilla camera
/// move, locks and inventory; the rest of the pass (no tutorial, no client
/// window, hidden host window) still applies. Not Sing: the bard stands on the
/// tavern stage, which spectStage shows in the spectator's own view.
const CAMERA_ACTIVITIES: [&str; 3] = ["LockPick", "NinePuzzle", "CylinderPuzzle"];

pub(crate) struct Plan {
    act_t: RefType,
    game_t: RefType,
    mode_t: RefType,
    ui_t: RefType,
    unit_t: RefType,
    player_t: RefType,
    host_t: RefType,
    int_t: RefType,
    win_t: RefType,
    flow_t: RefType,
    ctrl_t: RefType,
    inf_t: RefType,
    cb_t: RefType,
    tut_t: RefType,
    str_t: RefType,
    dyn_t: RefType,
    bool_: RefType,
    i32_: RefType,
    void_: RefType,
    a_game: RefField,
    a_host: RefField,
    a_unit: RefField,
    a_int: RefField,
    a_cc: RefField,
    #[cfg_attr(not(test), allow(dead_code))]
    a_prev: RefField,
    a_inf: RefField,
    i_id: RefField,
    u_owner: RefField,
    g_me: RefField,
    g_auth: RefField,
    #[cfg_attr(not(test), allow(dead_code))]
    g_mode: RefField,
    /// BasePlayer.lockedWith (a busy watcher keeps its own camera).
    p_locked: (RefField, RefType),
    #[cfg_attr(not(test), allow(dead_code))]
    g_gui: RefField,
    g_ui: RefField,
    #[cfg_attr(not(test), allow(dead_code))]
    m_lock: RefField,
    #[cfg_attr(not(test), allow(dead_code))]
    gui_pad: RefField,
    #[cfg_attr(not(test), allow(dead_code))]
    pad_locked: RefField,
    w_root: RefField,
    set_window: RefFun,
    add_help: RefFun,
    show_tut: RefFun,
    net_fade: RefFun,
    set_vis: RefFun,
    set_vis_pi: i32,
    obj_remove: RefFun,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    str_cmp: RefFun,
    start_fi: usize,
    si_at: usize,
    ent_t: RefType,
    obj3_t: RefType,
    a_target: RefField,
    e_obj: RefField,
    o3_flags: RefField,
    /// h3d.scene.Object.cullingCollider, as onActionDone's Sing block calls it.
    cull_pi: RefField,
    cancel_fi: usize,
    /// _cancel__impl's lockCamera / padCursor.locked writes; Activity.cameraSave.
    c_lock_at: usize,
    c_pad_at: usize,
    a_cam: RefField,
    cam_t: RefType,
    lock_at: usize,
    pad_at: usize,
    cam_at: usize,
    /// spectInv: GameUI.getGameInventory, GameInventory.inventory (h2d.Object), Object.visible.
    ginv_t: RefType,
    obj2_t: RefType,
    get_ginv: RefFun,
    gi_inv: RefField,
    o_visible: RefField,
    ready_fi: usize,
    tut_at: usize,
    make_fi: usize,
    win_at: usize,
    help_at: usize,
    help_fi: usize,
    help_ret: Reg,
    fade_fi: usize,
    fade_at: usize,
    fade_act: Reg,
    dbg_file: usize,
    /// Step-out: _cancel__impl's camera restore + PlaceView camera reset (its
    /// ops [cam_s, cam_e), copied as spectCam with its register file).
    cam_ops: Vec<Opcode>,
    cam_regs: Vec<RefType>,
    /// The CallMethod slot of that PlaceView call (the only one the block makes).
    #[cfg_attr(not(test), allow(dead_code))]
    cam_pi: RefField,
    /// hl.BaseType.check; ui.win.ActivityWindow class global.
    base_check: RefFun,
    aw_cls: (RefGlobal, RefType),
    /// The windows of CAMERA_ACTIVITIES: class global, type, `activity` field,
    /// stopActivity (the Leave button's onClick) and initInteractive (fn index,
    /// its Ret) fn indices.
    cam_wins: Vec<CamWin>,
    /// h2d.Flow.interactive, h2d.Interactive.propagateEvents, GameMode.currentWindow.
    flow_int: RefField,
    int_prop: RefField,
    m_cur: RefField,
    gui_t: RefType,
    pad_t: RefType,
    /// Game.update's Escape loop: fn index, the `NullCheck w` before
    /// `w.canEscape()` (the hook goes in front), the window register, the
    /// `found = true` op after its escape call (break), the loop's back edge.
    esc_fi: usize,
    esc_at: usize,
    esc_w: Reg,
    esc_found: usize,
    esc_next: usize,
}

pub(crate) struct CamWin {
    cls: (RefGlobal, RefType),
    activity: RefField,
    stop_fi: usize,
    init_fi: usize,
    init_ret: usize,
}

fn want(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("unexpected {what} signature");
    }
    Ok(())
}

/// The register an op writes, if any (every opcode names it `dst`).
pub(crate) fn dst_of(op: &Opcode) -> Option<Reg> {
    let d = format!("{op:?}");
    let i = d.find("dst: Reg(")? + "dst: Reg(".len();
    let n: String = d[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok().map(Reg)
}

/// Some op of `f` jumps to op `at`.
fn jump_targets_any(f: &Function, at: usize) -> bool {
    (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&at))
}

/// The only op of `f` calling `fun`.
fn only_call(f: &Function, fun: RefFun, what: &str) -> Result<usize> {
    let at: Vec<usize> = (0..f.ops.len())
        .filter(|&i| call_of(&f.ops[i]).is_some_and(|(g, _)| g == fun))
        .collect();
    match at[..] {
        [i] => Ok(i),
        _ => bail!("fn@{}: {} calls of {what}, want 1", f.findex.0, at.len()),
    }
}

pub(crate) fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    let act_t = obj_type(code, "ui.win.Activity")?;
    let game_t = obj_type(code, "Game")?;
    let mode_t = obj_type(code, "GameMode")?;
    let gui_t = obj_type(code, "ui.GlobalUI")?;
    let pad_t = obj_type(code, "gamepad.PadCursorController")?;
    let ui_t = obj_type(code, "ui.GameUI")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let host_t = obj_type(code, "hxbit.NetworkHost")?;
    let int_t = obj_type(code, "h2d.Interactive")?;
    let win_t = obj_type(code, "ui.Window")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let obj2_t = obj_type(code, "h2d.Object")?;
    let ctrl_t = obj_type(code, "st.Controller")?;

    let a_game = typed(code, act_t, "game", game_t)?;
    let a_host = typed(code, act_t, "__host", host_t)?;
    let a_unit = typed(code, act_t, "unit", unit_t)?;
    let a_int = typed(code, act_t, "int", int_t)?;
    let a_cc = typed(code, act_t, "changeCamera", bool_)?;
    let a_prev = typed(code, act_t, "prevInventory", bool_)?;
    let (a_cam, cam_t) = field(code, act_t, "cameraSave")?;
    if !matches!(code.types[cam_t.0], Type::Virtual { .. }) {
        bail!("Activity.cameraSave is not a virtual");
    }
    let (a_inf, inf_t) = field(code, act_t, "inf")?;
    let (i_id, id_t) = field_of_virtual(code, inf_t, "id")?;
    if id_t != str_t {
        bail!("activity inf.id is not a String");
    }
    let u_owner = typed(code, unit_t, "owner", player_t)?;
    let g_me = typed(code, game_t, "me", player_t)?;
    let g_auth = typed(code, game_t, "isAuth", bool_)?;
    let g_mode = typed(code, game_t, "mode", mode_t)?;
    let g_gui = typed(code, game_t, "globalUI", gui_t)?;
    let g_ui = typed(code, game_t, "ui", ui_t)?;
    let m_lock = typed(code, mode_t, "lockCamera", bool_)?;
    let gui_pad = typed(code, gui_t, "padCursor", pad_t)?;
    let pad_locked = typed(code, pad_t, "locked", bool_)?;
    let w_root = typed(code, win_t, "windowRoot", flow_t)?;

    let get_ginv = method(code, ui_t, "getGameInventory")?.findex;
    let ginv_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    want(code, get_ginv, "GameUI.getGameInventory", &[ui_t], ginv_t)?;
    let (gi_inv, gi_inv_t) = field(code, ginv_t, "inventory")?;
    if !is_sub(code, gi_inv_t, obj2_t) {
        bail!("GameInventory.inventory is not an h2d.Object");
    }
    let o_visible = typed(code, obj2_t, "visible", bool_)?;
    let show_inv = method(code, ui_t, "showInventory")?.findex;
    want(
        code,
        show_inv,
        "GameUI.showInventory",
        &[ui_t, bool_],
        bool_,
    )?;
    let set_window = method(code, mode_t, "setWindow")?.findex;
    want(
        code,
        set_window,
        "GameMode.setWindow",
        &[mode_t, win_t],
        void_,
    )?;
    let add_help = method(code, act_t, "addHelpIcon")?.findex;
    want(code, add_help, "Activity.addHelpIcon", &[act_t], void_)?;
    let help_impl = method(code, act_t, "addHelpIcon__impl")?;
    if fun_args(code, help_impl) != [act_t] {
        bail!("unexpected Activity.addHelpIcon__impl signature");
    }
    let help_ret = match help_impl.ops.last() {
        Some(Opcode::Ret { ret }) if help_impl.regs[ret.0 as usize] == void_ => *ret,
        _ => bail!("Activity.addHelpIcon__impl does not end in a void Ret"),
    };
    let (show_tut, tut_args) = {
        let f = method(code, ui_t, "showTutorial")?;
        (f.findex, fun_args(code, f))
    };
    let (tut_t, cb_t) = match tut_args[..] {
        [u, s, o, c] if u == ui_t && s == str_t => (o, c),
        _ => bail!("unexpected GameUI.showTutorial signature"),
    };
    match &code.types[cb_t.0] {
        Type::Fun(f) if f.args.is_empty() && f.ret == void_ => {}
        _ => bail!("showTutorial callback is not () -> Void"),
    }
    if sig(code, show_tut)?.1 != bool_ {
        bail!("showTutorial does not return Bool");
    }
    let net_fade = method(code, ctrl_t, "netFade")?.findex;
    want(
        code,
        net_fade,
        "Controller.netFade",
        &[ctrl_t, bool_, cb_t],
        void_,
    )?;
    let (set_vis, set_vis_pi) = vproto(code, obj2_t, "set_visible")?;
    want(code, set_vis, "Object.set_visible", &[obj2_t, bool_], bool_)?;
    let obj_remove = proto(code, obj2_t, "remove")?;
    want(code, obj_remove, "Object.remove", &[obj2_t], void_)?;

    let println_f = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println_f) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println_f.findex;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;
    let str_cmp = method(code, str_t, "__compare")?.findex;
    match sig(code, str_cmp)? {
        (a, r)
            if r == i32_ && a.len() == 2 && a[0] == str_t && (a[1] == str_t || a[1] == dyn_t) => {}
        _ => bail!("unexpected String.__compare signature"),
    }

    // _start: one Ret; the vanilla prefix (start cost) is untouched.
    let start = method(code, act_t, "_start")?;
    if fun_args(code, start) != [act_t] {
        bail!("unexpected Activity._start signature");
    }
    if matches!(start.ops.first(), Some(Opcode::Call1 { arg0: Reg(0), .. })) {
        bail!("already applied");
    }
    if !matches!(start.ops.first(), Some(Opcode::GetThis { .. })) {
        bail!("Activity._start: unexpected first op");
    }
    // The four writes a watching spectator skips, each the only one of its kind
    // in _start and not a jump target (a gate inserted before it is never jumped
    // over): `prevInventory = ui.showInventory(false)` (the Call2 and the SetThis
    // right after it), `mode.lockCamera = ...`, `padCursor.locked = ...`,
    // `this.cameraSave = ...`.
    let only = |what: &str, hit: &dyn Fn(&Opcode) -> bool| -> Result<usize> {
        let at: Vec<usize> = (0..start.ops.len())
            .filter(|&i| hit(&start.ops[i]))
            .collect();
        match at[..] {
            [i] if jump_targets_any(start, i) => {
                bail!("Activity._start: {what} is a jump target")
            }
            [i] => Ok(i),
            _ => bail!("Activity._start: {} {what}, want 1", at.len()),
        }
    };
    let si_at = only(
        "showInventory",
        &|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == show_inv),
    )?;
    let Opcode::Call2 { dst: si_dst, .. } = start.ops[si_at] else {
        unreachable!()
    };
    if !matches!(start.ops[si_at + 1], Opcode::SetThis { field, src } if field == a_prev && src == si_dst)
        || jump_targets_any(start, si_at + 1)
    {
        bail!("Activity._start: showInventory's result is not stored in prevInventory");
    }
    let lock_at = only(
        "lockCamera",
        &|o| matches!(o, Opcode::SetField { obj, field, .. } if *field == m_lock && start.regs[obj.0 as usize] == mode_t),
    )?;
    let pad_at = only(
        "padCursor.locked",
        &|o| matches!(o, Opcode::SetField { obj, field, .. } if *field == pad_locked && start.regs[obj.0 as usize] == pad_t),
    )?;
    let cam_at = only(
        "cameraSave",
        &|o| matches!(o, Opcode::SetThis { field, .. } if *field == a_cam),
    )?;
    if !start
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::GetThis { field, .. } if *field == a_cc))
    {
        bail!("Activity._start: no changeCamera");
    }
    let start_fi = fun_index(code, start.findex)?;

    // onReady: the closure of _start (bound to this) that calls showTutorial
    let readies: Vec<RefFun> = start
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::InstanceClosure {
                fun, obj: Reg(0), ..
            } => Some(*fun),
            _ => None,
        })
        .filter(|f| {
            fun_index(code, *f)
                .map(|i| diag::calls(&code.functions[i], show_tut))
                .unwrap_or(false)
        })
        .collect();
    let [ready] = readies[..] else {
        bail!(
            "Activity._start: {} onReady closures, want 1",
            readies.len()
        );
    };
    let ready_fi = fun_index(code, ready)?;
    let rf = &code.functions[ready_fi];
    if fun_args(code, rf) != [act_t] {
        bail!("Activity onReady: unexpected signature");
    }
    let tut_at = only_call(rf, show_tut, "showTutorial")?;
    if !matches!(rf.ops[tut_at], Opcode::Call4 { .. }) {
        bail!("Activity onReady: showTutorial is not a Call4");
    }
    let make_fi = closure_passed_to(code, ready_fi, show_tut)?;
    let mf = &code.functions[make_fi];
    if fun_args(code, mf) != [act_t] {
        bail!("Activity window closure: unexpected signature");
    }
    let win_at = only_call(mf, set_window, "setWindow")?;
    if !matches!(mf.ops[win_at], Opcode::Call2 { .. }) {
        bail!("Activity window closure: setWindow is not a Call2");
    }
    let help_at = only_call(mf, add_help, "addHelpIcon")?;
    if !matches!(mf.ops[help_at], Opcode::Call1 { arg0: Reg(0), .. }) {
        bail!("Activity window closure: addHelpIcon is not on this");
    }
    if !diag::calls(mf, method(code, win_t, "netSharing")?.findex) {
        bail!("Activity window closure: no netSharing");
    }

    // onActionDone's end: the only netFade of Activity.hx, with the Activity in a register
    let dbg = debug_file(code, "src/ui/win/Activity.hx")?;
    let fades: Vec<usize> = (0..code.functions.len())
        .filter(|&i| {
            let f = &code.functions[i];
            f.debug_info
                .as_ref()
                .is_some_and(|d| d.first().is_some_and(|(file, _)| *file == dbg))
                && diag::calls(f, net_fade)
        })
        .collect();
    let [fade_fi] = fades[..] else {
        bail!(
            "Activity.hx: {} functions call netFade, want 1",
            fades.len()
        );
    };
    let ff = &code.functions[fade_fi];
    let fade_at = only_call(ff, net_fade, "netFade")?;
    if !matches!(ff.ops[fade_at], Opcode::Call3 { .. }) {
        bail!("onActionDone fade: netFade is not a Call3");
    }
    // the Activity register: last written before the call by an EnumField
    // (the closure's capture), not written again until the call
    let acts: Vec<Reg> = (0..ff.regs.len())
        .map(|r| Reg(r as u32))
        .filter(|r| ff.regs[r.0 as usize] == act_t)
        .filter(|r| {
            matches!(
                ff.ops[..fade_at]
                    .iter()
                    .rev()
                    .find(|o| dst_of(o) == Some(*r)),
                Some(Opcode::EnumField { .. })
            )
        })
        .collect();
    let [fade_act] = acts[..] else {
        bail!(
            "onActionDone fade: {} Activity registers, want 1",
            acts.len()
        );
    };

    // spectEnd: the Sing stage's end half, as onActionDone's closure has it:
    // `target.obj.flags &= ~2`, then `obj.cullingCollider("HandTool")` (a
    // CallMethod on h3d.scene.Object) `.flags |= 2`. Exactly one Activity.hx
    // function calls cullingCollider("HandTool") and then sets a flag with Or.
    let ent_t = obj_type(code, "ent.Entity")?;
    let obj3_t = obj_type(code, "h3d.scene.Object")?;
    let a_target = typed(code, act_t, "target", ent_t)?;
    let e_obj = typed(code, ent_t, "obj", obj3_t)?;
    let o3_flags = typed(code, obj3_t, "flags", i32_)?;
    let act_dbg = debug_file(code, "src/ui/win/Activity.hx")?;
    let mut culls = vec![];
    for f in &code.functions {
        if !f
            .debug_info
            .as_ref()
            .is_some_and(|d| d.first().is_some_and(|(file, _)| *file == act_dbg))
        {
            continue;
        }
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::CallMethod { dst, field, args } = op else {
                continue;
            };
            let [o, s] = args[..] else { continue };
            if f.regs[o.0 as usize] != obj3_t {
                continue;
            }
            let hand = f.ops[..i].iter().rev().find(|w| dst_of(w) == Some(s));
            let is_hand = matches!(hand, Some(Opcode::GetGlobal { global, .. })
                if super::job_xp::const_str(code, *global) == Some("HandTool"));
            let or_after = f.ops[i + 1..(i + 8).min(f.ops.len())]
                .iter()
                .any(|w| matches!(w, Opcode::Or { .. }))
                && matches!(f.ops.get(i + 1), Some(Opcode::JNull { reg, .. }) if reg == dst);
            if is_hand && or_after {
                culls.push(*field);
            }
        }
    }
    let [cull_pi] = culls[..] else {
        bail!(
            "Activity.hx: {} cullingCollider(\"HandTool\") calls before an Or, want 1",
            culls.len()
        );
    };
    let cancel = method(code, act_t, "_cancel__impl")?;
    if fun_args(code, cancel) != [act_t] || jump_targets_any(cancel, 0) {
        bail!("Activity._cancel__impl: unexpected shape");
    }
    let cancel_fi = fun_index(code, cancel.findex)?;
    // its lockCamera = false / padCursor.locked = false (gated for a spectator,
    // who never set them: a watcher's own minigame may hold them now)
    let c_only = |what: &str, want: &dyn Fn(&Opcode) -> bool| -> Result<usize> {
        let at: Vec<usize> = (0..cancel.ops.len()).filter(|&k| want(&cancel.ops[k])).collect();
        match at[..] {
            [k] if !jump_targets_any(cancel, k) => Ok(k),
            _ => bail!("Activity._cancel__impl: not one plain {what} write"),
        }
    };
    let c_lock_at = c_only(
        "lockCamera",
        &|o| matches!(o, Opcode::SetField { obj, field, .. } if *field == m_lock && cancel.regs[obj.0 as usize] == mode_t),
    )?;
    let c_pad_at = c_only(
        "padCursor.locked",
        &|o| matches!(o, Opcode::SetField { obj, field, .. } if *field == pad_locked && cancel.regs[obj.0 as usize] == pad_t),
    )?;

    // Step-out. spectCam = _cancel__impl's ops right after its lockCamera write up
    // to its PlaceView camera call: camera restored from cameraSave, the view's
    // activity camera dropped. Self-contained: jumps stay inside, one BaseType.check.
    let view_t = obj_type(code, "world.PlaceView")?;
    let base_t = obj_type(code, "hl.BaseType")?;
    let base_check = method(code, base_t, "check")?.findex;
    let cam_s = c_lock_at + 1;
    let cam_e = 1
        + (cam_s..cancel.ops.len())
            .find(|&k| {
                matches!(&cancel.ops[k], Opcode::CallMethod { args, .. }
            if args.first().is_some_and(|r| cancel.regs[r.0 as usize] == view_t))
            })
            .context("Activity._cancel__impl: no PlaceView camera call")?;
    let cam_ops = cancel.ops[cam_s..cam_e].to_vec();
    let Some(Opcode::CallMethod { field: cam_pi, .. }) = cam_ops.last().cloned() else {
        unreachable!()
    };
    let inside = (cam_s..cam_e).all(|k| {
        jump_targets(cancel, k)
            .iter()
            .all(|&t| (cam_s..=cam_e).contains(&t))
    }) && (0..cancel.ops.len())
        .filter(|k| !(cam_s..cam_e).contains(k))
        .all(|k| {
            jump_targets(cancel, k)
                .iter()
                .all(|&t| t <= cam_s || t > cam_e)
        });
    let calls_ok = cam_ops
        .iter()
        .all(|o| call_of(o).is_none_or(|(g, _)| g == base_check));
    if !matches!(cam_ops[0], Opcode::GetThis { field, .. } if field == a_game)
        || !cam_ops
            .iter()
            .any(|o| matches!(o, Opcode::GetThis { field, .. } if *field == a_cam))
        || !inside
        || !calls_ok
        || cam_ops
            .iter()
            .filter(|o| matches!(o, Opcode::CallMethod { .. }))
            .count()
            != 1
    {
        bail!("Activity._cancel__impl: unexpected camera restore block");
    }
    let cam_regs = cancel.regs.clone();
    let aw_t = obj_type(code, "ui.win.ActivityWindow")?;
    if !is_sub(code, aw_t, win_t) {
        bail!("ui.win.ActivityWindow is not a ui.Window");
    }
    let aw_cls = class_global(code, "ui.win.ActivityWindow")?;
    let mut cam_wins = vec![];
    for name in [
        "ui.win.LockPick",
        "ui.win.NinePuzzle",
        "ui.win.CylinderPuzzle",
    ] {
        let t = obj_type(code, name)?;
        if !is_sub(code, t, aw_t) {
            bail!("{name} is not an ActivityWindow");
        }
        let activity = typed(code, t, "activity", act_t)?;
        let stop = method(code, t, "stopActivity")?;
        let init = method(code, t, "initInteractive")?;
        for (f, what) in [(stop, "stopActivity"), (init, "initInteractive")] {
            if fun_args(code, f) != [t] || sig(code, f.findex)?.1 != void_ || jump_targets_any(f, 0)
            {
                bail!("{name}.{what}: unexpected shape");
            }
        }
        let init_ret = init.ops.len() - 1;
        if !matches!(init.ops[init_ret], Opcode::Ret { .. })
            || init
                .ops
                .iter()
                .filter(|o| matches!(o, Opcode::Ret { .. }))
                .count()
                != 1
            || jump_targets_any(init, init_ret)
        {
            bail!("{name}.initInteractive: not one plain Ret at its end");
        }
        // the Leave button calls it: `leaveButton.onClick = this.stopActivity` in init
        let ini = method(code, t, "init")?;
        if !ini.ops.iter().any(|o| matches!(o, Opcode::InstanceClosure { fun, obj: Reg(0), .. } if *fun == stop.findex)) {
            bail!("{name}.init: the Leave button is not this.stopActivity");
        }
        cam_wins.push(CamWin {
            cls: class_global(code, name)?,
            activity,
            stop_fi: fun_index(code, stop.findex)?,
            init_fi: fun_index(code, init.findex)?,
            init_ret,
        });
    }
    let flow_int = typed(code, flow_t, "interactive", int_t)?;
    let int_prop = typed(code, int_t, "propagateEvents", bool_)?;
    let m_cur = typed(code, mode_t, "currentWindow", win_t)?;
    // Game.update, Escape: for each window w (top first):
    //   NullCheck w; c = w.canEscape(); JFalse c -> modal test;
    //   h = w.__host; JNull h -> esc; isAuth; JFalse -> found; esc: w.escape();
    //   found = true; ...; JAlways break.   The modal test ends in the back edge.
    let up = method(code, game_t, "update")?;
    let o = &up.ops;
    let sites: Vec<usize> = (1..o.len().saturating_sub(9))
        .filter(|&k| {
            let Opcode::CallMethod { dst: c, args, .. } = &o[k] else {
                return false;
            };
            let [w] = args[..] else { return false };
            up.regs[w.0 as usize] == win_t
                && matches!(o[k - 1], Opcode::NullCheck { reg } if reg == w)
                && matches!(o[k + 1], Opcode::JFalse { cond, .. } if cond == *c)
                && matches!(o[k + 2], Opcode::Field { obj, .. } if obj == w)
                && matches!(o[k + 3], Opcode::JNull { .. })
                && matches!(o[k + 4], Opcode::GetThis { field, .. } if field == g_auth)
                && matches!(o[k + 5], Opcode::JFalse { .. })
                && matches!(&o[k + 6], Opcode::CallMethod { args, .. } if args[..] == [w])
                && matches!(
                    o[k + 7],
                    Opcode::Bool {
                        value: ValBool(true),
                        ..
                    }
                )
        })
        .collect();
    let [k] = sites[..] else {
        bail!("Game.update: {} Escape window loops, want 1", sites.len());
    };
    let Opcode::CallMethod { args, .. } = &o[k] else {
        unreachable!()
    };
    let esc_w = args[0];
    let modal_at = jump_targets(up, k + 1)[0];
    let esc_next = (modal_at..o.len())
        .find(|&j| matches!(o[j], Opcode::JAlways { offset } if offset < 0))
        .context("Game.update: no Escape loop back edge")?;
    if jump_targets_any(up, k - 1) || jump_targets(up, esc_next)[0] > k - 1 {
        bail!("Game.update: unexpected Escape loop shape");
    }

    Ok(Plan {
        act_t,
        game_t,
        mode_t,
        ui_t,
        unit_t,
        player_t,
        host_t,
        int_t,
        win_t,
        flow_t,
        ctrl_t,
        inf_t,
        cb_t,
        tut_t,
        str_t,
        dyn_t,
        bool_,
        i32_,
        void_,
        a_game,
        a_host,
        a_unit,
        a_int,
        a_cc,
        a_prev,
        a_inf,
        i_id,
        u_owner,
        g_me,
        g_auth,
        g_mode,
        p_locked: field(code, player_t, "lockedWith")?,
        g_gui,
        g_ui,
        m_lock,
        gui_pad,
        pad_locked,
        w_root,
        set_window,
        add_help,
        show_tut,
        net_fade,
        set_vis,
        set_vis_pi,
        obj_remove,
        println,
        std_string,
        str_add,
        str_cmp,
        start_fi,
        si_at,
        ent_t,
        obj3_t,
        a_target,
        e_obj,
        o3_flags,
        cull_pi,
        cancel_fi,
        c_lock_at,
        c_pad_at,
        a_cam,
        cam_t,
        lock_at,
        pad_at,
        cam_at,
        ginv_t,
        obj2_t,
        get_ginv,
        gi_inv,
        o_visible,
        ready_fi,
        tut_at,
        make_fi,
        win_at,
        help_at,
        help_fi: fun_index(code, help_impl.findex)?,
        help_ret,
        fade_fi,
        fade_at,
        fade_act,
        dbg_file: dbg,
        cam_ops,
        cam_regs,
        cam_pi,
        base_check,
        aw_cls,
        cam_wins,
        flow_int,
        int_prop,
        m_cur,
        gui_t,
        pad_t,
        esc_fi: fun_index(code, up.findex)?,
        esc_at: k - 1,
        esc_w,
        esc_found: k + 7,
        esc_next,
    })
}

fn int(a: &mut Asm, code: &mut Bytecode, dst: Reg, v: i32) {
    let ptr = int_const(code, v);
    a.op(Opcode::Int { dst, ptr });
}

/// `spect(act) -> Bool`: this machine watches `act` (see the header).
fn add_spect(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (res, exc, host, unit, owner, game, me) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.host_t),
        r.r(p.unit_t),
        r.r(p.player_t),
        r.r(p.game_t),
        r.r(p.player_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    for (dst, obj, field) in [
        (host, act, p.a_host),
        (unit, act, p.a_unit),
        (owner, unit, p.u_owner),
        (game, act, p.a_game),
    ] {
        a.op(Opcode::Field { dst, obj, field });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "untrap",
        );
    }
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me,
    });
    a.jmp(
        Opcode::JEq {
            a: owner,
            b: me,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.act_t], p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectPre(act) -> Bool`: true when this machine watches `act` from its own
/// camera (a spectator, not a CAMERA_ACTIVITIES one): changeCamera = false, the
/// chooseUnit full-screen wait Interactive (added on every client before the
/// start) removed, `mp: spectate <id> 1`. A spectator of a camera activity
/// logs `<id> 0` and gets false (vanilla start). Everyone else: false, no log.
/// _start then skips its own camera / input / inventory writes when true.
fn add_pre(
    code: &mut Bytecode,
    p: &Plan,
    spect: RefFun,
    stage: RefFun,
    tag: RefGlobal,
    sp: RefGlobal,
    cam_ids: &[RefGlobal],
) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (g, b, exc, k, z, it, v) = (
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.int_t),
        r.r(p.void_),
    );
    let (gm, md, pm, lw) = (
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.player_t),
        r.r(p.p_locked.1),
    );
    let (inf, id, acc, t, d, m) = (
        r.r(p.inf_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.i32_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: g,
        value: ValBool(false),
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    int(&mut a, code, m, 0);
    a.op(Opcode::Null { dst: id });
    a.op(Opcode::Field {
        dst: inf,
        obj: act,
        field: p.a_inf,
    });
    a.jmp(
        Opcode::JNull {
            reg: inf,
            offset: 0,
        },
        "watch",
    );
    a.op(Opcode::Field {
        dst: id,
        obj: inf,
        field: p.i_id,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "watch");
    // a watcher busy with its own minigame / lock (mode.lockCamera set by its
    // own activity, me.lockedWith): its own camera, never pulled into the view
    a.op(Opcode::Field {
        dst: gm,
        obj: act,
        field: p.a_game,
    });
    a.jmp(Opcode::JNull { reg: gm, offset: 0 }, "watch");
    a.op(Opcode::Field {
        dst: md,
        obj: gm,
        field: p.g_mode,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "watch");
    a.op(Opcode::Field {
        dst: b,
        obj: md,
        field: p.m_lock,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "watch");
    a.op(Opcode::Field {
        dst: pm,
        obj: gm,
        field: p.g_me,
    });
    a.jmp(Opcode::JNull { reg: pm, offset: 0 }, "watch");
    a.op(Opcode::Field {
        dst: lw,
        obj: pm,
        field: p.p_locked.0,
    });
    a.jmp(Opcode::JNotNull { reg: lw, offset: 0 }, "watch");
    // an idle watcher of a camera mini-game: the vanilla view (camera on the
    // lock / board, the window), its input and camera stay free (_start skips
    // lockCamera / padCursor.locked for every spectator)
    int(&mut a, code, z, 0);
    for gl in cam_ids {
        a.op(Opcode::GetGlobal {
            dst: t,
            global: *gl,
        });
        a.op(Opcode::Call2 {
            dst: k,
            fun: p.str_cmp,
            arg0: id,
            arg1: t,
        });
        a.jmp(
            Opcode::JEq {
                a: k,
                b: z,
                offset: 0,
            },
            "int",
        );
    }
    // watching from its own camera: no camera change, the wait Interactive
    // gone; the bard's stage shown as the skipped fade-in would
    a.label("watch");
    a.op(Opcode::Call1 {
        dst: v,
        fun: stage,
        arg0: act,
    });
    int(&mut a, code, m, 1);
    a.op(Opcode::Bool {
        dst: g,
        value: ValBool(true),
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_cc,
        src: b,
    });
    // every spectator loses the chooseUnit wait Interactive (10000 x 10000 on
    // s2d: a camera watcher kept it, so the whole screen took no click)
    a.label("int");
    a.op(Opcode::Field {
        dst: it,
        obj: act,
        field: p.a_int,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "log");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.obj_remove,
        arg0: it,
    });
    a.op(Opcode::Null { dst: it });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_int,
        src: it,
    });
    // println("mp: spectate " + inf.id + " " + m)
    a.label("log");
    a.op(Opcode::GetGlobal {
        dst: acc,
        global: tag,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "num");
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: id,
    });
    a.label("num");
    a.op(Opcode::GetGlobal { dst: t, global: sp });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: t,
    });
    a.op(Opcode::ToDyn { dst: d, src: m });
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.std_string,
        arg0: d,
    });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: t,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: acc,
    });
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: g });
    push_fn(code, vec![p.act_t], p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectInv(act) -> Bool`: whether this machine's inventory panel is shown now
/// (`game.ui.getGameInventory().inventory.visible`, what showInventory would
/// return as the previous state); false when unknown. A gated spectator's
/// `prevInventory`, so the vanilla end (`showInventory(prevInventory)`) keeps it.
fn add_inv(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (res, exc, game, ui, gi, inv) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.game_t),
        r.r(p.ui_t),
        r.r(p.ginv_t),
        r.r(p.obj2_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: game,
        obj: act,
        field: p.a_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.g_ui,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "untrap");
    a.op(Opcode::Call1 {
        dst: gi,
        fun: p.get_ginv,
        arg0: ui,
    });
    a.jmp(Opcode::JNull { reg: gi, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: inv,
        obj: gi,
        field: p.gi_inv,
    });
    a.jmp(
        Opcode::JNull {
            reg: inv,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: res,
        obj: inv,
        field: p.o_visible,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.act_t], p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectTut(ui, id, opts, cb, act) -> Bool`: a spectator gets no tutorial
/// (false); cb (the window closure) runs at once on the host only. The host
/// playing in co-op: cb at once, then the tutorial without callback (over the window). A client
/// spectator never builds the mini-game window: its constructor / netSharing
/// are owner- or host-only (GatherAction register, FishingAction
/// keepOnGathered, LockPick lockData) and threw out of `_start`.
fn add_tut(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let args = vec![p.ui_t, p.str_t, p.tut_t, p.cb_t, p.act_t];
    let mut r = Regs(args.clone());
    let (b, v, game) = (r.r(p.bool_), r.r(p.void_), r.r(p.game_t));
    let (host, nc) = (r.r(p.host_t), r.r(p.cb_t));
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: Reg(4),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(4),
        field: p.a_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "none",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "none");
    a.op(Opcode::CallClosure {
        dst: v,
        fun: Reg(3),
        args: vec![],
    });
    // no tutorial shown
    a.label("none");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    // the player in co-op: the mini-game window first, the tutorial over it (no
    // callback): the minigame exists while its tutorial is read, so another
    // player's window waits and a party move waits (force_leave flMinigame /
    // flActivity), instead of a gap before the window
    a.label("vanilla");
    a.op(Opcode::Field {
        dst: host,
        obj: Reg(4),
        field: p.a_host,
    });
    a.jmp(Opcode::JNull { reg: host, offset: 0 }, "solo");
    // the host only: a client's window registers once its ownership arrived
    // (cb run early threw "Can't register ... without ownership"); a client's
    // minigame is covered meanwhile by the host's own copy / start record
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(4),
        field: p.a_game,
    });
    a.jmp(Opcode::JNull { reg: game, offset: 0 }, "solo");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "solo");
    a.op(Opcode::CallClosure {
        dst: v,
        fun: Reg(3),
        args: vec![],
    });
    a.op(Opcode::Null { dst: nc });
    a.op(Opcode::Call4 {
        dst: b,
        fun: p.show_tut,
        arg0: Reg(0),
        arg1: Reg(1),
        arg2: Reg(2),
        arg3: nc,
    });
    // the window is built: true, so the caller never runs cb a second time
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("solo");
    a.op(Opcode::Call4 {
        dst: b,
        fun: p.show_tut,
        arg0: Reg(0),
        arg1: Reg(1),
        arg2: Reg(2),
        arg3: Reg(3),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, args, p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectQuiet(act) -> Bool`, at _cancel__impl entry: true on a spectator (the
/// end then skips lockCamera / padCursor.locked = false: it never set them);
/// a spectator now busy with its own minigame or lock (mode.lockCamera,
/// me.lockedWith) also loses `cameraSave`, so the end leaves its camera alone.
fn add_quiet(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let (b, gm, md, pm, lw, cs) = (
        r.r(p.bool_),
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.player_t),
        r.r(p.p_locked.1),
        r.r(p.cam_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: gm,
        obj: Reg(0),
        field: p.a_game,
    });
    a.jmp(Opcode::JNull { reg: gm, offset: 0 }, "yes");
    a.op(Opcode::Field {
        dst: md,
        obj: gm,
        field: p.g_mode,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "lw");
    a.op(Opcode::Field {
        dst: b,
        obj: md,
        field: p.m_lock,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "busy");
    a.label("lw");
    a.op(Opcode::Field {
        dst: pm,
        obj: gm,
        field: p.g_me,
    });
    a.jmp(Opcode::JNull { reg: pm, offset: 0 }, "yes");
    a.op(Opcode::Field {
        dst: lw,
        obj: pm,
        field: p.p_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "yes");
    a.label("busy");
    a.op(Opcode::Null { dst: cs });
    a.op(Opcode::SetField {
        obj: Reg(0),
        field: p.a_cam,
        src: cs,
    });
    a.label("yes");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![p.act_t], p.bool_, r.0, a.finish(), p.dbg_file)
}
/// `spectWin(mode, win, act)`: a spectator's window is hidden, never the current one.
fn add_win(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let args = vec![p.mode_t, p.win_t, p.act_t];
    let mut r = Regs(args.clone());
    let (mode, win, act) = (Reg(0), Reg(1), Reg(2));
    let (b, exc, root, f, v) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.flow_t),
        r.r(p.bool_),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    // a failure while hiding shows the window the vanilla way
    a.jmp(Opcode::Trap { exc, offset: 0 }, "vanilla");
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    let hide = |a: &mut Asm, o: Reg| {
        if p.set_vis_pi >= 0 {
            a.op(Opcode::CallMethod {
                dst: b,
                field: RefField(p.set_vis_pi as usize),
                args: vec![o, f],
            });
        } else {
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.set_vis,
                arg0: o,
                arg1: f,
            });
        }
    };
    a.op(Opcode::Field {
        dst: root,
        obj: win,
        field: p.w_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "self",
    );
    hide(&mut a, root);
    a.label("self");
    hide(&mut a, win);
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("vanilla");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_window,
        arg0: mode,
        arg1: win,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, args, p.void_, r.0, a.finish(), p.dbg_file)
}

/// `spectHelp(act)`: addHelpIcon() unless this machine is a spectator.
fn add_help(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let (b, v) = (r.r(p.bool_), r.r(p.void_));
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.add_help,
        arg0: Reg(0),
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.act_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `spectCam(act)`: _cancel__impl's own camera restore (cameraSave) and PlaceView
/// camera reset, copied with its register file (see Plan::cam_ops).
fn add_cam(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut ops = p.cam_ops.clone();
    let mut regs = p.cam_regs.clone();
    regs.push(p.void_);
    ops.push(Opcode::Ret {
        ret: Reg((regs.len() - 1) as u32),
    });
    push_fn(code, vec![p.act_t], p.void_, regs, ops, p.dbg_file)
}

/// `spectAct(w) -> Activity`: the activity of a CAMERA_ACTIVITIES window, else null.
fn add_act_of(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.win_t]);
    let (b, act) = (r.r(p.bool_), r.r(p.act_t));
    let cw: Vec<(Reg, Reg)> = p
        .cam_wins
        .iter()
        .map(|c| (r.r(c.cls.1), r.r(obj_t_of(code, c))))
        .collect();
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: act });
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "ret",
    );
    let next = ["n0", "n1", "n2"];
    for (i, (c, (cls, w))) in p.cam_wins.iter().zip(&cw).enumerate() {
        a.op(Opcode::GetGlobal {
            dst: *cls,
            global: c.cls.0,
        });
        a.op(Opcode::Call2 {
            dst: b,
            fun: p.base_check,
            arg0: *cls,
            arg1: Reg(0),
        });
        a.jmp(Opcode::JFalse { cond: b, offset: 0 }, next[i]);
        a.op(Opcode::SafeCast {
            dst: *w,
            src: Reg(0),
        });
        a.op(Opcode::Field {
            dst: act,
            obj: *w,
            field: c.activity,
        });
        a.jmp(Opcode::JAlways { offset: 0 }, "ret");
        a.label(next[i]);
    }
    a.label("ret");
    a.op(Opcode::Ret { ret: act });
    push_fn(code, vec![p.win_t], p.act_t, r.0, a.finish(), p.dbg_file)
}

/// The window type of a CamWin (its `activity` field's owner).
fn obj_t_of(code: &Bytecode, c: &CamWin) -> RefType {
    code.functions[c.stop_fi].regs[0]
}

/// `spectOut(win, act) -> Bool`: the watcher steps out of the live view of
/// another player's minigame, on this machine only (nothing is sent): the
/// copy is hidden (window and windowRoot: no draw, no clicks; it stays
/// registered and updated), no longer the mode's current window, and an idle
/// watcher's camera comes back (spectCam: _cancel__impl's own restore, then
/// cameraSave = null so the end leaves it alone); a watcher busy with its own
/// minigame or lock keeps its camera. The chooseUnit full-screen Interactive
/// goes too. `mp: spectate <id> out`. False (nothing done) unless spect(act).
fn add_out(code: &mut Bytecode, p: &Plan, spect: RefFun, cam: RefFun) -> Result<RefFun> {
    let args = vec![p.win_t, p.act_t];
    let mut r = Regs(args.clone());
    let (win, act) = (Reg(0), Reg(1));
    let (res, b, f, exc, root, gm, md, cur, cs, pm, lw, it, v) = (
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.flow_t),
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.win_t),
        r.r(p.cam_t),
        r.r(p.player_t),
        r.r(p.p_locked.1),
        r.r(p.int_t),
        r.r(p.void_),
    );
    let (inf, id, s, t) = (r.r(p.inf_t), r.r(p.str_t), r.r(p.str_t), r.r(p.str_t));
    let (gui, pad) = (r.r(p.gui_t), r.r(p.pad_t));
    let tag = str_global(code, p.str_t, "mp: spectate ");
    let out = str_global(code, p.str_t, " out");
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    let hide = |a: &mut Asm, o: Reg| {
        if p.set_vis_pi >= 0 {
            a.op(Opcode::CallMethod {
                dst: b,
                field: RefField(p.set_vis_pi as usize),
                args: vec![o, f],
            });
        } else {
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.set_vis,
                arg0: o,
                arg1: f,
            });
        }
    };
    a.op(Opcode::Field {
        dst: root,
        obj: win,
        field: p.w_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "self",
    );
    hide(&mut a, root);
    a.label("self");
    hide(&mut a, win);
    a.op(Opcode::Field {
        dst: gm,
        obj: act,
        field: p.a_game,
    });
    a.jmp(Opcode::JNull { reg: gm, offset: 0 }, "int");
    a.op(Opcode::Field {
        dst: md,
        obj: gm,
        field: p.g_mode,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "int");
    a.op(Opcode::Field {
        dst: cur,
        obj: md,
        field: p.m_cur,
    });
    a.jmp(
        Opcode::JNotEq {
            a: cur,
            b: win,
            offset: 0,
        },
        "cam",
    );
    a.op(Opcode::Null { dst: cur });
    a.op(Opcode::SetField {
        obj: md,
        field: p.m_cur,
        src: cur,
    });
    // the camera: an idle watcher's (cameraSave set, no lock of its own)
    a.label("cam");
    a.op(Opcode::Field {
        dst: cs,
        obj: act,
        field: p.a_cam,
    });
    a.jmp(Opcode::JNull { reg: cs, offset: 0 }, "int");
    // busy: its own minigame locks the pad cursor (_start, never for a
    // spectator); mode.lockCamera is the watched view's own (doSetCamera)
    a.op(Opcode::Field {
        dst: gui,
        obj: gm,
        field: p.g_gui,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "lw",
    );
    a.op(Opcode::Field {
        dst: pad,
        obj: gui,
        field: p.gui_pad,
    });
    a.jmp(
        Opcode::JNull {
            reg: pad,
            offset: 0,
        },
        "lw",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: pad,
        field: p.pad_locked,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "drop");
    a.label("lw");
    a.op(Opcode::Field {
        dst: pm,
        obj: gm,
        field: p.g_me,
    });
    a.jmp(Opcode::JNull { reg: pm, offset: 0 }, "drop");
    a.op(Opcode::Field {
        dst: lw,
        obj: pm,
        field: p.p_locked.0,
    });
    a.jmp(Opcode::JNotNull { reg: lw, offset: 0 }, "drop");
    a.op(Opcode::Call1 {
        dst: v,
        fun: cam,
        arg0: act,
    });
    a.label("drop");
    a.op(Opcode::Null { dst: cs });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_cam,
        src: cs,
    });
    a.label("int");
    a.op(Opcode::Field {
        dst: it,
        obj: act,
        field: p.a_int,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "log");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.obj_remove,
        arg0: it,
    });
    a.op(Opcode::Null { dst: it });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_int,
        src: it,
    });
    a.label("log");
    a.op(Opcode::GetGlobal {
        dst: s,
        global: tag,
    });
    a.op(Opcode::Field {
        dst: inf,
        obj: act,
        field: p.a_inf,
    });
    a.jmp(
        Opcode::JNull {
            reg: inf,
            offset: 0,
        },
        "tail",
    );
    a.op(Opcode::Field {
        dst: id,
        obj: inf,
        field: p.i_id,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "tail");
    a.op(Opcode::Call2 {
        dst: s,
        fun: p.str_add,
        arg0: s,
        arg1: id,
    });
    a.label("tail");
    a.op(Opcode::GetGlobal {
        dst: t,
        global: out,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: p.str_add,
        arg0: s,
        arg1: t,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s,
    });
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, args, p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectEsc(w) -> Int`, Game.update's Escape loop for window `w` and the
/// watcher's Leave button: 1 = `w` was another player's live minigame copy on
/// this machine and the watcher stepped out (spectOut): Escape is used up;
/// 2 = `w` is a hidden ActivityWindow (a spectator's copy: spectWin, or one
/// stepped out of): not this player's, Escape goes on to the next window
/// (vanilla found it first: a client did nothing, the host ran the other
/// player's minigame escape); 0 = vanilla.
fn add_esc(code: &mut Bytecode, p: &Plan, act_of: RefFun, out: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.win_t]);
    let (k, b, cls, root, act) = (
        r.r(p.i32_),
        r.r(p.bool_),
        r.r(p.aw_cls.1),
        r.r(p.flow_t),
        r.r(p.act_t),
    );
    let mut a = Asm::new();
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.aw_cls.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cls,
        arg1: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: root,
        obj: Reg(0),
        field: p.w_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "act",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: root,
        field: p.o_visible,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "act");
    int(&mut a, code, k, 2);
    a.jmp(Opcode::JAlways { offset: 0 }, "ret");
    a.label("act");
    a.op(Opcode::Call1 {
        dst: act,
        fun: act_of,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JNull {
            reg: act,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: out,
        arg0: Reg(0),
        arg1: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    int(&mut a, code, k, 1);
    a.label("ret");
    a.op(Opcode::Ret { ret: k });
    push_fn(code, vec![p.win_t], p.i32_, r.0, a.finish(), p.dbg_file)
}

/// `spectInt(w)`, at the end of a CAMERA_ACTIVITIES window's initInteractive:
/// on a watcher its full-window input catcher (windowRoot.interactive, the
/// player's lock / board input) lets every event through (propagateEvents),
/// so the watcher's clicks reach its own scene; the window's buttons keep theirs.
fn add_int(code: &mut Bytecode, p: &Plan, spect: RefFun, act_of: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.win_t]);
    let (act, b, root, it, v) = (
        r.r(p.act_t),
        r.r(p.bool_),
        r.r(p.flow_t),
        r.r(p.int_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: act,
        fun: act_of,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JNull {
            reg: act,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: root,
        obj: Reg(0),
        field: p.w_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: it,
        obj: root,
        field: p.flow_int,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "ret");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetField {
        obj: it,
        field: p.int_prop,
        src: b,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.win_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// Vanilla's Sing stage pair on a spectator of the bard's song: `_start`'s
/// fade-in shows the stage (`target.obj.flags |= 2`, the parent of the unit's
/// unitView: the bard) and hides its `HandTool`; onActionDone's closure does
/// the reverse. A spectator runs neither (changeCamera = false; a client has no
/// window), so `show` builds `spectStage(act)` (the start half, from spectPre)
/// and `!show` builds `spectEnd(act)` (the end half, at `_cancel__impl` entry,
/// the end every machine runs). Idempotent where the closures run as well.
fn add_stage(
    code: &mut Bytecode,
    p: &Plan,
    spect: RefFun,
    sing: RefGlobal,
    hand: RefGlobal,
    show: bool,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (b, exc, inf, id, t, k, z, tgt, o, h, fl, m, v) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.inf_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.ent_t),
        r.r(p.obj3_t),
        r.r(p.obj3_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: inf,
        obj: act,
        field: p.a_inf,
    });
    a.jmp(
        Opcode::JNull {
            reg: inf,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: id,
        obj: inf,
        field: p.i_id,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "untrap");
    a.op(Opcode::GetGlobal {
        dst: t,
        global: sing,
    });
    a.op(Opcode::Call2 {
        dst: k,
        fun: p.str_cmp,
        arg0: id,
        arg1: t,
    });
    int(&mut a, code, z, 0);
    a.jmp(
        Opcode::JNotEq {
            a: k,
            b: z,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: tgt,
        obj: act,
        field: p.a_target,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: o,
        obj: tgt,
        field: p.e_obj,
    });
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: fl,
        obj: o,
        field: p.o3_flags,
    });
    // visible bit (2): set on one of stage / HandTool, cleared on the other
    let flip = |a: &mut Asm, code: &mut Bytecode, set: bool| {
        if set {
            int(a, code, m, 2);
            a.op(Opcode::Or {
                dst: fl,
                a: fl,
                b: m,
            });
        } else {
            int(a, code, m, !2);
            a.op(Opcode::And {
                dst: fl,
                a: fl,
                b: m,
            });
        }
    };
    flip(&mut a, code, show);
    a.op(Opcode::SetField {
        obj: o,
        field: p.o3_flags,
        src: fl,
    });
    a.op(Opcode::GetGlobal {
        dst: t,
        global: hand,
    });
    a.op(Opcode::CallMethod {
        dst: h,
        field: p.cull_pi,
        args: vec![o, t],
    });
    a.jmp(Opcode::JNull { reg: h, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: fl,
        obj: h,
        field: p.o3_flags,
    });
    flip(&mut a, code, !show);
    a.op(Opcode::SetField {
        obj: h,
        field: p.o3_flags,
        src: fl,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.act_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `spectFade(ctrl, replicated, cb, act)`: no replicated fade when the others
/// watch from their own camera (the activity has a unit with an owner).
fn add_fade(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let args = vec![p.ctrl_t, p.bool_, p.cb_t, p.act_t];
    let mut r = Regs(args.clone());
    let (ctrl, rep, cb, act) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let (exc, unit, owner, v) = (r.r(p.dyn_t), r.r(p.unit_t), r.r(p.player_t), r.r(p.void_));
    let mut a = Asm::new();
    a.jmp(
        Opcode::JFalse {
            cond: rep,
            offset: 0,
        },
        "call",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "call");
    a.op(Opcode::Field {
        dst: unit,
        obj: act,
        field: p.a_unit,
    });
    a.jmp(
        Opcode::JNull {
            reg: unit,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: owner,
        obj: unit,
        field: p.u_owner,
    });
    a.jmp(
        Opcode::JNull {
            reg: owner,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Bool {
        dst: rep,
        value: ValBool(false),
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("call");
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.net_fade,
        arg0: ctrl,
        arg1: rep,
        arg2: cb,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, args, p.void_, r.0, a.finish(), p.dbg_file)
}

/// New functions, in this order: spect, spectStage, spectPre, spectInv,
/// spectTut, spectWin, spectHelp, spectFade, spectEnd, (spectQuiet), spectCam,
/// spectAct, spectOut, spectEsc, spectInt (the returned array skips spectQuiet).
fn apply(code: &mut Bytecode, p: &Plan) -> Result<[RefFun; 14]> {
    let tag = str_global(code, p.str_t, "mp: spectate ");
    let sp = str_global(code, p.str_t, " ");
    let spect = add_spect(code, p)?;
    let sing = str_global(code, p.str_t, "Sing");
    let hand = str_global(code, p.str_t, "HandTool");
    let stage = add_stage(code, p, spect, sing, hand, true)?;
    let cam_ids: Vec<RefGlobal> = CAMERA_ACTIVITIES
        .iter()
        .map(|s| str_global(code, p.str_t, s))
        .collect();
    let pre = add_pre(code, p, spect, stage, tag, sp, &cam_ids)?;
    let inv = add_inv(code, p)?;
    let tut = add_tut(code, p, spect)?;
    let win = add_win(code, p, spect)?;
    let help = add_help(code, p, spect)?;
    let fade = add_fade(code, p)?;
    let end = add_stage(code, p, spect, sing, hand, false)?;

    // _cancel__impl: spectEnd(this) first (a spectator's end of the Sing stage),
    // then q = spectQuiet(this); "if (!q)" over its lockCamera / padCursor writes
    let quiet = add_quiet(code, p, spect)?;
    {
        let f = &mut code.functions[p.cancel_fi];
        f.regs.push(p.void_);
        let v = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.bool_);
        let q = Reg((f.regs.len() - 1) as u32);
        let mut sites = [p.c_lock_at, p.c_pad_at];
        sites.sort_unstable_by(|x, y| y.cmp(x));
        for at in sites {
            insert_ops(f, at, vec![Opcode::JTrue { cond: q, offset: 1 }]);
        }
        insert_ops(
            f,
            0,
            vec![
                Opcode::Call1 {
                    dst: v,
                    fun: end,
                    arg0: Reg(0),
                },
                Opcode::Call1 {
                    dst: q,
                    fun: quiet,
                    arg0: Reg(0),
                },
            ],
        );
    }
    // _start: s = spect(this), g = spectPre(this) at entry; the vanilla writes
    // a spectator must not get are skipped, the rest of _start runs as is:
    //   prevInventory = s ? spectInv(this) : ui.showInventory(false)
    //     (every spectator: a client spectator of a camera activity never runs
    //     the end that shows the inventory again, onActionDone's closure)
    //   if (!s) mode.lockCamera = true; if (!s) padCursor.locked = true;
    //   if (!g) this.cameraSave = <camera snapshot>   (g: watching from its
    //     own camera; an idle camera activity's watcher gets the vanilla camera,
    //     restored at the end, with free input)
    // Inserted from the last site up, so every earlier index still holds.
    {
        let f = &mut code.functions[p.start_fi];
        f.regs.push(p.bool_);
        let s = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.bool_);
        let g = Reg((f.regs.len() - 1) as u32);
        let Opcode::Call2 { dst: si_dst, .. } = f.ops[p.si_at] else {
            unreachable!()
        };
        // cameraSave: only a watcher on its own camera skips it (a camera
        // watcher's view is restored at the end); lockCamera / padCursor.locked:
        // every spectator skips them (input and camera stay free)
        let mut sites = [(p.cam_at, g), (p.pad_at, s), (p.lock_at, s)];
        sites.sort_by(|x, y| y.0.cmp(&x.0));
        for (at, cond) in sites {
            insert_ops(f, at, vec![Opcode::JTrue { cond, offset: 1 }]);
        }
        insert_ops(
            f,
            p.si_at,
            vec![
                Opcode::JFalse { cond: s, offset: 2 },
                Opcode::Call1 {
                    dst: si_dst,
                    fun: inv,
                    arg0: Reg(0),
                },
                Opcode::JAlways { offset: 1 },
            ],
        );
        insert_ops(
            f,
            0,
            vec![
                Opcode::Call1 {
                    dst: s,
                    fun: spect,
                    arg0: Reg(0),
                },
                Opcode::Call1 {
                    dst: g,
                    fun: pre,
                    arg0: Reg(0),
                },
            ],
        );
    }
    // onReady: showTutorial(ui, id, opts, cb) -> spectTut(ui, id, opts, cb, this)
    {
        let f = &mut code.functions[p.ready_fi];
        let Opcode::Call4 {
            dst,
            arg0,
            arg1,
            arg2,
            arg3,
            ..
        } = f.ops[p.tut_at]
        else {
            unreachable!()
        };
        f.ops[p.tut_at] = Opcode::CallN {
            dst,
            fun: tut,
            args: vec![arg0, arg1, arg2, arg3, Reg(0)],
        };
    }
    // window closure: setWindow(mode, win) -> spectWin(mode, win, this);
    // addHelpIcon(this) -> spectHelp(this)
    {
        let f = &mut code.functions[p.make_fi];
        let Opcode::Call2 {
            dst, arg0, arg1, ..
        } = f.ops[p.win_at]
        else {
            unreachable!()
        };
        f.ops[p.win_at] = Opcode::Call3 {
            dst,
            fun: win,
            arg0,
            arg1,
            arg2: Reg(0),
        };
        let Opcode::Call1 { dst, .. } = f.ops[p.help_at] else {
            unreachable!()
        };
        f.ops[p.help_at] = Opcode::Call1 {
            dst,
            fun: help,
            arg0: Reg(0),
        };
    }
    // addHelpIcon__impl: if (spect(this)) return;
    {
        let f = &mut code.functions[p.help_fi];
        let ret = p.help_ret;
        f.regs.push(p.bool_);
        let b = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            0,
            vec![
                Opcode::Call1 {
                    dst: b,
                    fun: spect,
                    arg0: Reg(0),
                },
                Opcode::JFalse { cond: b, offset: 1 },
                Opcode::Ret { ret },
            ],
        );
    }
    // onActionDone's fade: netFade(ctrl, coop, cb) -> spectFade(ctrl, coop, cb, act)
    {
        let f = &mut code.functions[p.fade_fi];
        let Opcode::Call3 {
            dst,
            arg0,
            arg1,
            arg2,
            ..
        } = f.ops[p.fade_at]
        else {
            unreachable!()
        };
        f.ops[p.fade_at] = Opcode::Call4 {
            dst,
            fun: fade,
            arg0,
            arg1,
            arg2,
            arg3: p.fade_act,
        };
    }
    // Step-out of a watched camera minigame (this machine only)
    let cam = add_cam(code, p)?;
    let act_of = add_act_of(code, p)?;
    let out = add_out(code, p, spect, cam)?;
    let esc = add_esc(code, p, act_of, out)?;
    let wint = add_int(code, p, spect, act_of)?;
    // Game.update's Escape loop: k = spectEsc(w); 1 -> found (break); 2 -> next window
    {
        let one = int_const(code, 1);
        let two = int_const(code, 2);
        let f = &mut code.functions[p.esc_fi];
        f.regs.push(p.i32_);
        let k = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.i32_);
        let c = Reg((f.regs.len() - 1) as u32);
        let at = p.esc_at;
        let n = 5;
        let to = |from: usize, target: usize| (target + n) as i32 - from as i32 - 1;
        insert_ops(
            f,
            at,
            vec![
                Opcode::Call1 {
                    dst: k,
                    fun: esc,
                    arg0: p.esc_w,
                },
                Opcode::Int { dst: c, ptr: one },
                Opcode::JEq {
                    a: k,
                    b: c,
                    offset: to(at + 2, p.esc_found),
                },
                Opcode::Int { dst: c, ptr: two },
                Opcode::JEq {
                    a: k,
                    b: c,
                    offset: to(at + 4, p.esc_next),
                },
            ],
        );
    }
    // the Leave button (stopActivity): `if (spectEsc(this) != 0) return;`;
    // initInteractive: spectInt(this) before its Ret
    {
        let zero = int_const(code, 0);
        for c in &p.cam_wins {
            let f = &mut code.functions[c.stop_fi];
            f.regs.push(p.i32_);
            let k = Reg((f.regs.len() - 1) as u32);
            f.regs.push(p.i32_);
            let z = Reg((f.regs.len() - 1) as u32);
            f.regs.push(p.void_);
            let v = Reg((f.regs.len() - 1) as u32);
            insert_ops(
                f,
                0,
                vec![
                    Opcode::Call1 {
                        dst: k,
                        fun: esc,
                        arg0: Reg(0),
                    },
                    Opcode::Int { dst: z, ptr: zero },
                    Opcode::JEq {
                        a: k,
                        b: z,
                        offset: 1,
                    },
                    Opcode::Ret { ret: v },
                ],
            );
            let f = &mut code.functions[c.init_fi];
            f.regs.push(p.void_);
            let v = Reg((f.regs.len() - 1) as u32);
            insert_ops(
                f,
                c.init_ret,
                vec![Opcode::Call1 {
                    dst: v,
                    fun: wint,
                    arg0: Reg(0),
                }],
            );
        }
    }
    eprintln!(
        "patched coop spectate: watcher step-out spectEsc fn@{} (Game.update Escape, Leave buttons), spectOut fn@{}, spectInt fn@{}",
        esc.0, out.0, wint.0
    );
    eprintln!(
        "patched coop spectate: Activity._start fn@{} (gate fn@{}, inventory fn@{}), onReady fn@{} \
         tutorial -> fn@{}, window closure fn@{} setWindow -> fn@{} / addHelpIcon -> fn@{}, \
         addHelpIcon__impl gated (spect fn@{}), end fade fn@{} -> fn@{}, Sing stage fn@{} / _cancel__impl -> spectEnd fn@{}: \
         other players keep their scene and watch the worker",
        code.functions[p.start_fi].findex.0,
        pre.0,
        inv.0,
        code.functions[p.ready_fi].findex.0,
        tut.0,
        code.functions[p.make_fi].findex.0,
        win.0,
        help.0,
        spect.0,
        code.functions[p.fade_fi].findex.0,
        fade.0,
        stage.0,
        end.0
    );
    Ok([
        spect, stage, pre, inv, tut, win, help, fade, end, cam, act_of, out, esc, wint,
    ])
}

/// Keeps shared mini-game windows off the other players' screens, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_coop_spectate(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p).map(|_| ()));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("coop spectate skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::same;
    use crate::asm::testutil::*;
    use crate::testsim::{Core, Sim, V};

    /// Five sites edited, seven well-typed functions appended, nothing else
    /// touched; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_coop_spectate(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 15);
        let mut sites = vec![
            p.start_fi,
            p.ready_fi,
            p.make_fi,
            p.help_fi,
            p.fade_fi,
            p.cancel_fi,
            p.esc_fi,
        ];
        sites.extend(p.cam_wins.iter().flat_map(|c| [c.stop_fi, c.init_fi]));
        for i in 0..n {
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                sites.contains(&i),
                "function #{i}"
            );
        }
        let nf = |k: usize| back.functions[n + k].findex;
        let (spect, stage, pre, inv, tut, win, help, fade, end) = (
            nf(0),
            nf(1),
            nf(2),
            nf(3),
            nf(4),
            nf(5),
            nf(6),
            nf(7),
            nf(8),
        );
        for &fi in &sites {
            let f = &back.functions[fi];
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        for f in &back.functions[n..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // _start: s = spect(this), g = spectPre(this) in front; before
        // showInventory a 3-op `s ? spectInv(this) : ...` into the same register;
        // a JTrue s over lockCamera / padCursor.locked, a JTrue g over cameraSave
        let (a, b) = (&orig.functions[p.start_fi], &back.functions[p.start_fi]);
        let g = Reg((b.regs.len() - 1) as u32);
        let sr = Reg((b.regs.len() - 2) as u32);
        assert_eq!(b.regs[g.0 as usize], p.bool_);
        assert_eq!(b.regs[sr.0 as usize], p.bool_);
        assert!(
            matches!(b.ops[0], Opcode::Call1 { dst, fun, arg0: Reg(0) } if fun == spect && dst == sr)
        );
        assert!(
            matches!(b.ops[1], Opcode::Call1 { dst, fun, arg0: Reg(0) } if fun == pre && dst == g)
        );
        assert_eq!(b.ops.len(), a.ops.len() + 8);
        // where original op i went
        let at = |i: usize| -> usize {
            2 + i
                + 3 * usize::from(i >= p.si_at)
                + usize::from(i >= p.lock_at)
                + usize::from(i >= p.pad_at)
                + usize::from(i >= p.cam_at)
        };
        let s = at(p.si_at);
        assert!(matches!(b.ops[s - 3], Opcode::JFalse { cond, offset: 2 } if cond == sr));
        let Opcode::Call2 { dst: si_dst, .. } = a.ops[p.si_at] else {
            panic!()
        };
        assert!(
            matches!(b.ops[s - 2], Opcode::Call1 { dst, fun, arg0: Reg(0) } if fun == inv && dst == si_dst)
        );
        assert!(matches!(b.ops[s - 1], Opcode::JAlways { offset: 1 }));
        // cameraSave: watchers on their own camera; lock / pad: every spectator
        for (site, want) in [(p.lock_at, sr), (p.pad_at, sr), (p.cam_at, g)] {
            assert!(matches!(b.ops[at(site) - 1], Opcode::JTrue { cond, offset: 1 } if cond == want));
        }
        for i in 0..a.ops.len() {
            let want: Vec<usize> = jump_targets(a, i).into_iter().map(at).collect();
            assert_eq!(jump_targets(b, at(i)), want, "_start op {i}");
            if want.is_empty() {
                assert_eq!(format!("{:?}", b.ops[at(i)]), format!("{:?}", a.ops[i]));
            }
        }
        // _cancel__impl: spectEnd(this) in front, the rest shifted by one
        let (ca, cb) = (&orig.functions[p.cancel_fi], &back.functions[p.cancel_fi]);
        assert!(matches!(cb.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == end));
        let q = Reg((cb.regs.len() - 1) as u32);
        assert!(matches!(cb.ops[1], Opcode::Call1 { dst, arg0: Reg(0), .. } if dst == q));
        assert_eq!(cb.ops.len(), ca.ops.len() + 4);
        // the two gated writes: a JTrue q right before each
        let cat = |i: usize| 2 + i + usize::from(i >= p.c_lock_at) + usize::from(i >= p.c_pad_at);
        for site in [p.c_lock_at, p.c_pad_at] {
            assert!(matches!(cb.ops[cat(site) - 1], Opcode::JTrue { cond, offset: 1 } if cond == q));
        }
        check_flow(cb);
        // the three call sites keep their registers, plus this
        let ready = &back.functions[p.ready_fi];
        assert!(matches!(&ready.ops[p.tut_at],
            Opcode::CallN { fun, args, .. } if *fun == tut && args.len() == 5 && args[4] == Reg(0)));
        let make = &back.functions[p.make_fi];
        assert!(
            matches!(make.ops[p.win_at], Opcode::Call3 { fun, arg2: Reg(0), .. } if fun == win)
        );
        assert!(
            matches!(make.ops[p.help_at], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == help)
        );
        let ff = &back.functions[p.fade_fi];
        assert!(
            matches!(ff.ops[p.fade_at], Opcode::Call4 { fun, arg3, .. } if fun == fade && arg3 == p.fade_act)
        );
        for (fi, at) in [
            (p.ready_fi, p.tut_at),
            (p.make_fi, p.win_at),
            (p.make_fi, p.help_at),
            (p.fade_fi, p.fade_at),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(a.ops.len(), b.ops.len());
            for i in (0..a.ops.len())
                .filter(|&i| i != at && !(fi == p.make_fi && (i == p.win_at || i == p.help_at)))
            {
                assert_eq!(
                    format!("{:?}", a.ops[i]),
                    format!("{:?}", b.ops[i]),
                    "fn #{fi} op {i}"
                );
            }
        }
        // the fade's Activity register is the closure capture of onActionDone
        assert_eq!(ff.regs[p.fade_act.0 as usize], p.act_t);
        // addHelpIcon__impl: if (spect(this)) return;
        let hf = &back.functions[p.help_fi];
        let mut want = orig.functions[p.help_fi].clone();
        want.regs = hf.regs.clone();
        shifted(&want, hf, 0, 3);
        assert!(matches!(hf.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == spect));
        assert_eq!(jump_targets(hf, 1), vec![3]);
        // every vanilla call made by the new functions is one the plan validated
        let allowed = [
            p.get_ginv,
            p.set_window,
            p.add_help,
            p.show_tut,
            p.net_fade,
            p.set_vis,
            p.obj_remove,
            p.println,
            p.std_string,
            p.str_add,
            p.str_cmp,
            p.base_check,
            spect,
            stage,
            nf(10),
            nf(11),
            nf(12),
        ];
        for f in &back.functions[n..] {
            for op in &f.ops {
                if let Some((g, _)) = call_of(op) {
                    assert!(allowed.contains(&g), "fn@{} calls fn@{}", f.findex.0, g.0);
                }
                if let Opcode::CallMethod { field, .. } = op {
                    assert!(
                        field.0 as i32 == p.set_vis_pi || *field == p.cull_pi || *field == p.cam_pi
                    );
                }
            }
        }
        let mut again = read(&patched);
        assert_eq!(
            format!("{:#}", plan(&again).err().unwrap()),
            "already applied"
        );
        patch_coop_spectate(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass runs in the full pipeline.
    #[test]
    fn applies_in_pipeline() {
        let Some(image) = game() else { return };
        let code = read(&crate::patch_image(&image).expect("patch"));
        assert_eq!(
            format!("{:#}", plan(&code).err().unwrap()),
            "already applied"
        );
    }

    /// A mismatch skips the whole pass and leaves the image as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let breakers: Vec<(&str, Box<dyn Fn(&mut Bytecode)>)> = vec![
            (
                "no setWindow",
                Box::new(move |c: &mut Bytecode| {
                    c.functions[p.make_fi].ops[p.win_at] = Opcode::Nop
                }),
            ),
            (
                "two lockCamera writes",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[p.start_fi];
                    let w = f.ops[p.lock_at].clone();
                    f.ops[p.lock_at - 1] = w;
                }),
            ),
            (
                "showInventory result not kept",
                Box::new(move |c: &mut Bytecode| {
                    c.functions[p.start_fi].ops[p.si_at + 1] = Opcode::Nop;
                }),
            ),
            (
                "no fade capture",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[p.fade_fi];
                    let i = f.ops[..p.fade_at]
                        .iter()
                        .rposition(|o| dst_of(o) == Some(p.fade_act))
                        .unwrap();
                    f.ops[i] = Opcode::Null { dst: p.fade_act };
                }),
            ),
        ];
        for (what, brk) in breakers {
            let mut code = read(&image);
            brk(&mut code);
            assert!(plan(&code).is_err(), "{what}");
            let before = write(&code);
            patch_coop_spectate(&mut code);
            assert!(write(&code) == before, "{what}: image changed");
        }
    }

    // ---------- behaviour ----------

    fn patched() -> Option<(Bytecode, usize, Plan)> {
        let image = std::fs::read(HLBOOT).ok()?;
        let p = plan(&read(&image)).unwrap();
        let mut code = read(&image);
        let n = code.functions.len();
        patch_coop_spectate(&mut code);
        Some((code, n, p))
    }

    fn sim<'a>(code: &'a Bytecode, n: usize, p: &'a Plan) -> Sim<'a> {
        Sim::new(
            code,
            n,
            move |c: &mut Core, f: RefFun, a: &[V]| {
                let log = |c: &mut Core, k: &'static str, v: V| {
                    c.log.push((k, a.to_vec()));
                    Some(v)
                };
                if f == p.get_ginv {
                    // the GameInventory the test put on the ui object
                    Some(c.key_get(&a[0], "ginv"))
                } else if f == p.set_window {
                    log(c, "setWindow", V::Null)
                } else if f == p.add_help {
                    log(c, "addHelpIcon", V::Null)
                } else if f == p.show_tut {
                    log(c, "showTutorial", V::B(true))
                } else if f == p.net_fade {
                    log(c, "netFade", V::Null)
                } else if f == p.set_vis {
                    log(c, "visible", V::B(false))
                } else if f == p.obj_remove {
                    log(c, "remove", V::Null)
                } else if f == p.println {
                    log(c, "println", V::Null)
                } else if f == p.std_string {
                    match &a[0] {
                        V::I(n) => Some(V::S(n.to_string())),
                        o => panic!("Std.string {o:?}"),
                    }
                } else if f == p.str_add {
                    match (&a[0], &a[1]) {
                        (V::S(x), V::S(y)) => Some(V::S(format!("{x}{y}"))),
                        o => panic!("String add {o:?}"),
                    }
                } else if f == p.str_cmp {
                    match (&a[0], &a[1]) {
                        (V::S(x), V::S(y)) => Some(V::I(i32::from(x != y))),
                        o => panic!("String compare {o:?}"),
                    }
                } else {
                    None
                }
            },
            move |c: &mut Core, pi: usize, a: &[V]| {
                if pi == p.cull_pi.0 {
                    // obj.cullingCollider(name): the child the test hung on obj
                    let V::S(name) = &a[1] else {
                        panic!("collider name")
                    };
                    return c.key_get(&a[0], name);
                }
                assert_eq!(pi as i32, p.set_vis_pi);
                c.log.push(("visible", a.to_vec()));
                V::B(false)
            },
        )
    }

    struct W {
        act: V,
        game: V,
        me: V,
        other: V,
        mode: V,
        pad: V,
        unit: V,
    }

    /// A machine (`me`) with an activity whose unit belongs to `other`.
    fn world(s: &mut Sim, p: &Plan) -> W {
        let c = &mut s.c;
        let (me, other) = (c.obj(&[]), c.obj(&[]));
        let mode = c.obj(&[(p.m_lock, V::B(false))]);
        let pad = c.obj(&[(p.pad_locked, V::B(false))]);
        let gui = c.obj(&[(p.gui_pad, pad.clone())]);
        let ui = c.obj(&[]);
        let game = c.obj(&[
            (p.g_me, me.clone()),
            (p.g_mode, mode.clone()),
            (p.g_gui, gui),
            (p.g_ui, ui),
        ]);
        let unit = c.obj(&[(p.u_owner, other.clone())]);
        let host = c.obj(&[]);
        let inf = c.obj(&[(p.i_id, V::S("Fish".into()))]);
        let act = c.obj(&[
            (p.a_game, game.clone()),
            (p.a_host, host),
            (p.a_unit, unit.clone()),
            (p.a_inf, inf),
            (p.a_cc, V::B(true)),
            (p.a_prev, V::B(true)),
        ]);
        W {
            act,
            game,
            me,
            other,
            mode,
            pad,
            unit,
        }
    }

    /// Who is a spectator: replicated activity, unit set, owned by another player.
    #[test]
    fn spectator_test() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let spect = code.functions[n].findex;
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(true));
        // the player doing it
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(false));
        s.c.set(&w.unit, p.u_owner, w.other.clone());
        // not replicated (a local, non-coop activity), no unit, no owner
        for (o, f) in [(&w.act, p.a_host), (&w.act, p.a_unit), (&w.unit, p.u_owner)] {
            let keep = s.c.get(o, f);
            s.c.set(o, f, V::Null);
            assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(false));
            s.c.set(o, f, keep);
        }
        assert!(s.c.log.is_empty());
    }

    /// spectPre on a watching spectator: true, no camera change, the chooseUnit
    /// wait Interactive gone; on a camera-activity spectator and on the player:
    /// false and nothing changed. spectInv: the inventory panel's visibility.
    #[test]
    fn start_gate() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let (pre, inv) = (code.functions[n + 2].findex, code.functions[n + 3].findex);
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let it = s.c.obj(&[]);
        s.c.set(&w.act, p.a_int, it.clone());
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::B(true));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(false));
        assert_eq!(s.c.get(&w.act, p.a_int), V::Null);
        assert_eq!(s.c.take("remove"), vec![vec![it.clone()]]);
        assert_eq!(
            s.c.take("println"),
            vec![vec![V::S("mp: spectate Fish 1".into())]]
        );
        // the locks and the inventory are not touched by the gate itself
        assert_eq!(s.c.get(&w.mode, p.m_lock), V::B(false));
        assert_eq!(s.c.get(&w.pad, p.pad_locked), V::B(false));
        assert!(s.c.log.is_empty());

        // a spectator of a camera-only mini-game keeps the vanilla start, but
        // not the full-screen wait Interactive
        for id in CAMERA_ACTIVITIES {
            let inf = s.c.get(&w.act, p.a_inf);
            s.c.set(&inf, p.i_id, V::S(id.into()));
            s.c.set(&w.act, p.a_cc, V::B(true));
            s.c.set(&w.act, p.a_int, it.clone());
            assert_eq!(s.run(pre, vec![w.act.clone()]), V::B(false));
            assert_eq!(s.c.get(&w.act, p.a_cc), V::B(true));
            assert_eq!(s.c.get(&w.act, p.a_int), V::Null);
            assert_eq!(s.c.take("remove"), vec![vec![it.clone()]]);
            assert_eq!(
                s.c.take("println"),
                vec![vec![V::S(format!("mp: spectate {id} 0"))]]
            );
            assert!(s.c.log.is_empty());
        }

        // the player doing it: false, nothing changed, no log
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::B(false));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(true));
        assert!(s.c.log.is_empty());

        // spectInv: GameInventory.inventory.visible, false when unknown
        let ui = s.c.get(&w.game, p.g_ui);
        assert_eq!(s.run(inv, vec![w.act.clone()]), V::B(false));
        for vis in [true, false] {
            let panel = s.c.obj(&[(p.o_visible, V::B(vis))]);
            let gi = s.c.obj(&[(p.gi_inv, panel)]);
            s.c.key_set(&ui, "ginv".into(), gi);
            assert_eq!(s.run(inv, vec![w.act.clone()]), V::B(vis));
        }
        assert!(s.c.log.is_empty());
    }

    /// The Sing stage pair on a spectator: spectPre (watching, no camera) shows
    /// the stage and hides its HandTool, spectEnd at _cancel reverts both (the
    /// other bits kept); nothing for the player or another activity.
    #[test]
    fn sing_stage() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let (pre, end) = (code.functions[n + 2].findex, code.functions[n + 8].findex);
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let hand = s.c.obj(&[(p.o3_flags, V::I(3))]);
        let obj = s.c.obj(&[(p.o3_flags, V::I(1))]);
        s.c.key_set(&obj, "HandTool".into(), hand.clone());
        let tgt = s.c.obj(&[(p.e_obj, obj.clone())]);
        s.c.set(&w.act, p.a_target, tgt);
        let inf = s.c.get(&w.act, p.a_inf);
        let flags = |s: &Sim| (s.c.get(&obj, p.o3_flags), s.c.get(&hand, p.o3_flags));
        // another activity: untouched
        s.run(pre, vec![w.act.clone()]);
        s.run(end, vec![w.act.clone()]);
        assert_eq!(flags(&s), (V::I(1), V::I(3)));
        s.c.set(&inf, p.i_id, V::S("Sing".into()));
        // the player: untouched, vanilla camera start
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        s.c.set(&w.act, p.a_cc, V::B(true));
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::B(false));
        s.run(end, vec![w.act.clone()]);
        assert_eq!(flags(&s), (V::I(1), V::I(3)));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(true));
        // a spectator of the song: own camera, stage shown, then hidden again
        s.c.set(&w.unit, p.u_owner, w.other.clone());
        s.c.take("println");
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::B(true));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(false));
        assert_eq!(flags(&s), (V::I(3), V::I(1)));
        assert_eq!(
            s.c.take("println"),
            vec![vec![V::S("mp: spectate Sing 1".into())]]
        );
        s.run(end, vec![w.act.clone()]);
        assert_eq!(flags(&s), (V::I(1), V::I(3)));
        assert!(s.c.log.is_empty());
    }

    /// Window, tutorial, help icon and end fade on a spectator and on the player.
    #[test]
    fn window_tutorial_help_fade() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let f = |k: usize| code.functions[n + k].findex;
        let (tut, win, help, fade) = (f(4), f(5), f(6), f(7));
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let root = s.c.obj(&[]);
        let window = s.c.obj(&[(p.w_root, root.clone())]);
        let ui = s.c.get(&w.game, p.g_ui);
        let ctrl = s.c.obj(&[]);

        // spectator: hidden, not the current window; no tutorial, no help icon
        s.run(win, vec![w.mode.clone(), window.clone(), w.act.clone()]);
        assert_eq!(
            s.c.take("visible"),
            vec![
                vec![root.clone(), V::B(false)],
                vec![window.clone(), V::B(false)]
            ]
        );
        assert!(s.c.take("setWindow").is_empty());
        // the tutorial callback (spectHelp bound to the activity) runs at once
        let cb = V::Clo(help, Box::new(w.act.clone()));
        let args = |cb: &V| {
            vec![
                ui.clone(),
                V::S("Fish".into()),
                V::Null,
                cb.clone(),
                w.act.clone(),
            ]
        };
        assert_eq!(s.run(tut, args(&cb)), V::B(false));
        assert!(s.c.take("showTutorial").is_empty());
        assert!(s.c.take("addHelpIcon").is_empty());
        // the window closure (probe: logs addHelpIcon): never on a client
        // spectator, at once on the host
        let probe = V::Clo(p.add_help, Box::new(w.act.clone()));
        assert_eq!(s.run(tut, args(&probe)), V::B(false));
        assert!(s.c.take("addHelpIcon").is_empty());
        s.c.set(&w.game, p.g_auth, V::B(true));
        assert_eq!(s.run(tut, args(&probe)), V::B(false));
        assert_eq!(s.c.take("addHelpIcon"), vec![vec![w.act.clone()]]);
        assert!(s.c.take("showTutorial").is_empty());
        s.c.set(&w.game, p.g_auth, V::B(false));
        s.run(help, vec![w.act.clone()]);
        assert!(s.c.take("addHelpIcon").is_empty());
        // end fade with coop = true: not replicated (the others watch from their camera)
        s.run(fade, vec![ctrl.clone(), V::B(true), V::Null, w.act.clone()]);
        assert_eq!(
            s.c.take("netFade"),
            vec![vec![ctrl.clone(), V::B(false), V::Null]]
        );
        assert!(s.c.log.is_empty());

        // the player: vanilla calls
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        s.run(win, vec![w.mode.clone(), window.clone(), w.act.clone()]);
        assert_eq!(
            s.c.take("setWindow"),
            vec![vec![w.mode.clone(), window.clone()]]
        );
        // a client playing: the vanilla tutorial with its callback
        assert_eq!(s.run(tut, args(&cb)), V::B(true));
        let st = s.c.take("showTutorial");
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].last(), Some(&cb));
        // the host playing: the window closure at once, the tutorial over it
        // without callback
        s.c.set(&w.game, p.g_auth, V::B(true));
        assert_eq!(s.run(tut, args(&cb)), V::B(true));
        let st = s.c.take("showTutorial");
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].last(), Some(&V::Null));
        assert_eq!(s.c.take("addHelpIcon"), vec![vec![w.act.clone()]]);
        s.c.set(&w.game, p.g_auth, V::B(false));
        s.run(help, vec![w.act.clone()]);
        assert_eq!(s.c.take("addHelpIcon"), vec![vec![w.act.clone()]]);
        // a unit-less activity keeps the shared fade; coop = false stays false
        s.c.set(&w.act, p.a_unit, V::Null);
        s.run(fade, vec![ctrl.clone(), V::B(true), V::Null, w.act.clone()]);
        s.run(
            fade,
            vec![ctrl.clone(), V::B(false), V::Null, w.act.clone()],
        );
        let fades: Vec<V> =
            s.c.take("netFade")
                .into_iter()
                .map(|a| a[1].clone())
                .collect();
        assert_eq!(fades, [V::B(true), V::B(false)]);
        assert!(s.c.log.is_empty());
    }
}
