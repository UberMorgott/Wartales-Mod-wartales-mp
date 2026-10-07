// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a leave from a place (town, tavern, location) or from the owned tavern
// is never blocked by another player's business.
//
// Vanilla, in a place (world.PlaceView), the host decides every leave:
// `PlaceContent.update` disables the Leave button while ANY connected player
// is `lockedWith` an entity (Game.anyPlayerLocked), and `PlaceView.tryClose`
// refuses (silently, once, no retry) on the same lock, a cinematic, a gamepad
// global window, a fade, missing votes and a dialog camera move. A player who
// inspects an NPC (tavern recruits: Npc.inspect, also the dialog "inspect"
// choice) is locked until they close that UnitInfo, so nobody else can leave.
// The coop_gates pass already made every leave request forced (G2:
// `Place.leaveState.forced`, replicated to the clients) and dropped the
// "a window is open" part of the lock (G6).
//
// This pass:
//   F1 PlaceContent.update: another player's lock no longer disables Leave
//      (`place.lockLeave`, set by scripts and activities, still hides it).
//   F2 PlaceView.update, every frame on every machine, `flUpdate(view)`:
//        if (!isMulti || view.leaving || !place.leaveState.forced
//            || place.leaveState.players.length == 0) { reset; return; }
//        // (`forced` outlives a withdrawn request: the gamepad toggle removes its
//        // player only; an empty list means nobody asks any more)
//        // a leave is pending: release this machine's own lock the vanilla way
//        if (!game.inFade() && me.lockedWith != null) {
//            w = flClosable(game): the top window, in game.mode.windows then
//                game.globalUI.windows, of a lock-holding class whose tryClose()
//                (canBeClosed) is true (LOCK_WINDOWS);
//            if (w != null && w != lastClosed) { lastClosed = w; println; w.close(); }
//        }
//        // host: leave as soon as nothing refuses, waiting (never skipping) otherwise
//        if (isAuth) { why = flWhy(view, true);
//                      if (why == 0 || why == dialog open) why = flDialog(view) || why;
//                      print it when it changes; if (why == 0) view.tryClose(); }
//      `close()` is the class's own close: Window.close (removeChild -> onRemove
//      -> the onClose the lock site installed, which clears lockedWith and, for
//      the dialog inspect, unhides the dialog), ChooseUnit.close -> cancel() ->
//      onCanceled (dialog customize: cancelCost refund + unlock), i.e. what the
//      player's own Escape / Cancel does. The lock clears on the owner and
//      replicates; tryClose re-checks everything and leave() (guarded by
//      `leaving`) runs syncLeaveMode, the normal barrier.
//      Crafting that has not started is cancelled the same way: ui.win.Craft
//      (opened by Tool.startCraft with its Activity in `act`) has the onClose
//      `if (!act.started) { act.cancel(); act.onClose(); }` (Tool.hx), which is
//      what its X / Escape runs; flClosable takes a Craft only while
//      `!act.started`; a location modal over a guarded one (MODAL_OK: Confirm,
//      NetConfirm, InputNumber, CraftConfirm) is closed first (the topmost, with
//      its own close()), then the window itself; a lock-class modal that cannot
//      close (started Alter) keeps it. Only location windows are closed: UnitInfo
//      only as the NPC inspect (guard 3); the player's own sheet, inventory,
//      trees, journal, map, options are ignored (never closed, never waited on).
//      Gathering
//      and scripted activities are cancelled in their ChooseUnit, as before.
//      Started activities, crafts and gathering have no safe cancel: the leave
//      waits until they end ("player busy" with their `lockedWith`). Their
//      minigame windows (ui.win.ForgeAction, GatherAction, AnalyzeAction,
//      SingAction, FishingAction, UnitAction: ActivityWindows) only exist after
//      Activity._start paid `onStartActivityCost`; their close() only drops the
//      window (no Activity.cancel / onClose, the lock and the locked camera stay),
//      so they are never on the close list.
//      Owner rules (co-op): only a running minigame holds a leave; location
//      windows are closed; location-independent ones neither close nor block.
//      So a location modal (MODAL_OK) is closed only when it sits directly over
//      a guarded lock window in the same list (no parentWindow link exists);
//      anywhere else it is ignored. UnitInfo closes only as the sheet of a
//      unit outside the troop (`!unit.isInTroop()`) during an NPC lock.
//      flRelease (every machine, after the closing): this machine's own lock
//      that no minigame holds (flMinigame: an ActivityWindow, or a Craft /
//      Alter whose activity started) and that outlives RELEASE_AFTER s (an
//      NPC / dialog-action lock, a window under a personal modal, a lock whose
//      window never opened) is cleared with `me.set_lockedWith(null)`; the mode
//      switch disposes what stays open. The host leaves with `leave(null)`
//      once flWhy(view, true) is 0: tryClose's own refusals but the gamepad
//      global-window test (the host's own windows never hold the leave).
//      Wait timer (every machine, `pend_at` = first pending frame): after
//      WARN_AFTER s the machine whose `me.lockedWith` still holds prints "mp: leave
//      waits here: <lockedWith> top <top mode window>" and shows HERE_NOTE on
//      screen (once per lock); the host shows (GameUI.localNotify, local) and
//      prints "Leave waits: <why> [<lockedWith> (this|other player) <name>]",
//      again every WARN_EVERY s. Nothing is forced.
//   F4 The shared dialog (host): flDialog(view) finds the ui.win.Dialog in
//      game.mode.windows and ends it with its own Dialog.tryClose (what Escape
//      calls: `visible && api.allowLeave` -> leave(null), the Leave button's
//      action) when the Leave choice is on screen (`leaveButton`) and the dialog
//      is visible; while it is hidden (a choice resolves: dialog__impl hides it
//      before its scriptEvent, gains, a special action) it waits, so a choice
//      is never cut mid-action; idle and visible without a usable Leave choice
//      (props.noLeave / allowLeave = false), or with a modal window of the
//      host's over it, (owner decision: a conversation never holds the party)
//      with Dialog.leave(null) directly. It still waits for a leave
//      already running (`alreadyLeave`), a mode switch, pause and fade; flWhy's
//      earlier refusals (a player lockedWith, fade, cinematic, camera entering
//      the dialog) come first. After the leave, its choice buttons are reset
//      (resetChoices), so a late Leave / choice click (setClick finds buttons by
//      net id in the window tree) cannot run leave() a second time (Dialog.leave
//      re-runs onLeft when alreadyLeave). The place leave also waits while a
//      Dialog window still exists (its leave script event runs after the camera
//      returns). Only a started activity (its minigame) keeps a long wait.
//   T  The owned tavern (world.tavern.TavernMode). Vanilla Tavern.askLeave__impl
//      (TavernWindow Leave, RPC to the host) returns silently while any player
//      is locked or the controller is locked, and never retries;
//      Controller.closeTavern__impl (Escape) skips the player-lock test.
//      Tavern.askLeave__impl gets `if (tlAsk(this)) return;` in front:
//        tlAsk (host, multi): why = tlWhy(game) (player lockedWith, fade, alive
//        lock, mode-switch barrier running); 0 -> clear pending, vanilla body
//        runs (syncLeaveMode); else mark pending (global = this TavernMode;
//        `tavern.leaveState.forced = true` through the proxy mark, as
//        Place.setLeaveState__impl does: that field is replicated, saved and
//        otherwise unused, Tavern.enter recreates it) and return.
//      Controller.closeTavern__impl gets `if (tlClose(this)) return;`: in multi,
//      in a TavernMode, it runs tavern.askLeave__impl() instead (same policy).
//      TavernMode.update, every machine, `tlUpdate(mode)`: while
//      `leaveState.forced` (host: and the pending global is this mode, else a
//      stale flag from a save or an earlier visit is cleared) every machine
//      closes its own lock-holding window as F2; the host re-checks tlWhy each
//      frame and calls askLeave__impl once nothing refuses.
//   C  Leaving the camp (world.camp.CampMode -> world map). Vanilla, the Camp
//      button / Escape sends Controller.toggleCamp (RPC) and the host's
//      toggleCamp__impl -> GameUI.toggleCamp returns silently while any player is
//      lockedWith something (`anyPlayerLocked(true)`: a camp tool window opened by
//      CampEntryEntity.onAction such as the commander's StrategyTable, the camp
//      chest, the banner editor, a craft), a fade / alive lock / mode switch runs,
//      or the host has a modal window open; nothing retries. Camp dialogs are
//      DialogOut place modes, shared by every player (not a camp window).
//      Controller.toggleCamp__impl gets `if (cpAsk(this)) return;` in front:
//        cpAsk (host, multi, game.mode is a CampMode): why = cpWhy(game, mode):
//        a rest running -> 0 (the vanilla body refuses, as before); a player
//        lockedWith; fade; alive lock; mode-switch barrier; cinematic. 0 ->
//        clear pending, set `go`, the vanilla body runs past GameUI.toggleCamp's
//        window tests (canToggleButton, canLeaveCamp: gtc_hooks skip them while
//        `go`, so the host's own windows never hold the camp leave), then
//        confession, syncLeaveMode: every machine switches together through
//        the normal barrier; else pending
//        (global = this CampMode), log on change, and for a lock call
//        cpAsks(game), at most every ASK_EVERY s: for every player lockedWith an
//        st.item.Tool (every camp tool lock), `tool.closeActionWindow()`. That
//        is a vanilla Tool RPC (host -> every machine); the game calls it only
//        from GridData.removeTool__impl (a tool taken off the camp grid).
//      Tool.closeActionWindow__impl gets `if (cpClose(this)) return;`: in
//      multi, on the machine whose `me.lockedWith` is this tool, it closes
//      top-down (at most 4, while still locked with it) the lock-holding
//      windows flClosable finds (LOCK_WINDOWS + the camp tool windows of
//      CAMP_WINDOWS, same guards: no modal window over it, a Craft only before
//      its activity starts), never during a fade, with the class's own close()
//      (Window.onRemove -> the onClose onAction installed, which clears
//      lockedWith; what the window's X / Escape does). Vanilla's body (close
//      whatever window is current, unguarded) runs in single player only, so
//      in co-op a removed tool also leaves a started craft or a window under a
//      modal confirm open.
//      StrategyTable applies each strategy toggle at once (Simulation
//      add/removeStrategy), so closing it leaves no half-made selection.
//      CampMode.update, host, `cpUpdate(mode)`: a pending request of another
//      mode is dropped; for this mode it calls toggleCamp__impl again every
//      frame (cpAsk re-checks), so the leave happens once nothing refuses.
//      Started crafts / activities (their window is not closable) and a host
//      modal window keep the wait; the shared dialog is a mode of its own.
//   F3 Diagnostics (shim.log "game: mp: ..." lines): tryClose prints the reason
//      it refuses ("mp: tryClose refused: <why>"), the pending leave prints each
//      change of what it waits on ("mp: leave pending: <why|go>", "mp: tavern
//      leave pending: <why|go>"), every window it closes ("mp: leave closes
//      <type>") and every dialog it ends ("mp: leave ends dialog");
//      PlaceView.leave prints a refusal while the game is paused (at most every
//      5 s). The camp leave prints "mp: camp leave pending: <why|go>" (on
//      change) and "mp: camp leave closes <type>" on the machine that closes.
//
// Coordination: the G1 repeat guard still runs the Leave button once per click
// burst and drops clicks while the mode-switch barrier runs; the pending leave
// waits for that barrier too and leave() runs once per PlaceView. The follow
// pass is world-map only.
//
// Validated before editing; a mismatch skips the pass (logged). The tavern part
// (T) and the camp part (C) are validated on their own and skipped alone on a
// mismatch.

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefGlobal, RefString, ValBool};

const PENDING: &str = "mp: leave pending: ";
const REFUSED: &str = "mp: tryClose refused: ";
const CLOSES: &str = "mp: leave closes ";
const PAUSED: &str = "mp: leave refused: paused";
const ENDS: &str = "mp: leave ends dialog";
const FORCED: &str = "mp: leave forces dialog leave";
const TPENDING: &str = "mp: tavern leave pending: ";
const CPENDING: &str = "mp: camp leave pending: ";
const CCLOSES: &str = "mp: camp leave closes ";
/// Host on-screen / log text once a place leave has waited WARN_AFTER s.
const WAITS: &str = "Leave waits: ";
const LOG_P: &str = "mp: ";
/// This machine's player still holds a lock WARN_AFTER s into a pending leave.
const HERE: &str = "mp: leave waits here: ";
const HERE_TOP: &str = " top ";
const HERE_NOTE: &str = "The party is leaving: finish or close what you are doing";
/// Notify id of the on-screen line (follow.rs uses it too).
const NOTIFY_ID: &str = "ArenaNotif";
/// Seconds a place leave may wait before the host / the busy player are told.
const WARN_AFTER: f64 = 15.0;
/// Seconds between two host reminders while the leave keeps waiting.
const WARN_EVERY: f64 = 30.0;
/// Seconds between two "paused" refusal lines.
const PAUSED_EVERY: f64 = 5.0;
/// Seconds between two rounds of camp close requests (Tool.closeActionWindow RPCs).
const ASK_EVERY: f64 = 1.0;
/// Seconds a pending leave lets this machine's own non-minigame lock outlive
/// its window closing before flRelease clears it.
const RELEASE_AFTER: f64 = 3.0;
const RELEASES: &str = "mp: leave releases lock ";

/// Reason codes of `flWhy` / `flDialog` / `tlWhy` / `cpWhy` (0 = nothing refuses) and their log text.
const REASONS: [&str; 15] = [
    "go",
    "lockLeave",
    "player busy",
    "cinematic",
    "gamepad global window",
    "fade",
    "votes",
    "entering dialog",
    "dialog open",
    "mode switch running",
    "paused",
    "dialog can't be left",
    "dialog busy",
    "alive lock",
    "window open",
];
const R_LOCKED: usize = 2;
const R_FADE: usize = 5;
const R_DIALOG: usize = 8;
const R_SYNC: usize = 9;
const R_PAUSED: usize = 10;
const R_ALIVE: usize = 13;

/// Window classes whose `close()` is the vanilla cancel of a `lockedWith`, and the
/// extra test before the pending leave closes one: 0 none; 1 no modal window is
/// open over it (above it in its list, or in globalUI.windows for a mode
/// window); 2 = 1 and its `act` (ui.win.Activity) has not started; 3 only while
/// `me.lockedWith` is an ent.p.Npc and only for a unit outside the troop
/// (Npc.inspect, the dialog inspect: a sheet of the player's own units is
/// location-independent and stays open).
/// Location-independent windows (UnitInfo of the player's own units, inventory,
/// skill / path trees, journal, map, options...) are never closed and never block.
const LOCK_WINDOWS: [(&str, i32); 8] = [
    ("ui.win.UnitInfo", 3),
    ("ui.win.ChooseUnit", 0),
    ("ui.win.fief.FiefMandateDetails", 0),
    ("ui.win.GarnisonManager", 0),
    ("ui.win.CounterChest", 0),
    ("ui.win.Craft", 2),
    ("ui.win.Alter", 2),
    ("ui.win.Dismantle", 1),
];
/// Modal windows of a location interaction that are closed (their own close())
/// before the guarded window under them; any other modal window is ignored.
const MODAL_OK: [&str; 4] = [
    "ui.win.Confirm",
    "ui.win.NetConfirm",
    "ui.win.InputNumber",
    "ui.win.CraftConfirm",
];
/// Camp tool windows (Tool.getToolAction), each opened by CampEntryEntity.onAction
/// with `me.lockedWith = tool` and an onClose that clears it; guard 1. Added to
/// the LOCK_WINDOWS list only when the camp part is planned.
const CAMP_WINDOWS: [&str; 7] = [
    "ui.win.StrategyTable",
    "ui.win.CampChest",
    "ui.win.BannerCamp",
    "ui.win.ConverterTool",
    "ui.win.LecternTool",
    "ui.win.Lute",
    "ui.win.Stake",
];
const SKIP: [[&str; 16]; 2] = [
    [
        "s00", "s01", "s02", "s03", "s04", "s05", "s06", "s07", "s08", "s09", "s0a", "s0b", "s0c",
        "s0d", "s0e", "s0f",
    ],
    [
        "s10", "s11", "s12", "s13", "s14", "s15", "s16", "s17", "s18", "s19", "s1a", "s1b", "s1c",
        "s1d", "s1e", "s1f",
    ],
];

type F = (RefField, RefType);

struct T {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    str_: RefType,
}

/// A LOCK_WINDOWS entry: class global, its type, the instance type, the guard
/// and (guard 2) the `act` field.
struct Lock {
    g: RefGlobal,
    gt: RefType,
    ct: RefType,
    guard: i32,
    act: Option<RefField>,
    /// Guard 3: UnitWindow.unit (the sheet's unit).
    unit: Option<RefField>,
}

/// GameUI.localNotify and its `{title}` options.
struct Ntf {
    local_notify: RefFun,
    opts_t: RefType,
    title_s: RefString,
    dynobj: RefType,
}

/// ui.win.Dialog.
struct Dlg {
    t: RefType,
    cls: (RefGlobal, RefType),
    already: RefField,
    visible: RefField,
    leave_btn: F,
    try_close: RefFun,
    reset: RefFun,
    /// Dialog.leave(onEnd) and its callback type.
    leave: RefFun,
    leave_cb: RefType,
}

/// The owned tavern part.
struct Tav {
    tm_t: RefType,
    tm_cls: (RefGlobal, RefType),
    tm_game: RefField,
    get_tavern: RefFun,
    tav_t: RefType,
    tav_game: RefField,
    tav_ls: RefField,
    ctrl_game: RefField,
    px_obj: F,
    px_bit: RefField,
    px_mark: RefField,
    is_locked: RefFun,
    ask: RefFun,
    ask_fi: usize,
    close_fi: usize,
    upd_fi: usize,
    dbg_file: usize,
}

/// The camp part.
struct Camp {
    cm_t: RefType,
    cm_cls: (RefGlobal, RefType),
    cm_game: RefField,
    cm_rest: F,
    tool_t: RefType,
    tool_cls: (RefGlobal, RefType),
    tool_game: RefField,
    /// Tool.closeActionWindow, the RPC wrapper.
    close_rpc: RefFun,
    ctrl_game: RefField,
    is_locked: RefFun,
    /// Controller.toggleCamp__impl (the retry calls it).
    toggle: RefFun,
    /// GameUI.toggleCamp, its `Ret` after the canToggleButton test and its
    /// `JFalse canLeaveCamp`.
    gtc_fi: usize,
    ct_ret: usize,
    cl_jf: usize,
    ask_fi: usize,
    upd_fi: usize,
    close_fi: usize,
    dbg_file: usize,
}

