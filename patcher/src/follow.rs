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
//   Controller.playerGoto (unless sent by follow), Controller.playerGotoEntity,
//   BasePlayer.updateMoveHold (mouse hold / pad):        followCancel(game)
//   World.dispose (quit / load):                         followReset()
//
//   followUpdate(world, dt):
//     fIssuing = false;
//     if (Key.isPressed(F) && sevents.getFocus() == null && game.isMulti) {
//         fOn = !fOn; fManual = false; fNext = 0; notify(fOn ? "Follow: ON" : "Follow: OFF");
//     }
//     inactive (fActive = false, fLeader = null; fOn = false when not multi) unless:
//       isMulti, game.mode == world, game.battle == null, !cinematicMode,
//       state.currentCity == null, (fOn || ui.hasWindowOpened()),
//       no own manual move pending (fManual: until me.target == null and MANUAL_HOLD s passed),
//       me.lockedWith, me.waitActionIcon, me.scriptedMoveData, world.currentWindow all null,
//       !me.onWater, a leader exists and is not on water;
//     leader = best other connected, visible (on land, has units) player:
//       +16 playerMovePriority, +4 current leader still moving, +2 moving (target != null),
//       +1 host (state.player), -8 in a menu (hasWindowOpened: likely following itself),
//       -8 softTarget (vanilla regroup); first in players order on a tie;
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
//   followCancel(game): if (!fIssuing) { fManual = true; fManualAt = now;
//                                        if (fOn) { fOn = false; notify OFF } }
//   followReset(): fOn = fManual = fIssuing = fActive = false; fLeader = null.
//
// Only the RPCs a click and the sprint key already send are used: the host
// validates and moves the caravan as for a click. State lives in new globals
// (zero-initialised), local to each machine. The toast is
// `GameUI.localNotify("ArenaNotif", {title: ...})` (`NotifyData.getTitle` returns
// `opts.title`; cdb ArenaNotif: log line, not in the journal). Hotkey F: no
// world-map binding uses it (cdb `input`); the only hard-coded uses are admin
// keys and the title screen.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefGlobal, RefString, ValBool};
use std::collections::HashMap;

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
/// A manual move suppresses follow at least this long (its RPC round trip).
const MANUAL_HOLD: f64 = 1.5;
const SHIFT_BIT: i32 = 8;

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

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

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let fun = &code.functions[fun_index(code, f)?];
    let t = fun.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

fn string_index(code: &Bytecode, value: &str) -> Result<RefString> {
    code.strings
        .iter()
        .position(|v| v.as_str() == value)
        .map(RefString)
        .with_context(|| format!("string {value:?} not in the pool"))
}

fn is_subclass(code: &Bytecode, t: RefType, of: RefType) -> Result<bool> {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if c == of {
            return Ok(true);
        }
        cur = obj(code, c)?.super_;
    }
    Ok(false)
}

