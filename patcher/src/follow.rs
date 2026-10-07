// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op auto-follow on the world map.
//
// Every co-op player drives a caravan of their own (a `BasePlayer`). A click on
// the ground sends `Controller.playerGoto(x, y, inertia)`, a client -> host RPC
// the host runs as `getActionPlayer().gotoPoint(x, y)`, then
// `me.resetSoftTarget(null)`, which on the host makes the clicking player
// `GameState.playerMovePriority` (synced: vanilla's "who leads" for its own
// regroup). The sprint key sends `Controller.playerSetShift(b)`, bit 8 of the
// player's synced `flags` (the host's `World.updateSprint` turns it into
// Run/Walk). In co-op an open window does not pause the world
// (`Window.pauseGame` needs a single player), so `World.update` keeps running
// while a character sheet or the inventory is open. This pass adds, on every
// machine, for its own caravan:
//
//   World.update, right before `updateSprint()`:        followUpdate(this, dt)
//   World.updateSprint, its `playerSetShift` send:       skipped while fActive
//   Controller.playerGoto (unless sent by follow):       followCancel(game, false)
//   Controller.playerGotoEntity (entity click):          followCancel(game, true)
//   BasePlayer.updateMoveHold (mouse hold / pad):        followCancel(game, false)
//   World.dispose (quit / load):                         followReset()
//
//   followUpdate(world, dt):
//     fIssuing = false;  now = sys_time();
//     inactive (fActive = false, fLeader = null) unless isMulti;
//     once per session: fLoaded = true; followLoad()   // fOff = Storage "mpFollowOff" != null
//     if (Key.isPressed(F) && sevents.getFocus() == null) {
//         fOff = !fOff; fManual = false; fNext = 0; followSave(fOff);
//         notify(fOff ? "Follow: OFF" : "Follow: ON");
//     }
//     host own move: host = state.player; if (host.target != null &&
//       (playerMovePriority == null || == host || now - fHostOwnAt <= OWN_HOLD))
//       fHostOwnAt = now;   // the host's own move, kept while its target lasts
//     gated (if fManual: fArrivedAt = now; then inactive) unless:
//       game.mode == world, game.battle == null, !cinematicMode,
//       state.currentCity == null, me.lockedWith, me.waitActionIcon,
//       me.scriptedMoveData, world.currentWindow all null, !me.onWater;
//     inactive while fOff (F: personal opt-out; ON by default, kept in Storage);
//     own manual move (fManual) pending:
//       me.target != null -> fSawTarget = true, fArrivedAt = 0, inactive;
//       !fSawTarget && now < fManualAt + MANUAL_HOLD -> inactive (client target
//         appears only after the host round trip);
//       fArrivedAt == 0 -> fArrivedAt = now;  now < fArrivedAt + IDLE -> inactive;
//       else fManual = false;
//     leader = state.playerMovePriority (the last player who moved by their own
//       input: the host sets it on a ground click, mouse hold, pad move and, via
//       followCancel, an entity click; a follow move never takes it), or
//       state.player (host) while it is null; but the host while the host's own
//       move is still going (now - fHostOwnAt <= OWN_HOLD: two players moving
//       by their own input -> the host leads); none if that is me (I lead) or
//       it is offline / hidden / on water. Never "whoever moves": that picked
//       other followers and chained the caravans (A -> B -> C);
//     fActive = true; fLeader = leader; at most every TICK s (sys_time):
//       want = leader.target != null && leader.flags & 8 != 0;  // a resting leader: walk
//       if (want != (me.flags & 8 != 0)) ctrl.playerSetShift(want);
//       d = |leader - me|; if (d > START) {
//         p = leader - (leader - me) * GAP / d;            // GAP short of the leader
//         if (!me.isPointReachable(p)) p = leader;         // water, wall: where the leader stands
//         unless (me.target != null && |p - me.target| < REISSUE):
//           fIssuing = true; ctrl.playerGoto(p.x, p.y, null); fIssuing = false;
//           me.resetSoftTarget(&true);   // like a click, without taking playerMovePriority
//       }
//   followCancel(game, claim): if (!fIssuing) { fManual = true; fManualAt = now;
//       fSawTarget = false; fArrivedAt = 0;   // pauses follow, F stays as it is
//       if (claim && me != null) me.resetSoftTarget(null); }  // entity click: take priority
//   followReset(): fManual = fIssuing = fActive = fSawTarget = fLoaded = false;
//       fArrivedAt = fHostOwnAt = 0; fLeader = null (fOff stays: a preference).
//   followLoad() / followSave(off): mpman.Storage get/setUserData("mpFollowOff",
//       off ? true : null), each under a trap (a throw keeps the current value).
//
// Only the RPCs a click and the sprint key already send are used: the host
// validates and moves the caravan as for a click. State lives in new globals
// (zero-initialised: fOff = false is ON), local to each machine. The toast is
// `GameUI.localNotify("ArenaNotif", {title: ...})` (`NotifyData.getTitle` returns
// `opts.title`; cdb ArenaNotif: log line, not in the journal). Hotkey F: no
// world-map binding uses it (cdb `input`); the only hard-coded uses are admin
// keys and the title screen.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefGlobal, RefString, ValBool};

pub(crate) const FOLLOW_ON: &str = "Follow: ON";
pub(crate) const FOLLOW_OFF: &str = "Follow: OFF";
const NOTIFY_ID: &str = "ArenaNotif";
const KEY_F: i32 = 70;
/// Seconds between two follow decisions (RPCs).
const TICK: f64 = 0.5;
/// Start moving when the leader is farther than this (world units; walk speed 6/s).
const START: f64 = 9.0;
/// Stop this far short of the leader.
const GAP: f64 = 5.0;
/// Do not re-send a goto whose point is this close to the current target.
const REISSUE: f64 = 1.5;
/// A manual move whose target never shows up (unreachable, refused, lost RPC)
/// counts as arrived after this long (its RPC round trip).
const MANUAL_HOLD: f64 = 1.5;
/// Follow resumes this long after an own manual move arrived.
const IDLE: f64 = 1.0;
/// The host's own move counts as ongoing while its target lasts, with gaps up to this long.
const OWN_HOLD: f64 = 1.0;
/// mpman.Storage key of the personal opt-out (present = OFF).
pub(crate) const PREF_KEY: &str = "mpFollowOff";
const SHIFT_BIT: i32 = 8;

/// The unique code function `name` with exactly this signature.
fn static_fn(code: &Bytecode, name: &str, args: &[RefType], ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == name
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == args && t.ret == ret)
        })
        .map(|f| f.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one {name}{args:?}, found {}", hits.len()),
    }
}

struct Types {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    dynobj: RefType,
    str_: RefType,
}

type F = (RefField, RefType);