struct Plan {
    t: T,
    view_t: RefType,
    game_t: RefType,
    win_t: RefType,
    // PlaceView
    v_game: F,
    v_place: F,
    v_leaving: RefField,
    v_entering: RefField,
    v_focus: F,
    // Place / leaveState
    p_lock_leave: RefField,
    p_leave_state: F,
    ls_forced: RefField,
    ls_players: F,
    // Game
    g_me: F,
    g_auth: RefField,
    g_state: F,
    g_ui: F,
    g_ctrl: F,
    g_mode: F,
    g_gui: F,
    st_cine: RefField,
    st_players: F,
    sp_array: F,
    c_lock_sync: RefField,
    c_wait_locks: F,
    bp_locked: F,
    /// BasePlayer.name (String).
    bp_name: RefField,
    ntf: Ntf,
    // Window lists
    m_windows: F,
    u_windows: F,
    arr_t: RefType,
    a_len: RefField,
    a_raw: F,
    w_name: F,
    w_try_close: RefField,
    w_close: RefField,
    w_modal: F,
    /// Construct index of `WindowModalMode.None`.
    modal_none: i32,
    locks: Vec<Lock>,
    /// MODAL_OK class globals.
    modal_ok: Vec<(RefGlobal, RefType)>,
    /// ent.p.Npc class global (guard 3).
    npc_cls: (RefGlobal, RefType),
    act_t: RefType,
    act_started: RefField,
    base_check: RefFun,
    dlg: Dlg,
    /// PlaceView.leave slot (the host leaves without tryClose's own refusals).
    v_leave: RefField,
    v_leave_cb: RefType,
    /// BasePlayer.set_lockedWith (the networked setter).
    set_locked: RefFun,
    /// ui.win.ActivityWindow class global: a running minigame.
    act_win_cls: (RefGlobal, RefType),
    /// st.Unit.isInTroop (guard 3: the player's own company) and its Null<Bool> result.
    in_troop: RefFun,
    in_troop_t: RefType,
    // functions
    is_multi: RefFun,
    any_locked: (RefFun, RefType),
    get_locked: RefFun,
    in_fade: RefFun,
    paused: RefFun,
    pad_active: RefFun,
    global_windows: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    println: RefFun,
    sys_time: RefFun,
    // patch sites
    update_fi: usize,
    try_close_fi: usize,
    leave_fi: usize,
    /// PlaceView.leave: the `Ret` of its `if (game.paused) return`.
    paused_ret: usize,
    content_fi: usize,
    /// PlaceContent.update: the `JTrue anyPlayerLocked` that disables Leave.
    content_at: usize,
    dbg_file: usize,
    tav: Option<Tav>,
    camp: Option<Camp>,
}

/// The vtable slot of method `name` declared (or overridden) by class `t`.
fn slot(code: &Bytecode, t: RefType, name: &str) -> Result<RefField> {
    let o = obj(code, t)?;
    let p = o
        .protos
        .iter()
        .find(|p| s(code, p.name) == name)
        .with_context(|| format!("proto {name} not found on {}", s(code, o.name)))?;
    let i = usize::try_from(p.pindex).context("negative proto index")?;
    Ok(RefField(i))
}

fn no_jump_to(f: &Function, at: usize) -> bool {
    !(0..f.ops.len()).any(|i| jump_targets(f, i).contains(&at))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let t = T {
        void: prim("void", |t| matches!(t, Type::Void))?,
        bool_: prim("bool", |t| matches!(t, Type::Bool))?,
        i32_: prim("i32", |t| matches!(t, Type::I32))?,
        f64_: prim("f64", |t| matches!(t, Type::F64))?,
        str_: obj_type(code, "String")?,
    };
    let dyn_t = prim("dynamic", |t| matches!(t, Type::Dyn))?;
    if code.functions.iter().any(|f| {
        f.ops.iter().any(
            |op| matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(PENDING)),
        )
    }) {
        bail!("already applied");
    }
    let typed = |o: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, o, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let view_t = obj_type(code, "world.PlaceView")?;
    let game_t = obj_type(code, "Game")?;
    let place_t = obj_type(code, "ent.Place")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let win_t = obj_type(code, "ui.Window")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let ui_t = obj_type(code, "ui.GameUI")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;

    let v_game = field(code, view_t, "game")?;
    let v_place = field(code, view_t, "place")?;
    if v_game.1 != game_t || v_place.1 != place_t {
        bail!("PlaceView.game / place have unexpected types");
    }
    let v_leaving = typed(view_t, "leaving", t.bool_)?;
    let v_entering = typed(view_t, "enteringDialog", t.bool_)?;
    let v_focus = field(code, view_t, "focusElement")?;
    let p_lock_leave = typed(place_t, "lockLeave", t.bool_)?;
    let p_leave_state = field(code, place_t, "leaveState")?;
    let ls_forced = typed(p_leave_state.1, "forced", t.bool_)?;
    let ls_players = field(code, p_leave_state.1, "players")?;
    if ls_players.1 != arr_t {
        bail!("leaveState.players is not an ArrayObj");
    }
    let g_me = field(code, game_t, "me")?;
    if g_me.1 != bp_t {
        bail!("Game.me is not a BasePlayer");
    }
    let g_auth = typed(game_t, "isAuth", t.bool_)?;
    let g_state = field(code, game_t, "state")?;
    let g_ui = field(code, game_t, "ui")?;
    let g_ctrl = field(code, game_t, "ctrl")?;
    let g_mode = field(code, game_t, "mode")?;
    let g_gui = field(code, game_t, "globalUI")?;
    if g_ui.1 != ui_t || g_ctrl.1 != ctrl_t {
        bail!("Game.ui / Game.ctrl have unexpected types");
    }
    let st_cine = typed(g_state.1, "cinematicMode", t.bool_)?;
    let st_players = field(code, g_state.1, "players")?;
    let sp_array = field(code, st_players.1, "array")?;
    let c_lock_sync = typed(ctrl_t, "lockSyncMode", t.bool_)?;
    let c_wait_locks = field(code, ctrl_t, "waitLocks")?;
    if c_wait_locks.1 != arr_t {
        bail!("Controller.waitLocks is not an ArrayObj");
    }
    let bp_locked = field(code, bp_t, "lockedWith")?;
    let bp_name = typed(bp_t, "name", t.str_)?;
    let local_notify = method(code, ui_t, "localNotify")?.findex;
    let (ln_args, _) = sig(code, local_notify)?;
    if ln_args.len() != 3 || ln_args[0] != ui_t || ln_args[1] != t.str_ {
        bail!("unexpected GameUI.localNotify signature");
    }
    let has_title = match &code.types[ln_args[2].0] {
        Type::Virtual { fields } => fields
            .iter()
            .any(|f| s(code, f.name) == "title" && f.t == t.str_),
        _ => false,
    };
    if !has_title {
        bail!("localNotify options have no String title");
    }
    let ntf = Ntf {
        local_notify,
        opts_t: ln_args[2],
        title_s: string_index(code, "title")?,
        dynobj: prim("dynobj", |t| matches!(t, Type::DynObj))?,
    };
    let m_windows = field(code, g_mode.1, "windows")?;
    let u_windows = field(code, g_gui.1, "windows")?;
    if m_windows.1 != arr_t || u_windows.1 != arr_t {
        bail!("BaseUI.windows is not an ArrayObj");
    }
    let a_len = typed(arr_t, "length", t.i32_)?;
    let a_raw = field(code, arr_t, "array")?;
    let w_name = field(code, win_t, "windowTypeName")?;
    if w_name.1 != t.str_ {
        bail!("Window.windowTypeName is not a String");
    }
    let w_try_close = slot(code, win_t, "tryClose")?;
    let w_close = slot(code, win_t, "close")?;
    let w_modal = field(code, win_t, "modal")?;
    let modal_none = match &code.types[w_modal.1 .0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|c| s(code, c.name) == "None" && c.params.is_empty()),
        _ => None,
    }
    .context("Window.modal is not an enum with a None construct")?;
    let modal_none = i32::try_from(modal_none)?;
    let act_t = obj_type(code, "ui.win.Activity")?;
    let act_started = typed(act_t, "started", t.bool_)?;
    let unit_t = obj_type(code, "st.Unit")?;
    let in_troop = method(code, unit_t, "isInTroop")?.findex;
    let in_troop_t = match sig(code, in_troop)? {
        (a, r) if a == [unit_t] && matches!(code.types[r.0], Type::Null(x) if x == t.bool_) => r,
        _ => bail!("unexpected Unit.isInTroop signature"),
    };
    let mut locks = vec![];
    for (name, guard) in LOCK_WINDOWS {
        let ct = obj_type(code, name)?;
        if !is_sub(code, ct, win_t) {
            bail!("{name} is not a ui.Window");
        }
        let (g, gt) = class_global(code, name)?;
        let act = if guard == 2 {
            Some(typed(ct, "act", act_t)?)
        } else {
            None
        };
        let unit = if guard == 3 {
            Some(typed(ct, "unit", unit_t)?)
        } else {
            None
        };
        locks.push(Lock {
            g,
            gt,
            ct,
            guard,
            act,
            unit,
        });
    }
    let aw_t = obj_type(code, "ui.win.ActivityWindow")?;
    if !is_sub(code, aw_t, win_t) {
        bail!("ui.win.ActivityWindow is not a ui.Window");
    }
    let act_win_cls = class_global(code, "ui.win.ActivityWindow")?;
    let set_locked = method(code, bp_t, "set_lockedWith")?.findex;
    match sig(code, set_locked)? {
        (a, r) if a.len() == 2 && a[0] == bp_t && a[1] == bp_locked.1 && r == bp_locked.1 => {}
        s => bail!("unexpected BasePlayer.set_lockedWith signature {s:?}"),
    }
    let mut modal_ok = vec![];
    for name in MODAL_OK {
        if !is_sub(code, obj_type(code, name)?, win_t) {
            bail!("{name} is not a ui.Window");
        }
        modal_ok.push(class_global(code, name)?);
    }
    let npc_t = obj_type(code, "ent.p.Npc")?;
    if !is_sub(code, npc_t, bp_locked.1) {
        bail!("ent.p.Npc is not a BasePlayer.lockedWith type");
    }
    let npc_cls = class_global(code, "ent.p.Npc")?;
    let base_t = obj_type(code, "hl.BaseType")?;
    let check = method(code, base_t, "check")?;
    if fun_args(code, check).len() != 2 || check.t.as_fun(code).map(|f| f.ret) != Some(t.bool_) {
        bail!("hl.BaseType.check is not (BaseType, v) -> Bool");
    }

    let m = |o: RefType, name: &str, want_args: &[RefType], want_ret: RefType| -> Result<RefFun> {
        let f = method(code, o, name)?.findex;
        if sig(code, f)? != (want_args.to_vec(), want_ret) {
            bail!("unexpected {name} signature");
        }
        Ok(f)
    };

    // ui.win.Dialog: Escape's tryClose = `visible && allowLeave -> leave(null)`.
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    if !is_sub(code, dlg_t, win_t) {
        bail!("ui.win.Dialog is not a ui.Window");
    }
    let leave_btn = field(code, dlg_t, "leaveButton")?;
    let choices = field(code, dlg_t, "currentChoices")?;
    if obj(code, leave_btn.1).is_err() || !matches!(code.types[choices.1 .0], Type::Virtual { .. })
    {
        bail!("Dialog.leaveButton / currentChoices have unexpected types");
    }
    let dlg_try_close = proto(code, dlg_t, "tryClose")?;
    if sig(code, dlg_try_close)? != (vec![dlg_t], t.bool_) {
        bail!("unexpected Dialog.tryClose signature");
    }
    let dlg_leave = method(code, dlg_t, "leave")?.findex;
    let leave_cb = match sig(code, dlg_leave)? {
        (a, r)
            if a.len() == 2
                && a[0] == dlg_t
                && r == t.void
                && matches!(code.types[a[1].0], Type::Fun(_)) =>
        {
            a[1]
        }
        _ => bail!("unexpected Dialog.leave signature"),
    };
    let tc = &code.functions[fun_index(code, dlg_try_close)?];
    let api_allow = tc.ops.iter().any(|op| {
        matches!(op, Opcode::Field { obj, field, .. }
            if obj_field_name(code, tc.regs[obj.0 as usize], *field) == Some("allowLeave"))
    });
    let calls_leave = tc
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::Call2 { fun, .. } if *fun == dlg_leave));
    if !api_allow || !calls_leave {
        bail!("Dialog.tryClose does not test allowLeave and call leave()");
    }
    let dlg = Dlg {
        t: dlg_t,
        cls: class_global(code, "ui.win.Dialog")?,
        already: typed(dlg_t, "alreadyLeave", t.bool_)?,
        visible: typed(dlg_t, "visible", t.bool_)?,
        leave_btn,
        try_close: dlg_try_close,
        reset: m(dlg_t, "resetChoices", &[dlg_t], t.void)?,
        leave: dlg_leave,
        leave_cb,
    };

    let is_multi = m(game_t, "get_isMulti", &[game_t], t.bool_)?;
    let in_fade = m(game_t, "inFade", &[game_t], t.bool_)?;
    let paused = m(game_t, "get_paused", &[game_t], t.bool_)?;
    let global_windows = m(ui_t, "hasGlobalWindows", &[ui_t], t.bool_)?;
    let any_locked = method(code, game_t, "anyPlayerLocked")?.findex;
    let lock_ref = match sig(code, any_locked)? {
        (a, r)
            if r == t.bool_
                && a.len() == 2
                && a[0] == game_t
                && matches!(code.types[a[1].0], Type::Ref(b) if b == t.bool_) =>
        {
            a[1]
        }
        _ => bail!("unexpected Game.anyPlayerLocked signature"),
    };
    let get_locked = m(game_t, "getPlayerLocked", &[game_t, t.bool_], arr_t)?;
    let static_fn = crate::diag::static_fn;
    let pad_active = static_fn(code, "gamepad.$Pad", "get_active")?.findex;
    if sig(code, pad_active)? != (vec![], t.bool_) {
        bail!("unexpected Pad.get_active signature");
    }
    let std_string = static_fn(code, "$Std", "string")?.findex;
    if sig(code, std_string)? != (vec![dyn_t], t.str_) {
        bail!("unexpected Std.string signature");
    }
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![t.str_, t.str_], t.str_) {
        bail!("unexpected String.__add__ signature");
    }
    let println = static_fn(code, "$Sys", "println")?.findex;
    if sig(code, println)?.0 != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let sys_time = native(code, "sys_time", &[], t.f64_)?;

    // PlaceView.tryClose: the refusals flWhy mirrors, in this order.
    let tc = method(code, view_t, "tryClose")?;
    let try_close_fi = fun_index(code, tc.findex)?;
    let reads = |f: &Function, want: RefField, on: RefType| {
        f.ops.iter().position(|op| match op {
            Opcode::Field { obj, field, .. } => *field == want && f.regs[obj.0 as usize] == on,
            Opcode::GetThis { field, .. } => *field == want && f.regs[0] == on,
            _ => false,
        })
    };
    let calls_at = |f: &Function, want: RefFun| {
        f.ops.iter().position(|op| {
            matches!(op, Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } if *fun == want)
        })
    };
    let order = [
        reads(tc, p_lock_leave, place_t),
        calls_at(tc, any_locked),
        reads(tc, st_cine, g_state.1),
        calls_at(tc, pad_active),
        calls_at(tc, global_windows),
        calls_at(tc, in_fade),
        reads(tc, g_auth, game_t),
        reads(tc, ls_forced, p_leave_state.1),
        reads(tc, v_entering, view_t),
    ];
    if order.iter().any(Option::is_none) || !order.windows(2).all(|w| w[0] < w[1]) {
        bail!("PlaceView.tryClose: refusals not found in the expected order: {order:?}");
    }
    let leave_slot = slot(code, view_t, "leave")?;
    // leave(onEnd: () -> Void): tryClose passes null.
    let v_leave_cb = match sig(code, method(code, view_t, "leave")?.findex)? {
        (a, r)
            if a.len() == 2
                && a[0] == view_t
                && r == t.void
                && matches!(code.types[a[1].0], Type::Fun(_)) =>
        {
            a[1]
        }
        _ => bail!("unexpected PlaceView.leave signature"),
    };
    if !tc
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::CallThis { field, .. } if *field == leave_slot))
    {
        bail!("PlaceView.tryClose does not call leave()");
    }
    if !no_jump_to(tc, 0) {
        bail!("PlaceView.tryClose: a jump targets op 0");
    }

    // PlaceView.update: the hook goes in front of op 0.
    let upd = method(code, view_t, "update")?;
    if fun_args(code, upd) != [view_t, t.f64_] {
        bail!("unexpected PlaceView.update signature");
    }
    let update_fi = fun_index(code, upd.findex)?;
    if !no_jump_to(upd, 0) {
        bail!("PlaceView.update: a jump targets op 0");
    }

    // PlaceView.leave: `if (game.paused) return;` then `if (leaving) return;`.
    let lv = method(code, view_t, "leave")?;
    let leave_fi = fun_index(code, lv.findex)?;
    let pc = calls_at(lv, paused).context("PlaceView.leave: no paused test")?;
    let paused_ret = pc + 2;
    let shape = match (&lv.ops[pc], lv.ops.get(pc + 1), lv.ops.get(paused_ret)) {
        (
            Opcode::Call1 { dst, .. },
            Some(Opcode::JFalse { cond, offset: 1 }),
            Some(Opcode::Ret { .. }),
        ) => cond == dst,
        _ => false,
    };
    if !shape || reads(lv, v_leaving, view_t).is_none_or(|i| i < paused_ret) {
        bail!("PlaceView.leave: no `if (paused) return` before the leaving test");
    }
    if !no_jump_to(lv, paused_ret) {
        bail!("PlaceView.leave: a jump targets the paused return");
    }

    // PlaceContent.update: exit.set_enable(!lockLeave && !anyPlayerLocked).
    let pc_t = obj_type(code, "world.PlaceContent")?;
    let cu = method(code, pc_t, "update")?;
    let content_fi = fun_index(code, cu.findex)?;
    let sites: Vec<usize> = (0..cu.ops.len().saturating_sub(5))
        .filter(|&i| {
            matches!(
                (&cu.ops[i], &cu.ops[i + 1], &cu.ops[i + 2], &cu.ops[i + 3], &cu.ops[i + 4]),
                (
                    Opcode::Call2 { dst, fun, .. },
                    Opcode::JTrue { cond, offset: 2 },
                    Opcode::Bool { value: ValBool(true), .. },
                    Opcode::JAlways { offset: 1 },
                    Opcode::Bool { value: ValBool(false), .. },
                ) if *fun == any_locked && cond == dst
            )
        })
        .collect();
    let [call_at] = sites[..] else {
        bail!(
            "PlaceContent.update: expected one `!anyPlayerLocked` enable test, found {}",
            sites.len()
        );
    };
    let content_at = call_at + 1;
    let set_enable = &cu.ops[call_at + 5];
    let enables = matches!(set_enable, Opcode::Call2 { fun, .. }
        if code.functions.iter().any(|f| f.findex == *fun && s(code, f.name) == "set_enable"));
    if !enables {
        bail!("PlaceContent.update: the lock test does not feed set_enable");
    }

    let mut p = Plan {
        dbg_file: debug_file(code, "src/world/PlaceView.hx")?,
        t,
        view_t,
        game_t,
        win_t,
        v_game,
        v_place,
        v_leaving,
        v_entering,
        v_focus,
        p_lock_leave,
        p_leave_state,
        ls_forced,
        ls_players,
        g_me,
        g_auth,
        g_state,
        g_ui,
        g_ctrl,
        g_mode,
        g_gui,
        st_cine,
        st_players,
        sp_array,
        c_lock_sync,
        c_wait_locks,
        bp_locked,
        bp_name,
        ntf,
        m_windows,
        u_windows,
        arr_t,
        a_len,
        a_raw,
        w_name,
        w_try_close,
        w_close,
        w_modal,
        modal_none,
        locks,
        modal_ok,
        npc_cls,
        act_t,
        act_started,
        base_check: check.findex,
        dlg,
        v_leave: leave_slot,
        v_leave_cb,
        set_locked,
        act_win_cls,
        in_troop,
        in_troop_t,
        is_multi,
        any_locked: (any_locked, lock_ref),
        get_locked,
        in_fade,
        paused,
        pad_active,
        global_windows,
        std_string,
        str_add,
        println,
        sys_time,
        update_fi,
        try_close_fi,
        leave_fi,
        paused_ret,
        content_fi,
        content_at,
        tav: None,
        camp: None,
    };
    match plan_tavern(code, &p, place_t) {
        Ok(tv) => p.tav = Some(tv),
        Err(e) => eprintln!("force leave: owned tavern part skipped: {e:#}"),
    }
    match plan_camp(code, &p) {
        Ok((c, locks)) => {
            p.locks.extend(locks);
            p.camp = Some(c);
        }
        Err(e) => eprintln!("force leave: camp part skipped: {e:#}"),
    }
    Ok(p)
}

