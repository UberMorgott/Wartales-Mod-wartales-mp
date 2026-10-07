// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: no more "waiting for the other players" after the world is loaded.
//
// Every co-op consensus in the world is decided on the host (isAuth) and every
// one of them already has a vanilla FORCE path: hold the button until the bar
// fills (`Button` onPush -> `Window.forcedClick` -> `netForcedClick__impl` sets
// `ButtonData.forced` and re-triggers the click). This pass makes the plain
// click take that forced path, so the first player who clicks decides:
//
//   G1 Button.waitAllPlayers (host click closure, Button.hx:91)
//        if (bd.forced) { cb(); return; }  push player; if (all) { reset; cb(); }
//      -> the `forced` branch is always taken (the JFalse gets offset 0), behind
//      a repeat guard: a click on the same button data within 2 s of the click
//      that ran it (same player) or 5 s (another player) returns without cb().
//      Vanilla swallowed those in the vote: the button acted once, when the last
//      vote came in. Without the guard a double click, or two players clicking
//      Fight/Leave/Continue within the round trip, ran the action twice (two
//      startAttack calls, two leave requests into the mode-switch barrier). The
//      record is per button data, in two new global ObjectMaps (bd -> time,
//      bd -> player), so clicking another button in between does not reset it,
//      and a later click still acts: a button the game re-uses while its window
//      stays open (rest after a cancelled confirm, tutorial pages) keeps working.
//      While the host's mode-switch barrier runs (Controller.lockSyncMode, or
//      clients in waitLocks) every wait-all click is dropped, however late:
//      syncLeaveMode/syncEnterMode queue a second request behind the first and
//      run it afterwards. Known limit: a click after 2 s / 5 s while the first
//      action waits on an open confirm acts again (a second confirm); there is
//      no generic "action pending" state, and vanilla's own hold-to-force path
//      let every later click act, unguarded.
//      onPush (Button.hx:153-163), when the pushing player has not voted yet,
//      calls `interactive.onClick(e)` with the push event: the click handler
//      (host: Button.hx:82-90) takes it as a left click, i.e. a vote cast on
//      mouse DOWN (it turns a push into an un-vote only in gamepad mode for a
//      player already in the list, which that call excludes). With the vote
//      acting at once the release would click again, so onPush no longer
//      calls it (its `JNotLt` becomes `JAlways`) and the hold bar never starts
//      (its `players + me < total` JSLt gets offset 0 and falls into `Ret`). A
//      gamepad press has no release: Button.doPadClick called onPush alone and
//      now calls `interactive.onClick` instead, the same entry a mouse click uses.
//      Covers town/location, camp fire, rest confirm/report, debriefs, group
//      fight, travel post, trade route, sport, stealth, tavern resume, pit,
//      tutorial, welcome, troop choice windows.
//   G2 Place.setLeaveState__impl(leave, force): `if (leave) force = leave;`
//      PlaceView.tryClose then leaves on `leaveState.forced` (vanilla force).
//   G3 Controller.playerSetRestState__impl(set, force):
//        if (set && game != null) {
//            if (!Std.isOfType(game.mode, CampMode)) return false;
//            if (!game.resting) force = true;
//        }
//      netStartRest / CampFire.rest then start on `restState.forced`. A late
//      request (camp already over) is dropped before it touches restState:
//      CampFire.rest casts `game.mode` to CampMode and would throw on the host.
//      A request during the rest does not force, so no stale flag survives it.
//   G4 Dialog.checkAllReady: `PREFS.coopSkipDialogInstanlty` reads as true, so
//      the first player's "next" advances the dialog (vanilla host option).
//   G6 "someone has a window open" lock: every client reports
//      `BasePlayer.hasWindowOpened` (GameUI.update, synced), and
//      `Game.anyPlayerLocked(true)` / `Player.canCamp` refuse camp, leaving camp,
//      camp mode changes, leaving the tavern and tavern customer actions while
//      ANY player has a window open (e.g. one player reading a UnitInfo sheet
//      blocks everyone). Both reads become `false`: a window is local and never
//      blocks. `lockedWith` (a player busy with an NPC, chest, craft or
//      gathering) still blocks, as it does for the vanilla force path; for a
//      place leave, force_leave.rs closes those windows first and then leaves.
//
// Not touched (see coop-gates notes): mode-transition load barriers
// (Controller.waitForClients/waitForUnlock/waitForHost), battle round sync
// (BattleMode.waitForPlayers/askWhenReady), owner-only NetConfirm, and the
// world-map proximity gathering (BasePlayer.waitAction).
//
// Game modes are global (Game.currentModes is replicated and the host drives
// every switch through Controller.syncEnterMode/syncLeaveMode), so when the
// host transitions, every client's old mode is disposed together with the
// windows it owns: nobody stays in a town, camp or tavern screen.
//
// G1 click, G2, G3, G4 decide on the host; the G1 push/pad changes and G6 run
// on every machine (input and UI enabling on clients), so all players must run
// the same image. Each gate is validated on its own before it is edited; a
// mismatch skips that gate (logged) and the others still apply.

use super::*;

/// The functions a method's closures (and their closures, one level down) point to.
fn closures(code: &Bytecode, f: &Function) -> Vec<usize> {
    let mut out = vec![];
    let push = |findex: RefFun, out: &mut Vec<usize>| {
        if let Ok(i) = fun_index(code, findex) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    };
    for op in &f.ops {
        if let Opcode::InstanceClosure { fun, .. } = op {
            push(*fun, &mut out);
        }
    }
    let first = out.clone();
    for i in first {
        for op in &code.functions[i].ops {
            if let Opcode::InstanceClosure { fun, .. } = op {
                push(*fun, &mut out);
            }
        }
    }
    out
}