struct Plan {
    t: Types,
    world_t: RefType,
    game_t: RefType,
    bp_t: RefType,
    /// World.update and its `updateSprint()` call op.
    update_fi: usize,
    update_at: usize,
    /// World.updateSprint and its `playerSetShift` send.
    sprint_fi: usize,
    sprint_at: usize,
    /// (function index, `game` field of arg 0, claims priority, name) of the cancel hooks.
    hooks: Vec<(usize, RefField, bool, &'static str)>,
    /// World.dispose (end of the world map: quit or load).
    dispose_fi: usize,
    dbg_file: usize,
    world_game: RefField,
    world_cur_win: F,
    game_me: RefField,
    game_state: F,
    game_ui: F,
    game_ctrl: F,
    game_sevents: F,
    game_mode: F,
    game_battle: F,
    state_player: F,
    state_priority: F,
    state_city: F,
    state_cine: RefField,
    bp_x: RefField,
    bp_y: RefField,
    bp_target: F,
    target_x: RefField,
    target_y: RefField,
    bp_flags: F,
    flags_value: RefField,
    bp_connected: RefField,
    bp_locked: F,
    bp_wait: F,
    bp_scripted: F,
    pt_t: RefType,
    pt_x: RefField,
    pt_y: RefField,
    key_pressed: RefFun,
    get_focus: (RefFun, RefType),
    is_multi: RefFun,
    get_ud: RefFun,
    set_ud: RefFun,
    dyn_t: RefType,
    local_notify: RefFun,
    notify_opts_t: RefType,
    on_water: RefFun,
    is_visible: RefFun,
    reachable: RefFun,
    reset_soft: (RefFun, RefType),
    set_shift: RefFun,
    player_goto: RefFun,
    goto_inertia_t: RefType,
    sys_time: RefFun,
    sqrt: RefFun,
    title_s: RefString,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let t = Types {
        void: prim("void", |t| matches!(t, Type::Void))?,
        bool_: prim("bool", |t| matches!(t, Type::Bool))?,
        i32_: prim("i32", |t| matches!(t, Type::I32))?,
        f64_: prim("f64", |t| matches!(t, Type::F64))?,
        dynobj: prim("dynobj", |t| matches!(t, Type::DynObj))?,
        str_: obj_type(code, "String")?,
    };
    if code.functions.iter().any(|f| {
        f.ops.iter().any(|op| {
            matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(FOLLOW_ON))
        })
    }) {
        bail!("already applied");
    }
    let world_t = obj_type(code, "world.World")?;
    let game_t = obj_type(code, "Game")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let ui_t = obj_type(code, "ui.GameUI")?;
    let typed = |o: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, o, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };

    let world_game = typed(world_t, "game", game_t)?;
    let world_cur_win = field(code, world_t, "currentWindow")?;
    let game_me = typed(game_t, "me", bp_t)?;
    let game_state = field(code, game_t, "state")?;
    let game_ui = field(code, game_t, "ui")?;
    let game_ctrl = field(code, game_t, "ctrl")?;
    let game_sevents = field(code, game_t, "sevents")?;
    let game_mode = field(code, game_t, "mode")?;
    let game_battle = field(code, game_t, "battle")?;
    if game_ui.1 != ui_t || game_ctrl.1 != ctrl_t {
        bail!("Game.ui / Game.ctrl have unexpected types");
    }
    if !is_sub(code, world_t, game_mode.1) {
        bail!("World is not a Game.mode type");
    }
    let state_t = game_state.1;
    let state_player = field(code, state_t, "player")?;
    let state_priority = field(code, state_t, "playerMovePriority")?;
    let state_city = field(code, state_t, "currentCity")?;
    let state_cine = typed(state_t, "cinematicMode", t.bool_)?;
    for (what, ft) in [
        ("player", state_player.1),
        ("playerMovePriority", state_priority.1),
    ] {
        if !is_sub(code, ft, bp_t) {
            bail!("GameState.{what} is not a BasePlayer");
        }
    }

    let bp_x = typed(bp_t, "x", t.f64_)?;
    let bp_y = typed(bp_t, "y", t.f64_)?;
    let bp_target = field(code, bp_t, "target")?;
    let target_x = typed(bp_target.1, "x", t.f64_)?;
    let target_y = typed(bp_target.1, "y", t.f64_)?;
    let bp_flags = field(code, bp_t, "flags")?;
    let flags_value = typed(bp_flags.1, "value", t.i32_)?;
    let bp_connected = typed(bp_t, "connected", t.bool_)?;
    let bp_locked = field(code, bp_t, "lockedWith")?;
    let bp_wait = field(code, bp_t, "waitActionIcon")?;
    let bp_scripted = field(code, bp_t, "scriptedMoveData")?;
    let bp_game = typed(bp_t, "game", game_t)?;
    let ctrl_game = typed(ctrl_t, "game", game_t)?;
    let pt_t = obj_type(code, "h2d.col.PointImpl")?;
    let pt_x = typed(pt_t, "x", t.f64_)?;
    let pt_y = typed(pt_t, "y", t.f64_)?;

    let key_pressed = static_fn(code, "isPressed", &[t.i32_], t.bool_)?;
    let gf = method(code, game_sevents.1, "getFocus")?;
    let get_focus = (gf.findex, gf.t.as_fun(code).context("getFocus type")?.ret);
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], t.bool_) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let get_ud = crate::diag::static_fn(code, "mpman.$Storage", "getUserData")?.findex;
    if sig(code, get_ud)? != (vec![t.str_, dyn_t], dyn_t) {
        bail!("unexpected Storage.getUserData signature");
    }
    let set_ud = crate::diag::static_fn(code, "mpman.$Storage", "setUserData")?.findex;
    if sig(code, set_ud)? != (vec![t.str_, dyn_t], t.void) {
        bail!("unexpected Storage.setUserData signature");
    }
    let local_notify = method(code, ui_t, "localNotify")?.findex;
    let (ln_args, _) = sig(code, local_notify)?;
    if ln_args.len() != 3 || ln_args[1] != t.str_ {
        bail!("unexpected GameUI.localNotify signature");
    }
    let notify_opts_t = ln_args[2];
    let has_title = match &code.types[notify_opts_t.0] {
        Type::Virtual { fields } => fields
            .iter()
            .any(|f| s(code, f.name) == "title" && f.t == t.str_),
        _ => false,
    };
    if !has_title {
        bail!("localNotify options have no String title");
    }
    let title_s = string_index(code, "title")?;
    let on_water = method(code, bp_t, "get_onWater")?.findex;
    let is_visible = method(code, bp_t, "isVisible")?.findex;
    for f in [on_water, is_visible] {
        if sig(code, f)? != (vec![bp_t], t.bool_) {
            bail!("unexpected BasePlayer fn@{} signature", f.0);
        }
    }
    let reachable = method(code, bp_t, "isPointReachable")?.findex;
    if sig(code, reachable)? != (vec![bp_t, pt_t], t.bool_) {
        bail!("unexpected BasePlayer.isPointReachable signature");
    }
    let reset_soft = method(code, bp_t, "resetSoftTarget")?.findex;
    let (rs_args, _) = sig(code, reset_soft)?;
    let ref_bool = match rs_args[..] {
        [a, r] if a == bp_t && matches!(code.types[r.0], Type::Ref(b) if b == t.bool_) => r,
        _ => bail!("unexpected BasePlayer.resetSoftTarget signature"),
    };
    let set_shift = method(code, ctrl_t, "playerSetShift")?.findex;
    if sig(code, set_shift)?.0 != [ctrl_t, t.bool_] {
        bail!("unexpected Controller.playerSetShift signature");
    }
    let player_goto = method(code, ctrl_t, "playerGoto")?.findex;
    let (pg_args, _) = sig(code, player_goto)?;
    if pg_args.len() != 4
        || pg_args[1] != t.f64_
        || pg_args[2] != t.f64_
        || !matches!(code.types[pg_args[3].0], Type::Null(b) if b == t.bool_)
    {
        bail!("unexpected Controller.playerGoto signature");
    }
    let goto_inertia_t = pg_args[3];
    let goto_entity = method(code, ctrl_t, "playerGotoEntity")?.findex;
    let move_hold = method(code, bp_t, "updateMoveHold")?.findex;
    let sys_time = native(code, "sys_time", &[], t.f64_)?;
    let sqrt = native(code, "math_sqrt", &[t.f64_], t.f64_)?;

    // World.update: the single `updateSprint()` call.
    let update = method(code, world_t, "update")?;
    if fun_args(code, update) != [world_t, t.f64_] {
        bail!("unexpected World.update signature");
    }
    let update_fi = fun_index(code, update.findex)?;
    let upd_sprint = method(code, world_t, "updateSprint")?.findex;
    let sites: Vec<usize> = (0..update.ops.len())
        .filter(|&i| {
            matches!(update.ops[i], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == upd_sprint)
        })
        .collect();
    let [update_at] = sites[..] else {
        bail!(
            "World.update: expected one updateSprint() call, found {}",
            sites.len()
        );
    };
    if (0..update.ops.len()).any(|i| jump_targets(update, i).contains(&update_at)) {
        bail!("World.update: a jump targets the updateSprint() call");
    }

    // World.updateSprint: the one `ctrl.playerSetShift(shouldSprint)` send.
    let sprint_fi = fun_index(code, upd_sprint)?;
    let us = &code.functions[sprint_fi];
    let sends: Vec<usize> = (0..us.ops.len())
        .filter(|&i| matches!(us.ops[i], Opcode::Call2 { fun, .. } if fun == set_shift))
        .collect();
    let [sprint_at] = sends[..] else {
        bail!(
            "World.updateSprint: expected one playerSetShift send, found {}",
            sends.len()
        );
    };
    if (0..us.ops.len()).any(|i| jump_targets(us, i).contains(&sprint_at)) {
        bail!("World.updateSprint: a jump targets the playerSetShift send");
    }

    let mut hooks = vec![];
    // A ground click (World.onEvent) and a hold / pad move (playerUpdateTarget)
    // take playerMovePriority themselves; an entity click does not.
    for (f, game_f, claim, what) in [
        (player_goto, ctrl_game, false, "Controller.playerGoto"),
        (goto_entity, ctrl_game, true, "Controller.playerGotoEntity"),
        (move_hold, bp_game, false, "BasePlayer.updateMoveHold"),
    ] {
        let fi = fun_index(code, f)?;
        let g = &code.functions[fi];
        if (0..g.ops.len()).any(|i| jump_targets(g, i).contains(&0)) {
            bail!("{what}: a jump targets op 0");
        }
        hooks.push((fi, game_f, claim, what));
    }
    let disp = method(code, world_t, "dispose")?;
    if fun_args(code, disp) != [world_t] {
        bail!("unexpected World.dispose signature");
    }
    let dispose_fi = fun_index(code, disp.findex)?;
    if (0..disp.ops.len()).any(|i| jump_targets(disp, i).contains(&0)) {
        bail!("World.dispose: a jump targets op 0");
    }
    let dbg_file = debug_file(code, "src/world/World.hx")?;

    Ok(Plan {
        t,
        world_t,
        game_t,
        bp_t,
        update_fi,
        update_at,
        sprint_fi,
        sprint_at,
        hooks,
        dispose_fi,
        dbg_file,
        world_game,
        world_cur_win,
        game_me,
        game_state,
        game_ui,
        game_ctrl,
        game_sevents,
        game_mode,
        game_battle,
        state_player,
        state_priority,
        state_city,
        state_cine,
        bp_x,
        bp_y,
        bp_target,
        target_x,
        target_y,
        bp_flags,
        flags_value,
        bp_connected,
        bp_locked,
        bp_wait,
        bp_scripted,
        pt_t,
        pt_x,
        pt_y,
        key_pressed,
        get_focus,
        is_multi,
        get_ud,
        set_ud,
        dyn_t,
        local_notify,
        notify_opts_t,
        on_water,
        is_visible,
        reachable,
        reset_soft: (reset_soft, ref_bool),
        set_shift,
        player_goto,
        goto_inertia_t,
        sys_time,
        sqrt,
        title_s,
    })
}