/// Functions that call `want` directly.
fn callers(code: &Bytecode, want: RefFun) -> Vec<RefFun> {
    code.functions
        .iter()
        .filter(|f| {
            f.ops.iter().any(|op| match op {
                Opcode::Call0 { fun, .. }
                | Opcode::Call1 { fun, .. }
                | Opcode::Call2 { fun, .. }
                | Opcode::Call3 { fun, .. }
                | Opcode::Call4 { fun, .. }
                | Opcode::CallN { fun, .. } => *fun == want,
                _ => false,
            })
        })
        .map(|f| f.findex)
        .collect()
}

/// The camp sites: Controller.toggleCamp__impl, GameUI.toggleCamp (checked
/// only), CampMode.update, Tool.closeActionWindow(__impl), and the camp tool
/// windows for flClosable.
fn plan_camp(code: &Bytecode, p: &Plan) -> Result<(Camp, Vec<Lock>)> {
    let t = &p.t;
    let ctrl_t = p.g_ctrl.1;
    let ui_t = p.g_ui.1;
    let typed = |o: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, o, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let m =
        |o: RefType, name: &str, want_args: &[RefType], want_ret: RefType| -> Result<&Function> {
            let f = method(code, o, name)?;
            if sig(code, f.findex)? != (want_args.to_vec(), want_ret) {
                bail!("unexpected {name} signature");
            }
            Ok(f)
        };
    let calls = |f: &Function, want: RefFun| {
        f.ops.iter().position(|op| match op {
            Opcode::Call0 { fun, .. }
            | Opcode::Call1 { fun, .. }
            | Opcode::Call2 { fun, .. }
            | Opcode::Call3 { fun, .. }
            | Opcode::Call4 { fun, .. }
            | Opcode::CallN { fun, .. } => *fun == want,
            _ => false,
        })
    };

    let cm_t = obj_type(code, "world.camp.CampMode")?;
    if !is_sub(code, cm_t, p.g_mode.1) {
        bail!("CampMode is not a GameMode");
    }
    let cm_cls = class_global(code, "world.camp.CampMode")?;
    let cm_game = typed(cm_t, "game", p.game_t)?;
    let cm_rest = field(code, cm_t, "rest")?;
    if obj(code, cm_rest.1).is_err() && !matches!(code.types[cm_rest.1 .0], Type::Virtual { .. }) {
        bail!("CampMode.rest is not an object");
    }
    let tool_t = obj_type(code, "st.item.Tool")?;
    if !is_sub(code, tool_t, p.bp_locked.1) {
        bail!("st.item.Tool is not a lockedWith type");
    }
    let tool_cls = class_global(code, "st.item.Tool")?;
    let tool_game = typed(tool_t, "game", p.game_t)?;

    // Tool.closeActionWindow__impl: `if (game.me.lockedWith == this) { w =
    // game.mode.getCurrentWindow(); if (w != null) w.close(); game.mode.setWindow(null); }`,
    // reached only through its RPC wrapper / RPC dispatch, which nothing calls.
    let imp = m(tool_t, "closeActionWindow__impl", &[tool_t], t.void)?;
    let rpc = m(tool_t, "closeActionWindow", &[tool_t], t.void)?;
    if calls(rpc, imp.findex).is_none() {
        bail!("Tool.closeActionWindow does not call its __impl");
    }
    let reads_lock = imp.ops.iter().any(|op| {
        matches!(op, Opcode::Field { obj, field, .. }
            if *field == p.bp_locked.0 && imp.regs[obj.0 as usize] == p.g_me.1)
    });
    let tests_this = imp
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::JNotEq { a, b, .. } if *a == Reg(0) || *b == Reg(0)));
    let closes = imp
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::CallMethod { field, .. } if *field == p.w_close));
    if !reads_lock || !tests_this || !closes || !no_jump_to(imp, 0) {
        bail!("Tool.closeActionWindow__impl has an unexpected shape");
    }
    let rpc_dispatch = proto(code, tool_t, "networkRPC")?;
    let imp_callers = callers(code, imp.findex);
    if imp_callers
        .iter()
        .any(|&f| f != rpc.findex && f != rpc_dispatch)
    {
        bail!("Tool.closeActionWindow__impl has unexpected callers: {imp_callers:?}");
    }
    // The game's only caller: GridData.removeTool__impl (a tool taken off the
    // camp grid closes its user's window).
    let grid_t = obj_type(code, "st.player.GridData")?;
    let remove = method(code, grid_t, "removeTool__impl")?.findex;
    let rpc_callers = callers(code, rpc.findex);
    if rpc_callers != [remove] {
        bail!("Tool.closeActionWindow has unexpected callers: {rpc_callers:?}");
    }

    // Controller.toggleCamp__impl: `if (isLocked() || waitLocks.length > 0)
    // return; game.ui.toggleCamp();`
    let ctrl_game = typed(ctrl_t, "game", p.game_t)?;
    let is_locked = m(ctrl_t, "isLocked", &[ctrl_t], t.bool_)?.findex;
    let ask = m(ctrl_t, "toggleCamp__impl", &[ctrl_t], t.void)?;
    let gtc = m(ui_t, "toggleCamp", &[ui_t], t.void)?;
    if calls(ask, is_locked).is_none() || calls(ask, gtc.findex).is_none() || !no_jump_to(ask, 0) {
        bail!("Controller.toggleCamp__impl has an unexpected shape");
    }
    // GameUI.toggleCamp: the lock test, the CampMode test, the rest / modal
    // window test (getAllWindows), then syncLeaveMode, in this order.
    let all_windows = m(ui_t, "getAllWindows", &[ui_t], p.arr_t)?.findex;
    let sync = method(code, ctrl_t, "syncLeaveMode")?.findex;
    let order = [
        calls(gtc, p.any_locked.0),
        gtc.ops
            .iter()
            .position(|op| matches!(op, Opcode::GetGlobal { global, .. } if *global == cm_cls.0)),
        gtc.ops.iter().position(
            |op| matches!(op, Opcode::Field { obj, field, .. } if *field == cm_rest.0 && gtc.regs[obj.0 as usize] == cm_t),
        ),
        calls(gtc, all_windows),
        calls(gtc, sync),
    ];
    if order.iter().any(Option::is_none) || !order.windows(2).all(|w| w[0] < w[1]) {
        bail!("GameUI.toggleCamp: tests not found in the expected order: {order:?}");
    }
    // Its window tests the pending camp leave skips (the host's own windows never
    // hold it): `if (!canToggleButton(..)) return;` (`Call4; JTrue +1; Ret`) and
    // `if (canLeaveCamp)` (the first `JFalse` after getAllWindows on a register
    // only `Bool` ops write between the two).
    let ctb = method(code, ui_t, "canToggleButton")?.findex;
    let ct_at = calls(gtc, ctb).context("GameUI.toggleCamp: no canToggleButton call")?;
    let ct_ret = ct_at + 2;
    let ct_ok = match (&gtc.ops[ct_at], gtc.ops.get(ct_at + 1), gtc.ops.get(ct_ret)) {
        (
            Opcode::Call4 { dst, .. },
            Some(Opcode::JTrue { cond, offset: 1 }),
            Some(Opcode::Ret { .. }),
        ) => cond == dst,
        _ => false,
    };
    if !ct_ok || ct_at > order[1].unwrap_or(0) || !no_jump_to(gtc, ct_ret) {
        bail!("GameUI.toggleCamp: unexpected canToggleButton test");
    }
    let (aw_at, sync_at) = (order[3].unwrap_or(0), order[4].unwrap_or(0));
    let cl_jf = (aw_at..sync_at)
        .find(|&i| matches!(gtc.ops[i], Opcode::JFalse { .. }))
        .context("GameUI.toggleCamp: no canLeaveCamp test")?;
    let Opcode::JFalse {
        cond: cl_reg,
        offset: cl_off,
    } = gtc.ops[cl_jf]
    else {
        unreachable!()
    };
    let writes = |i: usize| {
        let o = format!("{:?}", gtc.ops[i]);
        o.contains(&format!("dst: Reg({}),", cl_reg.0))
            || o.contains(&format!("dst: Reg({}) ", cl_reg.0))
    };
    let bool_writes = (aw_at..cl_jf).filter(|&i| writes(i)).collect::<Vec<_>>();
    if bool_writes.len() < 2
        || !bool_writes
            .iter()
            .all(|&i| matches!(gtc.ops[i], Opcode::Bool { .. }))
        || cl_off <= 0
        || cl_jf as i64 + 1 + cl_off as i64 > sync_at as i64 + 64
    {
        bail!("GameUI.toggleCamp: unexpected canLeaveCamp test");
    }
    let upd = m(cm_t, "update", &[cm_t, t.f64_], t.void)?;
    if !no_jump_to(upd, 0) {
        bail!("CampMode.update: a jump targets op 0");
    }

    let mut locks = vec![];
    for name in CAMP_WINDOWS {
        let ct = obj_type(code, name)?;
        if !is_sub(code, ct, p.win_t) {
            bail!("{name} is not a ui.Window");
        }
        let (g, gt) = class_global(code, name)?;
        locks.push(Lock {
            g,
            gt,
            ct,
            guard: 1,
            act: None,
            unit: None,
        });
    }
    if p.locks.len() + locks.len() > SKIP[0].len() {
        bail!("too many lock window classes");
    }
    Ok((
        Camp {
            cm_t,
            cm_cls,
            cm_game,
            cm_rest,
            tool_t,
            tool_cls,
            tool_game,
            close_rpc: rpc.findex,
            ctrl_game,
            is_locked,
            toggle: ask.findex,
            gtc_fi: fun_index(code, gtc.findex)?,
            ct_ret,
            cl_jf,
            ask_fi: fun_index(code, ask.findex)?,
            upd_fi: fun_index(code, upd.findex)?,
            close_fi: fun_index(code, imp.findex)?,
            dbg_file: debug_file(code, "src/world/camp/CampMode.hx")?,
        },
        locks,
    ))
}

/// Name of field `f` of object type `t`, if `t` is an object.
fn obj_field_name(code: &Bytecode, t: RefType, f: RefField) -> Option<&str> {
    let o = code.types[t.0].get_type_obj()?;
    o.fields.get(f.0).map(|x| s(code, x.name))
}

/// The owned-tavern sites: Tavern.askLeave__impl, Controller.closeTavern__impl,
/// TavernMode.update, and the leaveState proxy mark copied from
/// Place.setLeaveState__impl.
fn plan_tavern(code: &Bytecode, p: &Plan, place_t: RefType) -> Result<Tav> {
    let t = &p.t;
    let ctrl_t = p.g_ctrl.1;
    let tm_t = obj_type(code, "world.tavern.TavernMode")?;
    let tav_t = obj_type(code, "st.player.Tavern")?;
    let typed = |o: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, o, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let m =
        |o: RefType, name: &str, want_args: &[RefType], want_ret: RefType| -> Result<&Function> {
            let f = method(code, o, name)?;
            if sig(code, f.findex)? != (want_args.to_vec(), want_ret) {
                bail!("unexpected {name} signature");
            }
            Ok(f)
        };
    let tm_game = typed(tm_t, "game", p.game_t)?;
    let get_tavern = m(tm_t, "get_tavern", &[tm_t], tav_t)?.findex;
    let tav_game = typed(tav_t, "game", p.game_t)?;
    let tav_ls = typed(tav_t, "leaveState", p.p_leave_state.1)?;
    let ctrl_game = typed(ctrl_t, "game", p.game_t)?;
    let is_locked = m(ctrl_t, "isLocked", &[ctrl_t], t.bool_)?.findex;
    let sync = method(code, ctrl_t, "syncLeaveMode")?.findex;

    // The proxy mark, as Place.setLeaveState__impl writes `forced`:
    //   o = ls.obj; NullCheck o; bit = ls.bit; o.<mark>(bit); ls.forced = v
    let px_t = p.p_leave_state.1;
    let sl = method(code, place_t, "setLeaveState__impl")?;
    let px = |r: &Reg| sl.regs[r.0 as usize] == px_t;
    let hits: Vec<(F, RefField, RefField)> = (4..sl.ops.len())
        .filter_map(|k| match &sl.ops[k - 4..=k] {
            [Opcode::Field {
                dst: o,
                obj: x0,
                field: objf,
            }, Opcode::NullCheck { reg: o2 }, Opcode::Field {
                dst: b,
                obj: x1,
                field: bitf,
            }, Opcode::CallMethod {
                field: mark, args, ..
            }, Opcode::SetField {
                obj: x2,
                field: forced,
                ..
            }] if *forced == p.ls_forced
                && px(x0)
                && px(x1)
                && px(x2)
                && o == o2
                && args[..] == [*o, *b] =>
            {
                Some(((*objf, sl.regs[o.0 as usize]), *bitf, *mark))
            }
            _ => None,
        })
        .collect();
    let [(px_obj, px_bit, px_mark)] = hits[..] else {
        bail!(
            "Place.setLeaveState__impl: expected one marked `forced` write, found {}",
            hits.len()
        );
    };
    if !matches!(code.types[px_obj.1 .0], Type::Virtual { .. })
        || field_t(code, px_t, px_bit) != Some(t.i32_)
        || field_t(code, px_t, px_obj.0) != Some(px_obj.1)
    {
        bail!("leaveState proxy obj / bit have unexpected types");
    }

    // Tavern.askLeave__impl: `if (anyPlayerLocked(&true)) return; if
    // (ctrl.isLocked()) return; ctrl.syncLeaveMode(..)`.
    let ask = m(tav_t, "askLeave__impl", &[tav_t], t.void)?;
    let call_of = |f: &Function, want: RefFun| {
        f.ops.iter().position(|op| match op {
            Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } | Opcode::Call3 { fun, .. } => {
                *fun == want
            }
            _ => false,
        })
    };
    let order = [
        call_of(ask, p.any_locked.0),
        call_of(ask, is_locked),
        call_of(ask, sync),
    ];
    if order.iter().any(Option::is_none) || !order.windows(2).all(|w| w[0] < w[1]) {
        bail!("Tavern.askLeave__impl: lock tests / syncLeaveMode not found: {order:?}");
    }
    if !no_jump_to(ask, 0) {
        bail!("Tavern.askLeave__impl: a jump targets op 0");
    }
    // Controller.closeTavern__impl: `if (isLocked() || waitLocks.length > 0)
    // return; if (mode is TavernMode) syncLeaveMode(..)`.
    let ct = m(ctrl_t, "closeTavern__impl", &[ctrl_t], t.void)?;
    if call_of(ct, is_locked).is_none() || call_of(ct, sync).is_none() || !no_jump_to(ct, 0) {
        bail!("Controller.closeTavern__impl has an unexpected shape");
    }
    let upd = m(tm_t, "update", &[tm_t, t.f64_], t.void)?;
    if !no_jump_to(upd, 0) {
        bail!("TavernMode.update: a jump targets op 0");
    }
    Ok(Tav {
        tm_t,
        tm_cls: class_global(code, "world.tavern.TavernMode")?,
        tm_game,
        get_tavern,
        tav_t,
        tav_game,
        tav_ls,
        ctrl_game,
        px_obj,
        px_bit,
        px_mark,
        is_locked,
        ask: ask.findex,
        ask_fi: fun_index(code, ask.findex)?,
        close_fi: fun_index(code, ct.findex)?,
        upd_fi: fun_index(code, upd.findex)?,
        dbg_file: debug_file(code, "src/world/tavern/TavernMode.hx")?,
    })
}

fn field_t(code: &Bytecode, t: RefType, f: RefField) -> Option<RefType> {
    code.types[t.0].get_type_obj()?.fields.get(f.0).map(|x| x.t)
}

struct Globals {
    /// Last logged pending reason + 1 (0 = none yet).
    last: RefGlobal,
    /// The window the pending leave closed last.
    closed: RefGlobal,
    /// sys_time of the last "paused" refusal line.
    paused_at: RefGlobal,
    /// Host: the TavernMode whose leave is pending (null = none).
    tl_mode: RefGlobal,
    /// Last logged tavern pending reason + 1.
    tl_last: RefGlobal,
    /// The window the pending tavern leave closed last.
    tl_closed: RefGlobal,
}

/// Int constant refs for every reason code.
fn reason_ints(code: &mut Bytecode) -> Vec<hlbc::types::RefInt> {
    (0..REASONS.len() as i32)
        .map(|i| int_const(code, i))
        .collect()
}