// ---------- G1: Button.waitAllPlayers ----------

/// A repeat click on the same button within this many seconds of the click that
/// ran it is dropped: by the same player (double click, impatient re-click) ...
const REPEAT_SAME_S: f64 = 2.0;
/// ... or by another player (both clicked within the network round trip, or
/// before the host's action reached their screen).
const REPEAT_OTHER_S: f64 = 5.0;

struct ButtonPlan {
    click_fi: usize,
    click_at: usize,
    /// The click closure's `bd` (button data) register and the op loading the button from its env.
    bd: Reg,
    load_button: Opcode,
    button_t: RefType,
    game_f: RefField,
    game_t: RefType,
    action_player: RefFun,
    player_t: RefType,
    sys_time: RefFun,
    f64_t: RefType,
    dyn_t: RefType,
    /// haxe.ds.ObjectMap: per-button-data records (when it last acted, for whom).
    map_t: RefType,
    map_new: RefFun,
    map_get: RefFun,
    map_set: RefFun,
    /// Game.ctrl, Controller.lockSyncMode, Controller.waitLocks(.length).
    ctrl_f: RefField,
    ctrl_t: RefType,
    lock_f: RefField,
    locks_f: RefField,
    locks_t: RefType,
    len_f: RefField,
    bool_t: RefType,
    i32_t: RefType,
    hold_fi: usize,
    hold_at: usize,
    /// onPush's `JNotLt 0.0, delay` guarding "cast my vote now" (`interactive.onClick(e)`).
    push_at: usize,
    /// Button.doPadClick: the wait-all branch's `Field cb = interactive.onPush`.
    pad_fi: usize,
    pad_at: usize,
    on_click_f: RefField,
}