struct Globals {
    off: RefGlobal,
    manual: RefGlobal,
    manual_at: RefGlobal,
    issuing: RefGlobal,
    active: RefGlobal,
    next: RefGlobal,
    leader: RefGlobal,
    arrived_at: RefGlobal,
    saw_target: RefGlobal,
    loaded: RefGlobal,
    host_own_at: RefGlobal,
}

/// `notify(game, on)`: `game.ui.localNotify("ArenaNotif", {title: on ? ON : OFF})`.
fn add_notify(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let id = str_global(code, p.t.str_, NOTIFY_ID);
    let on = str_global(code, p.t.str_, FOLLOW_ON);
    let off = str_global(code, p.t.str_, FOLLOW_OFF);
    let mut r = Regs(vec![p.game_t, p.t.bool_]);
    let (ui, sid, text, o, opts, v) = (
        r.r(p.game_ui.1),
        r.r(p.t.str_),
        r.r(p.t.str_),
        r.r(p.t.dynobj),
        r.r(p.notify_opts_t),
        r.r(p.t.void),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: ui,
        obj: Reg(0),
        field: p.game_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: sid,
        global: id,
    });
    a.jmp(
        Opcode::JFalse {
            cond: Reg(1),
            offset: 0,
        },
        "off",
    );
    a.op(Opcode::GetGlobal {
        dst: text,
        global: on,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "go");
    a.label("off");
    a.op(Opcode::GetGlobal {
        dst: text,
        global: off,
    });
    a.label("go");
    a.op(Opcode::New { dst: o });
    a.op(Opcode::DynSet {
        obj: o,
        field: p.title_s,
        src: text,
    });
    a.op(Opcode::ToVirtual { dst: opts, src: o });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.local_notify,
        arg0: ui,
        arg1: sid,
        arg2: opts,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.t.bool_],
        p.t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `followCancel(game, claim)`: a manual move by this machine's player pauses
/// follow (F stays as it is). `claim`: the move does not take
/// `playerMovePriority` by itself (entity click), so take it here with
/// `me.resetSoftTarget(null)`, as a ground click does.
fn add_cancel(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let f0 = float_const(code, 0.0);
    let mut r = Regs(vec![p.game_t, p.t.bool_]);
    let (b, now, v, me, rb) = (
        r.r(p.t.bool_),
        r.r(p.t.f64_),
        r.r(p.t.void),
        r.r(p.bp_t),
        r.r(p.reset_soft.1),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.issuing,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.manual,
        src: b,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.saw_target,
        src: b,
    });
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::SetGlobal {
        global: g.manual_at,
        src: now,
    });
    a.op(Opcode::Float { dst: now, ptr: f0 });
    a.op(Opcode::SetGlobal {
        global: g.arrived_at,
        src: now,
    });
    a.jmp(
        Opcode::JFalse {
            cond: Reg(1),
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: Reg(0),
        field: p.game_me,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "end");
    a.op(Opcode::Null { dst: rb });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.reset_soft.0,
        arg0: me,
        arg1: rb,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.t.bool_],
        p.t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `followLoad()`: `fOff = Storage.getUserData(PREF_KEY, null) != null`; a throw keeps fOff.
fn add_load(code: &mut Bytecode, p: &Plan, g: &Globals, key: RefGlobal) -> Result<RefFun> {
    let mut r = Regs(vec![]);
    let (v, exc, k, nd, d, b) = (
        r.r(p.t.void),
        r.r(p.dyn_t),
        r.r(p.t.str_),
        r.r(p.dyn_t),
        r.r(p.dyn_t),
        r.r(p.t.bool_),
    );
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetGlobal {
        dst: k,
        global: key,
    });
    a.op(Opcode::Null { dst: nd });
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.get_ud,
        arg0: k,
        arg1: nd,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "set");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.label("set");
    a.op(Opcode::SetGlobal {
        global: g.off,
        src: b,
    });
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![], p.t.void, r.0, a.finish(), p.dbg_file)
}

/// `followSave(off)`: `Storage.setUserData(PREF_KEY, off ? true : null)`; a throw is dropped.
fn add_save(code: &mut Bytecode, p: &Plan, key: RefGlobal) -> Result<RefFun> {
    let mut r = Regs(vec![p.t.bool_]);
    let (v, exc, k, d) = (r.r(p.t.void), r.r(p.dyn_t), r.r(p.t.str_), r.r(p.dyn_t));
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetGlobal {
        dst: k,
        global: key,
    });
    a.op(Opcode::Null { dst: d });
    a.jmp(
        Opcode::JFalse {
            cond: Reg(0),
            offset: 0,
        },
        "go",
    );
    a.op(Opcode::ToDyn {
        dst: d,
        src: Reg(0),
    });
    a.label("go");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_ud,
        arg0: k,
        arg1: d,
    });
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.t.bool_], p.t.void, r.0, a.finish(), p.dbg_file)
}