/// `Field md = w.modal; JNull md -> l; mi = EnumIndex md; JEq mi, none -> l`:
/// falls through only when `w` is a modal window.
fn modal_test(a: &mut Asm, p: &Plan, w: Reg, md: Reg, mi: Reg, none: Reg, l: &'static str) {
    a.op(Opcode::Field {
        dst: md,
        obj: w,
        field: p.w_modal.0,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, l);
    a.op(Opcode::EnumIndex { dst: mi, value: md });
    a.jmp(
        Opcode::JEq {
            a: mi,
            b: none,
            offset: 0,
        },
        l,
    );
}

/// Loop over `lst` top-down: `i = lst.length; head: if (i <= 0) goto out;
/// w = lst[--i]; if (w == null) goto head;` (the caller emits the body).
#[allow(clippy::too_many_arguments)]
fn top_down(
    a: &mut Asm,
    p: &Plan,
    lst: Reg,
    i: Reg,
    zero: Reg,
    raw: Reg,
    d: Reg,
    w: Reg,
    head: &'static str,
    out: &'static str,
) {
    a.op(Opcode::Field {
        dst: i,
        obj: lst,
        field: p.a_len,
    });
    a.loop_head(head);
    a.jmp(
        Opcode::JSLte {
            a: i,
            b: zero,
            offset: 0,
        },
        out,
    );
    a.op(Opcode::Decr { dst: i });
    a.op(Opcode::Field {
        dst: raw,
        obj: lst,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::UnsafeCast { dst: w, src: d });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, head);
}

/// `flWhy(view, poll) -> I32`: the first thing that makes `view.tryClose()`
/// refuse (same order), and with `poll` what the pending leave also waits on.
fn add_why(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let ints = reason_ints(code);
    let t = &p.t;
    let mut r = Regs(vec![p.view_t, t.bool_]);
    let (game, place, b, code_r, nul) = (
        r.r(p.game_t),
        r.r(p.v_place.1),
        r.r(t.bool_),
        r.r(t.i32_),
        r.r(p.any_locked.1),
    );
    let (st, ui, ls, arr, n, total) = (
        r.r(p.g_state.1),
        r.r(p.g_ui.1),
        r.r(p.p_leave_state.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
    );
    let (sp, spa, all, fe, ctrl, wl, zero) = (
        r.r(p.st_players.1),
        r.r(p.sp_array.1),
        r.r(p.arr_t),
        r.r(p.v_focus.1),
        r.r(p.g_ctrl.1),
        r.r(p.arr_t),
        r.r(t.i32_),
    );
    let mut a = Asm::new();
    let ret = |a: &mut Asm, k: usize| {
        a.op(Opcode::Int {
            dst: code_r,
            ptr: ints[k],
        });
        a.op(Opcode::Ret { ret: code_r });
    };
    a.op(Opcode::GetThis {
        dst: game,
        field: p.v_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "ok",
    );
    a.op(Opcode::GetThis {
        dst: place,
        field: p.v_place.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: place,
            offset: 0,
        },
        "ok",
    );
    // 1 lockLeave
    a.op(Opcode::Field {
        dst: b,
        obj: place,
        field: p.p_lock_leave,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "locked");
    ret(&mut a, 1);
    // 2 a connected player lockedWith something
    a.label("locked");
    a.op(Opcode::Null { dst: nul });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.any_locked.0,
        arg0: game,
        arg1: nul,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "cine");
    ret(&mut a, R_LOCKED);
    // 3 cinematic
    a.label("cine");
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.g_state.0,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "pad");
    a.op(Opcode::Field {
        dst: b,
        obj: st,
        field: p.st_cine,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "pad");
    ret(&mut a, 3);
    // 4 gamepad + a global window (tryClose only: the pending leave never waits
    // on the host's own windows; it calls leave() itself)
    a.label("pad");
    a.jmp(
        Opcode::JTrue {
            cond: Reg(1),
            offset: 0,
        },
        "fade",
    );
    a.op(Opcode::Call0 {
        dst: b,
        fun: p.pad_active,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "fade");
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.g_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "fade");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.global_windows,
        arg0: ui,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "fade");
    ret(&mut a, 4);
    // 5 fade
    a.label("fade");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "auth");
    ret(&mut a, R_FADE);
    // a client's tryClose forwards to the host: nothing else refuses there
    a.label("auth");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ok");
    // 6 votes: multi && !forced && players.length < state.players.length
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "dialog");
    a.op(Opcode::Field {
        dst: ls,
        obj: place,
        field: p.p_leave_state.0,
    });
    a.jmp(Opcode::JNull { reg: ls, offset: 0 }, "dialog");
    a.op(Opcode::Field {
        dst: b,
        obj: ls,
        field: p.ls_forced,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "dialog");
    a.op(Opcode::Field {
        dst: arr,
        obj: ls,
        field: p.ls_players.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "dialog",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.a_len,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "dialog");
    a.op(Opcode::Field {
        dst: sp,
        obj: st,
        field: p.st_players.0,
    });
    a.jmp(Opcode::JNull { reg: sp, offset: 0 }, "dialog");
    a.op(Opcode::Field {
        dst: spa,
        obj: sp,
        field: p.sp_array.0,
    });
    a.op(Opcode::SafeCast { dst: all, src: spa });
    a.jmp(
        Opcode::JNull {
            reg: all,
            offset: 0,
        },
        "dialog",
    );
    a.op(Opcode::Field {
        dst: total,
        obj: all,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: n,
            b: total,
            offset: 0,
        },
        "dialog",
    );
    ret(&mut a, 6);
    // 7 the camera is moving into a dialog
    a.label("dialog");
    a.op(Opcode::GetThis {
        dst: b,
        field: p.v_entering,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "poll");
    ret(&mut a, 7);
    // poll only: 8 a dialog is open, 9 the mode-switch barrier runs, 10 paused
    a.label("poll");
    a.jmp(
        Opcode::JFalse {
            cond: Reg(1),
            offset: 0,
        },
        "ok",
    );
    a.op(Opcode::GetThis {
        dst: fe,
        field: p.v_focus.0,
    });
    a.jmp(Opcode::JNull { reg: fe, offset: 0 }, "sync");
    ret(&mut a, R_DIALOG);
    a.label("sync");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: game,
        field: p.g_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "paused",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: ctrl,
        field: p.c_lock_sync,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "busy");
    a.op(Opcode::Field {
        dst: wl,
        obj: ctrl,
        field: p.c_wait_locks.0,
    });
    a.jmp(Opcode::JNull { reg: wl, offset: 0 }, "paused");
    a.op(Opcode::Field {
        dst: n,
        obj: wl,
        field: p.a_len,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "paused",
    );
    a.label("busy");
    ret(&mut a, R_SYNC);
    a.label("paused");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.paused,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ok");
    ret(&mut a, R_PAUSED);
    a.label("ok");
    ret(&mut a, 0);
    push_fn(
        code,
        vec![p.view_t, t.bool_],
        t.i32_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

const TEXT_LABELS: [&str; 15] = [
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10", "t11", "t12", "t13", "t14",
];

/// `flLog(game, prefix, why)`: println(flText(game, prefix, why)).
fn add_log(code: &mut Bytecode, p: &Plan, text: RefFun) -> Result<RefFun> {
    let t = &p.t;
    let mut r = Regs(vec![p.game_t, t.str_, t.i32_]);
    let (s_r, v) = (r.r(t.str_), r.r(t.void));
    let mut a = Asm::new();
    a.op(Opcode::Call3 {
        dst: s_r,
        fun: text,
        arg0: Reg(0),
        arg1: Reg(1),
        arg2: Reg(2),
    });
    // A String goes to println's Dyn argument as is.
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, t.str_, t.i32_],
        t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `flNotify(game, text)`: `game.ui.localNotify("ArenaNotif", {title: text})`, the
/// on-screen line follow.rs uses (log line, not in the journal); local only.
fn add_notify(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let id = str_global(code, p.t.str_, NOTIFY_ID);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t, t.str_]);
    let (ui, sid, o, opts, v) = (
        r.r(p.g_ui.1),
        r.r(t.str_),
        r.r(p.ntf.dynobj),
        r.r(p.ntf.opts_t),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: ui,
        obj: Reg(0),
        field: p.g_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: sid,
        global: id,
    });
    a.op(Opcode::New { dst: o });
    a.op(Opcode::DynSet {
        obj: o,
        field: p.ntf.title_s,
        src: Reg(1),
    });
    a.op(Opcode::ToVirtual { dst: opts, src: o });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.ntf.local_notify,
        arg0: ui,
        arg1: sid,
        arg2: opts,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, t.str_],
        t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `flText(game, prefix, why) -> String`: prefix + text(why); for "player busy"
/// also the first locked player's `lockedWith`, whose it is and their name.
fn add_text(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let ints = reason_ints(code);
    let texts: Vec<_> = REASONS
        .iter()
        .map(|x| str_global(code, p.t.str_, x))
        .collect();
    let space = str_global(code, p.t.str_, " ");
    let own = str_global(code, p.t.str_, " (this player)");
    let other = str_global(code, p.t.str_, " (other player)");
    let unknown = str_global(code, p.t.str_, "?");
    let t = &p.t;
    let bp_t = p.g_me.1;
    let mut r = Regs(vec![p.game_t, t.str_, t.i32_]);
    let (s_r, x, y, k, arr, n, zero) = (
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.i32_),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
    );
    let (raw, d, pl, lw, me, b) = (
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(bp_t),
        r.r(p.bp_locked.1),
        r.r(bp_t),
        r.r(t.bool_),
    );
    let game = Reg(0);
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: x,
        global: unknown,
    });
    for (i, g) in texts.iter().enumerate() {
        a.op(Opcode::Int {
            dst: k,
            ptr: ints[i],
        });
        a.jmp(
            Opcode::JNotEq {
                a: Reg(2),
                b: k,
                offset: 0,
            },
            TEXT_LABELS[i],
        );
        a.op(Opcode::GetGlobal { dst: x, global: *g });
        a.jmp(Opcode::JAlways { offset: 0 }, "text");
        a.label(TEXT_LABELS[i]);
    }
    a.label("text");
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: Reg(1),
        arg1: x,
    });
    a.op(Opcode::Int {
        dst: k,
        ptr: ints[R_LOCKED],
    });
    a.jmp(
        Opcode::JNotEq {
            a: Reg(2),
            b: k,
            offset: 0,
        },
        "print",
    );
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "print",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: arr,
        fun: p.get_locked,
        arg0: game,
        arg1: b,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "print",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "print",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: zero,
    });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "print");
    a.op(Opcode::Field {
        dst: lw,
        obj: pl,
        field: p.bp_locked.0,
    });
    // An object goes to Std.string's Dyn argument as is (what the compiler emits).
    a.op(Opcode::Call1 {
        dst: x,
        fun: p.std_string,
        arg0: lw,
    });
    a.op(Opcode::GetGlobal {
        dst: y,
        global: space,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: y,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: x,
    });
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(
        Opcode::JEq {
            a: pl,
            b: me,
            offset: 0,
        },
        "own",
    );
    a.op(Opcode::GetGlobal {
        dst: y,
        global: other,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "who");
    a.label("own");
    a.op(Opcode::GetGlobal {
        dst: y,
        global: own,
    });
    a.label("who");
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: y,
    });
    // ... and the player's name
    a.op(Opcode::Field {
        dst: x,
        obj: pl,
        field: p.bp_name,
    });
    a.jmp(Opcode::JNull { reg: x, offset: 0 }, "print");
    a.op(Opcode::GetGlobal {
        dst: y,
        global: space,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: y,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: x,
    });
    a.label("print");
    a.op(Opcode::Ret { ret: s_r });
    push_fn(
        code,
        vec![p.game_t, t.str_, t.i32_],
        t.str_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `flRefused(view)`, at the top of PlaceView.tryClose: prints why it is about to refuse.
fn add_refused(code: &mut Bytecode, p: &Plan, why: RefFun, log: RefFun) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let refused = str_global(code, p.t.str_, REFUSED);
    let t = &p.t;
    let mut r = Regs(vec![p.view_t]);
    let (b, c, zero, pre, v, game) = (
        r.r(t.bool_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.str_),
        r.r(t.void),
        r.r(p.game_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: p.v_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: c,
        fun: why,
        arg0: Reg(0),
        arg1: b,
    });
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.jmp(
        Opcode::JEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: pre,
        global: refused,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: log,
        arg0: game,
        arg1: pre,
        arg2: c,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.view_t], t.void, r.0, a.finish(), p.dbg_file)
}

/// `flClosable(game) -> Window`: the top window of a lock-holding class that
/// may be closed (`tryClose()` and its LOCK_WINDOWS guard), in game.mode.windows
/// then game.globalUI.windows. A location modal (MODAL_OK) is closed first only
/// when it sits directly over a guarded lock window in the same list (their
/// constructors pass a null parentWindow, so adjacency is the only link);
/// anywhere else it is a personal window: neither closed nor in the way. A
/// lock-class modal that cannot close (a started Alter) keeps the windows under
/// it. A UnitInfo is closed only while this player is locked with an NPC and
/// only as the sheet of a unit outside the troop (`!unit.isInTroop()`). Other
/// windows (own character sheet, inventory, trees, journal, options...) are
/// ignored.
fn add_closable(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let inone = int_const(code, p.modal_none);
    let unit_t = obj_type(code, "st.Unit")?;
    let t = &p.t;
    let mut r = Regs(vec![p.game_t]);
    let (mode, gui, lst, i, zero, none, raw, d, w) = (
        r.r(p.g_mode.1),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(p.win_t),
    );
    let (b, above, gm, md, mi, act) = (
        r.r(t.bool_),
        r.r(t.bool_),
        r.r(t.bool_),
        r.r(p.w_modal.1),
        r.r(t.i32_),
        r.r(p.act_t),
    );
    // cand: the window just above `w` in its list when it is a location modal
    let cand = r.r(p.win_t);
    let (npc, me, lw, ncr, un, nb) = (
        r.r(t.bool_),
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.npc_cls.1),
        r.r(unit_t),
        r.r(p.in_troop_t),
    );
    let cls: Vec<(Reg, Option<Reg>)> = p
        .locks
        .iter()
        .map(|l| {
            (
                r.r(l.gt),
                (l.act.is_some() || l.unit.is_some()).then(|| r.r(l.ct)),
            )
        })
        .collect();
    let mcls: Vec<(RefGlobal, Reg)> = p.modal_ok.iter().map(|&(g, gt)| (g, r.r(gt))).collect();
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    // Guard 3: falls through only for the sheet of a unit outside the troop
    // while this player is locked with an NPC; else jumps to `not`.
    let sheet = |a: &mut Asm, l: &Lock, cw: Reg, not: &'static str, tag: &str| {
        let out: &'static str = Box::leak(format!("{tag}_sheet").into_boxed_str());
        a.jmp(
            Opcode::JFalse {
                cond: npc,
                offset: 0,
            },
            not,
        );
        a.op(Opcode::UnsafeCast { dst: cw, src: w });
        a.op(Opcode::Field {
            dst: un,
            obj: cw,
            field: l.unit.expect("guard 3 unit"),
        });
        a.jmp(Opcode::JNull { reg: un, offset: 0 }, not);
        // isInTroop: null (no owner: a recruit / NPC) or Bool
        a.op(Opcode::Call1 {
            dst: nb,
            fun: p.in_troop,
            arg0: un,
        });
        a.jmp(Opcode::JNull { reg: nb, offset: 0 }, out);
        a.op(Opcode::SafeCast { dst: b, src: nb });
        a.jmp(Opcode::JTrue { cond: b, offset: 0 }, not);
        a.label(out);
    };
    // `w` (a modal window) belongs to the location: MODAL_OK -> `ok`, a lock
    // class (guard 3 only as the NPC sheet) -> `block` (it was not closable: it
    // keeps what is under it), else `skip` (ignored).
    let location_modal =
        |a: &mut Asm, site: &str, ok: &'static str, block: &'static str, skip: &'static str| {
            for &(g, cr) in &mcls {
                a.op(Opcode::GetGlobal { dst: cr, global: g });
                a.op(Opcode::Call2 {
                    dst: b,
                    fun: p.base_check,
                    arg0: cr,
                    arg1: w,
                });
                a.jmp(Opcode::JTrue { cond: b, offset: 0 }, ok);
            }
            for (j, (l, &(cr, cw))) in p.locks.iter().zip(&cls).enumerate() {
                a.op(Opcode::GetGlobal {
                    dst: cr,
                    global: l.g,
                });
                a.op(Opcode::Call2 {
                    dst: b,
                    fun: p.base_check,
                    arg0: cr,
                    arg1: w,
                });
                if l.guard == 3 {
                    let next = leak(format!("{site}_n{j}"));
                    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, next);
                    sheet(a, l, cw.expect("guard 3 reg"), next, next);
                    a.jmp(Opcode::JAlways { offset: 0 }, block);
                    a.label(next);
                } else {
                    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, block);
                }
            }
            a.jmp(Opcode::JAlways { offset: 0 }, skip);
        };
    let mut a = Asm::new();
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::Int {
        dst: none,
        ptr: inone,
    });
    // npc: this player's lock is an NPC (inspect)
    a.op(Opcode::Bool {
        dst: npc,
        value: ValBool(false),
    });
    a.op(Opcode::Field {
        dst: me,
        obj: Reg(0),
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "npc_done");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "npc_done");
    a.op(Opcode::GetGlobal {
        dst: ncr,
        global: p.npc_cls.0,
    });
    // An object goes to BaseType.check's Dyn argument as is.
    a.op(Opcode::Call2 {
        dst: npc,
        fun: p.base_check,
        arg0: ncr,
        arg1: lw,
    });
    a.label("npc_done");
    // gm: a lock-class modal is open in globalUI.windows (over every mode window)
    a.op(Opcode::Bool {
        dst: gm,
        value: ValBool(false),
    });
    a.op(Opcode::Field {
        dst: gui,
        obj: Reg(0),
        field: p.g_gui.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "pre_done",
    );
    a.op(Opcode::Field {
        dst: lst,
        obj: gui,
        field: p.u_windows.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "pre_done",
    );
    top_down(&mut a, p, lst, i, zero, raw, d, w, "pre", "pre_done");
    modal_test(&mut a, p, w, md, mi, none, "pre");
    location_modal(&mut a, "pre", "pre", "pre_block", "pre");
    a.label("pre_block");
    a.op(Opcode::Bool {
        dst: gm,
        value: ValBool(true),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "pre");
    a.label("pre_done");
    for (k, (next, head, miss, mok, mblock)) in [
        ("gui", "loop1", "miss1", "mok1", "mblock1"),
        ("none", "loop2", "miss2", "mok2", "mblock2"),
    ]
    .into_iter()
    .enumerate()
    {
        let nc = leak(format!("nc{k}"));
        if k == 0 {
            a.op(Opcode::Field {
                dst: mode,
                obj: Reg(0),
                field: p.g_mode.0,
            });
            a.jmp(
                Opcode::JNull {
                    reg: mode,
                    offset: 0,
                },
                next,
            );
            a.op(Opcode::Field {
                dst: lst,
                obj: mode,
                field: p.m_windows.0,
            });
            a.op(Opcode::Mov {
                dst: above,
                src: gm,
            });
        } else {
            a.label("gui");
            a.op(Opcode::Field {
                dst: gui,
                obj: Reg(0),
                field: p.g_gui.0,
            });
            a.jmp(
                Opcode::JNull {
                    reg: gui,
                    offset: 0,
                },
                next,
            );
            a.op(Opcode::Field {
                dst: lst,
                obj: gui,
                field: p.u_windows.0,
            });
            a.op(Opcode::Bool {
                dst: above,
                value: ValBool(false),
            });
        }
        a.op(Opcode::Null { dst: cand });
        a.jmp(
            Opcode::JNull {
                reg: lst,
                offset: 0,
            },
            next,
        );
        top_down(&mut a, p, lst, i, zero, raw, d, w, head, next);
        for (j, (l, &(cr, cw))) in p.locks.iter().zip(&cls).enumerate() {
            a.op(Opcode::GetGlobal {
                dst: cr,
                global: l.g,
            });
            // A Window goes to BaseType.check's Dyn argument as is.
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.base_check,
                arg0: cr,
                arg1: w,
            });
            a.jmp(Opcode::JFalse { cond: b, offset: 0 }, SKIP[k][j]);
            if l.guard == 3 {
                // the player's own sheet (or any while not NPC-locked): not a
                // location window
                sheet(&mut a, l, cw.expect("guard 3 reg"), SKIP[k][j], SKIP[k][j]);
            }
            if let (Some(f), Some(cw)) = (l.act, cw) {
                a.op(Opcode::UnsafeCast { dst: cw, src: w });
                a.op(Opcode::Field {
                    dst: act,
                    obj: cw,
                    field: f,
                });
                a.jmp(
                    Opcode::JNull {
                        reg: act,
                        offset: 0,
                    },
                    miss,
                );
                a.op(Opcode::Field {
                    dst: b,
                    obj: act,
                    field: p.act_started,
                });
                a.jmp(Opcode::JTrue { cond: b, offset: 0 }, miss);
            }
            if l.guard == 1 || l.guard == 2 {
                // its own confirm right over it: close that first; a lock-class
                // modal above (a started sub-activity) keeps it
                let go = leak(format!("go{k}_{j}"));
                a.jmp(
                    Opcode::JNull {
                        reg: cand,
                        offset: 0,
                    },
                    go,
                );
                a.op(Opcode::Ret { ret: cand });
                a.label(go);
                a.jmp(
                    Opcode::JTrue {
                        cond: above,
                        offset: 0,
                    },
                    miss,
                );
            }
            a.op(Opcode::CallMethod {
                dst: b,
                field: p.w_try_close,
                args: vec![w],
            });
            a.jmp(Opcode::JFalse { cond: b, offset: 0 }, miss);
            a.op(Opcode::Ret { ret: w });
            a.label(SKIP[k][j]);
        }
        // not taken: a location modal becomes the candidate for the window
        // right under it; a lock-class modal covers the ones below it
        a.label(miss);
        modal_test(&mut a, p, w, md, mi, none, nc);
        location_modal(&mut a, miss, mok, mblock, nc);
        a.label(mok);
        a.op(Opcode::Mov { dst: cand, src: w });
        a.jmp(Opcode::JAlways { offset: 0 }, head);
        a.label(mblock);
        a.op(Opcode::Bool {
            dst: above,
            value: ValBool(true),
        });
        a.label(nc);
        a.op(Opcode::Null { dst: cand });
        a.jmp(Opcode::JAlways { offset: 0 }, head);
    }
    a.label("none");
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::Ret { ret: w });
    push_fn(code, vec![p.game_t], p.win_t, r.0, a.finish(), p.dbg_file)
}