fn plan_button(code: &Bytecode) -> Result<ButtonPlan> {
    let button_t = obj_type(code, "ui.comp.Button")?;
    let window_t = obj_type(code, "ui.Window")?;
    let set_wait = method(code, button_t, "set_waitAllPlayers")?;
    let is_wait = method(code, button_t, "isWaitAllPlayers")?.findex;
    let get_bd = method(code, window_t, "getButtonData")?;
    let bd_t = get_bd.t.as_fun(code).context("getButtonData type")?.ret;
    let (forced_f, _) = field(code, bd_t, "forced")?;
    let wait_until = method(code, obj_type(code, "hxd.WaitEvent")?, "waitUntil")?.findex;

    let cands = closures(code, set_wait);
    // Host click closure: `Field r = bd.forced; JFalse r +3; NullCheck cb; cb(); Ret`.
    let mut clicks = vec![];
    for &fi in &cands {
        let f = &code.functions[fi];
        let o = &f.ops;
        for i in 0..o.len().saturating_sub(4) {
            let Opcode::Field { dst, obj, field } = o[i] else {
                continue;
            };
            if field != forced_f || f.regs[obj.0 as usize] != bd_t {
                continue;
            }
            let ok = matches!(o[i + 1], Opcode::JFalse { cond, offset: 3 } if cond == dst)
                && matches!((&o[i + 2], &o[i + 3]),
                    (Opcode::NullCheck { reg }, Opcode::CallClosure { fun, args, .. }) if fun == reg && args.is_empty())
                && matches!(o[i + 4], Opcode::Ret { .. });
            if ok {
                clicks.push((fi, i + 1));
            }
        }
    }
    let [(click_fi, click_at)] = clicks[..] else {
        bail!(
            "button: expected one `if (bd.forced) cb()` site, found {}",
            clicks.len()
        );
    };
    // The repeat guard: the button (closure env), its game's action player and the clock.
    let cf = &code.functions[click_fi];
    let Opcode::Field { obj: bd, .. } = cf.ops[click_at - 1] else {
        unreachable!()
    };
    let load_button = cf.ops[..click_at]
        .iter()
        .find(|op| matches!(op, Opcode::EnumField { dst, value: Reg(0), .. } if cf.regs[dst.0 as usize] == button_t))
        .cloned()
        .context("button: click closure does not load its button from the env")?;
    let (game_f, game_t) = field(code, button_t, "game")?;
    let action_player = method(code, game_t, "getActionPlayer")?;
    let player_t = action_player
        .t
        .as_fun(code)
        .context("getActionPlayer type")?
        .ret;
    if fun_args(code, action_player).len() != 1 || code.types[player_t.0].get_type_obj().is_none() {
        bail!("button: Game.getActionPlayer is not (Game) -> player object");
    }
    let f64_t = prim_type(code, "F64", |t| matches!(t, Type::F64))?;
    let sys_time = {
        let hits: Vec<RefFun> = code
            .natives
            .iter()
            .filter(|n| {
                s(code, n.name) == "sys_time"
                    && n.t
                        .as_fun(code)
                        .is_some_and(|t| t.args.is_empty() && t.ret == f64_t)
            })
            .map(|n| n.findex)
            .collect();
        let [f] = hits[..] else {
            bail!("button: expected one native sys_time, found {}", hits.len());
        };
        f
    };
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let map_t = obj_type(code, "haxe.ds.ObjectMap")?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let sig = |name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, map_t, name)?;
        if fun_args(code, f) != args || f.t.as_fun(code).map(|t| t.ret) != Some(ret) {
            bail!("button: ObjectMap.{name} has an unexpected signature");
        }
        Ok(f.findex)
    };
    let map_new = sig("__constructor__", &[map_t], void_t)?;
    let map_get = sig("get", &[map_t, dyn_t], dyn_t)?;
    let map_set = sig("set", &[map_t, dyn_t, dyn_t], void_t)?;
    // The host's mode-switch barrier: Controller.syncLeaveMode/syncEnterMode
    // queue a second request while `lockSyncMode` is set and run it after the
    // first, and `waitLocks` holds the clients still to answer.
    let (ctrl_f, ctrl_t) = field(code, game_t, "ctrl")?;
    if obj(code, ctrl_t).map(|o| s(code, o.name)).ok() != Some("st.Controller") {
        bail!("button: Game.ctrl is not st.Controller");
    }
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let (lock_f, lock_t) = field(code, ctrl_t, "lockSyncMode")?;
    let (locks_f, locks_t) = field(code, ctrl_t, "waitLocks")?;
    let (len_f, len_t) = field(code, locks_t, "length")?;
    if lock_t != bool_t || len_t != i32_t {
        bail!("button: Controller.lockSyncMode / waitLocks.length have unexpected types");
    }
    // onPush closure: calls isWaitAllPlayers and WaitEvent.waitUntil; `JSLt +1; Ret` skips the hold start.
    let mut holds = vec![];
    for &fi in &cands {
        let f = &code.functions[fi];
        let calls = |target: RefFun| {
            f.ops.iter().any(|op| {
                matches!(op, Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } if *fun == target)
            })
        };
        if !calls(is_wait) || !calls(wait_until) {
            continue;
        }
        for i in 0..f.ops.len().saturating_sub(1) {
            if matches!(f.ops[i], Opcode::JSLt { offset: 1, .. })
                && matches!(f.ops[i + 1], Opcode::Ret { .. })
            {
                holds.push((fi, i));
            }
        }
    }
    let [(hold_fi, hold_at)] = holds[..] else {
        bail!(
            "button: expected one hold-to-force start test, found {}",
            holds.len()
        );
    };
    // Same onPush: `JNotLt 0.0, delay +k; ...; Field cb = interactive.onClick; ...; cb(e)`
    // casts the vote on mouse DOWN; the release then clicks again.
    let f = &code.functions[hold_fi];
    let mut pushes = vec![];
    for i in 0..hold_at {
        let Opcode::JNotLt { offset, .. } = f.ops[i] else {
            continue;
        };
        let end = i + 1 + offset as usize;
        if offset < 2 || end > hold_at {
            continue;
        }
        let calls_back =
            matches!(&f.ops[end - 1], Opcode::CallClosure { args, .. } if args.len() == 1);
        let reads_on_click = f.ops[i + 1..end].iter().any(|op| {
            matches!(op, Opcode::Field { obj, field, .. }
                if field_name(code, f.regs[obj.0 as usize], *field) == Some("onClick"))
        });
        if calls_back && reads_on_click {
            pushes.push(i);
        }
    }
    let [push_at] = pushes[..] else {
        bail!(
            "button: expected one vote-on-push block, found {}",
            pushes.len()
        );
    };
    // Button.doPadClick: a gamepad press of a wait-all button calls onPush alone
    // (no release follows). It now calls onClick, the same entry a mouse click uses.
    let pad = method(code, button_t, "doPadClick")?;
    let pad_fi = fun_index(code, pad.findex)?;
    let reads: Vec<(usize, RefType)> = pad
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Field { obj, field, .. }
                if field_name(code, pad.regs[obj.0 as usize], *field) == Some("onPush") =>
            {
                Some((i, pad.regs[obj.0 as usize]))
            }
            _ => None,
        })
        .collect();
    let [(pad_at, inter_t)] = reads[..] else {
        bail!(
            "button: expected one onPush read in doPadClick, found {}",
            reads.len()
        );
    };
    if !pad.ops[..pad_at]
        .iter()
        .any(|op| matches!(op, Opcode::Call1 { fun, .. } if *fun == is_wait))
    {
        bail!("button: doPadClick's onPush call is not behind isWaitAllPlayers");
    }
    let (on_push_f, on_push_t) = field(code, inter_t, "onPush")?;
    let (on_click_f, on_click_t) = field(code, inter_t, "onClick")?;
    let Opcode::Field { field: read_f, .. } = pad.ops[pad_at] else {
        unreachable!()
    };
    if read_f != on_push_f || on_push_t != on_click_t {
        bail!("button: interactive.onPush and onClick differ in type");
    }
    Ok(ButtonPlan {
        click_fi,
        click_at,
        bd,
        load_button,
        button_t,
        game_f,
        game_t,
        action_player: action_player.findex,
        player_t,
        sys_time,
        f64_t,
        dyn_t,
        map_t,
        map_new,
        map_get,
        map_set,
        ctrl_f,
        ctrl_t,
        lock_f,
        locks_f,
        locks_t,
        len_f,
        bool_t,
        i32_t,
        hold_fi,
        hold_at,
        push_at,
        pad_fi,
        pad_at,
        on_click_f,
    })
}

