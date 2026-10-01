// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a leave from a place (town, tavern, location) is never blocked by
// another player's business.
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
//            w = the top window, in game.mode.windows then game.globalUI.windows,
//                of a lock-holding class (UnitInfo, ChooseUnit, FiefMandateDetails,
//                GarnisonManager, CounterChest) whose tryClose() (canBeClosed) is true;
//            if (w != null && w != lastClosed) { lastClosed = w; println; w.close(); }
//        }
//        // host: leave as soon as nothing refuses, waiting (never skipping) otherwise
//        if (isAuth) { why = flWhy(view, true); print it when it changes;
//                      if (why == 0) view.tryClose(); }
//      `close()` is the class's own close: Window.close (removeChild -> onRemove
//      -> the onClose the lock site installed, which clears lockedWith and, for
//      the dialog inspect, unhides the dialog), ChooseUnit.close -> cancel() ->
//      onCanceled (dialog customize: cancelCost refund + unlock), i.e. what the
//      player's own Escape / Cancel does. The lock clears on the owner and
//      replicates; tryClose re-checks everything and leave() (guarded by
//      `leaving`) runs syncLeaveMode, the normal barrier.
//      Waited on, not cancelled: activities / crafting / gathering (they set
//      lockLeave themselves and end on their own), a shared dialog (every player
//      sees it and can end it), fades, cinematics, a running mode switch.
//   F3 Diagnostics (shim.log "game: mp: ..." lines): tryClose prints the reason
//      it refuses ("mp: tryClose refused: <why>"), the pending leave prints each
//      change of what it waits on ("mp: leave pending: <why|go>") and every window
//      it closes ("mp: leave closes <type>"), PlaceView.leave prints a refusal
//      while the game is paused (at most every 5 s).
//
// Coordination: the G1 repeat guard still runs the Leave button once per click
// burst and drops clicks while the mode-switch barrier runs; the pending leave
// waits for that barrier too and leave() runs once per PlaceView. The follow
// pass is world-map only.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefGlobal, ValBool};

const PENDING: &str = "mp: leave pending: ";
const REFUSED: &str = "mp: tryClose refused: ";
const CLOSES: &str = "mp: leave closes ";
const PAUSED: &str = "mp: leave refused: paused";
/// Seconds between two "paused" refusal lines.
const PAUSED_EVERY: f64 = 5.0;

/// Reason codes of `flWhy` (0 = nothing refuses) and their log text.
const REASONS: [&str; 11] = [
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
];
const R_LOCKED: i32 = 2;

/// Window classes whose `close()` is the vanilla cancel of a `lockedWith`.
const LOCK_WINDOWS: [&str; 5] = [
    "ui.win.UnitInfo",
    "ui.win.ChooseUnit",
    "ui.win.fief.FiefMandateDetails",
    "ui.win.GarnisonManager",
    "ui.win.CounterChest",
];

type F = (RefField, RefType);