/// Function-local assembler with symbolic jump labels.
struct Asm {
    ops: Vec<Opcode>,
    fix: Vec<(usize, &'static str)>,
    labels: HashMap<&'static str, usize>,
}

impl Asm {
    fn new() -> Self {
        Asm {
            ops: vec![],
            fix: vec![],
            labels: HashMap::new(),
        }
    }
    fn op(&mut self, o: Opcode) {
        self.ops.push(o);
    }
    /// A jump op (offset placeholder) to `label`.
    fn jmp(&mut self, o: Opcode, label: &'static str) {
        self.fix.push((self.ops.len(), label));
        self.ops.push(o);
    }
    fn label(&mut self, l: &'static str) {
        assert!(
            self.labels.insert(l, self.ops.len()).is_none(),
            "label {l} twice"
        );
    }
    fn finish(mut self) -> Vec<Opcode> {
        for (i, l) in std::mem::take(&mut self.fix) {
            let t = *self.labels.get(l).unwrap_or_else(|| panic!("label {l}"));
            let off = t as i32 - i as i32 - 1;
            match &mut self.ops[i] {
                Opcode::JTrue { offset, .. }
                | Opcode::JFalse { offset, .. }
                | Opcode::JNull { offset, .. }
                | Opcode::JNotNull { offset, .. }
                | Opcode::JSLt { offset, .. }
                | Opcode::JSGte { offset, .. }
                | Opcode::JSGt { offset, .. }
                | Opcode::JSLte { offset, .. }
                | Opcode::JEq { offset, .. }
                | Opcode::JNotEq { offset, .. }
                | Opcode::JAlways { offset } => *offset = off,
                o => panic!("not a jump: {o:?}"),
            }
            if off < 0 {
                assert!(
                    matches!(self.ops[t], Opcode::Label),
                    "backward jump to a non-Label"
                );
            }
        }
        self.ops
    }
}

/// Register list of a new function.
struct Regs(Vec<RefType>);

impl Regs {
    fn r(&mut self, t: RefType) -> Reg {
        self.0.push(t);
        Reg((self.0.len() - 1) as u32)
    }
}

struct Types {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    dyn_: RefType,
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
    /// (function index, `game` field of arg 0, name) of the cancel hooks.
    hooks: Vec<(usize, RefField, &'static str)>,
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
    state_players: F,
    state_player: F,
    state_priority: F,
    state_city: F,
    state_cine: RefField,
    proxy_array: F,
    arr_t: RefType,
    arr_len: RefField,
    arr_raw: F,
    bp_x: RefField,
    bp_y: RefField,
    bp_target: F,
    target_x: RefField,
    target_y: RefField,
    bp_flags: F,
    flags_value: RefField,
    bp_connected: RefField,
    bp_window: RefField,
    bp_soft: RefField,
    bp_locked: F,
    bp_wait: F,
    bp_scripted: F,
    pt_t: RefType,
    pt_x: RefField,
    pt_y: RefField,
    key_pressed: RefFun,
    get_focus: (RefFun, RefType),
    is_multi: RefFun,
    has_window: RefFun,
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
        dyn_: prim("dynamic", |t| matches!(t, Type::Dyn))?,
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
    if !is_subclass(code, world_t, game_mode.1)? {
        bail!("World is not a Game.mode type");
    }
    let state_t = game_state.1;
    let state_players = field(code, state_t, "players")?;
    let state_player = field(code, state_t, "player")?;
    let state_priority = field(code, state_t, "playerMovePriority")?;
    let state_city = field(code, state_t, "currentCity")?;
    let state_cine = typed(state_t, "cinematicMode", t.bool_)?;
    for (what, ft) in [
        ("player", state_player.1),
        ("playerMovePriority", state_priority.1),
    ] {
        if !is_subclass(code, ft, bp_t)? {
            bail!("GameState.{what} is not a BasePlayer");
        }
    }
    let proxy_array = field(code, state_players.1, "array")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let arr_len = typed(arr_t, "length", t.i32_)?;
    let arr_raw = field(code, arr_t, "array")?;

    let bp_x = typed(bp_t, "x", t.f64_)?;
    let bp_y = typed(bp_t, "y", t.f64_)?;
    let bp_target = field(code, bp_t, "target")?;
    let target_x = typed(bp_target.1, "x", t.f64_)?;
    let target_y = typed(bp_target.1, "y", t.f64_)?;
    let bp_flags = field(code, bp_t, "flags")?;
    let flags_value = typed(bp_flags.1, "value", t.i32_)?;
    let bp_connected = typed(bp_t, "connected", t.bool_)?;
    let bp_window = typed(bp_t, "hasWindowOpened", t.bool_)?;
    let bp_soft = typed(bp_t, "softTarget", t.bool_)?;
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
    let has_window = method(code, ui_t, "hasWindowOpened")?.findex;
    if sig(code, has_window)? != (vec![ui_t], t.bool_) {
        bail!("unexpected GameUI.hasWindowOpened signature");
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
    for (f, game_f, what) in [
        (player_goto, ctrl_game, "Controller.playerGoto"),
        (goto_entity, ctrl_game, "Controller.playerGotoEntity"),
        (move_hold, bp_game, "BasePlayer.updateMoveHold"),
    ] {
        let fi = fun_index(code, f)?;
        let g = &code.functions[fi];
        if (0..g.ops.len()).any(|i| jump_targets(g, i).contains(&0)) {
            bail!("{what}: a jump targets op 0");
        }
        hooks.push((fi, game_f, what));
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
        state_players,
        state_player,
        state_priority,
        state_city,
        state_cine,
        proxy_array,
        arr_t,
        arr_len,
        arr_raw,
        bp_x,
        bp_y,
        bp_target,
        target_x,
        target_y,
        bp_flags,
        flags_value,
        bp_connected,
        bp_window,
        bp_soft,
        bp_locked,
        bp_wait,
        bp_scripted,
        pt_t,
        pt_x,
        pt_y,
        key_pressed,
        get_focus,
        is_multi,
        has_window,
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
    on: RefGlobal,
    manual: RefGlobal,
    manual_at: RefGlobal,
    issuing: RefGlobal,
    active: RefGlobal,
    next: RefGlobal,
    leader: RefGlobal,
}

fn add_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

/// Appends `ops` as a new function `args -> void` and returns its findex.
fn push_fn(
    code: &mut Bytecode,
    p: &Plan,
    args: Vec<RefType>,
    regs: Vec<RefType>,
    ops: Vec<Opcode>,
) -> Result<RefFun> {
    let findex = next_findex(code)?;
    code.types.push(Type::Fun(TypeFun {
        args,
        ret: p.t.void,
    }));
    let fun_t = RefType(code.types.len() - 1);
    let n = ops.len();
    code.functions.push(Function {
        name: RefString(0),
        t: fun_t,
        findex,
        regs,
        ops,
        debug_info: Some(vec![(p.dbg_file, 1); n]),
        assigns: Some(vec![]),
        parent: None,
    });
    Ok(findex)
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
    push_fn(code, p, vec![p.game_t, p.t.bool_], r.0, a.finish())
}

/// `followCancel(game)`: a manual move by this machine's player.
fn add_cancel(code: &mut Bytecode, p: &Plan, g: &Globals, notify: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.game_t]);
    let (b, now, v) = (r.r(p.t.bool_), r.r(p.t.f64_), r.r(p.t.void));
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
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::SetGlobal {
        global: g.manual_at,
        src: now,
    });
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.on,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.on,
        src: b,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: notify,
        arg0: Reg(0),
        arg1: b,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, p, vec![p.game_t], r.0, a.finish())
}