/// Ops of the repeat guard inserted in front of the forced path's `cb()`:
///
///   p = button.game.getActionPlayer();
///   c = game.ctrl; if (c != null && (c.lockSyncMode || c.waitLocks.length > 0)) return;
///   now = sys_time();
///   if (times == null) { times = new ObjectMap(); players = new ObjectMap(); }
///   if (now - (times.get(bd) ?? 0) < (players.get(bd) == p ? SAME : OTHER)) return;
///   times.set(bd, now); players.set(bd, p);
///
/// Vanilla swallowed repeat clicks in the vote (the button acted once, when the
/// last vote arrived); with the first click acting, a double click or a second
/// player's click would run the action again. The record is per button data
/// (window button or shared place/camp state), so clicking another button in
/// between does not reset it. A later click still acts, so a button the game
/// re-uses (rest after a cancelled confirm, tutorial pages) keeps working.
fn repeat_guard(
    code: &mut Bytecode,
    p: &ButtonPlan,
    globals: [hlbc::types::RefGlobal; 2],
) -> Vec<Opcode> {
    let same = float_const(code, REPEAT_SAME_S);
    let other = float_const(code, REPEAT_OTHER_S);
    let [g_times, g_players] = globals;
    let f = &mut code.functions[p.click_fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (btn, game, player, now, times, players, key, val, dt, lim, last_player) = (
        reg(p.button_t),
        reg(p.game_t),
        reg(p.player_t),
        reg(p.f64_t),
        reg(p.map_t),
        reg(p.map_t),
        reg(p.dyn_t),
        reg(p.dyn_t),
        reg(p.f64_t),
        reg(p.f64_t),
        reg(p.player_t),
    );
    // The forced path's own `return` (its cb() result register, Void).
    let Opcode::Ret { ret: void } = f.ops[p.click_at + 3] else {
        unreachable!()
    };
    let mut load = p.load_button.clone();
    if let Opcode::EnumField { dst, .. } = &mut load {
        *dst = btn;
    }
    let mut ops = vec![
        load,                           // 0
        Opcode::NullCheck { reg: btn }, // 1
        Opcode::Field {
            dst: game,
            obj: btn,
            field: p.game_f,
        }, // 2
        Opcode::NullCheck { reg: game }, // 3
        Opcode::Call1 {
            dst: player,
            fun: p.action_player,
            arg0: game,
        }, // 4
        Opcode::Call0 {
            dst: now,
            fun: p.sys_time,
        }, // 5
        Opcode::GetGlobal {
            dst: times,
            global: g_times,
        }, // 6
        Opcode::JNotNull {
            reg: times,
            offset: 0,
        }, // 7 -> 14
        Opcode::New { dst: times },     // 8
        Opcode::Call1 {
            dst: void,
            fun: p.map_new,
            arg0: times,
        }, // 9
        Opcode::SetGlobal {
            global: g_times,
            src: times,
        }, // 10
        Opcode::New { dst: players },   // 11
        Opcode::Call1 {
            dst: void,
            fun: p.map_new,
            arg0: players,
        }, // 12
        Opcode::SetGlobal {
            global: g_players,
            src: players,
        }, // 13
        Opcode::GetGlobal {
            dst: players,
            global: g_players,
        }, // 14
        // An object goes to a Dyn argument as is (what the compiler emits).
        Opcode::Mov {
            dst: key,
            src: p.bd,
        }, // 15
        Opcode::Call2 {
            dst: val,
            fun: p.map_get,
            arg0: times,
            arg1: key,
        }, // 16
        Opcode::SafeCast { dst: dt, src: val }, // 17: null (never acted) -> 0
        Opcode::Sub {
            dst: dt,
            a: now,
            b: dt,
        }, // 18
        Opcode::Call2 {
            dst: val,
            fun: p.map_get,
            arg0: players,
            arg1: key,
        }, // 19
        Opcode::SafeCast {
            dst: last_player,
            src: val,
        }, // 20
        Opcode::Float {
            dst: lim,
            ptr: same,
        }, // 21
        Opcode::JEq {
            a: player,
            b: last_player,
            offset: 0,
        }, // 22 -> 24
        Opcode::Float {
            dst: lim,
            ptr: other,
        }, // 23
        Opcode::JSGte {
            a: dt,
            b: lim,
            offset: 0,
        }, // 24 -> FIRE
        Opcode::Ret { ret: void },              // 25 repeat: dropped
        Opcode::ToDyn { dst: val, src: now },   // 26 FIRE
        Opcode::Call3 {
            dst: void,
            fun: p.map_set,
            arg0: times,
            arg1: key,
            arg2: val,
        }, // 27
        Opcode::Mov {
            dst: val,
            src: player,
        }, // 28
        Opcode::Call3 {
            dst: void,
            fun: p.map_set,
            arg0: players,
            arg1: key,
            arg2: val,
        }, // 29
    ];
    // Ops 5..16: while the host's mode-switch barrier runs, every click is
    // dropped (a second leave/enter request would be queued behind the first and
    // run after it, whenever it arrives); then the timing check (ops 16..).
    let (ctrl, lock, locks, len, zero) = {
        let f = &mut code.functions[p.click_fi];
        let mut reg = |t: RefType| {
            f.regs.push(t);
            Reg((f.regs.len() - 1) as u32)
        };
        (
            reg(p.ctrl_t),
            reg(p.bool_t),
            reg(p.locks_t),
            reg(p.i32_t),
            reg(p.i32_t),
        )
    };
    let zero_c = int_const(code, 0);
    let barrier = vec![
        Opcode::Field {
            dst: ctrl,
            obj: game,
            field: p.ctrl_f,
        }, // 5
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        }, // 6 -> 16
        Opcode::Field {
            dst: lock,
            obj: ctrl,
            field: p.lock_f,
        }, // 7
        Opcode::JFalse {
            cond: lock,
            offset: 0,
        }, // 8 -> 10
        Opcode::Ret { ret: void }, // 9 a mode switch is running
        Opcode::Field {
            dst: locks,
            obj: ctrl,
            field: p.locks_f,
        }, // 10
        Opcode::JNull {
            reg: locks,
            offset: 0,
        }, // 11 -> 16
        Opcode::Field {
            dst: len,
            obj: locks,
            field: p.len_f,
        }, // 12
        Opcode::Int {
            dst: zero,
            ptr: zero_c,
        }, // 13
        Opcode::JSGte {
            a: zero,
            b: len,
            offset: 0,
        }, // 14 -> 16 (no client is locked)
        Opcode::Ret { ret: void }, // 15 clients are still in the barrier
    ];
    let k = barrier.len();
    ops.splice(5..5, barrier);
    let shift = |i: usize| if i >= 5 { i + k } else { i };
    let mut jumps: Vec<(usize, usize)> = [(7, 14), (22, 24), (24, 26)]
        .iter()
        .map(|&(a, b)| (shift(a), shift(b)))
        .collect();
    jumps.extend([(6, 16), (8, 10), (11, 16), (14, 16)]);
    resolve_jumps(&mut ops, &jumps);
    ops
}