/// `flDialog(view) -> I32` (host, pending leave): 0 when no ui.win.Dialog is in
/// game.mode.windows; otherwise ends it at an idle choice point (see F4) and
/// returns what the leave waits on (8 dialog open, 9, 10, 11, 12).
fn add_dialog(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let ints = reason_ints(code);
    let inone = int_const(code, p.modal_none);
    let ends = str_global(code, p.t.str_, ENDS);
    let forced = str_global(code, p.t.str_, FORCED);
    let t = &p.t;
    let dl_t = &p.dlg;
    let mut r = Regs(vec![p.view_t]);
    let (game, mode, lst, i, zero, none, raw, d, w) = (
        r.r(p.game_t),
        r.r(p.g_mode.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(p.win_t),
    );
    let (b, above, md, mi, cr, dl, lb, cb, ctrl, n, c, s_r, v) = (
        r.r(t.bool_),
        r.r(t.bool_),
        r.r(p.w_modal.1),
        r.r(t.i32_),
        r.r(dl_t.cls.1),
        r.r(dl_t.t),
        r.r(dl_t.leave_btn.1),
        r.r(dl_t.leave_cb),
        r.r(p.g_ctrl.1),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.str_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    let ret = |a: &mut Asm, k: usize| {
        a.op(Opcode::Int {
            dst: c,
            ptr: ints[k],
        });
        a.op(Opcode::Ret { ret: c });
    };
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.op(Opcode::Int {
        dst: none,
        ptr: inone,
    });
    a.op(Opcode::GetThis {
        dst: game,
        field: p.v_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: lst,
        obj: mode,
        field: p.m_windows.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Bool {
        dst: above,
        value: ValBool(false),
    });
    top_down(&mut a, p, lst, i, zero, raw, d, w, "scan", "no");
    a.op(Opcode::GetGlobal {
        dst: cr,
        global: dl_t.cls.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cr,
        arg1: w,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "found");
    modal_test(&mut a, p, w, md, mi, none, "scan");
    a.op(Opcode::Bool {
        dst: above,
        value: ValBool(true),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "scan");
    a.label("found");
    a.op(Opcode::UnsafeCast { dst: dl, src: w });
    // its leave runs: wait for it (the window goes once the leave event ends)
    a.op(Opcode::Field {
        dst: b,
        obj: dl,
        field: dl_t.already,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "sync");
    ret(&mut a, R_DIALOG);
    // A modal window over it (the host's own confirm / viewer) no longer keeps
    // the wait: the mode switch disposes the host's windows with the dialog.
    // flWhy reports "dialog open" before these: never during a mode switch / pause
    a.label("sync");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: game,
        field: p.g_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "paused",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: ctrl,
        field: p.c_lock_sync,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "syncing");
    a.op(Opcode::Field {
        dst: lst,
        obj: ctrl,
        field: p.c_wait_locks.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "paused",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: lst,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "paused",
    );
    a.label("syncing");
    ret(&mut a, R_SYNC);
    a.label("paused");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.paused,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "fade");
    ret(&mut a, R_PAUSED);
    a.label("fade");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "choice");
    ret(&mut a, R_FADE);
    // Hidden (`visible` = !hidden && !localHidden && text): a choice, its
    // scriptEvent / gains or a special action runs (dialog__impl hides the
    // window before scriptEvent): wait, never end it mid-action.
    // Idle with the Leave choice on screen: what Escape / the Leave button does
    // (allowLeave -> leave(null)). Idle without one (no Leave choice, or
    // allowLeave refused) the host leave ends it anyway with Dialog.leave(null):
    // a conversation never holds the party.
    a.label("choice");
    a.op(Opcode::Field {
        dst: b,
        obj: dl,
        field: dl_t.visible,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "busy");
    a.op(Opcode::Field {
        dst: lb,
        obj: dl,
        field: dl_t.leave_btn.0,
    });
    a.jmp(Opcode::JNull { reg: lb, offset: 0 }, "force");
    a.op(Opcode::Call1 {
        dst: b,
        fun: dl_t.try_close,
        arg0: dl,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "ended");
    a.label("force");
    a.op(Opcode::Null { dst: cb });
    a.op(Opcode::Call2 {
        dst: v,
        fun: dl_t.leave,
        arg0: dl,
        arg1: cb,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: forced,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    // drop the choice buttons: a late click finds nothing to run leave() again
    a.label("ended");
    a.op(Opcode::Call1 {
        dst: v,
        fun: dl_t.reset,
        arg0: dl,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: ends,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    ret(&mut a, R_DIALOG);
    a.label("busy");
    ret(&mut a, R_DIALOG);
    a.label("no");
    ret(&mut a, 0);
    push_fn(code, vec![p.view_t], t.i32_, r.0, a.finish(), p.dbg_file)
}

/// `flMinigame(game) -> Bool`: this machine runs a minigame: an ActivityWindow
/// (ForgeAction, GatherAction, AnalyzeAction, SingAction, FishingAction,
/// UnitAction) or a Craft / Alter whose activity started, in game.mode.windows
/// or game.globalUI.windows. The only lock a pending leave waits for.
fn add_minigame(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t]);
    let (mode, gui, lst, i, zero, raw, d, w, b, acr, act) = (
        r.r(p.g_mode.1),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(p.win_t),
        r.r(t.bool_),
        r.r(p.act_win_cls.1),
        r.r(p.act_t),
    );
    let acts: Vec<(&Lock, Reg, Reg, RefField)> = p
        .locks
        .iter()
        .filter_map(|l| l.act.map(|f| (l, f)))
        .map(|(l, f)| (l, r.r(l.gt), r.r(l.ct), f))
        .collect();
    let mut a = Asm::new();
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    for (k, (head, out)) in [("m_loop", "m_gui"), ("g_loop", "no")]
        .into_iter()
        .enumerate()
    {
        if k == 0 {
            a.op(Opcode::Field {
                dst: mode,
                obj: Reg(0),
                field: p.g_mode.0,
            });
            a.jmp(
                Opcode::JNull {
                    reg: mode,
                    offset: 0,
                },
                out,
            );
            a.op(Opcode::Field {
                dst: lst,
                obj: mode,
                field: p.m_windows.0,
            });
        } else {
            a.label("m_gui");
            a.op(Opcode::Field {
                dst: gui,
                obj: Reg(0),
                field: p.g_gui.0,
            });
            a.jmp(
                Opcode::JNull {
                    reg: gui,
                    offset: 0,
                },
                out,
            );
            a.op(Opcode::Field {
                dst: lst,
                obj: gui,
                field: p.u_windows.0,
            });
        }
        a.jmp(
            Opcode::JNull {
                reg: lst,
                offset: 0,
            },
            out,
        );
        top_down(&mut a, p, lst, i, zero, raw, d, w, head, out);
        a.op(Opcode::GetGlobal {
            dst: acr,
            global: p.act_win_cls.0,
        });
        a.op(Opcode::Call2 {
            dst: b,
            fun: p.base_check,
            arg0: acr,
            arg1: w,
        });
        a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "yes");
        for &(l, cr, cw, f) in &acts {
            a.op(Opcode::GetGlobal {
                dst: cr,
                global: l.g,
            });
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.base_check,
                arg0: cr,
                arg1: w,
            });
            let next: &'static str = Box::leak(format!("{head}_{}", cr.0).into_boxed_str());
            a.jmp(Opcode::JFalse { cond: b, offset: 0 }, next);
            a.op(Opcode::UnsafeCast { dst: cw, src: w });
            a.op(Opcode::Field {
                dst: act,
                obj: cw,
                field: f,
            });
            a.jmp(
                Opcode::JNull {
                    reg: act,
                    offset: 0,
                },
                next,
            );
            a.op(Opcode::Field {
                dst: b,
                obj: act,
                field: p.act_started,
            });
            a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "yes");
            a.label(next);
        }
        a.jmp(Opcode::JAlways { offset: 0 }, head);
    }
    a.label("no");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("yes");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![p.game_t], t.bool_, r.0, a.finish(), p.dbg_file)
}

/// `flRelease(game)`, every machine while a leave is pending (place, owned
/// tavern, camp), after its window closing: this machine's own lock that no
/// minigame holds and that outlived RELEASE_AFTER s (no closable window took it
/// away: an NPC / dialog-action lock, a window under a personal modal, a lock
/// whose window never opened) is cleared with `me.set_lockedWith(null)` (the
/// networked setter its own lock sites use); the mode switch disposes what is
/// left open. A minigame lock is kept: only a running minigame holds a leave.
fn add_release(code: &mut Bytecode, p: &Plan, wg: &WGlobals, minigame: RefFun) -> Result<RefFun> {
    let f0 = float_const(code, 0.0);
    let f_after = float_const(code, RELEASE_AFTER);
    let releases = str_global(code, p.t.str_, RELEASES);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t]);
    let (me, lw, hl, nul, b, now, pa, lim, s_r, x, v) = (
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.bp_locked.1),
        r.r(p.bp_locked.1),
        r.r(t.bool_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: me,
        obj: Reg(0),
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "reset");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: b,
        fun: minigame,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "reset");
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: hl,
        global: wg.rel_lw,
    });
    a.jmp(
        Opcode::JEq {
            a: lw,
            b: hl,
            offset: 0,
        },
        "timed",
    );
    a.op(Opcode::SetGlobal {
        global: wg.rel_lw,
        src: lw,
    });
    a.op(Opcode::SetGlobal {
        global: wg.rel_at,
        src: now,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("timed");
    a.op(Opcode::GetGlobal {
        dst: pa,
        global: wg.rel_at,
    });
    a.op(Opcode::Sub {
        dst: pa,
        a: now,
        b: pa,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: f_after,
    });
    a.jmp(
        Opcode::JSLt {
            a: pa,
            b: lim,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: releases,
    });
    // An object goes to Std.string's Dyn argument as is.
    a.op(Opcode::Call1 {
        dst: x,
        fun: p.std_string,
        arg0: lw,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: x,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::Null { dst: nul });
    a.op(Opcode::Call2 {
        dst: nul,
        fun: p.set_locked,
        arg0: me,
        arg1: nul,
    });
    a.label("reset");
    a.op(Opcode::Null { dst: hl });
    a.op(Opcode::SetGlobal {
        global: wg.rel_lw,
        src: hl,
    });
    a.op(Opcode::Float { dst: pa, ptr: f0 });
    a.op(Opcode::SetGlobal {
        global: wg.rel_at,
        src: pa,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], t.void, r.0, a.finish(), p.dbg_file)
}

/// Close this machine's own lock-holding window (not during a fade), as F2:
/// `if (!inFade && me.lockedWith != null) { w = closable(game);
///  if (w != null && w != g_closed) { g_closed = w; println(CLOSES + name); w.close(); } }`
/// then falls through to `out`.
#[allow(clippy::too_many_arguments)]
fn close_own(
    a: &mut Asm,
    p: &Plan,
    closable: RefFun,
    closes: RefGlobal,
    g_closed: RefGlobal,
    game: Reg,
    regs: [Reg; 7],
    out: &'static str,
) {
    let [b, me, lw, w, last_w, name, s_r] = regs;
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: game,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, out);
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, out);
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, out);
    a.op(Opcode::Call1 {
        dst: w,
        fun: closable,
        arg0: game,
    });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, out);
    a.op(Opcode::GetGlobal {
        dst: last_w,
        global: g_closed,
    });
    a.jmp(
        Opcode::JEq {
            a: w,
            b: last_w,
            offset: 0,
        },
        out,
    );
    a.op(Opcode::SetGlobal {
        global: g_closed,
        src: w,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: closes,
    });
    a.op(Opcode::Field {
        dst: name,
        obj: w,
        field: p.w_name.0,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: name,
    });
}

/// Rest of [`close_own`]: println + close (separate so the void register is the caller's).
fn close_own_tail(a: &mut Asm, p: &Plan, w: Reg, s_r: Reg, v: Reg) {
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.w_close,
        args: vec![w],
    });
}

struct Fns {
    why: RefFun,
    log: RefFun,
    closable: RefFun,
    dialog: RefFun,
    text: RefFun,
    notify: RefFun,
    release: RefFun,
}

/// Wait-timer globals of the place leave (every machine).
struct WGlobals {
    /// sys_time the pending leave was first seen on this machine (0 = none).
    pend_at: RefGlobal,
    /// Host: sys_time of the last on-screen "Leave waits" line (0 = none yet).
    warned_at: RefGlobal,
    /// The `me.lockedWith` this machine last reported as still holding the leave.
    here_lw: RefGlobal,
    /// flRelease: the lock it times and since when (sys_time).
    rel_lw: RefGlobal,
    rel_at: RefGlobal,
}