struct T {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    str_: RefType,
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
    v_try_close: RefField,
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
    // Window lists
    m_windows: F,
    u_windows: F,
    arr_t: RefType,
    a_len: RefField,
    a_raw: F,
    w_name: F,
    w_try_close: RefField,
    w_close: RefField,
    classes: Vec<(RefGlobal, RefType)>,
    base_check: RefFun,
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
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let fun = &code.functions[fun_index(code, f)?];
    let t = fun.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

fn native(code: &Bytecode, name: &str, args: &[RefType], ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .natives
        .iter()
        .filter(|n| {
            s(code, n.name) == name
                && n.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == args && t.ret == ret)
        })
        .map(|n| n.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one native {name}, found {}", hits.len()),
    }
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

/// The class global of `name` (HL stores it 1-based) and its type `pkg.$Cls`.
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
    let v_try_close = slot(code, view_t, "tryClose")?;
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
    let mut classes = vec![];
    for name in LOCK_WINDOWS {
        let mut cur = Some(obj_type(code, name)?);
        while let Some(c) = cur.filter(|&c| c != win_t) {
            cur = obj(code, c)?.super_;
        }
        if cur.is_none() {
            bail!("{name} is not a ui.Window");
        }
        classes.push(class_global(code, name)?);
    }
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
    if !tc
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::CallThis { field, .. } if *field == leave_slot))
    {
        bail!("PlaceView.tryClose does not call leave()");
    }
    if (0..tc.ops.len()).any(|i| jump_targets(tc, i).contains(&0)) {
        bail!("PlaceView.tryClose: a jump targets op 0");
    }

    // PlaceView.update: the hook goes in front of op 0.
    let upd = method(code, view_t, "update")?;
    if fun_args(code, upd) != [view_t, t.f64_] {
        bail!("unexpected PlaceView.update signature");
    }
    let update_fi = fun_index(code, upd.findex)?;
    if (0..upd.ops.len()).any(|i| jump_targets(upd, i).contains(&0)) {
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
    if (0..lv.ops.len()).any(|i| jump_targets(lv, i).contains(&paused_ret)) {
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

    Ok(Plan {
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
        v_try_close,
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
        m_windows,
        u_windows,
        arr_t,
        a_len,
        a_raw,
        w_name,
        w_try_close,
        w_close,
        classes,
        base_check: check.findex,
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
    })
}

struct Globals {
    /// Last logged pending reason + 1 (0 = none yet).
    last: RefGlobal,
    /// The window the pending leave closed last.
    closed: RefGlobal,
    /// sys_time of the last "paused" refusal line.
    paused_at: RefGlobal,
}

fn add_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

/// `flWhy(view, poll) -> I32`: the first thing that makes `view.tryClose()`
/// refuse (same order), and with `poll` what the pending leave also waits on.
fn add_why(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let ints: Vec<_> = (0..REASONS.len() as i32)
        .map(|i| int_const(code, i))
        .collect();
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
    ret(&mut a, 2);
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
    // 4 gamepad + a global window
    a.label("pad");
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
    ret(&mut a, 5);
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
    ret(&mut a, 8);
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
    ret(&mut a, 9);
    a.label("paused");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.paused,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ok");
    ret(&mut a, 10);
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

const TEXT_LABELS: [&str; 11] = [
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10",
];

/// `flLog(view, prefix, why)`: println(prefix + text(why)); for "player busy"
/// also the first locked player's `lockedWith` and whose it is.
fn add_log(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let ints: Vec<_> = (0..REASONS.len() as i32)
        .map(|i| int_const(code, i))
        .collect();
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
    let mut r = Regs(vec![p.view_t, t.str_, t.i32_]);
    let (s_r, x, y, k, game, arr, n, zero) = (
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.str_),
        r.r(t.i32_),
        r.r(p.game_t),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
    );
    let (raw, d, pl, lw, me, b, v) = (
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(bp_t),
        r.r(p.bp_locked.1),
        r.r(bp_t),
        r.r(t.bool_),
        r.r(t.void),
    );
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
        ptr: ints[R_LOCKED as usize],
    });
    a.jmp(
        Opcode::JNotEq {
            a: Reg(2),
            b: k,
            offset: 0,
        },
        "print",
    );
    a.op(Opcode::GetThis {
        dst: game,
        field: p.v_game.0,
    });
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
    a.label("print");
    // A String goes to println's Dyn argument as is.
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s_r,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.view_t, t.str_, t.i32_],
        t.void,
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
    let (b, c, zero, pre, v) = (
        r.r(t.bool_),
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
        arg0: Reg(0),
        arg1: pre,
        arg2: c,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.view_t], t.void, r.0, a.finish(), p.dbg_file)
}