fn apply_button(code: &mut Bytecode, p: &ButtonPlan) {
    let globals = [p.map_t, p.map_t].map(|t| {
        code.globals.push(t);
        hlbc::types::RefGlobal(code.globals.len() - 1)
    });
    let guard = repeat_guard(code, p, globals);
    let n = guard.len();
    let f = &mut code.functions[p.click_fi];
    insert_ops(f, p.click_at + 1, guard);
    // The forced test falls through into the guard (insert_ops moved its target past it).
    if let Opcode::JFalse { offset, .. } = &mut f.ops[p.click_at] {
        *offset = 0;
    }
    eprintln!(
        "patched coop gate button-click fn@{} op {}: every click takes the forced path, a {n}-op guard drops repeats",
        f.findex.0, p.click_at
    );
    let f = &mut code.functions[p.hold_fi];
    if let Opcode::JSLt { offset, .. } = &mut f.ops[p.hold_at] {
        *offset = 0;
    }
    if let Opcode::JNotLt { offset, .. } = f.ops[p.push_at] {
        f.ops[p.push_at] = Opcode::JAlways { offset };
    }
    eprintln!(
        "patched coop gate button-push fn@{}: no vote on mouse down (op {}), no hold-to-force bar (op {})",
        f.findex.0, p.push_at, p.hold_at
    );
    let f = &mut code.functions[p.pad_fi];
    if let Opcode::Field { dst, obj, .. } = f.ops[p.pad_at] {
        f.ops[p.pad_at] = Opcode::Field {
            dst,
            obj,
            field: p.on_click_f,
        };
    }
    eprintln!(
        "patched coop gate button-pad fn@{} op {}: a gamepad press clicks",
        f.findex.0, p.pad_at
    );
}
// ---------- G2/G3: a request implies force ----------

/// Rest requests: a request that reaches the host when `game.mode` is not a
/// CampMode (camp already over) is dropped before it touches restState,
/// because netStartRest -> CampFire.rest casts `game.mode` to CampMode and
/// throws otherwise; one arriving while `game.resting` does not force, so no
/// stale `forced` outlives the rest.
struct ModeGuard {
    game_f: RefField,
    game_t: RefType,
    mode_f: RefField,
    mode_t: RefType,
    class_g: hlbc::types::RefGlobal,
    class_t: RefType,
    check: RefFun,
    bool_t: RefType,
    resting_f: RefField,
}

struct RequestPlan {
    fi: usize,
    guard: Option<ModeGuard>,
}

fn mode_guard(code: &Bytecode, this_class: &str, mode_class: &str) -> Result<ModeGuard> {
    let (game_f, game_t) = field(code, obj_type(code, this_class)?, "game")?;
    let (mode_f, mode_t) = field(code, game_t, "mode")?;
    let mode_cls = obj(code, obj_type(code, mode_class)?)?;
    // HL stores an object's class global 1-based (0 = none).
    let class_g = hlbc::types::RefGlobal(
        mode_cls
            .global
            .0
            .checked_sub(1)
            .with_context(|| format!("{mode_class}: no class global"))?,
    );
    let class_t = *code
        .globals
        .get(class_g.0)
        .with_context(|| format!("{mode_class}: class global out of range"))?;
    let (pkg, cls) = mode_class.rsplit_once('.').unwrap_or(("", mode_class));
    let want = if pkg.is_empty() {
        format!("${cls}")
    } else {
        format!("{pkg}.${cls}")
    };
    if obj(code, class_t).ok().map(|o| s(code, o.name)) != Some(want.as_str()) {
        bail!("{mode_class}: class global is not {want}");
    }
    let base_t = obj_type(code, "hl.BaseType")?;
    let check = method(code, base_t, "check")?;
    let args = fun_args(code, check);
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    if args.len() != 2 || check.t.as_fun(code).map(|t| t.ret) != Some(bool_t) {
        bail!("hl.BaseType.check is not (BaseType, v) -> Bool");
    }
    let (resting_f, resting_t) = field(code, game_t, "resting")?;
    if resting_t != bool_t {
        bail!("Game.resting is not Bool");
    }
    Ok(ModeGuard {
        resting_f,
        game_f,
        game_t,
        mode_f,
        mode_t,
        class_g,
        class_t,
        check: check.findex,
        bool_t,
    })
}