/// `flUpdate(view)`, at the top of PlaceView.update; see the module comment.
fn add_update(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    wg: &WGlobals,
    f: &Fns,
    dyn_t: RefType,
) -> Result<RefFun> {
    let ints = reason_ints(code);
    let i1 = int_const(code, 1);
    let f0 = float_const(code, 0.0);
    let f_after = float_const(code, WARN_AFTER);
    let f_every = float_const(code, WARN_EVERY);
    let pending = str_global(code, p.t.str_, PENDING);
    let closes = str_global(code, p.t.str_, CLOSES);
    let waits = str_global(code, p.t.str_, WAITS);
    let log_p = str_global(code, p.t.str_, LOG_P);
    let here = str_global(code, p.t.str_, HERE);
    let here_top = str_global(code, p.t.str_, HERE_TOP);
    let here_note = str_global(code, p.t.str_, HERE_NOTE);
    let dash = str_global(code, p.t.str_, "-");
    let t = &p.t;
    let mut r = Regs(vec![p.view_t]);
    let (game, b, place, ls, me, lw, w, last_w) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.v_place.1),
        r.r(p.p_leave_state.1),
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.win_t),
        r.r(p.win_t),
    );
    let arr = r.r(p.arr_t);
    let (name, s_r, c, c1, last, one, zero, v) = (
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.void),
    );
    let (c2, k, lcb) = (r.r(t.i32_), r.r(t.i32_), r.r(p.v_leave_cb));
    let (now, pa, el, lim, fz) = (
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
    );
    let (me2, lw2, hl, mode, lst, n, raw, d, tw, tn, x) = (
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.bp_locked.1),
        r.r(p.g_mode.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(p.win_t),
        r.r(t.str_),
        r.r(t.str_),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: p.v_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::GetThis {
        dst: b,
        field: p.v_leaving,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.op(Opcode::GetThis {
        dst: place,
        field: p.v_place.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: place,
            offset: 0,
        },
        "idle",
    );
    a.op(Opcode::Field {
        dst: ls,
        obj: place,
        field: p.p_leave_state.0,
    });
    a.jmp(Opcode::JNull { reg: ls, offset: 0 }, "idle");
    a.op(Opcode::Field {
        dst: b,
        obj: ls,
        field: p.ls_forced,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "idle");
    // ... and somebody still asks: `forced` survives a withdrawn request (the
    // gamepad toggle removes its player and leaves `forced` set).
    a.op(Opcode::Field {
        dst: arr,
        obj: ls,
        field: p.ls_players.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "idle",
    );
    a.op(Opcode::Field {
        dst: c,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JSLte {
            a: c,
            b: zero,
            offset: 0,
        },
        "idle",
    );
    // A leave is pending: release this machine's own lock, never during a fade.
    close_own(
        &mut a,
        p,
        f.closable,
        closes,
        g.closed,
        game,
        [b, me, lw, w, last_w, name, s_r],
        "timer",
    );
    close_own_tail(&mut a, p, w, s_r, v);
    // A lock no minigame holds does not outlive the window closing for long.
    a.label("timer");
    a.op(Opcode::Call1 {
        dst: v,
        fun: f.release,
        arg0: game,
    });
    // How long this machine has seen the leave pending (el = now - pend_at).
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::Float { dst: fz, ptr: f0 });
    a.op(Opcode::GetGlobal {
        dst: pa,
        global: wg.pend_at,
    });
    a.jmp(
        Opcode::JSLt {
            a: fz,
            b: pa,
            offset: 0,
        },
        "started",
    );
    a.op(Opcode::Mov { dst: pa, src: now });
    a.op(Opcode::SetGlobal {
        global: wg.pend_at,
        src: pa,
    });
    a.label("started");
    a.op(Opcode::Sub {
        dst: el,
        a: now,
        b: pa,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: f_after,
    });
    a.jmp(
        Opcode::JSLt {
            a: el,
            b: lim,
            offset: 0,
        },
        "host",
    );
    // Still locked here after WARN_AFTER s (a started activity, a guarded window):
    // tell this player once per lock, log the lock and the top mode window.
    a.op(Opcode::Field {
        dst: me2,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: me2,
            offset: 0,
        },
        "host",
    );
    a.op(Opcode::Field {
        dst: lw2,
        obj: me2,
        field: p.bp_locked.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lw2,
            offset: 0,
        },
        "host",
    );
    a.op(Opcode::GetGlobal {
        dst: hl,
        global: wg.here_lw,
    });
    a.jmp(
        Opcode::JEq {
            a: lw2,
            b: hl,
            offset: 0,
        },
        "host",
    );
    a.op(Opcode::SetGlobal {
        global: wg.here_lw,
        src: lw2,
    });
    a.op(Opcode::GetGlobal {
        dst: tn,
        global: dash,
    });
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "here",
    );
    a.op(Opcode::Field {
        dst: lst,
        obj: mode,
        field: p.m_windows.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "here",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: lst,
        field: p.a_len,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "here",
    );
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Sub {
        dst: n,
        a: n,
        b: one,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: lst,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: n,
    });
    a.op(Opcode::UnsafeCast { dst: tw, src: d });
    a.jmp(Opcode::JNull { reg: tw, offset: 0 }, "here");
    a.op(Opcode::Field {
        dst: x,
        obj: tw,
        field: p.w_name.0,
    });
    a.jmp(Opcode::JNull { reg: x, offset: 0 }, "here");
    a.op(Opcode::Mov { dst: tn, src: x });
    a.label("here");
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: here,
    });
    // An object goes to Std.string's Dyn argument as is.
    a.op(Opcode::Call1 {
        dst: x,
        fun: p.std_string,
        arg0: lw2,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: x,
    });
    a.op(Opcode::GetGlobal {
        dst: x,
        global: here_top,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: x,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: tn,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: here_note,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: f.notify,
        arg0: game,
        arg1: s_r,
    });
    // Host: leave once nothing refuses; print what it waits on when that changes.
    a.label("host");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: c,
        fun: f.why,
        arg0: Reg(0),
        arg1: b,
    });
    // nothing else refuses, or only "dialog open": the shared dialog decides
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "dlg",
    );
    a.op(Opcode::Int {
        dst: k,
        ptr: ints[R_DIALOG],
    });
    a.jmp(
        Opcode::JNotEq {
            a: c,
            b: k,
            offset: 0,
        },
        "log",
    );
    a.label("dlg");
    a.op(Opcode::Call1 {
        dst: c2,
        fun: f.dialog,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JEq {
            a: c2,
            b: zero,
            offset: 0,
        },
        "log",
    );
    a.op(Opcode::Mov { dst: c, src: c2 });
    a.label("log");
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Add {
        dst: c1,
        a: c,
        b: one,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.last,
    });
    a.jmp(
        Opcode::JEq {
            a: c1,
            b: last,
            offset: 0,
        },
        "go",
    );
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: c1,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: pending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: c,
    });
    a.label("go");
    a.jmp(
        Opcode::JNotEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "warn",
    );
    // flWhy(view, true) checked tryClose's refusals but the host's own gamepad
    // global windows: leave() directly (tryClose's success path).
    a.op(Opcode::Null { dst: lcb });
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.v_leave,
        args: vec![Reg(0), lcb],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    // Still waiting after WARN_AFTER s: show the host what on screen (and in the
    // log), again every WARN_EVERY s. Nothing is forced: what still refuses is a
    // started activity, a scripted dialog or a window the guards keep.
    a.label("warn");
    a.jmp(
        Opcode::JSLt {
            a: el,
            b: lim,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: pa,
        global: wg.warned_at,
    });
    a.op(Opcode::Sub {
        dst: pa,
        a: now,
        b: pa,
    });
    a.op(Opcode::Float {
        dst: fz,
        ptr: f_every,
    });
    a.jmp(
        Opcode::JSLt {
            a: pa,
            b: fz,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: wg.warned_at,
        src: now,
    });
    a.op(Opcode::GetGlobal {
        dst: x,
        global: waits,
    });
    a.op(Opcode::Call3 {
        dst: tn,
        fun: f.text,
        arg0: game,
        arg1: x,
        arg2: c,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: log_p,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: tn,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: f.notify,
        arg0: game,
        arg1: tn,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    // Nothing pending: forget the last reason and window, restart the wait timer.
    a.label("idle");
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: zero,
    });
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::SetGlobal {
        global: g.closed,
        src: w,
    });
    a.op(Opcode::Float { dst: fz, ptr: f0 });
    a.op(Opcode::SetGlobal {
        global: wg.pend_at,
        src: fz,
    });
    a.op(Opcode::SetGlobal {
        global: wg.warned_at,
        src: fz,
    });
    a.op(Opcode::Null { dst: hl });
    a.op(Opcode::SetGlobal {
        global: wg.here_lw,
        src: hl,
    });
    a.op(Opcode::SetGlobal {
        global: wg.rel_lw,
        src: hl,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.view_t], t.void, r.0, a.finish(), p.dbg_file)
}

/// `flPaused()`, in PlaceView.leave's `if (paused) return`: prints at most every PAUSED_EVERY s.
fn add_paused(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let every = float_const(code, PAUSED_EVERY);
    let paused = str_global(code, p.t.str_, PAUSED);
    let t = &p.t;
    let mut r = Regs(vec![]);
    let (now, last, diff, lim, s_r, v) = (
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.str_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.paused_at,
    });
    a.op(Opcode::Sub {
        dst: diff,
        a: now,
        b: last,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: every,
    });
    a.jmp(
        Opcode::JSLt {
            a: diff,
            b: lim,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: g.paused_at,
        src: now,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: paused,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![], t.void, r.0, a.finish(), p.dbg_file)
}

// ---------- owned tavern ----------

/// `tlMark(ls, v)`: `if (ls.forced != v) { if (ls.obj != null) ls.obj.<mark>(ls.bit);
/// ls.forced = v; }`, the replicated write Place.setLeaveState__impl does.
fn add_mark(code: &mut Bytecode, p: &Plan, tv: &Tav) -> Result<RefFun> {
    let t = &p.t;
    let px_t = p.p_leave_state.1;
    let mut r = Regs(vec![px_t, t.bool_]);
    let (b, o, bit, v) = (r.r(t.bool_), r.r(tv.px_obj.1), r.r(t.i32_), r.r(t.void));
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: b,
        obj: Reg(0),
        field: p.ls_forced,
    });
    a.jmp(
        Opcode::JEq {
            a: b,
            b: Reg(1),
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: o,
        obj: Reg(0),
        field: tv.px_obj.0,
    });
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "set");
    a.op(Opcode::Field {
        dst: bit,
        obj: Reg(0),
        field: tv.px_bit,
    });
    a.op(Opcode::CallMethod {
        dst: v,
        field: tv.px_mark,
        args: vec![o, bit],
    });
    a.label("set");
    a.op(Opcode::SetField {
        obj: Reg(0),
        field: p.ls_forced,
        src: Reg(1),
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![px_t, t.bool_],
        t.void,
        r.0,
        a.finish(),
        tv.dbg_file,
    )
}

/// `tlWhy(game) -> I32`: what Tavern.askLeave__impl refuses on (a player
/// lockedWith, fade, alive lock), then a running mode-switch barrier
/// (syncLeaveMode would queue or drop the request).
fn add_tl_why(code: &mut Bytecode, p: &Plan, tv: &Tav) -> Result<RefFun> {
    let ints = reason_ints(code);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t]);
    let (b, rf, c, ctrl, wl, n, zero) = (
        r.r(t.bool_),
        r.r(p.any_locked.1),
        r.r(t.i32_),
        r.r(p.g_ctrl.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
    );
    let mut a = Asm::new();
    let ret = |a: &mut Asm, k: usize| {
        a.op(Opcode::Int {
            dst: c,
            ptr: ints[k],
        });
        a.op(Opcode::Ret { ret: c });
    };
    // askLeave__impl's own test: anyPlayerLocked(true)
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ref { dst: rf, src: b });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.any_locked.0,
        arg0: Reg(0),
        arg1: rf,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "fade");
    ret(&mut a, R_LOCKED);
    // ctrl.isLocked() = lockAlives || inFade
    a.label("fade");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "alive");
    ret(&mut a, R_FADE);
    a.label("alive");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: Reg(0),
        field: p.g_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "ok",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: tv.is_locked,
        arg0: ctrl,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "sync");
    ret(&mut a, R_ALIVE);
    a.label("sync");
    a.op(Opcode::Field {
        dst: b,
        obj: ctrl,
        field: p.c_lock_sync,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "busy");
    a.op(Opcode::Field {
        dst: wl,
        obj: ctrl,
        field: p.c_wait_locks.0,
    });
    a.jmp(Opcode::JNull { reg: wl, offset: 0 }, "ok");
    a.op(Opcode::Field {
        dst: n,
        obj: wl,
        field: p.a_len,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "ok",
    );
    a.label("busy");
    ret(&mut a, R_SYNC);
    a.label("ok");
    ret(&mut a, 0);
    push_fn(code, vec![p.game_t], t.i32_, r.0, a.finish(), tv.dbg_file)
}

struct TFns {
    log: RefFun,
    closable: RefFun,
    mark: RefFun,
    why: RefFun,
    release: RefFun,
}

/// `tlAsk(tavern) -> Bool`, in front of Tavern.askLeave__impl: true = the
/// request is pending (askLeave returns), false = the vanilla body runs.
fn add_tl_ask(code: &mut Bytecode, p: &Plan, tv: &Tav, g: &Globals, f: &TFns) -> Result<RefFun> {
    let ints = reason_ints(code);
    let i1 = int_const(code, 1);
    let tpending = str_global(code, p.t.str_, TPENDING);
    let t = &p.t;
    let mut r = Regs(vec![tv.tav_t]);
    let (game, b, ls, c, c1, last, zero, one, md, s_r, v) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.p_leave_state.1),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.g_mode.1),
        r.r(t.str_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.op(Opcode::GetThis {
        dst: game,
        field: tv.tav_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::GetThis {
        dst: ls,
        field: tv.tav_ls,
    });
    a.jmp(Opcode::JNull { reg: ls, offset: 0 }, "vanilla");
    a.op(Opcode::Call1 {
        dst: c,
        fun: f.why,
        arg0: game,
    });
    a.jmp(
        Opcode::JNotEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "pend",
    );
    // Nothing refuses: no request pending any more; the vanilla body leaves now.
    a.op(Opcode::Null { dst: md });
    a.op(Opcode::SetGlobal {
        global: g.tl_mode,
        src: md,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: f.mark,
        arg0: ls,
        arg1: b,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.tl_last,
    });
    a.jmp(
        Opcode::JEq {
            a: last,
            b: zero,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::SetGlobal {
        global: g.tl_last,
        src: zero,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: tpending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: c,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "vanilla");
    // Pending: remember the mode, tell everyone (replicated flag), log changes.
    a.label("pend");
    a.op(Opcode::Field {
        dst: md,
        obj: game,
        field: p.g_mode.0,
    });
    a.op(Opcode::SetGlobal {
        global: g.tl_mode,
        src: md,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: f.mark,
        arg0: ls,
        arg1: b,
    });
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Add {
        dst: c1,
        a: c,
        b: one,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.tl_last,
    });
    a.jmp(
        Opcode::JEq {
            a: c1,
            b: last,
            offset: 0,
        },
        "handled",
    );
    a.op(Opcode::SetGlobal {
        global: g.tl_last,
        src: c1,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: tpending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: c,
    });
    a.label("handled");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("vanilla");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![tv.tav_t], t.bool_, r.0, a.finish(), tv.dbg_file)
}

/// `tlUpdate(mode)`, at the top of TavernMode.update (every machine).
fn add_tl_update(code: &mut Bytecode, p: &Plan, tv: &Tav, g: &Globals, f: &TFns) -> Result<RefFun> {
    let ints = reason_ints(code);
    let i1 = int_const(code, 1);
    let tpending = str_global(code, p.t.str_, TPENDING);
    let closes = str_global(code, p.t.str_, CLOSES);
    let t = &p.t;
    let mut r = Regs(vec![tv.tm_t]);
    let (game, b, auth, tav, ls, fl, md) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(t.bool_),
        r.r(tv.tav_t),
        r.r(p.p_leave_state.1),
        r.r(t.bool_),
        r.r(p.g_mode.1),
    );
    let (me, lw, w, last_w, name, s_r) = (
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.win_t),
        r.r(p.win_t),
        r.r(t.str_),
        r.r(t.str_),
    );
    let (c, c1, last, one, zero, v) = (
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.op(Opcode::GetThis {
        dst: game,
        field: tv.tm_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: tav,
        fun: tv.get_tavern,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JNull {
            reg: tav,
            offset: 0,
        },
        "idle",
    );
    a.op(Opcode::Field {
        dst: ls,
        obj: tav,
        field: tv.tav_ls,
    });
    a.jmp(Opcode::JNull { reg: ls, offset: 0 }, "idle");
    a.op(Opcode::Field {
        dst: fl,
        obj: ls,
        field: p.ls_forced,
    });
    a.op(Opcode::Field {
        dst: auth,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(
        Opcode::JFalse {
            cond: auth,
            offset: 0,
        },
        "client",
    );
    // Host: pending only for this mode's own request; any other `forced`
    // (a save, an earlier visit) is stale and cleared.
    a.op(Opcode::GetGlobal {
        dst: md,
        global: g.tl_mode,
    });
    a.jmp(
        Opcode::JEq {
            a: md,
            b: Reg(0),
            offset: 0,
        },
        "mine",
    );
    a.jmp(
        Opcode::JFalse {
            cond: fl,
            offset: 0,
        },
        "idle",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: f.mark,
        arg0: ls,
        arg1: b,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "idle");
    a.label("mine");
    a.label("client");
    a.jmp(
        Opcode::JFalse {
            cond: fl,
            offset: 0,
        },
        "idle",
    );
    // Pending: release this machine's own lock, never during a fade.
    close_own(
        &mut a,
        p,
        f.closable,
        closes,
        g.tl_closed,
        game,
        [b, me, lw, w, last_w, name, s_r],
        "host",
    );
    close_own_tail(&mut a, p, w, s_r, v);
    // Host: leave through askLeave__impl once nothing refuses.
    a.label("host");
    a.op(Opcode::Call1 {
        dst: v,
        fun: f.release,
        arg0: game,
    });
    a.jmp(
        Opcode::JFalse {
            cond: auth,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: c,
        fun: f.why,
        arg0: game,
    });
    a.jmp(
        Opcode::JNotEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "wait",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: tv.ask,
        arg0: tav,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("wait");
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Add {
        dst: c1,
        a: c,
        b: one,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.tl_last,
    });
    a.jmp(
        Opcode::JEq {
            a: c1,
            b: last,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: g.tl_last,
        src: c1,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: tpending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: c,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    // Nothing pending: forget the last reason and window.
    a.label("idle");
    a.op(Opcode::SetGlobal {
        global: g.tl_last,
        src: zero,
    });
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::SetGlobal {
        global: g.tl_closed,
        src: w,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![tv.tm_t], t.void, r.0, a.finish(), tv.dbg_file)
}

/// `tlClose(ctrl) -> Bool`, in front of Controller.closeTavern__impl (Escape in
/// the tavern): in multi, in a TavernMode, run tavern.askLeave__impl() (the
/// pending policy) instead and return true.
fn add_tl_close(code: &mut Bytecode, p: &Plan, tv: &Tav) -> Result<RefFun> {
    let t = &p.t;
    let ctrl_t = p.g_ctrl.1;
    let mut r = Regs(vec![ctrl_t]);
    let (game, b, md, cr, tm, tav, v) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.g_mode.1),
        r.r(tv.tm_cls.1),
        r.r(tv.tm_t),
        r.r(tv.tav_t),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: tv.ctrl_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: md,
        obj: game,
        field: p.g_mode.0,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "vanilla");
    a.op(Opcode::GetGlobal {
        dst: cr,
        global: tv.tm_cls.0,
    });
    // A GameMode goes to BaseType.check's Dyn argument as is.
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cr,
        arg1: md,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::UnsafeCast { dst: tm, src: md });
    a.op(Opcode::Call1 {
        dst: tav,
        fun: tv.get_tavern,
        arg0: tm,
    });
    a.jmp(
        Opcode::JNull {
            reg: tav,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: tv.ask,
        arg0: tav,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("vanilla");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![ctrl_t], t.bool_, r.0, a.finish(), tv.dbg_file)
}

// ---------- camp ----------

/// Host globals of the camp part.
struct CGlobals {
    /// The CampMode whose leave is pending (null = none).
    mode: RefGlobal,
    /// Last logged camp pending reason + 1 (0 = none yet).
    last: RefGlobal,
    /// sys_time of the last round of close requests.
    ask_at: RefGlobal,
}

/// `cpWhy(game, camp) -> I32`: 0 when the vanilla body should run (nothing
/// refuses, or a rest runs, which the vanilla body refuses on its own);
/// otherwise what the camp leave waits on.
fn add_cp_why(code: &mut Bytecode, p: &Plan, c: &Camp) -> Result<RefFun> {
    let ints = reason_ints(code);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t, c.cm_t]);
    let (rest, b, rf, k, ctrl, wl, n, zero, st) = (
        r.r(c.cm_rest.1),
        r.r(t.bool_),
        r.r(p.any_locked.1),
        r.r(t.i32_),
        r.r(p.g_ctrl.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.g_state.1),
    );
    let mut a = Asm::new();
    let ret = |a: &mut Asm, kk: usize| {
        a.op(Opcode::Int {
            dst: k,
            ptr: ints[kk],
        });
        a.op(Opcode::Ret { ret: k });
    };
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    // a rest runs: the vanilla body refuses (never queue a leave behind a rest)
    a.op(Opcode::Field {
        dst: rest,
        obj: Reg(1),
        field: c.cm_rest.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: rest,
            offset: 0,
        },
        "ok",
    );
    // the vanilla lock test: anyPlayerLocked(true)
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ref { dst: rf, src: b });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.any_locked.0,
        arg0: Reg(0),
        arg1: rf,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "fade");
    ret(&mut a, R_LOCKED);
    a.label("fade");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "alive");
    ret(&mut a, R_FADE);
    // toggleCamp__impl's own test: ctrl.isLocked() || waitLocks.length > 0
    a.label("alive");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: Reg(0),
        field: p.g_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "win",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: c.is_locked,
        arg0: ctrl,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "sync");
    ret(&mut a, R_ALIVE);
    a.label("sync");
    a.op(Opcode::Field {
        dst: b,
        obj: ctrl,
        field: p.c_lock_sync,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "busy");
    a.op(Opcode::Field {
        dst: wl,
        obj: ctrl,
        field: p.c_wait_locks.0,
    });
    a.jmp(Opcode::JNull { reg: wl, offset: 0 }, "win");
    a.op(Opcode::Field {
        dst: n,
        obj: wl,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSLte {
            a: n,
            b: zero,
            offset: 0,
        },
        "win",
    );
    a.label("busy");
    ret(&mut a, R_SYNC);
    // GameUI.toggleCamp's window tests (canToggleButton, canLeaveCamp) are
    // skipped for the pending leave (gtc_hooks): the host's own windows never
    // hold it. canToggleButton's cinematic test stays.
    a.label("win");
    a.op(Opcode::Field {
        dst: st,
        obj: Reg(0),
        field: p.g_state.0,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "ok");
    a.op(Opcode::Field {
        dst: b,
        obj: st,
        field: p.st_cine,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ok");
    ret(&mut a, 3);
    a.label("ok");
    ret(&mut a, 0);
    push_fn(
        code,
        vec![p.game_t, c.cm_t],
        t.i32_,
        r.0,
        a.finish(),
        c.dbg_file,
    )
}

/// `cpAsks(game)` (host): at most every ASK_EVERY s, `tool.closeActionWindow()`
/// for every player lockedWith an st.item.Tool.
fn add_cp_asks(
    code: &mut Bytecode,
    p: &Plan,
    c: &Camp,
    g: &CGlobals,
    dyn_t: RefType,
) -> Result<RefFun> {
    let every = float_const(code, ASK_EVERY);
    let i0 = int_const(code, 0);
    let t = &p.t;
    let bp_t = p.g_me.1;
    let mut r = Regs(vec![p.game_t]);
    let (now, last, diff, lim, st, sp, spa, all) = (
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(t.f64_),
        r.r(p.g_state.1),
        r.r(p.st_players.1),
        r.r(p.sp_array.1),
        r.r(p.arr_t),
    );
    let (n, i, raw, d, pl, lw, cr, b, tool, v) = (
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(bp_t),
        r.r(p.bp_locked.1),
        r.r(c.tool_cls.1),
        r.r(t.bool_),
        r.r(c.tool_t),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.ask_at,
    });
    a.op(Opcode::Sub {
        dst: diff,
        a: now,
        b: last,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: every,
    });
    a.jmp(
        Opcode::JSLt {
            a: diff,
            b: lim,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: g.ask_at,
        src: now,
    });
    a.op(Opcode::Field {
        dst: st,
        obj: Reg(0),
        field: p.g_state.0,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: sp,
        obj: st,
        field: p.st_players.0,
    });
    a.jmp(Opcode::JNull { reg: sp, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: spa,
        obj: sp,
        field: p.sp_array.0,
    });
    a.op(Opcode::SafeCast { dst: all, src: spa });
    a.jmp(
        Opcode::JNull {
            reg: all,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("loop");
    a.op(Opcode::Field {
        dst: n,
        obj: all,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: all,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: lw,
        obj: pl,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "loop");
    a.op(Opcode::GetGlobal {
        dst: cr,
        global: c.tool_cls.0,
    });
    // A State goes to BaseType.check's Dyn argument as is.
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cr,
        arg1: lw,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::UnsafeCast { dst: tool, src: lw });
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.close_rpc,
        arg0: tool,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], t.void, r.0, a.finish(), c.dbg_file)
}

struct CFns {
    log: RefFun,
    why: RefFun,
    asks: RefFun,
    /// Set by cpAsk right before the vanilla body runs a pending camp leave.
    go: RefGlobal,
}

/// `cpAsk(ctrl) -> Bool`, in front of Controller.toggleCamp__impl: true = the
/// request is pending (the impl returns), false = the vanilla body runs.
fn add_cp_ask(code: &mut Bytecode, p: &Plan, c: &Camp, g: &CGlobals, f: &CFns) -> Result<RefFun> {
    let ints = reason_ints(code);
    let i1 = int_const(code, 1);
    let cpending = str_global(code, p.t.str_, CPENDING);
    let t = &p.t;
    let ctrl_t = p.g_ctrl.1;
    let mut r = Regs(vec![ctrl_t]);
    let (game, b, md, cr, cm, k, k1, last, zero, one, s_r, v) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.g_mode.1),
        r.r(c.cm_cls.1),
        r.r(c.cm_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.str_),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: f.go,
        src: b,
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: ints[0],
    });
    a.op(Opcode::GetThis {
        dst: game,
        field: c.ctrl_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: md,
        obj: game,
        field: p.g_mode.0,
    });
    a.jmp(Opcode::JNull { reg: md, offset: 0 }, "vanilla");
    a.op(Opcode::GetGlobal {
        dst: cr,
        global: c.cm_cls.0,
    });
    // A GameMode goes to BaseType.check's Dyn argument as is.
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cr,
        arg1: md,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::UnsafeCast { dst: cm, src: md });
    a.op(Opcode::Call2 {
        dst: k,
        fun: f.why,
        arg0: game,
        arg1: cm,
    });
    a.jmp(
        Opcode::JNotEq {
            a: k,
            b: zero,
            offset: 0,
        },
        "pend",
    );
    // Nothing refuses: no request pending any more; the vanilla body runs now,
    // past GameUI.toggleCamp's window tests (the host's own windows never hold
    // the camp leave; the camp tool windows hold a lock, tested above).
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: f.go,
        src: b,
    });
    a.op(Opcode::Null { dst: md });
    a.op(Opcode::SetGlobal {
        global: g.mode,
        src: md,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.last,
    });
    a.jmp(
        Opcode::JEq {
            a: last,
            b: zero,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: zero,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: cpending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: k,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "vanilla");
    // Pending: remember the mode, ask the busy players' machines to close, log changes.
    a.label("pend");
    a.op(Opcode::SetGlobal {
        global: g.mode,
        src: md,
    });
    a.op(Opcode::Int {
        dst: k1,
        ptr: ints[R_LOCKED],
    });
    a.jmp(
        Opcode::JNotEq {
            a: k,
            b: k1,
            offset: 0,
        },
        "log",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: f.asks,
        arg0: game,
    });
    a.label("log");
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Add {
        dst: k1,
        a: k,
        b: one,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: g.last,
    });
    a.jmp(
        Opcode::JEq {
            a: k1,
            b: last,
            offset: 0,
        },
        "handled",
    );
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: k1,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: cpending,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: f.log,
        arg0: game,
        arg1: s_r,
        arg2: k,
    });
    a.label("handled");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("vanilla");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![ctrl_t], t.bool_, r.0, a.finish(), c.dbg_file)
}