/// `flClosable(game) -> Window`: the top window of a lock-holding class that
/// may be closed (`tryClose()`), in game.mode.windows then game.globalUI.windows.
fn add_closable(code: &mut Bytecode, p: &Plan, dyn_t: RefType) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let t = &p.t;
    let mut r = Regs(vec![p.game_t]);
    let (mode, gui, lst, i, zero, raw, d, w, b) = (
        r.r(p.g_mode.1),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(p.a_raw.1),
        r.r(dyn_t),
        r.r(p.win_t),
        r.r(t.bool_),
    );
    let cls: Vec<(RefGlobal, Reg)> = p.classes.iter().map(|&(g, ct)| (g, r.r(ct))).collect();
    let mut a = Asm::new();
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    for (k, (next, head, hit)) in [("gui", "loop1", "hit1"), ("none", "loop2", "hit2")]
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
                next,
            );
            a.op(Opcode::Field {
                dst: lst,
                obj: mode,
                field: p.m_windows.0,
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
        }
        a.jmp(
            Opcode::JNull {
                reg: lst,
                offset: 0,
            },
            next,
        );
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
            next,
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
        for &(g, cr) in &cls {
            a.op(Opcode::GetGlobal { dst: cr, global: g });
            // A Window goes to BaseType.check's Dyn argument as is.
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.base_check,
                arg0: cr,
                arg1: w,
            });
            a.jmp(Opcode::JTrue { cond: b, offset: 0 }, hit);
        }
        a.jmp(Opcode::JAlways { offset: 0 }, head);
        a.label(hit);
        a.op(Opcode::CallMethod {
            dst: b,
            field: p.w_try_close,
            args: vec![w],
        });
        a.jmp(Opcode::JFalse { cond: b, offset: 0 }, head);
        a.op(Opcode::Ret { ret: w });
    }
    a.label("none");
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::Ret { ret: w });
    push_fn(code, vec![p.game_t], p.win_t, r.0, a.finish(), p.dbg_file)
}