/// `name(this, request: Bool, force: Bool)` whose body stores `force` into a `forced` field.
fn plan_request_forces(
    code: &Bytecode,
    class: &str,
    name: &str,
    guard_mode: Option<&str>,
) -> Result<RequestPlan> {
    let t = obj_type(code, class)?;
    let f = method(code, t, name)?;
    let fi = fun_index(code, f.findex)?;
    let args = fun_args(code, f);
    if args.len() != 3
        || !matches!(code.types[args[1].0], Type::Bool)
        || !matches!(code.types[args[2].0], Type::Bool)
    {
        bail!("{name}: signature is not (this, Bool, Bool)");
    }
    let (req, force) = (Reg(1), Reg(2));
    if matches!(f.ops.first(), Some(Opcode::JFalse { cond, .. }) if *cond == req)
        && f.ops
            .iter()
            .take(12)
            .any(|op| matches!(op, Opcode::Mov { dst, src } if *dst == force && *src == req))
    {
        bail!("{name}: already applied");
    }
    let stores = f
        .ops
        .iter()
        .filter(|op| {
            matches!(op, Opcode::SetField { obj, field, src }
                if *src == force && field_name(code, f.regs[obj.0 as usize], *field) == Some("forced"))
        })
        .count();
    if stores != 1 {
        bail!("{name}: expected one `state.forced = force`, found {stores}");
    }
    let guard = match guard_mode {
        Some(m) => {
            let g = mode_guard(code, class, m)?;
            if f.t.as_fun(code).map(|t| t.ret) != Some(g.bool_t) {
                bail!("{name}: does not return Bool");
            }
            Some(g)
        }
        None => None,
    };
    Ok(RequestPlan { fi, guard })
}

fn apply_request_forces(code: &mut Bytecode, p: &RequestPlan, what: &str) {
    let f = &mut code.functions[p.fi];
    let (req, force) = (Reg(1), Reg(2));
    let ops = match &p.guard {
        None => vec![
            Opcode::JFalse {
                cond: req,
                offset: 1,
            },
            Opcode::Mov {
                dst: force,
                src: req,
            },
        ],
        Some(g) => {
            let mut reg = |t: RefType| {
                f.regs.push(t);
                Reg((f.regs.len() - 1) as u32)
            };
            let (game, mode, cls, ok, rest) = (
                reg(g.game_t),
                reg(g.mode_t),
                reg(g.class_t),
                reg(g.bool_t),
                reg(g.bool_t),
            );
            //  0 JFalse req ->11          1 game = this.game      2 JNull game ->11
            //  3 mode = game.mode         4 cls = <class global>  5 ok = BaseType.check(cls, mode)
            //  6 JTrue ok ->8             7 return ok (false: request dropped)
            //  8 rest = game.resting      9 JTrue rest ->11       10 force = req
            // 11 (original op 0)
            vec![
                Opcode::JFalse {
                    cond: req,
                    offset: 10,
                },
                Opcode::GetThis {
                    dst: game,
                    field: g.game_f,
                },
                Opcode::JNull {
                    reg: game,
                    offset: 8,
                },
                Opcode::Field {
                    dst: mode,
                    obj: game,
                    field: g.mode_f,
                },
                Opcode::GetGlobal {
                    dst: cls,
                    global: g.class_g,
                },
                Opcode::Call2 {
                    dst: ok,
                    fun: g.check,
                    arg0: cls,
                    arg1: mode,
                },
                Opcode::JTrue {
                    cond: ok,
                    offset: 1,
                },
                Opcode::Ret { ret: ok },
                Opcode::Field {
                    dst: rest,
                    obj: game,
                    field: g.resting_f,
                },
                Opcode::JTrue {
                    cond: rest,
                    offset: 1,
                },
                Opcode::Mov {
                    dst: force,
                    src: req,
                },
            ]
        }
    };
    let n = ops.len();
    insert_ops(f, 0, ops);
    eprintln!(
        "patched coop gate {what} fn@{}: {n}-op prologue, a request forces it",
        f.findex.0
    );
}
// ---------- G4/G6: a Bool field read becomes a constant ----------

/// A gate that reads Bool field `name` exactly `count` times in function `fi`.
struct BoolRead {
    what: &'static str,
    fi: usize,
    sites: Vec<usize>,
    value: bool,
}

fn plan_bool_read(
    code: &Bytecode,
    what: &'static str,
    fi: usize,
    name: &str,
    count: usize,
    value: bool,
) -> Result<BoolRead> {
    let f = &code.functions[fi];
    let sites: Vec<usize> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Field { dst, obj, field }
                if field_name(code, f.regs[obj.0 as usize], *field) == Some(name)
                    && matches!(code.types[f.regs[dst.0 as usize].0], Type::Bool) =>
            {
                Some(i)
            }
            _ => None,
        })
        .collect();
    if sites.len() != count {
        bail!(
            "{what}: expected {count} `{name}` read(s) in fn@{}, found {}",
            f.findex.0,
            sites.len()
        );
    }
    Ok(BoolRead {
        what,
        fi,
        sites,
        value,
    })
}

/// G4: `Dialog.checkAllReady` reads the host option `coopSkipDialogInstanlty` as on.
fn plan_dialog(code: &Bytecode) -> Result<BoolRead> {
    let f = method(code, obj_type(code, "ui.win.Dialog")?, "checkAllReady")?;
    let fi = fun_index(code, f.findex)?;
    plan_bool_read(code, "dialog", fi, "coopSkipDialogInstanlty", 2, true)
}