/// `cpUpdate(camp)`, at the top of CampMode.update (host): retry a pending
/// leave of this mode through toggleCamp__impl; drop one of another mode.
fn add_cp_update(code: &mut Bytecode, p: &Plan, c: &Camp, g: &CGlobals) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let t = &p.t;
    let mut r = Regs(vec![c.cm_t]);
    let (game, b, pend, zero, ctrl, v) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.g_mode.1),
        r.r(t.i32_),
        r.r(p.g_ctrl.1),
        r.r(t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: c.cm_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: pend,
        global: g.mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: pend,
            offset: 0,
        },
        "end",
    );
    a.jmp(
        Opcode::JEq {
            a: pend,
            b: Reg(0),
            offset: 0,
        },
        "mine",
    );
    // a request of an earlier camp: forget it
    a.op(Opcode::Null { dst: pend });
    a.op(Opcode::SetGlobal {
        global: g.mode,
        src: pend,
    });
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: zero,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("mine");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: game,
        field: p.g_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: c.toggle,
        arg0: ctrl,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![c.cm_t], t.void, r.0, a.finish(), c.dbg_file)
}

/// `cpClose(tool) -> Bool`, in front of Tool.closeActionWindow__impl: in multi,
/// on the machine locked with this tool, close the top closable lock window
/// (flClosable) and return true; false = the vanilla body (single player).
fn add_cp_close(
    code: &mut Bytecode,
    p: &Plan,
    c: &Camp,
    closable: RefFun,
    release: RefFun,
) -> Result<RefFun> {
    let ccloses = str_global(code, p.t.str_, CCLOSES);
    let t = &p.t;
    let i0 = int_const(code, 0);
    let imax = int_const(code, 4);
    let mut r = Regs(vec![c.tool_t]);
    let (game, b, me, lw, w, name, s_r, v) = (
        r.r(p.game_t),
        r.r(t.bool_),
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.win_t),
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.void),
    );
    let (cnt, lim, last_w) = (r.r(t.i32_), r.r(t.i32_), r.r(p.win_t));
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: c.tool_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "done");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: game,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "done");
    // Close top-down (a UnitInfo over the tool window first), at most 4 windows,
    // while this machine is still locked with this tool.
    a.op(Opcode::Int { dst: cnt, ptr: i0 });
    a.op(Opcode::Null { dst: last_w });
    a.loop_head("loop");
    a.op(Opcode::Int {
        dst: lim,
        ptr: imax,
    });
    a.jmp(
        Opcode::JSGte {
            a: cnt,
            b: lim,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Incr { dst: cnt });
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(
        Opcode::JNotEq {
            a: lw,
            b: Reg(0),
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Call1 {
        dst: w,
        fun: closable,
        arg0: game,
    });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "done");
    a.jmp(
        Opcode::JEq {
            a: w,
            b: last_w,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Mov {
        dst: last_w,
        src: w,
    });
    a.op(Opcode::GetGlobal {
        dst: s_r,
        global: ccloses,
    });
    a.op(Opcode::Field {
        dst: name,
        obj: w,
        field: p.w_name.0,
    });
    a.op(Opcode::Call2 {
        dst: s_r,
        fun: p.str_add,
        arg0: s_r,
        arg1: name,
    });
    close_own_tail(&mut a, p, w, s_r, v);
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("done");
    // a lock no closable window took away and no minigame holds
    a.op(Opcode::Call1 {
        dst: v,
        fun: release,
        arg0: game,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("vanilla");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![c.tool_t], t.bool_, r.0, a.finish(), c.dbg_file)
}

/// New functions, in the order `apply` appends them.
struct Added {
    why: RefFun,
    text: RefFun,
    log: RefFun,
    notify: RefFun,
    refused: RefFun,
    closable: RefFun,
    dialog: RefFun,
    minigame: RefFun,
    release: RefFun,
    update: RefFun,
    paused: RefFun,
    tav: Option<TAdded>,
    camp: Option<CAdded>,
}

struct CAdded {
    why: RefFun,
    asks: RefFun,
    ask: RefFun,
    update: RefFun,
    close: RefFun,
}

struct TAdded {
    mark: RefFun,
    why: RefFun,
    ask: RefFun,
    update: RefFun,
    close: RefFun,
}

/// `if (hook(this)) return;` in front of op 0 of a Void function.
fn guard_hook(f: &mut Function, bool_t: RefType, void_t: RefType, fun: RefFun) {
    f.regs.push(bool_t);
    let b = Reg((f.regs.len() - 1) as u32);
    f.regs.push(void_t);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Call1 {
                dst: b,
                fun,
                arg0: Reg(0),
            },
            Opcode::JFalse { cond: b, offset: 1 },
            Opcode::Ret { ret: v },
        ],
    );
}

/// GameUI.toggleCamp: while cpAsk lets a pending camp leave run (`go`), its
/// window tests pass. `if (!canToggleButton(..)) return;` gets `if (go)` around
/// its `Ret`; `JFalse canLeaveCamp` jumps to a block right after it that goes
/// on as `true` when `go` (clearing it), else to its old target. Both inserts
/// only jump forward (HL needs a Label at every backward target).
fn gtc_hooks(f: &mut Function, p: &Plan, c: &Camp, go: RefGlobal) {
    f.regs.push(p.t.bool_);
    let g = Reg((f.regs.len() - 1) as u32);
    let Opcode::JFalse { cond, offset } = f.ops[c.cl_jf] else {
        unreachable!("planned")
    };
    // B first (the higher index): [JAlways +4; GetGlobal g; JFalse g -> old
    // target; Bool g = false; SetGlobal go] after the JFalse.
    let at = c.cl_jf + 1;
    let old_false = (c.cl_jf as i32 + 1 + offset) as usize + 5;
    insert_ops(
        f,
        at,
        vec![
            Opcode::JAlways { offset: 4 },
            Opcode::GetGlobal { dst: g, global: go },
            Opcode::JFalse {
                cond: g,
                offset: old_false as i32 - (at as i32 + 2) - 1,
            },
            Opcode::Bool {
                dst: g,
                value: ValBool(false),
            },
            Opcode::SetGlobal { global: go, src: g },
        ],
    );
    f.ops[c.cl_jf] = Opcode::JFalse { cond, offset: 1 };
    // A: [GetGlobal g; JTrue g +1] in front of the Ret.
    insert_ops(
        f,
        c.ct_ret,
        vec![
            Opcode::GetGlobal { dst: g, global: go },
            Opcode::JTrue { cond: g, offset: 1 },
        ],
    );
}
fn apply(code: &mut Bytecode, p: &Plan) -> Result<Added> {
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let g = Globals {
        last: add_global(code, p.t.i32_),
        closed: add_global(code, p.win_t),
        paused_at: add_global(code, p.t.f64_),
        tl_mode: add_global(code, p.g_mode.1),
        tl_last: add_global(code, p.t.i32_),
        tl_closed: add_global(code, p.win_t),
    };
    let cg = p.camp.as_ref().map(|_| CGlobals {
        mode: add_global(code, p.g_mode.1),
        last: add_global(code, p.t.i32_),
        ask_at: add_global(code, p.t.f64_),
    });
    let wg = WGlobals {
        pend_at: add_global(code, p.t.f64_),
        warned_at: add_global(code, p.t.f64_),
        here_lw: add_global(code, p.bp_locked.1),
        rel_lw: add_global(code, p.bp_locked.1),
        rel_at: add_global(code, p.t.f64_),
    };
    let go = p.camp.as_ref().map(|_| add_global(code, p.t.bool_));
    let why = add_why(code, p)?;
    let text = add_text(code, p, dyn_t)?;
    let log = add_log(code, p, text)?;
    let notify = add_notify(code, p)?;
    let refused = add_refused(code, p, why, log)?;
    let closable = add_closable(code, p, dyn_t)?;
    let dialog = add_dialog(code, p, dyn_t)?;
    let minigame = add_minigame(code, p, dyn_t)?;
    let release = add_release(code, p, &wg, minigame)?;
    let fns = Fns {
        why,
        log,
        closable,
        dialog,
        text,
        notify,
        release,
    };
    let update = add_update(code, p, &g, &wg, &fns, dyn_t)?;
    let paused = add_paused(code, p, &g)?;

    let mut hook = |fi: usize, at: usize, op: fn(Reg, RefFun) -> Opcode, fun: RefFun| {
        let f = &mut code.functions[fi];
        f.regs.push(p.t.void);
        let v = Reg((f.regs.len() - 1) as u32);
        insert_ops(f, at, vec![op(v, fun)]);
    };
    let this_call = |dst, fun| Opcode::Call1 {
        dst,
        fun,
        arg0: Reg(0),
    };
    hook(p.update_fi, 0, this_call, update);
    hook(p.try_close_fi, 0, this_call, refused);
    hook(
        p.leave_fi,
        p.paused_ret,
        |dst, fun| Opcode::Call0 { dst, fun },
        paused,
    );
    let f = &mut code.functions[p.content_fi];
    if let Opcode::JTrue { cond, .. } = f.ops[p.content_at] {
        f.ops[p.content_at] = Opcode::JTrue { cond, offset: 0 };
    }

    let tav = match &p.tav {
        None => None,
        Some(tv) => {
            let mark = add_mark(code, p, tv)?;
            let twhy = add_tl_why(code, p, tv)?;
            let tf = TFns {
                log,
                closable,
                mark,
                why: twhy,
                release,
            };
            let ask = add_tl_ask(code, p, tv, &g, &tf)?;
            let tupdate = add_tl_update(code, p, tv, &g, &tf)?;
            let close = add_tl_close(code, p, tv)?;
            guard_hook(&mut code.functions[tv.ask_fi], p.t.bool_, p.t.void, ask);
            guard_hook(&mut code.functions[tv.close_fi], p.t.bool_, p.t.void, close);
            let f = &mut code.functions[tv.upd_fi];
            f.regs.push(p.t.void);
            let v = Reg((f.regs.len() - 1) as u32);
            insert_ops(f, 0, vec![this_call(v, tupdate)]);
            Some(TAdded {
                mark,
                why: twhy,
                ask,
                update: tupdate,
                close,
            })
        }
    };
    let camp = match (&p.camp, &cg, go) {
        (Some(c), Some(cg), Some(go)) => {
            let cwhy = add_cp_why(code, p, c)?;
            let asks = add_cp_asks(code, p, c, cg, dyn_t)?;
            let cf = CFns {
                log,
                why: cwhy,
                asks,
                go,
            };
            let ask = add_cp_ask(code, p, c, cg, &cf)?;
            let cupdate = add_cp_update(code, p, c, cg)?;
            let close = add_cp_close(code, p, c, closable, release)?;
            guard_hook(&mut code.functions[c.ask_fi], p.t.bool_, p.t.void, ask);
            guard_hook(&mut code.functions[c.close_fi], p.t.bool_, p.t.void, close);
            gtc_hooks(&mut code.functions[c.gtc_fi], p, c, go);
            let f = &mut code.functions[c.upd_fi];
            f.regs.push(p.t.void);
            let v = Reg((f.regs.len() - 1) as u32);
            insert_ops(f, 0, vec![this_call(v, cupdate)]);
            Some(CAdded {
                why: cwhy,
                asks,
                ask,
                update: cupdate,
                close,
            })
        }
        _ => None,
    };
    Ok(Added {
        why,
        text,
        log,
        notify,
        refused,
        closable,
        dialog,
        minigame,
        release,
        update,
        paused,
        tav,
        camp,
    })
}

/// Indices of every function `apply` edits.
fn touched(p: &Plan) -> Vec<usize> {
    let mut v = vec![p.update_fi, p.try_close_fi, p.leave_fi, p.content_fi];
    if let Some(tv) = &p.tav {
        v.extend([tv.ask_fi, tv.close_fi, tv.upd_fi]);
    }
    if let Some(c) = &p.camp {
        v.extend([c.ask_fi, c.close_fi, c.upd_fi, c.gtc_fi]);
    }
    v
}