/// `followUpdate(world, dt)`, see the module comment.
fn add_update(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    notify: RefFun,
    load: RefFun,
    save: RefFun,
) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let ikey = int_const(code, KEY_F);
    let imask = int_const(code, SHIFT_BIT);
    let f0 = float_const(code, 0.0);
    let ftick = float_const(code, TICK);
    let fhold = float_const(code, MANUAL_HOLD);
    let fidle = float_const(code, IDLE);
    let fown = float_const(code, OWN_HOLD);
    let fstart2 = float_const(code, START * START);
    let fgap = float_const(code, GAP);
    let freissue2 = float_const(code, REISSUE * REISSUE);
    let t = &p.t;
    let mut r = Regs(vec![p.world_t, t.f64_]);
    let (game, me, st, b, v) = (
        r.r(p.game_t),
        r.r(p.bp_t),
        r.r(p.game_state.1),
        r.r(t.bool_),
        r.r(t.void),
    );
    let (k, i, n, ki) = (r.r(t.i32_), r.r(t.i32_), r.r(t.i32_), r.r(t.i32_));
    let (sev, focus, mode, battle, city) = (
        r.r(p.game_sevents.1),
        r.r(p.get_focus.1),
        r.r(p.game_mode.1),
        r.r(p.game_battle.1),
        r.r(p.state_city.1),
    );
    let (tgt, lw, wait, cw, smd) = (
        r.r(p.bp_target.1),
        r.r(p.bp_locked.1),
        r.r(p.bp_wait.1),
        r.r(p.world_cur_win.1),
        r.r(p.bp_scripted.1),
    );
    let best = r.r(p.bp_t);
    let (host, prio) = (r.r(p.state_player.1), r.r(p.state_priority.1));
    let (ctrl, flags, want, mine, inertia, pt, rb) = (
        r.r(p.game_ctrl.1),
        r.r(p.bp_flags.1),
        r.r(t.bool_),
        r.r(t.bool_),
        r.r(p.goto_inertia_t),
        r.r(p.pt_t),
        r.r(p.reset_soft.1),
    );
    let mut fr = || r.r(t.f64_);
    let (fa, fb, now, mx, my, lx, ly, dx, dy, d2, dist, kf, tx, ty) = (
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
        fr(),
    );

    let mut a = Asm::new();
    // fIssuing = false (a throw inside an earlier playerGoto must not leave it set)
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.issuing,
        src: b,
    });
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(0),
        field: p.world_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.game_me,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "inactive");
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.game_state.0,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "inactive");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "inactive");

    // ---- the saved opt-out, once per session
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.loaded,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "loaded");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.loaded,
        src: b,
    });
    a.op(Opcode::Call0 { dst: v, fun: load });
    a.label("loaded");

    // ---- hotkey F: personal opt-out
    a.op(Opcode::Int { dst: ki, ptr: ikey });
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.key_pressed,
        arg0: ki,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "afterkey");
    a.op(Opcode::Field {
        dst: sev,
        obj: game,
        field: p.game_sevents.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: sev,
            offset: 0,
        },
        "afterkey",
    );
    a.op(Opcode::Call1 {
        dst: focus,
        fun: p.get_focus.0,
        arg0: sev,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: focus,
            offset: 0,
        },
        "afterkey",
    );
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.off,
    });
    a.op(Opcode::Not { dst: b, src: b });
    a.op(Opcode::SetGlobal {
        global: g.off,
        src: b,
    });
    a.op(Opcode::Bool {
        dst: want,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.manual,
        src: want,
    });
    a.op(Opcode::Float { dst: fa, ptr: f0 });
    a.op(Opcode::SetGlobal {
        global: g.next,
        src: fa,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: save,
        arg0: b,
    });
    a.op(Opcode::Not { dst: want, src: b });
    a.op(Opcode::Call2 {
        dst: v,
        fun: notify,
        arg0: game,
        arg1: want,
    });
    a.label("afterkey");

    // ---- the host's own move: started while it held playerMovePriority (or
    // nobody did), kept while its target lasts (gaps up to OWN_HOLD)
    a.op(Opcode::Field {
        dst: host,
        obj: st,
        field: p.state_player.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "tracked",
    );
    a.op(Opcode::Field {
        dst: tgt,
        obj: host,
        field: p.bp_target.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "tracked",
    );
    a.op(Opcode::Field {
        dst: prio,
        obj: st,
        field: p.state_priority.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: prio,
            offset: 0,
        },
        "own",
    );
    a.jmp(
        Opcode::JEq {
            a: prio,
            b: host,
            offset: 0,
        },
        "own",
    );
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: g.host_own_at,
    });
    a.op(Opcode::Sub {
        dst: fa,
        a: now,
        b: fa,
    });
    a.op(Opcode::Float { dst: fb, ptr: fown });
    a.jmp(
        Opcode::JSGt {
            a: fa,
            b: fb,
            offset: 0,
        },
        "tracked",
    );
    a.label("own");
    a.op(Opcode::SetGlobal {
        global: g.host_own_at,
        src: now,
    });
    a.label("tracked");

    // ---- gates (a pending manual move restarts its idle time while gated)
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.game_mode.0,
    });
    a.jmp(
        Opcode::JNotEq {
            a: mode,
            b: Reg(0),
            offset: 0,
        },
        "gated",
    );
    a.op(Opcode::Field {
        dst: battle,
        obj: game,
        field: p.game_battle.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: battle,
            offset: 0,
        },
        "gated",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: st,
        field: p.state_cine,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "gated");
    a.op(Opcode::Field {
        dst: city,
        obj: st,
        field: p.state_city.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: city,
            offset: 0,
        },
        "gated",
    );
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNotNull { reg: lw, offset: 0 }, "gated");
    a.op(Opcode::Field {
        dst: wait,
        obj: me,
        field: p.bp_wait.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: wait,
            offset: 0,
        },
        "gated",
    );
    a.op(Opcode::Field {
        dst: cw,
        obj: Reg(0),
        field: p.world_cur_win.0,
    });
    a.jmp(Opcode::JNotNull { reg: cw, offset: 0 }, "gated");
    a.op(Opcode::Field {
        dst: smd,
        obj: me,
        field: p.bp_scripted.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: smd,
            offset: 0,
        },
        "gated",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.on_water,
        arg0: me,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "gated");
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.off,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "inactive");

    // ---- own manual move: paused until it arrived and IDLE s passed
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.manual,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "nomanual");
    a.op(Opcode::Field {
        dst: tgt,
        obj: me,
        field: p.bp_target.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "stopped",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.saw_target,
        src: b,
    });
    a.op(Opcode::Float { dst: fa, ptr: f0 });
    a.op(Opcode::SetGlobal {
        global: g.arrived_at,
        src: fa,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "inactive");
    a.label("stopped");
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.saw_target,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "arrived");
    // no target yet: the host round trip, or the click was not walkable
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: g.manual_at,
    });
    a.op(Opcode::Float {
        dst: fb,
        ptr: fhold,
    });
    a.op(Opcode::Add {
        dst: fa,
        a: fa,
        b: fb,
    });
    a.jmp(
        Opcode::JSLt {
            a: now,
            b: fa,
            offset: 0,
        },
        "inactive",
    );
    a.label("arrived");
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: g.arrived_at,
    });
    a.op(Opcode::Float { dst: fb, ptr: f0 });
    a.jmp(
        Opcode::JSGt {
            a: fa,
            b: fb,
            offset: 0,
        },
        "idle",
    );
    a.op(Opcode::SetGlobal {
        global: g.arrived_at,
        src: now,
    });
    a.op(Opcode::Mov { dst: fa, src: now });
    a.label("idle");
    a.op(Opcode::Float {
        dst: fb,
        ptr: fidle,
    });
    a.op(Opcode::Add {
        dst: fa,
        a: fa,
        b: fb,
    });
    a.jmp(
        Opcode::JSLt {
            a: now,
            b: fa,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.manual,
        src: b,
    });
    a.label("nomanual");

    // ---- leader: the player who last moved by their own input
    // (GameState.playerMovePriority: the host sets it on a click, mouse hold,
    // pad move or, via followCancel, an entity click; a follow move never
    // takes it), or the host before anyone has moved; the host instead while
    // the host's own move is still going (several players moving: the host
    // leads). Ourselves (we lead), offline, hidden or on water: stay put.
    // Never "whoever is moving": that picked other followers and chained the
    // caravans.
    a.op(Opcode::Field {
        dst: best,
        obj: st,
        field: p.state_priority.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: best,
            offset: 0,
        },
        "haslead",
    );
    a.op(Opcode::Field {
        dst: best,
        obj: st,
        field: p.state_player.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: best,
            offset: 0,
        },
        "inactive",
    );
    a.label("haslead");
    a.op(Opcode::Field {
        dst: host,
        obj: st,
        field: p.state_player.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "chosen",
    );
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: g.host_own_at,
    });
    a.op(Opcode::Sub {
        dst: fa,
        a: now,
        b: fa,
    });
    a.op(Opcode::Float { dst: fb, ptr: fown });
    a.jmp(
        Opcode::JSGt {
            a: fa,
            b: fb,
            offset: 0,
        },
        "chosen",
    );
    a.op(Opcode::Mov {
        dst: best,
        src: host,
    });
    a.label("chosen");
    a.jmp(
        Opcode::JEq {
            a: best,
            b: me,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: best,
        field: p.bp_connected,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "inactive");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_visible,
        arg0: best,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "inactive");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.on_water,
        arg0: best,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "inactive");
    a.op(Opcode::SetGlobal {
        global: g.leader,
        src: best,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.active,
        src: b,
    });

    // ---- throttle
    a.op(Opcode::GetGlobal {
        dst: fa,
        global: g.next,
    });
    a.jmp(
        Opcode::JSLt {
            a: now,
            b: fa,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Float {
        dst: fb,
        ptr: ftick,
    });
    a.op(Opcode::Add {
        dst: fa,
        a: now,
        b: fb,
    });
    a.op(Opcode::SetGlobal {
        global: g.next,
        src: fa,
    });
    a.op(Opcode::Field {
        dst: ctrl,
        obj: game,
        field: p.game_ctrl.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "ret",
    );

    // ---- run mirror (only a moving leader's sprint: two players following
    // each other drop a copied sprint bit once they have met)
    a.op(Opcode::Int { dst: k, ptr: imask });
    a.op(Opcode::Int { dst: n, ptr: i0 });
    a.op(Opcode::Bool {
        dst: want,
        value: ValBool(false),
    });
    a.op(Opcode::Field {
        dst: tgt,
        obj: best,
        field: p.bp_target.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "wantset",
    );
    a.op(Opcode::Field {
        dst: flags,
        obj: best,
        field: p.bp_flags.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: flags,
            offset: 0,
        },
        "noshift",
    );
    a.op(Opcode::Field {
        dst: i,
        obj: flags,
        field: p.flags_value,
    });
    a.op(Opcode::And { dst: i, a: i, b: k });
    a.op(Opcode::Bool {
        dst: want,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JEq {
            a: i,
            b: n,
            offset: 0,
        },
        "wantset",
    );
    a.op(Opcode::Bool {
        dst: want,
        value: ValBool(true),
    });
    a.label("wantset");
    a.op(Opcode::Field {
        dst: flags,
        obj: me,
        field: p.bp_flags.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: flags,
            offset: 0,
        },
        "noshift",
    );
    a.op(Opcode::Field {
        dst: i,
        obj: flags,
        field: p.flags_value,
    });
    a.op(Opcode::And { dst: i, a: i, b: k });
    a.op(Opcode::Bool {
        dst: mine,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JEq {
            a: i,
            b: n,
            offset: 0,
        },
        "mineset",
    );
    a.op(Opcode::Bool {
        dst: mine,
        value: ValBool(true),
    });
    a.label("mineset");
    a.jmp(
        Opcode::JEq {
            a: want,
            b: mine,
            offset: 0,
        },
        "noshift",
    );
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_shift,
        arg0: ctrl,
        arg1: want,
    });
    a.label("noshift");

    // ---- distance, target point
    a.op(Opcode::Field {
        dst: mx,
        obj: me,
        field: p.bp_x,
    });
    a.op(Opcode::Field {
        dst: my,
        obj: me,
        field: p.bp_y,
    });
    a.op(Opcode::Field {
        dst: lx,
        obj: best,
        field: p.bp_x,
    });
    a.op(Opcode::Field {
        dst: ly,
        obj: best,
        field: p.bp_y,
    });
    a.op(Opcode::Sub {
        dst: dx,
        a: lx,
        b: mx,
    });
    a.op(Opcode::Sub {
        dst: dy,
        a: ly,
        b: my,
    });
    a.op(Opcode::Mul {
        dst: d2,
        a: dx,
        b: dx,
    });
    a.op(Opcode::Mul {
        dst: fa,
        a: dy,
        b: dy,
    });
    a.op(Opcode::Add {
        dst: d2,
        a: d2,
        b: fa,
    });
    a.op(Opcode::Float {
        dst: fb,
        ptr: fstart2,
    });
    a.jmp(
        Opcode::JSLte {
            a: d2,
            b: fb,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call1 {
        dst: dist,
        fun: p.sqrt,
        arg0: d2,
    });
    a.op(Opcode::Float { dst: fb, ptr: fgap });
    a.op(Opcode::SDiv {
        dst: kf,
        a: fb,
        b: dist,
    });
    a.op(Opcode::Mul {
        dst: fa,
        a: dx,
        b: kf,
    });
    a.op(Opcode::Sub {
        dst: tx,
        a: lx,
        b: fa,
    });
    a.op(Opcode::Mul {
        dst: fa,
        a: dy,
        b: kf,
    });
    a.op(Opcode::Sub {
        dst: ty,
        a: ly,
        b: fa,
    });
    a.op(Opcode::New { dst: pt });
    a.op(Opcode::SetField {
        obj: pt,
        field: p.pt_x,
        src: tx,
    });
    a.op(Opcode::SetField {
        obj: pt,
        field: p.pt_y,
        src: ty,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.reachable,
        arg0: me,
        arg1: pt,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "reachable");
    a.op(Opcode::Mov { dst: tx, src: lx });
    a.op(Opcode::Mov { dst: ty, src: ly });
    a.label("reachable");
    // still walking to (nearly) the same point: leave it
    a.op(Opcode::Field {
        dst: tgt,
        obj: me,
        field: p.bp_target.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "issue",
    );
    a.op(Opcode::Field {
        dst: fa,
        obj: tgt,
        field: p.target_x,
    });
    a.op(Opcode::Sub {
        dst: fa,
        a: tx,
        b: fa,
    });
    a.op(Opcode::Mul {
        dst: fa,
        a: fa,
        b: fa,
    });
    a.op(Opcode::Field {
        dst: fb,
        obj: tgt,
        field: p.target_y,
    });
    a.op(Opcode::Sub {
        dst: fb,
        a: ty,
        b: fb,
    });
    a.op(Opcode::Mul {
        dst: fb,
        a: fb,
        b: fb,
    });
    a.op(Opcode::Add {
        dst: fa,
        a: fa,
        b: fb,
    });
    a.op(Opcode::Float {
        dst: fb,
        ptr: freissue2,
    });
    a.jmp(
        Opcode::JSLt {
            a: fa,
            b: fb,
            offset: 0,
        },
        "ret",
    );
    a.label("issue");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.issuing,
        src: b,
    });
    a.op(Opcode::Null { dst: inertia });
    a.op(Opcode::Call4 {
        dst: v,
        fun: p.player_goto,
        arg0: ctrl,
        arg1: tx,
        arg2: ty,
        arg3: inertia,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.issuing,
        src: b,
    });
    a.op(Opcode::Bool {
        dst: want,
        value: ValBool(true),
    });
    a.op(Opcode::Ref { dst: rb, src: want });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.reset_soft.0,
        arg0: me,
        arg1: rb,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    // a gate while an own manual move is pending: its idle time restarts
    // when the gate clears (dialog, trade, POI after an entity click)
    a.label("gated");
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.manual,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "inactive");
    a.op(Opcode::SetGlobal {
        global: g.arrived_at,
        src: now,
    });
    a.label("inactive");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.active,
        src: b,
    });
    a.op(Opcode::Null { dst: best });
    a.op(Opcode::SetGlobal {
        global: g.leader,
        src: best,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.world_t, t.f64_],
        p.t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `followReset()`: World.dispose (quit to the menu or load) forgets the session.
/// The leader is an object of that session and must not outlive it. fOff is a
/// preference and stays; fLoaded = false re-reads it next session.
fn add_reset(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let f0 = float_const(code, 0.0);
    let mut r = Regs(vec![]);
    let (b, f, pl, v) = (r.r(p.t.bool_), r.r(p.t.f64_), r.r(p.bp_t), r.r(p.t.void));
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    for gl in [g.manual, g.issuing, g.active, g.saw_target, g.loaded] {
        a.op(Opcode::SetGlobal { global: gl, src: b });
    }
    a.op(Opcode::Float { dst: f, ptr: f0 });
    for gl in [g.arrived_at, g.host_own_at] {
        a.op(Opcode::SetGlobal { global: gl, src: f });
    }
    a.op(Opcode::Null { dst: pl });
    a.op(Opcode::SetGlobal {
        global: g.leader,
        src: pl,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![], p.t.void, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let (bool_t, f64_t) = (p.t.bool_, p.t.f64_);
    let g = Globals {
        off: add_global(code, bool_t),
        manual: add_global(code, bool_t),
        manual_at: add_global(code, f64_t),
        issuing: add_global(code, bool_t),
        active: add_global(code, bool_t),
        next: add_global(code, f64_t),
        leader: add_global(code, p.bp_t),
        arrived_at: add_global(code, f64_t),
        saw_target: add_global(code, bool_t),
        loaded: add_global(code, bool_t),
        host_own_at: add_global(code, f64_t),
    };
    let notify = add_notify(code, &p)?;
    let key = str_global(code, p.t.str_, PREF_KEY);
    let load = add_load(code, &p, &g, key)?;
    let save = add_save(code, &p, key)?;
    let cancel = add_cancel(code, &p, &g)?;
    let update = add_update(code, &p, &g, notify, load, save)?;
    let reset = add_reset(code, &p, &g)?;

    // World.dispose: forget the session (toggle, leader object).
    let f = &mut code.functions[p.dispose_fi];
    f.regs.push(p.t.void);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(f, 0, vec![Opcode::Call0 { dst: v, fun: reset }]);
    eprintln!(
        "patched follow fn@{}: World.dispose resets follow",
        f.findex.0
    );

    // World.update: followUpdate(this, dt) right before updateSprint().
    let f = &mut code.functions[p.update_fi];
    f.regs.push(p.t.void);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        p.update_at,
        vec![Opcode::Call2 {
            dst: v,
            fun: update,
            arg0: Reg(0),
            arg1: Reg(1),
        }],
    );
    eprintln!(
        "patched follow fn@{}: followUpdate fn@{} before updateSprint at op {}",
        f.findex.0, update.0, p.update_at
    );

    // World.updateSprint: no keys -> shift send while following.
    let f = &mut code.functions[p.sprint_fi];
    f.regs.push(bool_t);
    let b = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        p.sprint_at,
        vec![
            Opcode::GetGlobal {
                dst: b,
                global: g.active,
            },
            Opcode::JTrue { cond: b, offset: 1 }, // over the send
        ],
    );
    eprintln!(
        "patched follow fn@{}: playerSetShift send at op {} suppressed while following",
        f.findex.0,
        p.sprint_at + 2
    );

    // Manual moves cancel follow.
    for (fi, game_f, claim, what) in &p.hooks {
        let f = &mut code.functions[*fi];
        f.regs.push(p.game_t);
        let gr = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.t.bool_);
        let cb = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.t.void);
        let v = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            0,
            vec![
                Opcode::Field {
                    dst: gr,
                    obj: Reg(0),
                    field: *game_f,
                },
                Opcode::JNull { reg: gr, offset: 2 },
                Opcode::Bool {
                    dst: cb,
                    value: ValBool(*claim),
                },
                Opcode::Call2 {
                    dst: v,
                    fun: cancel,
                    arg0: gr,
                    arg1: cb,
                },
            ],
        );
        eprintln!(
            "patched follow fn@{}: {what} pauses follow{}",
            f.findex.0,
            if *claim { ", takes move priority" } else { "" }
        );
    }
    eprintln!(
        "follow: notify fn@{}, load fn@{}, save fn@{}, cancel fn@{}, update fn@{}, reset fn@{}; on by default, F opts out ({PREF_KEY}), start {START}, gap {GAP}, tick {TICK}s, idle {IDLE}s, host own move {OWN_HOLD}s",
        notify.0, load.0, save.0, cancel.0, update.0, reset.0
    );
    Ok(())
}