/// G6a: `Game.getPlayerLocked`'s filter closure no longer counts an open window
/// (`hasWindowOpened`, reported by every client's GameUI.update) as a lock; a
/// player interacting with an entity (`lockedWith`) still is one.
fn plan_window_lock(code: &Bytecode) -> Result<BoolRead> {
    let f = method(code, obj_type(code, "Game")?, "getPlayerLocked")?;
    let found: Vec<usize> = closures(code, f)
        .into_iter()
        .filter(|&fi| plan_bool_read(code, "", fi, "hasWindowOpened", 1, false).is_ok())
        .collect();
    let [fi] = found[..] else {
        bail!(
            "window-lock: expected one getPlayerLocked filter reading hasWindowOpened, found {}",
            found.len()
        );
    };
    plan_bool_read(code, "window-lock", fi, "hasWindowOpened", 1, false)
}

/// G6b: `Player.canCamp` no longer refuses while another player has a window open.
fn plan_camp_window(code: &Bytecode) -> Result<BoolRead> {
    let f = method(code, obj_type(code, "ent.Player")?, "canCamp")?;
    let fi = fun_index(code, f.findex)?;
    plan_bool_read(code, "camp-window", fi, "hasWindowOpened", 1, false)
}

fn apply_bool_read(code: &mut Bytecode, p: &BoolRead) {
    let f = &mut code.functions[p.fi];
    for &i in &p.sites {
        if let Opcode::Field { dst, .. } = f.ops[i] {
            f.ops[i] = Opcode::Bool {
                dst,
                value: hlbc::types::ValBool(p.value),
            };
        }
    }
    eprintln!(
        "patched coop gate {} fn@{} ops {:?}: read as {}",
        p.what, f.findex.0, p.sites, p.value
    );
}
const REQUESTS: [(&str, &str, &str, Option<&str>); 2] = [
    ("ent.Place", "setLeaveState__impl", "place-leave", None),
    (
        "st.Controller",
        "playerSetRestState__impl",
        "camp-rest",
        Some("world.camp.CampMode"),
    ),
];