/// Applies the force leave, or leaves `code` untouched and logs why.
pub(crate) fn patch_force_leave(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("force leave skipped: {e:#}");
            return;
        }
    };
    let snap = crate::asm::Snap::take(code);
    let sites = touched(&p);
    let saved: Vec<Function> = sites.iter().map(|&i| code.functions[i].clone()).collect();
    match apply(code, &p) {
        Ok(a) => {
            eprintln!(
                "patched force leave: PlaceView.update -> fn@{} (why fn@{}, log fn@{}, text fn@{}, notify fn@{}, windows fn@{}, dialog fn@{}, minigame fn@{}, release fn@{}), tryClose -> fn@{}, leave -> fn@{}",
                a.update.0, a.why.0, a.log.0, a.text.0, a.notify.0, a.closable.0, a.dialog.0, a.minigame.0, a.release.0, a.refused.0, a.paused.0
            );
            if let Some(t) = a.tav {
                eprintln!(
                    "patched force leave (owned tavern): askLeave -> fn@{}, closeTavern -> fn@{}, TavernMode.update -> fn@{} (why fn@{}, mark fn@{})",
                    t.ask.0, t.close.0, t.update.0, t.why.0, t.mark.0
                );
            }
            if let Some(c) = a.camp {
                eprintln!(
                    "patched force leave (camp): toggleCamp__impl -> fn@{}, CampMode.update -> fn@{}, Tool.closeActionWindow__impl -> fn@{} (why fn@{}, asks fn@{})",
                    c.ask.0, c.update.0, c.close.0, c.why.0, c.asks.0
                );
            }
        }
        Err(e) => {
            snap.restore(code);
            for (f, i) in saved.into_iter().zip(sites) {
                code.functions[i] = f;
            }
            eprintln!("force leave skipped: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let tv = p.tav.as_ref().expect("owned tavern part planned");
        let cp = p.camp.as_ref().expect("camp part planned");
        let mut code = read(&image);
        let added = apply(&mut code, &p).expect("apply");
        let ta = added.tav.as_ref().expect("owned tavern part applied");
        let ca = added.camp.as_ref().expect("camp part applied");
        let patched = write(&code);
        let back = read(&patched);

        // Appended only: 21 functions (+ their types), 15 globals.
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 21);
        assert_eq!(&back.types[..orig.types.len()], &orig.types[..]);
        let ng = orig.globals.len();
        assert_eq!(&back.globals[..ng], &orig.globals[..]);
        assert_eq!(
            &back.globals[ng..ng + 15],
            &[
                p.t.i32_,
                p.win_t,
                p.t.f64_,
                p.g_mode.1,
                p.t.i32_,
                p.win_t,
                p.g_mode.1,
                p.t.i32_,
                p.t.f64_,
                p.t.f64_,
                p.t.f64_,
                p.bp_locked.1,
                p.bp_locked.1,
                p.t.f64_,
                p.t.bool_
            ]
        );
        let touched = touched(&p);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                !touched.contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        let new: Vec<RefFun> = vec![
            added.why,
            added.text,
            added.log,
            added.notify,
            added.refused,
            added.closable,
            added.dialog,
            added.minigame,
            added.release,
            added.update,
            added.paused,
            ta.mark,
            ta.why,
            ta.ask,
            ta.update,
            ta.close,
            ca.why,
            ca.asks,
            ca.ask,
            ca.update,
            ca.close,
        ];
        for (k, f) in back.functions[nf..].iter().enumerate() {
            assert_eq!(f.findex, new[k]);
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }

        // One call inserted at the hook points, everything else shifted.
        for (fi, at, fun) in [
            (p.update_fi, 0, added.update),
            (p.try_close_fi, 0, added.refused),
            (p.leave_fi, p.paused_ret, added.paused),
            (tv.upd_fi, 0, ta.update),
            (cp.upd_fi, 0, ca.update),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, at, 1);
            assert!(
                matches!(b.ops[at], Opcode::Call1 { fun: f, arg0: Reg(0), .. } | Opcode::Call0 { fun: f, .. } if f == fun)
            );
            check_types(&back, b, at..at + 1);
        }
        // `if (hook(this)) return;` in front of askLeave__impl / closeTavern__impl,
        // toggleCamp__impl / Tool.closeActionWindow__impl.
        for (fi, fun) in [
            (tv.ask_fi, ta.ask),
            (tv.close_fi, ta.close),
            (cp.ask_fi, ca.ask),
            (cp.close_fi, ca.close),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, 0, 3);
            assert!(matches!(
                b.ops[..3],
                [Opcode::Call1 { fun: f, arg0: Reg(0), dst }, Opcode::JFalse { cond, offset: 1 }, Opcode::Ret { .. }]
                    if f == fun && cond == dst
            ));
            check_types(&back, b, 0..3);
            check_flow(b);
        }
        // GameUI.toggleCamp: [GetGlobal go; JTrue +1] before the canToggleButton
        // Ret, the go block after `JFalse canLeaveCamp`; forward jumps only.
        let (a, b) = (&orig.functions[cp.gtc_fi], &back.functions[cp.gtc_fi]);
        let n0 = a.ops.len();
        assert_eq!(b.ops.len(), n0 + 7);
        let (r, j) = (cp.ct_ret, cp.cl_jf + 2);
        assert_eq!(jump_targets(b, r + 1), vec![r + 3]);
        assert!(matches!(b.ops[r + 2], Opcode::Ret { .. }));
        assert_eq!(jump_targets(b, j), vec![j + 2]);
        assert_eq!(jump_targets(b, j + 1), vec![cp.cl_jf + 1 + 7]);
        let old_false = jump_targets(a, cp.cl_jf)[0];
        assert_eq!(jump_targets(b, j + 3), vec![old_false + 7]);
        check_flow(b);
        check_types(&back, b, r..r + 2);
        check_types(&back, b, j..j + 6);
        // leave: the probe sits between `JFalse paused` and its Ret.
        let lb = &back.functions[p.leave_fi];
        assert!(matches!(lb.ops[p.paused_ret - 1], Opcode::JFalse { .. }));
        assert!(matches!(lb.ops[p.paused_ret + 1], Opcode::Ret { .. }));
        assert_eq!(jump_targets(lb, p.paused_ret - 1), vec![p.paused_ret + 2]);
        // F1: the lock test always falls into "enable".
        let cb = &back.functions[p.content_fi];
        assert!(matches!(
            cb.ops[p.content_at],
            Opcode::JTrue { offset: 0, .. }
        ));
        assert_eq!(cb.ops.len(), orig.functions[p.content_fi].ops.len());

        // Camp: the retry re-enters the hooked toggleCamp__impl; the close requests
        // go through the vanilla RPC wrapper, whose dispatch reaches the hooked impl.
        let by = |fun: RefFun| back.functions.iter().find(|f| f.findex == fun).unwrap();
        let calls_fn = |f: &Function, want: RefFun| {
            f.ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == want))
        };
        assert!(calls_fn(by(ca.update), cp.toggle));
        assert_eq!(back.functions[cp.ask_fi].findex, cp.toggle);
        assert!(calls_fn(by(ca.asks), cp.close_rpc));
        assert!(calls_fn(by(ca.close), added.closable));
        let dispatch = proto(&orig, cp.tool_t, "networkRPC").unwrap();
        let imp = orig.functions[cp.close_fi].findex;
        assert!(calls_fn(by(dispatch), imp));
        assert!(calls_fn(by(cp.close_rpc), imp));
        // The camp tool windows enable their own close (Window.tryClose = canBeClosed).
        let enable = method(&orig, p.win_t, "enableClose").unwrap().findex;
        for l in &p.locks[LOCK_WINDOWS.len()..] {
            let init = method(&orig, l.ct, "init").unwrap();
            assert!(
                init.ops
                    .iter()
                    .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == enable)),
                "{} enables close",
                s(&orig, obj(&orig, l.ct).unwrap().name)
            );
        }

        // The whitelist is checked against window classes, the slots are Window's.
        assert_eq!(p.locks.len(), LOCK_WINDOWS.len() + CAMP_WINDOWS.len());
        assert_eq!(
            p.locks.iter().filter(|l| l.act.is_some()).count(),
            2,
            "Craft and Alter carry act"
        );
        let gu = method(&orig, obj_type(&orig, "Game").unwrap(), "update").unwrap();
        for slot in [p.w_try_close, p.w_close] {
            assert!(gu
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::CallMethod { field, .. } if *field == slot)));
        }
        // WindowModalMode.None is the default modal the Window constructor stores.
        let wc = method(&orig, p.win_t, "__constructor__").unwrap();
        let none_global = wc.ops.iter().enumerate().find_map(|(i, o)| match o {
            Opcode::SetThis { field, src } if *field == p.w_modal.0 => {
                wc.ops[..i].iter().rev().find_map(|o| match o {
                    Opcode::GetGlobal { dst, global } if dst == src => Some(*global),
                    _ => None,
                })
            }
            _ => None,
        });
        let none_global = none_global.expect("ctor stores a modal global");
        let set_modal = method(&orig, p.win_t, "setModal").unwrap();
        assert!(set_modal
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::GetGlobal { global, .. } if *global == none_global)));
        assert!(matches!(
            &orig.types[p.w_modal.1 .0],
            Type::Enum { constructs, .. } if s(&orig, constructs[p.modal_none as usize].name) == "None"
        ));
        // The mark is the vanilla proxy mark: same slot, receiver and argument types.
        let mark = back.functions.iter().find(|f| f.findex == ta.mark).unwrap();
        let mk = mark
            .ops
            .iter()
            .find_map(|o| match o {
                Opcode::CallMethod { field, args, .. } => Some((*field, args.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(mk.0, tv.px_mark);
        assert_eq!(mark.regs[mk.1[0].0 as usize], tv.px_obj.1);
        assert_eq!(mark.regs[mk.1[1].0 as usize], p.t.i32_);

        // Every virtual call resolves to the intended method with a matching signature.
        for f in &back.functions[nf..] {
            check_virtual_calls(&back, f);
        }

        // A second pass changes nothing.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_force_leave(&mut again);
        assert!(write(&again) == patched);
    }

    /// A started activity is never closed: its minigame windows are ActivityWindows
    /// that Activity._start opens after paying `onStartActivityCost`, and none of
    /// them is on the close list. A long wait is reported instead (host: on screen
    /// + log, busy machine: on screen + log), from flUpdate's wait timer.
    #[test]
    fn long_waits_are_reported_not_forced() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let aw = obj_type(&orig, "ui.win.ActivityWindow").unwrap();
        for name in [
            "ui.win.ForgeAction",
            "ui.win.GatherAction",
            "ui.win.AnalyzeAction",
            "ui.win.SingAction",
            "ui.win.FishingAction",
            "ui.win.UnitAction",
        ] {
            let ct = obj_type(&orig, name).unwrap();
            assert!(is_sub(&orig, ct, aw), "{name} is an ActivityWindow");
            assert!(p.locks.iter().all(|l| l.ct != ct), "{name} is never closed");
        }
        // Location-independent windows are neither closed nor a modal that is.
        for name in [
            "ui.win.Paths",
            "ui.win.TroopBonus",
            "ui.win.GameOptions",
            "ui.win.Pause",
            "ui.win.MiniMap",
            "ui.win.UnitCustomize",
            "ui.win.Grimoire",
        ] {
            let ct = obj_type(&orig, name).unwrap();
            assert!(p.locks.iter().all(|l| l.ct != ct), "{name} stays open");
            let (g, _) = class_global(&orig, name).unwrap();
            assert!(p.modal_ok.iter().all(|m| m.0 != g), "{name} stays open");
        }
        // The character sheet is closed only as the NPC inspect.
        let ui = obj_type(&orig, "ui.win.UnitInfo").unwrap();
        assert!(p.locks.iter().any(|l| l.ct == ui && l.guard == 3));
        let act_t = obj_type(&orig, "ui.win.Activity").unwrap();
        let start = method(&orig, act_t, "_start").unwrap();
        let (cost, _) = field(&orig, act_t, "onStartActivityCost").unwrap();
        assert!(start
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Field { field, .. } | Opcode::GetThis { field, .. } if *field == cost)));

        let mut code = read(&image);
        let added = apply(&mut code, &p).expect("apply");
        let by = |fun: RefFun| code.functions.iter().find(|f| f.findex == fun).unwrap();
        let count = |f: &Function, want: RefFun| {
            f.ops
                .iter()
                .filter(|o| {
                    matches!(o, Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. }
                        | Opcode::Call2 { fun, .. } | Opcode::Call3 { fun, .. } if *fun == want)
                })
                .count()
        };
        let up = by(added.update);
        assert_eq!(count(up, p.sys_time), 1);
        assert_eq!(count(up, added.text), 1, "host line names reason + player");
        assert_eq!(count(up, added.notify), 2, "host and busy player are told");
        assert_eq!(
            count(up, added.closable),
            1,
            "still closes only guarded windows"
        );
        // flLog keeps its shape (every caller) and prints flText.
        let lg = by(added.log);
        assert_eq!(count(lg, added.text), 1);
        assert_eq!(count(lg, p.println), 1);
        // flText reads BasePlayer.name for "player busy".
        assert!(by(added.text)
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Field { field, .. } if *field == p.bp_name)));
        // flNotify: GameUI.localNotify(id, {title}).
        assert_eq!(count(by(added.notify), p.ntf.local_notify), 1);
        // A dialog is ended even without a Leave choice / while hidden.
        assert_eq!(count(by(added.dialog), p.dlg.leave), 1);
        assert_eq!(count(by(added.dialog), p.dlg.try_close), 1);
        // flClosable has a second return: the topmost modal over a guarded window.
        let rets = by(added.closable)
            .ops
            .iter()
            .filter(|o| matches!(o, Opcode::Ret { .. }))
            .count();
        assert!(rets >= 3);
        assert!(WARN_AFTER > 0.0 && WARN_EVERY >= WARN_AFTER);
    }

    /// `CallMethod` receiver / slot / arguments / result agree with the method the
    /// slot holds in the receiver's class, and only tryClose / close are called on
    /// objects (a virtual receiver is the leaveState proxy mark, checked above).
    fn check_virtual_calls(code: &Bytecode, f: &Function) {
        for op in &f.ops {
            let Opcode::CallMethod { dst, field, args } = op else {
                continue;
            };
            let recv = f.regs[args[0].0 as usize];
            if matches!(code.types[recv.0], Type::Virtual { .. }) {
                continue;
            }
            let mut cur = Some(recv);
            let mut found = None;
            while let Some(c) = cur {
                let o = obj(code, c).expect("receiver is an object");
                if let Some(pr) = o.protos.iter().find(|pr| pr.pindex as usize == field.0) {
                    found = Some(pr.findex);
                    break;
                }
                cur = o.super_;
            }
            let target = found.expect("slot resolves on the receiver's class");
            let g = code
                .functions
                .iter()
                .find(|g| g.findex == target)
                .expect("method");
            let name = s(code, g.name);
            assert!(
                name == "tryClose" || name == "close" || name == "leave",
                "fn@{}: {name}",
                f.findex.0
            );
            let ft = g.t.as_fun(code).expect("fun type");
            assert_eq!(ft.args.len(), args.len(), "fn@{} {name}", f.findex.0);
            let dt = f.regs[dst.0 as usize];
            if name == "tryClose" {
                assert!(matches!(code.types[ft.ret.0], Type::Bool));
                assert!(matches!(code.types[dt.0], Type::Bool));
            } else {
                assert!(matches!(code.types[ft.ret.0], Type::Void));
                assert!(matches!(code.types[dt.0], Type::Void));
            }
        }
    }

    /// Every appended function reachable from `roots` (direct calls only).
    fn reachable<'a>(b: &'a Bytecode, first_new: usize, roots: &[RefFun]) -> Vec<&'a Function> {
        let by = |fun: RefFun| b.functions.iter().find(|g| g.findex == fun);
        let mut out: Vec<&Function> = vec![];
        let mut todo: Vec<RefFun> = roots.to_vec();
        while let Some(fun) = todo.pop() {
            let Some(g) = by(fun) else { continue };
            if out.iter().any(|o| o.findex == fun) {
                continue;
            }
            out.push(g);
            for op in &g.ops {
                if let Opcode::Call0 { fun, .. }
                | Opcode::Call1 { fun, .. }
                | Opcode::Call2 { fun, .. }
                | Opcode::Call3 { fun, .. } = op
                {
                    if fun.0 >= first_new {
                        todo.push(*fun);
                    }
                }
            }
        }
        out
    }

    /// The whole pipeline (coop_gates G1/G2/G6 before, diag and the rest after):
    /// the pass still applies on top of G2/G6, and its hooks reach type-correct code.
    #[test]
    fn composes_with_other_passes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let tv = p.tav.as_ref().expect("owned tavern part planned");
        let cp = p.camp.as_ref().expect("camp part planned");
        let out = crate::patch_image(&image).expect("patch_image");
        let b = read(&out);
        assert!(plan(&b).is_err(), "force leave was not applied");
        let callee = |fi: usize, at: usize| match b.functions[fi].ops[at] {
            Opcode::Call1 { fun, .. } | Opcode::Call0 { fun, .. } => fun,
            ref o => panic!("fn@{} op {at}: {o:?}", b.functions[fi].findex.0),
        };
        // The hooks stay at op 0 (no other pass touches these functions).
        let roots = [
            callee(p.update_fi, 0),
            callee(p.try_close_fi, 0),
            callee(tv.upd_fi, 0),
            callee(tv.ask_fi, 0),
            callee(tv.close_fi, 0),
            callee(cp.ask_fi, 0),
            callee(cp.close_fi, 0),
            callee(cp.upd_fi, 0),
        ];
        let first_new = orig.functions.len() + orig.natives.len();
        assert!(roots.iter().all(|f| f.0 >= first_new));
        let ours = reachable(&b, first_new, &roots);
        // All 21 but flPaused (it hangs off PlaceView.leave, which diag also edits).
        assert_eq!(ours.len(), 20, "functions the hooks reach");
        for g in ours {
            check_flow(g);
            check_types(&b, g, 0..g.ops.len());
            check_virtual_calls(&b, g);
        }
        // G2 still forces the request this pass relies on.
        let sl = method(
            &b,
            obj_type(&b, "ent.Place").unwrap(),
            "setLeaveState__impl",
        )
        .unwrap();
        assert!(matches!(
            sl.ops[..2],
            [
                Opcode::JFalse {
                    cond: Reg(1),
                    offset: 1
                },
                Opcode::Mov {
                    dst: Reg(2),
                    src: Reg(1)
                }
            ]
        ));
        // G6 still ignores an open window: no getPlayerLocked filter reads hasWindowOpened.
        let game_t = obj_type(&b, "Game").unwrap();
        let gpl = method(&b, game_t, "getPlayerLocked").unwrap();
        let bp_t = obj_type(&b, "ent.BasePlayer").unwrap();
        let (hwo, _) = field(&b, bp_t, "hasWindowOpened").unwrap();
        let filters: Vec<RefFun> = gpl
            .ops
            .iter()
            .filter_map(|o| match o {
                Opcode::InstanceClosure { fun, .. } => Some(*fun),
                _ => None,
            })
            .collect();
        assert!(!filters.is_empty());
        let by = |fun: RefFun| b.functions.iter().find(|g| g.findex == fun).expect("fn");
        for fun in filters {
            let g = by(fun);
            assert!(!g
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::Field { obj, field, .. }
                if *field == hwo && g.regs[obj.0 as usize] == bp_t)));
        }
    }
}