/// Adds co-op auto-follow, or leaves `code` untouched and logs why.
pub(crate) fn patch_follow(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("follow skipped: {e:#}");
            return;
        }
    };
    // apply appends everything before it edits an existing function; the only
    // error (findex space not dense) comes first, so roll the appends back.
    let snap = (
        code.types.len(),
        code.functions.len(),
        code.globals.len(),
        code.strings.len(),
        code.ints.len(),
        code.floats.len(),
        code.constants.as_ref().map_or(0, |c| c.len()),
    );
    if let Err(e) = apply(code, p) {
        eprintln!("follow skipped: {e:#}");
        code.types.truncate(snap.0);
        code.functions.truncate(snap.1);
        code.globals.truncate(snap.2);
        code.strings.truncate(snap.3);
        code.ints.truncate(snap.4);
        code.floats.truncate(snap.5);
        if let Some(c) = code.constants.as_mut() {
            c.truncate(snap.6);
        }
        code.globals_initializers.retain(|g, _| g.0 < snap.2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted};
    use std::collections::HashMap;

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// six functions are appended and well typed, only World.update,
    /// World.updateSprint, World.dispose and the three manual-move entries change (ops inserted,
    /// every original jump kept), the image round-trips, and a second pass
    /// changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_follow(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 6);
        assert_eq!(back.types.len(), orig.types.len() + 6);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        assert_eq!(back.floats[..orig.floats.len()], orig.floats[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        // 11 state globals (zero-initialised: no constant; fOff = false is ON)
        // + the two toast texts + the Storage key.
        const NG: usize = 11;
        assert_eq!(back.globals.len(), orig.globals.len() + NG + 3);
        let base = orig.globals.len();
        let added = &back.globals[base..base + NG];
        let (b, f) = (p.t.bool_, p.t.f64_);
        assert_eq!(added, [b, b, f, b, b, f, p.bp_t, f, b, b, f]);
        for g in base..base + NG {
            assert!(!back.globals_initializers.contains_key(&RefGlobal(g)));
        }
        let texts: Vec<Option<&str>> = (base + NG..back.globals.len())
            .map(|g| const_str(&back, RefGlobal(g)))
            .collect();
        assert_eq!(texts, [Some(FOLLOW_ON), Some(FOLLOW_OFF), Some(PREF_KEY)]);
        let gl = |i: usize| RefGlobal(base + i);
        let (g_off, g_manual, g_manual_at, g_active) = (gl(0), gl(1), gl(2), gl(4));
        let (g_arrived, g_saw, g_loaded, g_host_own) = (gl(7), gl(8), gl(9), gl(10));

        let mut touched = vec![(p.update_fi, p.update_at, 1), (p.sprint_fi, p.sprint_at, 2)];
        touched.extend(p.hooks.iter().map(|(fi, _, _, _)| (*fi, 0, 4)));
        touched.push((p.dispose_fi, 0, 1));
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            match touched.iter().find(|(fi, _, _)| *fi == i) {
                Some(&(_, at, n)) => {
                    shifted(a, b, at, n);
                    assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
                    check_types(&back, b, at..at + n);
                }
                None => assert!(
                    format!("{:?}", a.ops) == format!("{:?}", b.ops)
                        && a.regs == b.regs
                        && a.debug_info == b.debug_info,
                    "function #{i} (fn@{}) changed",
                    a.findex.0
                ),
            }
        }
        let (notify, load, save, cancel, update, reset) = (
            &back.functions[nf],
            &back.functions[nf + 1],
            &back.functions[nf + 2],
            &back.functions[nf + 3],
            &back.functions[nf + 4],
            &back.functions[nf + 5],
        );
        let sets = |f: &Function, g: RefGlobal| {
            f.ops
                .iter()
                .any(|o| matches!(o, Opcode::SetGlobal { global, .. } if *global == g))
        };
        let reads = |f: &Function, g: RefGlobal| {
            f.ops
                .iter()
                .any(|o| matches!(o, Opcode::GetGlobal { global, .. } if *global == g))
        };
        let count = |f: &Function, fun: RefFun| {
            f.ops
                .iter()
                .filter(|o| match o {
                    Opcode::Call0 { fun: x, .. }
                    | Opcode::Call1 { fun: x, .. }
                    | Opcode::Call2 { fun: x, .. }
                    | Opcode::Call4 { fun: x, .. } => *x == fun,
                    _ => false,
                })
                .count()
        };
        // World.dispose starts with followReset(): session state and the leader
        // are cleared, the F preference (fOff) is kept and re-read next session.
        assert!(
            matches!(back.functions[p.dispose_fi].ops[0], Opcode::Call0 { fun, .. } if fun == reset.findex)
        );
        for gi in [1, 3, 4, 6, 7, 8, 9, 10] {
            assert!(sets(reset, gl(gi)), "reset leaves global {gi}");
        }
        assert!(!sets(reset, g_off));
        for f in [notify, load, save, cancel, update, reset] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }
        // Storage: load reads the key into fOff, save writes it; both trapped.
        assert_eq!(count(load, p.get_ud), 1);
        assert!(sets(load, g_off));
        assert_eq!(count(save, p.set_ud), 1);
        for f in [load, save] {
            assert!(matches!(f.ops[0], Opcode::Trap { .. }));
            assert_eq!(
                f.ops
                    .iter()
                    .filter(|o| matches!(o, Opcode::EndTrap { .. }))
                    .count(),
                1
            );
        }
        // followUpdate loads once per session (fLoaded), saves and toasts on F only.
        assert_eq!(count(update, load.findex), 1);
        assert_eq!(count(update, save.findex), 1);
        assert_eq!(count(update, notify.findex), 1);
        assert!(reads(update, g_loaded) && sets(update, g_loaded));
        // Default ON: nothing but F (and followLoad) writes fOff; no window trigger.
        assert!(sets(update, g_off));
        assert!(!sets(cancel, g_off) && !sets(reset, g_off));
        let has_window = method(
            &back,
            obj_type(&back, "ui.GameUI").unwrap(),
            "hasWindowOpened",
        )
        .unwrap()
        .findex;
        assert_eq!(count(update, has_window), 0);
        // A manual move pauses (no toast, F untouched) and restarts arrival tracking.
        assert_eq!(count(cancel, notify.findex), 0);
        for g in [g_manual, g_manual_at, g_saw, g_arrived] {
            assert!(sets(cancel, g));
        }
        // Resume: saw-target and arrival time are tracked; MANUAL_HOLD and IDLE used.
        for g in [g_saw, g_arrived, g_manual] {
            assert!(reads(update, g) && sets(update, g));
        }
        let floats: Vec<f64> = update
            .ops
            .iter()
            .filter_map(|o| match o {
                Opcode::Float { ptr, .. } => Some(back.floats[ptr.0]),
                _ => None,
            })
            .collect();
        for c in [MANUAL_HOLD, IDLE, OWN_HOLD] {
            assert!(floats.contains(&c), "constant {c} unused");
        }
        // Leader: playerMovePriority, else the host; the host while its own move
        // lasts (fHostOwnAt, refreshed only while the host holds the priority or
        // its own move is still going).
        assert!(reads(update, g_host_own) && sets(update, g_host_own));
        assert!(update.ops.iter().any(|o| matches!(o,
            Opcode::Field { field, .. } if *field == p.state_priority.0)));
        assert!(update.ops.iter().any(|o| matches!(o,
            Opcode::Field { field, .. } if *field == p.state_player.0)));
        let mov_host = update.ops.iter().any(|o| match o {
            Opcode::Mov { dst, src } => {
                update.regs[dst.0 as usize] == p.bp_t
                    && update.regs[src.0 as usize] == p.state_player.1
            }
            _ => false,
        });
        assert!(mov_host, "no `leader = host` override");
        assert!(sets(update, g_active));

        // World.update calls followUpdate(this, dt) right before updateSprint().
        let u = &back.functions[p.update_fi];
        assert!(matches!(u.ops[p.update_at],
            Opcode::Call2 { fun, arg0: Reg(0), arg1: Reg(1), .. } if fun == update.findex));
        assert!(matches!(
            u.ops[p.update_at + 1],
            Opcode::Call1 { arg0: Reg(0), .. }
        ));
        // updateSprint: `if (!fActive) ctrl.playerSetShift(b)`.
        let sp = &back.functions[p.sprint_fi];
        let active = RefGlobal(orig.globals.len() + 4);
        assert!(matches!(sp.ops[p.sprint_at],
            Opcode::GetGlobal { global, .. } if global == active));
        assert_eq!(jump_targets(sp, p.sprint_at + 1), vec![p.sprint_at + 3]);
        assert!(matches!(sp.ops[p.sprint_at + 2], Opcode::Call2 { fun, .. } if fun == p.set_shift));
        // Each manual-move entry starts with
        // `if (this.game != null) followCancel(this.game, claim)`; only the entity click claims.
        for (fi, _, claim, what) in &p.hooks {
            let h = &back.functions[*fi];
            assert!(
                matches!(h.ops[0], Opcode::Field { obj: Reg(0), .. }),
                "{what}"
            );
            assert_eq!(jump_targets(h, 1), vec![4], "{what}");
            assert!(
                matches!(h.ops[2], Opcode::Bool { value: ValBool(c), .. } if c == *claim),
                "{what}"
            );
            assert!(
                matches!(h.ops[3], Opcode::Call2 { fun, .. } if fun == cancel.findex),
                "{what}"
            );
        }
        // followUpdate: one goto, one shift send, one resetSoftTarget(&true), all guarded.
        let calls = |fun: RefFun| {
            update
                .ops
                .iter()
                .filter(|o| match o {
                    Opcode::Call2 { fun: f, .. } | Opcode::Call4 { fun: f, .. } => *f == fun,
                    _ => false,
                })
                .count()
        };
        assert_eq!(calls(p.player_goto), 1);
        assert_eq!(calls(p.set_shift), 1);
        assert_eq!(calls(p.reset_soft.0), 1);
        // Leader = playerMovePriority (else the host): no scan over the players
        // that could pick another, moving follower.
        assert!(update
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Field { field, .. } if *field == p.state_priority.0)));
        assert!(!update
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::GetArray { .. })));
        // followCancel claims the priority (resetSoftTarget(null)) only when asked.
        assert_eq!(
            cancel
                .ops
                .iter()
                .filter(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == p.reset_soft.0))
                .count(),
            1
        );
        assert_eq!(p.hooks.iter().filter(|h| h.2).count(), 1);
        let goto_at = update
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Call4 { fun, .. } if *fun == p.player_goto))
            .unwrap();
        let issuing = RefGlobal(orig.globals.len() + 3);
        assert!(
            matches!(update.ops[goto_at - 2], Opcode::SetGlobal { global, .. } if global == issuing)
        );
        assert!(
            matches!(update.ops[goto_at + 2], Opcode::SetGlobal { global, .. } if global == issuing)
        );

        // The type check is not vacuous: a float load into an i32 register fails it.
        let mut bad = update.clone();
        let fi = bad
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Float { .. }))
            .unwrap();
        let i32_reg = Reg(bad.regs.iter().position(|t| *t == p.t.i32_).unwrap() as u32);
        if let Opcode::Float { dst, .. } = &mut bad.ops[fi] {
            *dst = i32_reg;
        }
        let n = bad.ops.len();
        let back_ref = &back;
        assert!(std::panic::catch_unwind(|| check_types(back_ref, &bad, 0..n)).is_err());

        // A second pass finds it applied and leaves the image alone.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_follow(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    // ---------- behaviour: a tiny interpreter for followUpdate / followCancel ----------

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        B(bool),
        I(i32),
        F(f64),
        O(usize),
    }

    struct Sim<'a> {
        code: &'a Bytecode,
        p: &'a Plan,
        globals: HashMap<usize, V>,
        heap: Vec<HashMap<usize, V>>,
        now: f64,
        key_f: bool,
        cancel: RefFun,
        load: RefFun,
        save: RefFun,
        notify: RefFun,
        /// (callee, args) of every side-effect call.
        log: Vec<(&'static str, Vec<V>)>,
    }

    impl<'a> Sim<'a> {
        fn obj(&mut self, fields: &[(usize, V)]) -> V {
            self.heap.push(fields.iter().cloned().collect());
            V::O(self.heap.len() - 1)
        }
        fn set(&mut self, o: &V, f: usize, v: V) {
            let V::O(i) = o else { panic!("set on {o:?}") };
            self.heap[*i].insert(f, v);
        }
        fn get(&self, o: &V, f: usize) -> V {
            let V::O(i) = o else {
                panic!("null access field {f}")
            };
            self.heap[*i].get(&f).cloned().unwrap_or(V::Null)
        }
        fn global(&self, g: RefGlobal) -> V {
            self.globals.get(&g.0).cloned().unwrap_or_else(|| {
                match self.code.types[self.code.globals[g.0].0] {
                    Type::Bool => V::B(false),
                    Type::F64 => V::F(0.0),
                    _ => V::Null,
                }
            })
        }
        fn fun(&self, f: RefFun) -> &'a Function {
            self.code.functions.iter().find(|x| x.findex == f).unwrap()
        }

        fn call(&mut self, f: RefFun, args: Vec<V>) -> V {
            let p = self.p;
            let b = |x: bool| V::B(x);
            if f == p.sys_time {
                return V::F(self.now);
            }
            if f == p.is_multi || f == p.is_visible || f == p.reachable {
                return b(true);
            }
            if f == p.key_pressed {
                return b(self.key_f);
            }
            if f == p.get_focus.0 {
                return V::Null;
            }
            if f == p.on_water {
                return b(false);
            }
            if f == p.sqrt {
                let V::F(x) = args[0] else { panic!() };
                return V::F(x.sqrt());
            }
            let name = if f == self.load {
                "load"
            } else if f == self.save {
                "save"
            } else if f == self.notify {
                "notify"
            } else if f == p.set_shift {
                "shift"
            } else if f == p.reset_soft.0 {
                "resetSoft"
            } else if f == p.player_goto {
                // the hooked Controller.playerGoto: followCancel(game, false) first
                let game = self.get(&args[0], p.hooks[0].1 .0);
                self.run(self.cancel, vec![game, V::B(false)]);
                "goto"
            } else if f == self.cancel {
                return self.run(f, args);
            } else {
                panic!("unexpected call fn@{}", f.0)
            };
            self.log.push((name, args));
            V::Null
        }

        fn run(&mut self, f: RefFun, args: Vec<V>) -> V {
            let fun = self.fun(f);
            let mut r = vec![V::Null; fun.regs.len()];
            for (i, a) in args.into_iter().enumerate() {
                r[i] = a;
            }
            let mut pc = 0usize;
            let num = |v: &V| match v {
                V::F(x) => *x,
                V::I(x) => *x as f64,
                o => panic!("not a number: {o:?}"),
            };
            loop {
                let op = &fun.ops[pc];
                let mut next = pc + 1;
                let jump = |off: i32| (pc as i64 + 1 + off as i64) as usize;
                let rr = |x: &Reg| x.0 as usize;
                match op {
                    Opcode::Bool { dst, value } => r[rr(dst)] = V::B(value.0),
                    Opcode::Int { dst, ptr } => r[rr(dst)] = V::I(self.code.ints[ptr.0]),
                    Opcode::Float { dst, ptr } => r[rr(dst)] = V::F(self.code.floats[ptr.0]),
                    Opcode::Null { dst } => r[rr(dst)] = V::Null,
                    Opcode::Mov { dst, src } => r[rr(dst)] = r[rr(src)].clone(),
                    Opcode::Not { dst, src } => {
                        r[rr(dst)] = V::B(r[rr(src)] != V::B(true));
                    }
                    Opcode::GetGlobal { dst, global } => r[rr(dst)] = self.global(*global),
                    Opcode::SetGlobal { global, src } => {
                        self.globals.insert(global.0, r[rr(src)].clone());
                    }
                    Opcode::Field { dst, obj, field } => {
                        r[rr(dst)] = self.get(&r[rr(obj)], field.0);
                    }
                    Opcode::SetField { obj, field, src } => {
                        let (o, v) = (r[rr(obj)].clone(), r[rr(src)].clone());
                        self.set(&o, field.0, v);
                    }
                    Opcode::New { dst } => r[rr(dst)] = self.obj(&[]),
                    Opcode::Ref { dst, .. } => r[rr(dst)] = V::Null,
                    Opcode::Add { dst, a, b }
                    | Opcode::Sub { dst, a, b }
                    | Opcode::Mul { dst, a, b }
                    | Opcode::SDiv { dst, a, b } => {
                        let (x, y) = (num(&r[rr(a)]), num(&r[rr(b)]));
                        let v = match op {
                            Opcode::Add { .. } => x + y,
                            Opcode::Sub { .. } => x - y,
                            Opcode::Mul { .. } => x * y,
                            _ => x / y,
                        };
                        r[rr(dst)] = match r[rr(a)] {
                            V::I(_) => V::I(v as i32),
                            _ => V::F(v),
                        };
                    }
                    Opcode::And { dst, a, b } => {
                        let (V::I(x), V::I(y)) = (&r[rr(a)], &r[rr(b)]) else {
                            panic!()
                        };
                        r[rr(dst)] = V::I(x & y);
                    }
                    Opcode::JAlways { offset } => next = jump(*offset),
                    Opcode::JTrue { cond, offset } if r[rr(cond)] == V::B(true) => {
                        next = jump(*offset)
                    }
                    Opcode::JFalse { cond, offset } if r[rr(cond)] != V::B(true) => {
                        next = jump(*offset)
                    }
                    Opcode::JNull { reg, offset } if r[rr(reg)] == V::Null => next = jump(*offset),
                    Opcode::JNotNull { reg, offset } if r[rr(reg)] != V::Null => {
                        next = jump(*offset)
                    }
                    Opcode::JEq { a, b, offset } if r[rr(a)] == r[rr(b)] => next = jump(*offset),
                    Opcode::JNotEq { a, b, offset } if r[rr(a)] != r[rr(b)] => next = jump(*offset),
                    Opcode::JSLt { a, b, offset }
                    | Opcode::JSGt { a, b, offset }
                    | Opcode::JSLte { a, b, offset }
                    | Opcode::JSGte { a, b, offset } => {
                        let (x, y) = (num(&r[rr(a)]), num(&r[rr(b)]));
                        let t = match op {
                            Opcode::JSLt { .. } => x < y,
                            Opcode::JSGt { .. } => x > y,
                            Opcode::JSLte { .. } => x <= y,
                            _ => x >= y,
                        };
                        if t {
                            next = jump(*offset);
                        }
                    }
                    Opcode::JTrue { .. }
                    | Opcode::JFalse { .. }
                    | Opcode::JNull { .. }
                    | Opcode::JNotNull { .. }
                    | Opcode::JEq { .. }
                    | Opcode::JNotEq { .. } => {}
                    Opcode::Call0 { dst, fun } => r[rr(dst)] = self.call(*fun, vec![]),
                    Opcode::Call1 { dst, fun, arg0 } => {
                        let a = vec![r[rr(arg0)].clone()];
                        r[rr(dst)] = self.call(*fun, a);
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } => {
                        let a = vec![r[rr(arg0)].clone(), r[rr(arg1)].clone()];
                        r[rr(dst)] = self.call(*fun, a);
                    }
                    Opcode::Call4 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                        arg2,
                        arg3,
                    } => {
                        let a = [arg0, arg1, arg2, arg3]
                            .iter()
                            .map(|x| r[rr(x)].clone())
                            .collect();
                        r[rr(dst)] = self.call(*fun, a);
                    }
                    Opcode::Ret { ret } => return r[rr(ret)].clone(),
                    o => panic!("interpreter: unsupported {o:?}"),
                }
                pc = next;
            }
        }
    }

    /// A co-op world: `me` plus `others` (host first when `me` is not the host).
    struct World {
        world: V,
        game: V,
        state: V,
        update: RefFun,
    }

    fn player(sim: &mut Sim, x: f64) -> V {
        let p = sim.p;
        let flags = sim.obj(&[(p.flags_value.0, V::I(0))]);
        sim.obj(&[
            (p.bp_x.0, V::F(x)),
            (p.bp_y.0, V::F(0.0)),
            (p.bp_flags.0 .0, flags),
            (p.bp_connected.0, V::B(true)),
        ])
    }

    fn world(sim: &mut Sim, me: &V, host: &V, update: RefFun) -> World {
        let p = sim.p;
        let state = sim.obj(&[
            (p.state_player.0 .0, host.clone()),
            (p.state_cine.0, V::B(false)),
        ]);
        let sev = sim.obj(&[]);
        let ctrl = sim.obj(&[]);
        let game = sim.obj(&[
            (p.game_me.0, me.clone()),
            (p.game_state.0 .0, state.clone()),
            (p.game_sevents.0 .0, sev),
        ]);
        sim.set(&ctrl, p.hooks[0].1 .0, game.clone());
        sim.set(&game, p.game_ctrl.0 .0, ctrl);
        let world = sim.obj(&[(p.world_game.0, game.clone())]);
        sim.set(&game, p.game_mode.0 .0, world.clone());
        World {
            world,
            game,
            state,
            update,
        }
    }

    impl World {
        /// One followUpdate at `t`; returns the x of the goto it issued, if any.
        fn tick(&self, sim: &mut Sim, t: f64) -> Option<f64> {
            sim.now = t;
            sim.log.clear();
            sim.run(self.update, vec![self.world.clone(), V::F(0.016)]);
            sim.key_f = false;
            sim.log
                .iter()
                .find(|(n, _)| *n == "goto")
                .map(|(_, a)| match a[1] {
                    V::F(x) => x,
                    _ => panic!(),
                })
        }
        fn target(&self, sim: &mut Sim, pl: &V, x: Option<f64>) {
            let p = sim.p;
            let t = match x {
                Some(x) => sim.obj(&[(p.target_x.0, V::F(x)), (p.target_y.0, V::F(0.0))]),
                None => V::Null,
            };
            sim.set(pl, p.bp_target.0 .0, t);
        }
        fn prio(&self, sim: &mut Sim, pl: V) {
            let f = sim.p.state_priority.0 .0;
            sim.set(&self.state, f, pl);
        }
        fn manual(&self, sim: &mut Sim, t: f64) {
            sim.now = t;
            sim.run(sim.cancel, vec![self.game.clone(), V::B(false)]);
        }
    }

    /// Default ON, the F opt-out, the pause on an own move and its resume
    /// (saw-target, unreachable fallback, gate), and the leader rule: the most
    /// recent own-input mover, the host while its own move lasts, never oneself.
    #[test]
    fn follow_behaviour() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_follow(&mut code);
        let nf = orig.functions.len();
        let f = |i: usize| code.functions[nf + i].findex;
        let (notify, load, save, cancel, update) = (f(0), f(1), f(2), f(3), f(4));
        assert_eq!(p.hooks[0].3, "Controller.playerGoto");
        let new_sim = || Sim {
            code: &code,
            p: &p,
            globals: HashMap::new(),
            heap: vec![],
            now: 100.0,
            key_f: false,
            cancel,
            load,
            save,
            notify,
            log: vec![],
        };
        let g_off = orig.globals.len();
        let g_manual = orig.globals.len() + 1;

        // -- a client, the host 50 to the right, nobody moved yet: follows the host.
        let mut sim = new_sim();
        let (me, host, other) = (
            player(&mut sim, 0.0),
            player(&mut sim, 50.0),
            player(&mut sim, -50.0),
        );
        let w = world(&mut sim, &me, &host, update);
        let x = w
            .tick(&mut sim, 100.0)
            .expect("default ON follows the host");
        assert!((x - 45.0).abs() < 1e-9, "GAP short of the leader: {x}");
        assert!(
            sim.log.iter().any(|(n, _)| *n == "load"),
            "opt-out read once"
        );
        // the follow goto went through the hooked playerGoto without pausing follow
        assert_ne!(sim.global(RefGlobal(g_manual)), V::B(true));
        // F: OFF (saved, toast), no goto; F again: ON.
        sim.key_f = true;
        assert_eq!(w.tick(&mut sim, 101.0), None);
        assert_eq!(sim.global(RefGlobal(g_off)), V::B(true));
        assert!(sim.log.contains(&("save", vec![V::B(true)])));
        assert!(sim
            .log
            .iter()
            .any(|(n, a)| *n == "notify" && a[1] == V::B(false)));
        assert_eq!(w.tick(&mut sim, 102.0), None, "OFF stays off");
        sim.key_f = true;
        assert!(w.tick(&mut sim, 103.0).is_some(), "F again: ON");
        assert!(
            !sim.log.iter().any(|(n, _)| *n == "load"),
            "loaded once per session"
        );

        // -- own move: paused until the target showed up, arrival, then IDLE.
        w.manual(&mut sim, 110.0);
        assert_eq!(w.tick(&mut sim, 110.5), None, "round trip pending");
        w.target(&mut sim, &me, Some(-20.0));
        assert_eq!(w.tick(&mut sim, 112.0), None, "walking");
        assert_eq!(w.tick(&mut sim, 115.0), None, "still walking");
        w.target(&mut sim, &me, None);
        assert_eq!(w.tick(&mut sim, 115.5), None, "arrived, idle starts");
        assert_eq!(w.tick(&mut sim, 116.4), None, "idle < 1 s");
        assert!(
            w.tick(&mut sim, 116.6).is_some(),
            "resumes 1 s after arrival"
        );

        // -- an unwalkable click (no target ever): MANUAL_HOLD, then IDLE.
        w.manual(&mut sim, 120.0);
        assert_eq!(w.tick(&mut sim, 121.4), None);
        assert_eq!(w.tick(&mut sim, 121.6), None, "counts as arrived at 1.6");
        assert_eq!(w.tick(&mut sim, 122.5), None);
        assert!(
            w.tick(&mut sim, 122.7).is_some(),
            "resumes ~2.5 s after the click"
        );

        // -- an interaction gate after an entity click restarts the idle time.
        let locked = sim.obj(&[]);
        w.manual(&mut sim, 130.0);
        w.target(&mut sim, &me, Some(-5.0));
        assert_eq!(w.tick(&mut sim, 130.5), None);
        w.target(&mut sim, &me, None);
        sim.set(&me, p.bp_locked.0 .0, locked);
        assert_eq!(w.tick(&mut sim, 135.0), None, "in the dialog");
        sim.set(&me, p.bp_locked.0 .0, V::Null);
        assert_eq!(w.tick(&mut sim, 135.8), None, "1 s after the dialog");
        assert!(w.tick(&mut sim, 136.1).is_some());

        // -- leader: the most recent own-input mover.
        w.prio(&mut sim, other.clone());
        let x = w.tick(&mut sim, 140.0).expect("follows the other client");
        assert!(x < 0.0);
        // never oneself
        w.prio(&mut sim, me.clone());
        assert_eq!(w.tick(&mut sim, 141.0), None);
        assert_eq!(sim.global(RefGlobal(orig.globals.len() + 4)), V::B(false));

        // -- the host moves by its own input, then another client clicks while
        // the host is still walking: the host keeps leading.
        w.prio(&mut sim, host.clone());
        w.target(&mut sim, &host, Some(80.0));
        assert!(w.tick(&mut sim, 150.0).unwrap() > 0.0);
        w.prio(&mut sim, other.clone());
        assert!(
            w.tick(&mut sim, 150.6).unwrap() > 0.0,
            "host still moving: host leads"
        );
        assert!(
            w.tick(&mut sim, 151.2).unwrap() > 0.0,
            "kept while its target lasts"
        );
        assert!(w.tick(&mut sim, 152.0).unwrap() > 0.0);
        // the host stops: the other (most recent) mover leads after OWN_HOLD
        w.target(&mut sim, &host, None);
        assert!(w.tick(&mut sim, 152.6).unwrap() > 0.0, "gap < OWN_HOLD");
        assert!(
            w.tick(&mut sim, 153.2).unwrap() < 0.0,
            "now the other mover"
        );
        // a host target that is a follow move (host not holding the priority,
        // its own move over) does not make it the leader again
        w.target(&mut sim, &host, Some(-40.0));
        assert!(w.tick(&mut sim, 154.0).unwrap() < 0.0);

        // -- on the host's machine: the host never follows itself while its own
        // move lasts, and follows the most recent mover afterwards.
        let mut sim = new_sim();
        let (hme, cl) = (player(&mut sim, 0.0), player(&mut sim, -50.0));
        let w = world(&mut sim, &hme, &hme, update);
        assert_eq!(
            w.tick(&mut sim, 100.0),
            None,
            "nobody moved: the host leads"
        );
        w.prio(&mut sim, hme.clone());
        w.target(&mut sim, &hme, Some(30.0));
        assert_eq!(w.tick(&mut sim, 101.0), None);
        w.prio(&mut sim, cl.clone());
        assert_eq!(
            w.tick(&mut sim, 101.5),
            None,
            "own move lasts: still the leader"
        );
        w.target(&mut sim, &hme, None);
        assert!(
            w.tick(&mut sim, 103.0).unwrap() < 0.0,
            "then follows the client"
        );
    }

    /// A second updateSprint() call in World.update, a second playerSetShift
    /// send, or a jump onto a hook's op 0: skipped, the image untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let ser = |c: &Bytecode| {
            let mut v = Vec::new();
            c.serialize(&mut v).expect("write");
            v
        };
        let base = ser(&orig);
        let hook = p.hooks[0].0;
        type Edit<'a> = (usize, Box<dyn Fn(&mut Function) + 'a>);
        let edits: Vec<Edit> = vec![
            (
                p.update_fi,
                Box::new(|f: &mut Function| {
                    let c = f.ops[p.update_at].clone();
                    f.ops[p.update_at - 1] = c;
                }),
            ),
            (
                p.sprint_fi,
                Box::new(|f: &mut Function| {
                    let c = f.ops[p.sprint_at].clone();
                    f.ops[p.sprint_at - 1] = c;
                }),
            ),
            (
                hook,
                Box::new(|f: &mut Function| {
                    let last = f.ops.len() - 1;
                    f.ops[last - 1] = Opcode::JAlways {
                        offset: -(last as i32),
                    };
                }),
            ),
        ];
        for (k, (fi, edit)) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code.functions[*fi]);
            let before = ser(&code);
            assert!(before != base, "edit {k} was a no-op");
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_follow(&mut code);
            assert!(ser(&code) == before, "edit {k} changed the code");
        }
    }
}