/// Applies every gate that matches; logs and skips the ones that do not.
pub(crate) fn patch_coop_gates(code: &mut Bytecode) {
    match plan_button(code) {
        Ok(p) => apply_button(code, &p),
        Err(e) => crate::skipped(format!("coop gate button skipped: {e:#}")),
    }
    for (class, name, what, guard) in REQUESTS {
        match plan_request_forces(code, class, name, guard) {
            Ok(p) => apply_request_forces(code, &p, what),
            Err(e) => crate::skipped(format!("coop gate {what} skipped: {e:#}")),
        }
    }
    for (what, plan) in [
        ("dialog", plan_dialog as fn(&Bytecode) -> Result<BoolRead>),
        ("window-lock", plan_window_lock),
        ("camp-window", plan_camp_window),
    ] {
        match plan(code) {
            Ok(p) => apply_bool_read(code, &p),
            Err(e) => crate::skipped(format!("coop gate {what} skipped: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// `b` is `a` with `n` ops inserted at `at`: every original jump keeps its target.
    fn shifted(a: &Function, b: &Function, at: usize, n: usize) {
        shifted_except(a, b, at, n, usize::MAX);
    }

    /// `shifted`, except for the jump at original op `skip`, which was retargeted.
    fn shifted_except(a: &Function, b: &Function, at: usize, n: usize, skip: usize) {
        assert_eq!(b.ops.len(), a.ops.len() + n);
        let map = |t: usize| if t < at { t } else { t + n };
        for i in (0..a.ops.len()).filter(|&i| i != skip) {
            let tb: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), tb, "fn@{} op {i}", a.findex.0);
        }
        for i in 0..b.ops.len() {
            for t in jump_targets(b, i) {
                assert!(t < b.ops.len(), "fn@{} op {i} out of range", b.findex.0);
            }
        }
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// every gate is found, only its functions change, the image round-trips,
    /// and a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let bp = plan_button(&orig).expect("button");
        let reqs: Vec<RequestPlan> = REQUESTS
            .iter()
            .map(|(c, n, _, g)| plan_request_forces(&orig, c, n, *g).expect(n))
            .collect();
        let reads = [
            plan_dialog(&orig).expect("dialog"),
            plan_window_lock(&orig).expect("window lock"),
            plan_camp_window(&orig).expect("camp window"),
        ];
        let mut touched = vec![bp.click_fi, bp.hold_fi, bp.pad_fi];
        touched.extend(reqs.iter().map(|r| r.fi));
        touched.extend(reads.iter().map(|r| r.fi));

        let mut code = read(&image);
        patch_coop_gates(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(back.types, orig.types);
        assert_eq!(back.strings, orig.strings);
        // Two new globals (the repeat guard's maps), appended.
        let ng = orig.globals.len();
        assert_eq!(&back.globals[..ng], &orig.globals[..]);
        assert_eq!(&back.globals[ng..], &[bp.map_t, bp.map_t]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same =
                ops(&a.ops) == ops(&b.ops) && a.regs == b.regs && a.debug_info == b.debug_info;
            assert_eq!(
                same,
                !touched.contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // G1 click: the forced test falls into the 41-op guard (mode-switch
        // barrier, then the per-button timing), which returns (dropped) or falls
        // through to the original `cb(); return`.
        let (oc, c) = (&orig.functions[bp.click_fi], &back.functions[bp.click_fi]);
        let (at, n) = (bp.click_at, 41);
        assert!(matches!(c.ops[at], Opcode::JFalse { offset: 0, .. }));
        shifted_except(oc, c, at + 1, n, at);
        assert_eq!(&c.regs[..oc.regs.len()], &oc.regs[..]);
        let g = &c.ops[at + 1..at + 1 + n];
        assert!(matches!(g[5], Opcode::Field { field, .. } if field == bp.ctrl_f));
        assert!(matches!(g[7], Opcode::Field { field, .. } if field == bp.lock_f));
        assert!(matches!(g[10], Opcode::Field { field, .. } if field == bp.locks_f));
        for i in [9, 15, 36] {
            assert!(matches!(g[i], Opcode::Ret { .. }), "guard op {i}");
        }
        assert!(matches!(g[16], Opcode::Call0 { fun, .. } if fun == bp.sys_time));
        assert!(matches!(g[17], Opcode::GetGlobal { global, .. } if global.0 == ng));
        assert!(matches!(g[24], Opcode::SetGlobal { global, .. } if global.0 == ng + 1));
        assert!(matches!(g[26], Opcode::Mov { src, .. } if src == bp.bd));
        for i in [27, 30] {
            assert!(matches!(g[i], Opcode::Call2 { fun, .. } if fun == bp.map_get));
        }
        for i in [38, 40] {
            assert!(matches!(g[i], Opcode::Call3 { fun, .. } if fun == bp.map_set));
        }
        let base = at + 1;
        for (from, to) in [
            (6, 16),
            (8, 10),
            (11, 16),
            (14, 16),
            (18, 25),
            (33, 35),
            (35, 37),
        ] {
            assert_eq!(
                jump_targets(c, base + from),
                vec![base + to],
                "guard op {from}"
            );
        }
        assert!(matches!(c.ops[base + n], Opcode::NullCheck { .. }));
        assert!(matches!(c.ops[base + n + 1], Opcode::CallClosure { .. }));
        assert!(matches!(c.ops[base + n + 2], Opcode::Ret { .. }));
        for (v, want) in [(REPEAT_SAME_S, 32), (REPEAT_OTHER_S, 34)] {
            assert!(matches!(g[want], Opcode::Float { ptr, .. } if back.floats[ptr.0] == v));
        }
        // G1 push: the vote-on-push test always jumps over the block, the hold
        // start falls into Ret, nothing moved; doPadClick reads onClick instead of onPush.
        let (ha, hb) = (&orig.functions[bp.hold_fi], &back.functions[bp.hold_fi]);
        assert_eq!(hb.ops.len(), ha.ops.len());
        assert_eq!(jump_targets(hb, bp.push_at), jump_targets(ha, bp.push_at));
        assert!(matches!(hb.ops[bp.push_at], Opcode::JAlways { .. }));
        assert!(matches!(hb.ops[bp.hold_at], Opcode::JSLt { offset: 0, .. }));
        assert!(matches!(hb.ops[bp.hold_at + 1], Opcode::Ret { .. }));
        let (pa, pb) = (&orig.functions[bp.pad_fi], &back.functions[bp.pad_fi]);
        assert_eq!(ops(&pa.ops[..bp.pad_at]), ops(&pb.ops[..bp.pad_at]));
        assert_eq!(ops(&pa.ops[bp.pad_at + 1..]), ops(&pb.ops[bp.pad_at + 1..]));
        assert!(matches!(pb.ops[bp.pad_at], Opcode::Field { field, .. } if field == bp.on_click_f));
        // G2: 2-op prologue; G3: 11-op prologue guarded by the camp mode and resting.
        for (r, n) in reqs.iter().zip([2, 11]) {
            let (a, b) = (&orig.functions[r.fi], &back.functions[r.fi]);
            shifted(a, b, 0, n);
            assert!(matches!(b.ops[0], Opcode::JFalse { cond: Reg(1), .. }));
            assert!(matches!(
                b.ops[n - 1],
                Opcode::Mov {
                    dst: Reg(2),
                    src: Reg(1)
                }
            ));
            for i in 0..n {
                for t in jump_targets(b, i) {
                    assert!(t <= n, "prologue op {i} jumps past the prologue end");
                }
            }
        }
        // The camp guard tests the same class global the game's own toggleCamp does.
        let g = reqs[1].guard.as_ref().expect("camp guard");
        let ui = method(&orig, obj_type(&orig, "ui.GameUI").unwrap(), "toggleCamp").unwrap();
        assert!(ui.ops.windows(2).any(|w| matches!(
            (&w[0], &w[1]),
            (Opcode::GetGlobal { dst, global }, Opcode::Call2 { fun, arg0, .. })
                if *global == g.class_g && *fun == g.check && arg0 == dst
        )));

        // G4/G6: the field reads are constants now, same register, same op count.
        assert_eq!(reads[0].sites.len(), 2);
        for r in &reads {
            let (a, b) = (&orig.functions[r.fi], &back.functions[r.fi]);
            assert_eq!(b.ops.len(), a.ops.len());
            for &i in &r.sites {
                let Opcode::Field { dst: was, .. } = a.ops[i] else {
                    panic!("{}: op {i} was not a field read", r.what);
                };
                assert!(
                    matches!(b.ops[i], Opcode::Bool { dst, value } if dst == was && value.0 == r.value),
                    "{} op {i}",
                    r.what
                );
            }
        }

        // A second pass finds nothing to patch and leaves the image alone.
        let mut again = read(&patched);
        assert!(plan_button(&again).is_err());
        for (c, n, _, g) in REQUESTS {
            assert!(plan_request_forces(&again, c, n, g).is_err(), "{n}");
        }
        assert!(plan_dialog(&again).is_err());
        assert!(plan_window_lock(&again).is_err());
        assert!(plan_camp_window(&again).is_err());
        patch_coop_gates(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