/// `followUpdate(world, dt)`, see the module comment.
fn add_update(code: &mut Bytecode, p: &Plan, g: &Globals, notify: RefFun) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let i1 = int_const(code, 1);
    let i2 = int_const(code, 2);
    let i4 = int_const(code, 4);
    let i8 = int_const(code, 8);
    let i16 = int_const(code, 16);
    let im1 = int_const(code, -1);
    let ikey = int_const(code, KEY_F);
    let imask = int_const(code, SHIFT_BIT);
    let f0 = float_const(code, 0.0);
    let ftick = float_const(code, TICK);
    let fhold = float_const(code, MANUAL_HOLD);
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
    let (k, i, n, score, best_score, ki) = (
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
        r.r(t.i32_),
    );
    let (sev, focus, ui, mode, battle, city) = (
        r.r(p.game_sevents.1),
        r.r(p.get_focus.1),
        r.r(p.game_ui.1),
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
    let (host, prio, cur, best, pl) = (
        r.r(p.state_player.1),
        r.r(p.state_priority.1),
        r.r(p.bp_t),
        r.r(p.bp_t),
        r.r(p.bp_t),
    );
    let (proxy, adyn, arr, raw, d) = (
        r.r(p.state_players.1),
        r.r(p.proxy_array.1),
        r.r(p.arr_t),
        r.r(p.arr_raw.1),
        r.r(t.dyn_),
    );
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
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "solo");

    // ---- hotkey F
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
        global: g.on,
    });
    a.op(Opcode::Not { dst: b, src: b });
    a.op(Opcode::SetGlobal {
        global: g.on,
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
    a.op(Opcode::Call2 {
        dst: v,
        fun: notify,
        arg0: game,
        arg1: b,
    });
    a.label("afterkey");

    // ---- preconditions
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
        "inactive",
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
        "inactive",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: st,
        field: p.state_cine,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "inactive");
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
        "inactive",
    );
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
        Opcode::JNotNull {
            reg: tgt,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
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
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.manual,
        src: b,
    });
    a.label("nomanual");
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.on,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "wanted");
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.game_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "inactive");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.has_window,
        arg0: ui,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "inactive");
    a.label("wanted");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNotNull { reg: lw, offset: 0 }, "inactive");
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
        "inactive",
    );
    a.op(Opcode::Field {
        dst: cw,
        obj: Reg(0),
        field: p.world_cur_win.0,
    });
    a.jmp(Opcode::JNotNull { reg: cw, offset: 0 }, "inactive");
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
        "inactive",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.on_water,
        arg0: me,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "inactive");

    // ---- leader
    a.op(Opcode::Field {
        dst: host,
        obj: st,
        field: p.state_player.0,
    });
    a.op(Opcode::Field {
        dst: prio,
        obj: st,
        field: p.state_priority.0,
    });
    a.op(Opcode::GetGlobal {
        dst: cur,
        global: g.leader,
    });
    a.op(Opcode::Null { dst: best });
    a.op(Opcode::Int {
        dst: best_score,
        ptr: im1,
    });
    a.op(Opcode::Field {
        dst: proxy,
        obj: st,
        field: p.state_players.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: proxy,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Field {
        dst: adyn,
        obj: proxy,
        field: p.proxy_array.0,
    });
    a.op(Opcode::SafeCast {
        dst: arr,
        src: adyn,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "inactive",
    );
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.label("loop");
    a.op(Opcode::Label);
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "picked",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "loop");
    a.jmp(
        Opcode::JEq {
            a: pl,
            b: me,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: pl,
        field: p.bp_connected,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_visible,
        arg0: pl,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::Int {
        dst: score,
        ptr: i0,
    });
    a.jmp(
        Opcode::JNotEq {
            a: pl,
            b: prio,
            offset: 0,
        },
        "notprio",
    );
    a.op(Opcode::Int { dst: k, ptr: i16 });
    a.op(Opcode::Add {
        dst: score,
        a: score,
        b: k,
    });
    a.label("notprio");
    a.op(Opcode::Field {
        dst: tgt,
        obj: pl,
        field: p.bp_target.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: tgt,
            offset: 0,
        },
        "notmoving",
    );
    a.op(Opcode::Int { dst: k, ptr: i2 });
    a.op(Opcode::Add {
        dst: score,
        a: score,
        b: k,
    });
    a.jmp(
        Opcode::JNotEq {
            a: pl,
            b: cur,
            offset: 0,
        },
        "notmoving",
    );
    a.op(Opcode::Int { dst: k, ptr: i4 });
    a.op(Opcode::Add {
        dst: score,
        a: score,
        b: k,
    });
    a.label("notmoving");
    a.jmp(
        Opcode::JNotEq {
            a: pl,
            b: host,
            offset: 0,
        },
        "nothost",
    );
    a.op(Opcode::Int { dst: k, ptr: i1 });
    a.op(Opcode::Add {
        dst: score,
        a: score,
        b: k,
    });
    a.label("nothost");
    a.op(Opcode::Int { dst: k, ptr: i8 });
    a.op(Opcode::Field {
        dst: b,
        obj: pl,
        field: p.bp_window,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "nowindow");
    a.op(Opcode::Sub {
        dst: score,
        a: score,
        b: k,
    });
    a.label("nowindow");
    a.op(Opcode::Field {
        dst: b,
        obj: pl,
        field: p.bp_soft,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "scored");
    a.op(Opcode::Sub {
        dst: score,
        a: score,
        b: k,
    });
    a.label("scored");
    a.jmp(
        Opcode::JSLte {
            a: score,
            b: best_score,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Mov { dst: best, src: pl });
    a.op(Opcode::Mov {
        dst: best_score,
        src: score,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("picked");
    a.jmp(
        Opcode::JNull {
            reg: best,
            offset: 0,
        },
        "inactive",
    );
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
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
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
    a.label("solo");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.on,
        src: b,
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
    a.op(Opcode::Null { dst: cur });
    a.op(Opcode::SetGlobal {
        global: g.leader,
        src: cur,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, p, vec![p.world_t, t.f64_], r.0, a.finish())
}

/// `followReset()`: World.dispose (quit to the menu or load) forgets the session.
/// The leader is an object of that session and must not outlive it.
fn add_reset(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![]);
    let (b, pl, v) = (r.r(p.t.bool_), r.r(p.bp_t), r.r(p.t.void));
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    for gl in [g.on, g.manual, g.issuing, g.active] {
        a.op(Opcode::SetGlobal { global: gl, src: b });
    }
    a.op(Opcode::Null { dst: pl });
    a.op(Opcode::SetGlobal {
        global: g.leader,
        src: pl,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, p, vec![], r.0, a.finish())
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let (bool_t, f64_t) = (p.t.bool_, p.t.f64_);
    let g = Globals {
        on: add_global(code, bool_t),
        manual: add_global(code, bool_t),
        manual_at: add_global(code, f64_t),
        issuing: add_global(code, bool_t),
        active: add_global(code, bool_t),
        next: add_global(code, f64_t),
        leader: add_global(code, p.bp_t),
    };
    let notify = add_notify(code, &p)?;
    let cancel = add_cancel(code, &p, &g, notify)?;
    let update = add_update(code, &p, &g, notify)?;
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
    for (fi, game_f, what) in &p.hooks {
        let f = &mut code.functions[*fi];
        f.regs.push(p.game_t);
        let gr = Reg((f.regs.len() - 1) as u32);
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
                Opcode::JNull { reg: gr, offset: 1 },
                Opcode::Call1 {
                    dst: v,
                    fun: cancel,
                    arg0: gr,
                },
            ],
        );
        eprintln!("patched follow fn@{}: {what} cancels follow", f.findex.0);
    }
    eprintln!(
        "follow: notify fn@{}, cancel fn@{}, update fn@{}, reset fn@{}; hotkey F, start {START}, gap {GAP}, tick {TICK}s",
        notify.0, cancel.0, update.0, reset.0
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

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// `b` is `a` with `n` ops inserted at `at`: every original jump keeps its target.
    fn shifted(a: &Function, b: &Function, at: usize, n: usize) {
        assert_eq!(b.ops.len(), a.ops.len() + n);
        let map = |t: usize| if t < at { t } else { t + n };
        for i in 0..a.ops.len() {
            let tb: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), tb, "fn@{} op {i}", a.findex.0);
            if tb.is_empty() {
                assert_eq!(ops(&b.ops[map(i)..=map(i)]), ops(&a.ops[i..=i]));
            }
        }
        for i in 0..b.ops.len() {
            for t in jump_targets(b, i) {
                assert!(t < b.ops.len(), "fn@{} op {i} out of range", b.findex.0);
            }
        }
    }

    fn assignable(code: &Bytecode, from: RefType, to: RefType) -> bool {
        from == to
            || matches!(code.types[to.0], Type::Dyn)
            || (code.types[from.0].get_type_obj().is_some()
                && code.types[to.0].get_type_obj().is_some()
                && is_subclass(code, from, to).unwrap_or(false))
    }

    fn field_type(code: &Bytecode, t: RefType, f: RefField) -> RefType {
        match &code.types[t.0] {
            Type::Virtual { fields } => fields[f.0].t,
            _ => obj(code, t).expect("object").fields[f.0].t,
        }
    }

    fn callee(code: &Bytecode, f: RefFun) -> TypeFun {
        let t = code
            .functions
            .iter()
            .find(|g| g.findex == f)
            .map(|g| g.t)
            .or_else(|| code.natives.iter().find(|n| n.findex == f).map(|n| n.t))
            .expect("callee");
        t.as_fun(code).expect("fun type").clone()
    }

    /// Register types agree with every field, global, call and arithmetic op in `range`.
    fn check_types(code: &Bytecode, f: &Function, range: std::ops::Range<usize>) {
        let rt = |r: &Reg| f.regs[r.0 as usize];
        let is = |r: &Reg, want: fn(&Type) -> bool| want(&code.types[rt(r).0]);
        for i in range {
            let op = &f.ops[i];
            let ok = match op {
                Opcode::Field { dst, obj, field } => {
                    assignable(code, field_type(code, rt(obj), *field), rt(dst))
                }
                Opcode::SetField { obj, field, src } => {
                    assignable(code, rt(src), field_type(code, rt(obj), *field))
                }
                Opcode::GetGlobal { dst, global } => {
                    assignable(code, code.globals[global.0], rt(dst))
                }
                Opcode::SetGlobal { global, src } => {
                    assignable(code, rt(src), code.globals[global.0])
                }
                Opcode::Mov { dst, src } => assignable(code, rt(src), rt(dst)),
                Opcode::Bool { dst, .. } | Opcode::Not { dst, .. } => {
                    is(dst, |t| matches!(t, Type::Bool))
                }
                Opcode::Int { dst, .. } | Opcode::Incr { dst } => {
                    is(dst, |t| matches!(t, Type::I32))
                }
                Opcode::Float { dst, .. } => is(dst, |t| matches!(t, Type::F64)),
                Opcode::Add { dst, a, b }
                | Opcode::Sub { dst, a, b }
                | Opcode::Mul { dst, a, b }
                | Opcode::SDiv { dst, a, b }
                | Opcode::And { dst, a, b } => {
                    rt(dst) == rt(a)
                        && rt(a) == rt(b)
                        && is(dst, |t| matches!(t, Type::I32 | Type::F64))
                }
                Opcode::JSLt { a, b, .. }
                | Opcode::JSGte { a, b, .. }
                | Opcode::JSLte { a, b, .. } => rt(a) == rt(b),
                Opcode::JEq { a, b, .. } | Opcode::JNotEq { a, b, .. } => {
                    assignable(code, rt(a), rt(b)) || assignable(code, rt(b), rt(a))
                }
                Opcode::JTrue { cond, .. } | Opcode::JFalse { cond, .. } => {
                    is(cond, |t| matches!(t, Type::Bool))
                }
                Opcode::Ref { dst, src } => {
                    matches!(code.types[rt(dst).0], Type::Ref(t) if t == rt(src))
                }
                Opcode::Call0 { dst, fun } => {
                    let t = callee(code, *fun);
                    t.args.is_empty() && assignable(code, t.ret, rt(dst))
                }
                Opcode::Call1 { dst, fun, arg0 } => {
                    let t = callee(code, *fun);
                    t.args.len() == 1
                        && assignable(code, rt(arg0), t.args[0])
                        && (assignable(code, t.ret, rt(dst))
                            || is(dst, |t| matches!(t, Type::Void)))
                }
                Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                } => {
                    let t = callee(code, *fun);
                    t.args.len() == 2
                        && assignable(code, rt(arg0), t.args[0])
                        && assignable(code, rt(arg1), t.args[1])
                        && (assignable(code, t.ret, rt(dst))
                            || is(dst, |t| matches!(t, Type::Void)))
                }
                Opcode::Call3 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                } => {
                    let t = callee(code, *fun);
                    t.args.len() == 3
                        && [arg0, arg1, arg2]
                            .iter()
                            .zip(&t.args)
                            .all(|(r, a)| assignable(code, rt(r), *a))
                        && (assignable(code, t.ret, rt(dst))
                            || is(dst, |t| matches!(t, Type::Void)))
                }
                Opcode::Call4 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                    arg3,
                } => {
                    let t = callee(code, *fun);
                    t.args.len() == 4
                        && [arg0, arg1, arg2, arg3]
                            .iter()
                            .zip(&t.args)
                            .all(|(r, a)| assignable(code, rt(r), *a))
                        && (assignable(code, t.ret, rt(dst))
                            || is(dst, |t| matches!(t, Type::Void)))
                }
                Opcode::CallN { .. } => panic!("unexpected CallN"),
                _ => true,
            };
            assert!(
                ok,
                "fn@{} op {i} {op:?}: register types do not match",
                f.findex.0
            );
        }
    }

    /// Jumps stay in range, backward jumps land on a Label, the function ends in Ret.
    fn check_flow(f: &Function) {
        let n = f.ops.len();
        for i in 0..n {
            for t in jump_targets(f, i) {
                assert!(t < n, "fn@{} op {i} jumps out of range", f.findex.0);
                if t <= i {
                    assert!(
                        matches!(f.ops[t], Opcode::Label),
                        "fn@{} op {i}",
                        f.findex.0
                    );
                }
            }
        }
        assert!(matches!(f.ops[n - 1], Opcode::Ret { .. }));
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// four functions are appended and well typed, only World.update,
    /// World.updateSprint, World.dispose and the three manual-move entries change (ops inserted,
    /// every original jump kept), the image round-trips, and a second pass
    /// changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_follow(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 4);
        assert_eq!(back.types.len(), orig.types.len() + 4);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        assert_eq!(back.floats[..orig.floats.len()], orig.floats[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        // 7 state globals (zero-initialised: no constant) + the two toast texts.
        assert_eq!(back.globals.len(), orig.globals.len() + 9);
        let added = &back.globals[orig.globals.len()..orig.globals.len() + 7];
        assert_eq!(
            added,
            [p.t.bool_, p.t.bool_, p.t.f64_, p.t.bool_, p.t.bool_, p.t.f64_, p.bp_t]
        );
        for g in orig.globals.len()..orig.globals.len() + 7 {
            assert!(!back.globals_initializers.contains_key(&RefGlobal(g)));
        }
        let texts: Vec<Option<&str>> = (orig.globals.len() + 7..back.globals.len())
            .map(|g| const_str(&back, RefGlobal(g)))
            .collect();
        assert_eq!(texts, [Some(FOLLOW_ON), Some(FOLLOW_OFF)]);

        let mut touched = vec![(p.update_fi, p.update_at, 1), (p.sprint_fi, p.sprint_at, 2)];
        touched.extend(p.hooks.iter().map(|(fi, _, _)| (*fi, 0, 3)));
        touched.push((p.dispose_fi, 0, 1));
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            match touched.iter().find(|(fi, _, _)| *fi == i) {
                Some(&(_, at, n)) => {
                    shifted(a, b, at, n);
                    assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
                    check_types(&back, b, at..at + n);
                }
                None => assert!(
                    ops(&a.ops) == ops(&b.ops) && a.regs == b.regs && a.debug_info == b.debug_info,
                    "function #{i} (fn@{}) changed",
                    a.findex.0
                ),
            }
        }
        let (notify, cancel, update, reset) = (
            &back.functions[nf],
            &back.functions[nf + 1],
            &back.functions[nf + 2],
            &back.functions[nf + 3],
        );
        // World.dispose starts with followReset(), which clears the toggle and the leader.
        assert!(
            matches!(back.functions[p.dispose_fi].ops[0], Opcode::Call0 { fun, .. } if fun == reset.findex)
        );
        let base = orig.globals.len();
        for gi in [0, 1, 3, 4, 6] {
            assert!(reset
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::SetGlobal { global, .. } if global.0 == base + gi)));
        }
        for f in [notify, cancel, update, reset] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }

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
        // Each manual-move entry starts with `if (this.game != null) followCancel(this.game)`.
        for (fi, _, what) in &p.hooks {
            let h = &back.functions[*fi];
            assert!(
                matches!(h.ops[0], Opcode::Field { obj: Reg(0), .. }),
                "{what}"
            );
            assert_eq!(jump_targets(h, 1), vec![3], "{what}");
            assert!(
                matches!(h.ops[2], Opcode::Call1 { fun, .. } if fun == cancel.findex),
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
}