/// `flUpdate(view)`, at the top of PlaceView.update; see the module comment.
fn add_update(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    why: RefFun,
    log: RefFun,
    closable: RefFun,
) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let i1 = int_const(code, 1);
    let pending = str_global(code, p.t.str_, PENDING);
    let closes = str_global(code, p.t.str_, CLOSES);
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
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.jmp(
        Opcode::JSLte {
            a: c,
            b: zero,
            offset: 0,
        },
        "idle",
    );
    // A leave is pending: release this machine's own lock, never during a fade.
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.in_fade,
        arg0: game,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "host");
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "host");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "host");
    a.op(Opcode::Call1 {
        dst: w,
        fun: closable,
        arg0: game,
    });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "host");
    a.op(Opcode::GetGlobal {
        dst: last_w,
        global: g.closed,
    });
    a.jmp(
        Opcode::JEq {
            a: w,
            b: last_w,
            offset: 0,
        },
        "host",
    );
    a.op(Opcode::SetGlobal {
        global: g.closed,
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
        fun: why,
        arg0: Reg(0),
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
        fun: log,
        arg0: Reg(0),
        arg1: s_r,
        arg2: c,
    });
    a.label("go");
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.jmp(
        Opcode::JNotEq {
            a: c,
            b: zero,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::CallMethod {
        dst: b,
        field: p.v_try_close,
        args: vec![Reg(0)],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    // Nothing pending: forget the last reason and window.
    a.label("idle");
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::SetGlobal {
        global: g.last,
        src: zero,
    });
    a.op(Opcode::Null { dst: w });
    a.op(Opcode::SetGlobal {
        global: g.closed,
        src: w,
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

/// New functions, in the order `apply` appends them.
struct Added {
    why: RefFun,
    log: RefFun,
    refused: RefFun,
    closable: RefFun,
    update: RefFun,
    paused: RefFun,
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<Added> {
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let g = Globals {
        last: add_global(code, p.t.i32_),
        closed: add_global(code, p.win_t),
        paused_at: add_global(code, p.t.f64_),
    };
    let why = add_why(code, p)?;
    let log = add_log(code, p, dyn_t)?;
    let refused = add_refused(code, p, why, log)?;
    let closable = add_closable(code, p, dyn_t)?;
    let update = add_update(code, p, &g, why, log, closable)?;
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
    Ok(Added {
        why,
        log,
        refused,
        closable,
        update,
        paused,
    })
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
    let saved: Vec<Function> = [p.update_fi, p.try_close_fi, p.leave_fi, p.content_fi]
        .iter()
        .map(|&i| code.functions[i].clone())
        .collect();
    match apply(code, &p) {
        Ok(a) => eprintln!(
            "patched force leave: PlaceView.update -> fn@{} (why fn@{}, log fn@{}, windows fn@{}), tryClose -> fn@{}, leave -> fn@{}",
            a.update.0, a.why.0, a.log.0, a.closable.0, a.refused.0, a.paused.0
        ),
        Err(e) => {
            snap.restore(code);
            for (f, i) in saved
                .into_iter()
                .zip([p.update_fi, p.try_close_fi, p.leave_fi, p.content_fi])
            {
                code.functions[i] = f;
            }
            eprintln!("force leave skipped: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, shifted, write, HLBOOT};

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

        // Appended only: 6 functions (+ their types), 3 globals.
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 6);
        assert_eq!(&back.types[..orig.types.len()], &orig.types[..]);
        let ng = orig.globals.len();
        assert_eq!(&back.globals[..ng], &orig.globals[..]);
        assert_eq!(&back.globals[ng..ng + 3], &[p.t.i32_, p.win_t, p.t.f64_]);
        let touched = [p.update_fi, p.try_close_fi, p.leave_fi, p.content_fi];
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
            added.log,
            added.refused,
            added.closable,
            added.update,
            added.paused,
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
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, at, 1);
            assert!(
                matches!(b.ops[at], Opcode::Call1 { fun: f, arg0: Reg(0), .. } | Opcode::Call0 { fun: f, .. } if f == fun)
            );
            check_types(&back, b, at..at + 1);
        }
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

        // The whitelist is checked against window classes, the slots are Window's.
        assert_eq!(p.classes.len(), LOCK_WINDOWS.len());
        let gu = method(&orig, obj_type(&orig, "Game").unwrap(), "update").unwrap();
        for slot in [p.w_try_close, p.w_close] {
            assert!(gu
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::CallMethod { field, .. } if *field == slot)));
        }

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

    /// `CallMethod` receiver / slot / arguments / result agree with the method the
    /// slot holds in the receiver's class, and only tryClose / close are called.
    fn check_virtual_calls(code: &Bytecode, f: &Function) {
        for op in &f.ops {
            let Opcode::CallMethod { dst, field, args } = op else {
                continue;
            };
            let recv = f.regs[args[0].0 as usize];
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
                name == "tryClose" || name == "close",
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

    /// The whole pipeline (coop_gates G1/G2/G6 before, diag and the rest after):
    /// the pass still applies on top of G2/G6, and its hooks reach type-correct code.
    #[test]
    fn composes_with_other_passes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let out = crate::patch_image(&image).expect("patch_image");
        let b = read(&out);
        assert!(plan(&b).is_err(), "force leave was not applied");
        let callee = |fi: usize, at: usize| match b.functions[fi].ops[at] {
            Opcode::Call1 { fun, .. } | Opcode::Call0 { fun, .. } => fun,
            ref o => panic!("fn@{} op {at}: {o:?}", b.functions[fi].findex.0),
        };
        let by = |fun: RefFun| b.functions.iter().find(|g| g.findex == fun).expect("fn");
        // PlaceView.update and tryClose keep the hook at op 0 (diag does not touch them).
        let upd = by(callee(p.update_fi, 0));
        let refused = by(callee(p.try_close_fi, 0));
        let mut ours = vec![upd, refused];
        for g in [upd, refused] {
            for op in &g.ops {
                if let Opcode::Call1 { fun, .. }
                | Opcode::Call2 { fun, .. }
                | Opcode::Call3 { fun, .. } = op
                {
                    if fun.0 >= orig.functions.len() + orig.natives.len() {
                        ours.push(by(*fun));
                    }
                }
            }
        }
        assert!(
            ours.len() >= 5,
            "update/refused reach why, log and the window scan"
        );
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
