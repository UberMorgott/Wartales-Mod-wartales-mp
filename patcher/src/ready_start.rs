// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op load never waits forever for a player who left.
//
// After a save is loaded the host starts gameplay only once every player is
// ready. Game.startServer sets `playersReady = 0`, the host counts itself
// once its server is up, and the host's message handler (the closure that
// startServer binds as `host.onMessage`) counts one more per ReadyToStart:
//
//   case ReadyToStart:
//     if (hasGameplayStarted) client.sendMessage(ReadyToStart);   // late joiner
//     else {
//       nbPlayers = count of state.players not in options.multi.playersRemoved;
//       if (++playersReady == nbPlayers) { gameplayStart(); send ReadyToStart to every client; }
//     }
//
// The check runs only when a ReadyToStart arrives, and nbPlayers counts every
// player of the save, connected or not. A player whose client drops before it
// sends ReadyToStart (crash while loading, a Join the barrier parked until
// JOIN_CAP and then disconnected) is waited for forever: every screen stays on
// the loading screen and the whole party has to restart. Vanilla bug; co-op
// with three or more players and the mod's barrier make it common.
//
// This pass adds, on the host only:
//
//   Game.startServer, at entry:  readyReset()
//   Game.update, at entry:       readyTick(this)
//   the ReadyToStart branch (gameplay not started), at its head:
//                                if (!readyMark(this, client)) return
//
//   readyReset(): new empty seen-clients / seen-users / ready-users /
//     ready-clients lists.
//   readyFix(game): a user counted as ready whose connected client is not the
//     one whose ReadyToStart was counted (forced after it left, or a reconnect
//     during the wait) is uncounted (ready-users and playersReady). Vanilla
//     sends the start to every host.clients and a client runs gameplayStart on
//     it unconditionally, so a still-loading connection must be waited for.
//   readyMark(game, c): readyFix first; a second ReadyToStart from a client
//     already counted is ignored; otherwise the user and the client are
//     recorded as ready and the vanilla count goes on. A forced call (below) is
//     let through without a second record.
//   readyTick(game), every frame while the host waits (isAuth, server up,
//     gameplay not started, not reloading): readyFix;
//     every client in host.clients is remembered with its user id (also
//     before the host counted itself); once playersReady > 0:
//     a remembered user that is no longer connected (no client of that user in
//     host.clients), never counted as ready, and owns a player the count
//     includes (in state.players, not in playersRemoved) is marked ready and
//     the vanilla handler is run for it with ReadyToStart: it is counted, and
//     if it was the last one gameplay starts for everyone still here, exactly
//     as vanilla does. When it reconnects it joins as a late joiner (vanilla
//     `hasGameplayStarted` path), or, if the others are still loading,
//     readyFix uncounts it until its new connection sends ReadyToStart.
//
// The count only knows state.players. A client whose Join the host has not
// handled yet (still connecting, or parked by the barrier's joinGate) has no
// player in it, so the known players alone reach `==` and vanilla sends the
// start to every host.clients: the unsynced client runs gameplayStart before
// its SyncDone and never leaves the loading screen. So, in the same handler:
//
//   the Join case, at its head:   readyJoined(client)
//   the start, before gameplayStart: if (readyHold(this, client)) return
//   the start broadcast, per client: if (!readySendTo(c)) continue
//
//   readyJoined(c): c is synced (seen-clients of the Join handler; a parked
//     Join returns before it and its replay comes back through it).
//   readyWaiting(game): a client in host.clients (with a user) not synced.
//   readyHold(game, c): while readyWaiting, the start is held: the count of
//     this ReadyToStart is undone (playersReady--) and c is kept as `held`.
//   readyTick, last: a held start with nothing waiting any more (the client
//     synced, or dropped: hxbit took it out of host.clients) is replayed: the
//     handler is run for `held` as a forced ReadyToStart, which counts it
//     again; a joined new player is in the count by then and is waited for.
//   readySendTo(c): c sent a ReadyToStart that was counted. A client that has
//     not (a synced player the count leaves out) is not started; its own
//     ReadyToStart takes the vanilla late-joiner path.
// A host reload (in-game load, battle restart, backup load: LoadGame.loadGame,
// Game.loadGame, Pause's load closures) rebuilds options.multi as {hostID,
// isServer, serverID}: playersRemoved is gone and the count includes the save's
// absent players, so the reload never starts. readyRemoved(game, uid), from
// readyTick and from readyMark before the vanilla count, remembers a list the
// game has, gives it back to a game without one, forgets it with no
// options.multi (solo), and takes a listed player who sent ReadyToStart off it.
//
// No timer of its own: a parked Join is bounded by the barrier's JOIN_CAP
// (the client is disconnected, and so no longer waited for).
//
// Not handled: a player whose client never connects to the host at all is
// still waited for (it cannot be told from one still on its way).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefEnumConstruct, RefGlobal, RefInt, ValBool};

const LOG_FORCED: &str = "mp: ready: a player who left is no longer waited for: ";
const LOG_HOLD: &str = "mp: ready: start held: a connected player has not joined yet";
const LOG_REPLAY: &str = "mp: ready: every connected player joined, the held start goes ahead";

type F = (RefField, RefType);

/// `new hl.types.ArrayObj()` as the game writes it: alloc_array(type, 0) wrapped.
struct NewArr {
    ty: RefType,
    type_t: RefType,
    raw_t: RefType,
    cast_t: RefType,
    alloc: RefFun,
    wrap: RefFun,
    zero: RefInt,
}

struct Plan {
    void_t: RefType,
    bool_t: RefType,
    i32_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    game_t: RefType,
    nc_t: RefType,
    arr_t: RefType,
    msg_t: RefType,
    cwt_t: RefType,
    cwt_cls: RefGlobal,
    check: RefFun,
    get_user: RefFun,
    user_t: RefType,
    user_id: RefField,
    player_t: RefType,
    player_user: RefField,
    game_auth: RefField,
    game_started: RefField,
    game_reloading: RefField,
    game_ready: RefField,
    game_host: F,
    host_clients: RefField,
    game_state: F,
    state_players: F,
    proxy_array: F,
    game_options: F,
    opt_multi: F,
    multi_removed: F,
    arr_len: RefField,
    arr_raw: F,
    contains: RefFun,
    push: RefFun,
    remove: RefFun,
    println: RefFun,
    str_add: RefFun,
    new_arr: NewArr,
    ready_ctor: RefEnumConstruct,
    join_fi: usize,
    /// The JFalse on hasGameplayStarted that enters the counting branch.
    branch_jump: usize,
    /// First op of the counting branch.
    branch_at: usize,
    /// The Switch on the message, and the first op of its Join case.
    switch_at: usize,
    join_at: usize,
    /// The gameplayStart call that opens the start block.
    start_at: usize,
    /// The broadcast's `GetGlobal msg; c.sendMessage(msg)` and its client.
    send_at: usize,
    send_c: Reg,
    update_fi: usize,
    start_fi: usize,
    dbg_file: usize,
}

/// The 5-op empty-array idiom (`Int 0; Type; alloc; UnsafeCast; wrap`) the game
/// uses in onClientReady__impl to reset waitLockCallbs.
fn plan_new_arr(code: &Bytecode, arr_t: RefType) -> Result<NewArr> {
    let ctrl_t = obj_type(code, "st.Controller")?;
    let f = method(code, ctrl_t, "onClientReady__impl")?;
    let ops = &f.ops;
    let rt = |r: Reg| f.regs[r.0 as usize];
    for at in 4..ops.len() {
        let (
            Opcode::Int { dst: n, ptr },
            Opcode::Type { dst: t, ty },
            Opcode::Call2 {
                dst: z,
                fun: alloc,
                arg0: t2,
                arg1: n2,
            },
            Opcode::UnsafeCast { dst: y, src: z2 },
            Opcode::Call1 {
                dst: a,
                fun: wrap,
                arg0: y2,
            },
        ) = (
            &ops[at - 4],
            &ops[at - 3],
            &ops[at - 2],
            &ops[at - 1],
            &ops[at],
        )
        else {
            continue;
        };
        if t == t2 && n == n2 && z == z2 && y == y2 && code.ints[ptr.0] == 0 && rt(*a) == arr_t {
            return Ok(NewArr {
                ty: *ty,
                type_t: rt(*t),
                raw_t: rt(*z),
                cast_t: rt(*y),
                alloc: *alloc,
                wrap: *wrap,
                zero: *ptr,
            });
        }
    }
    bail!("onClientReady__impl: no new empty array to copy")
}

/// The host's message handler: the `(Game, NetworkClient, NetworkMessage) -> Void`
/// closure that Game.startServer binds as `host.onMessage`.
fn plan_handler(code: &Bytecode, game_t: RefType, nc_t: RefType) -> Result<(usize, RefType)> {
    let start = method(code, game_t, "startServer")?;
    let mut hits = vec![];
    for op in &start.ops {
        let Opcode::InstanceClosure { fun, .. } = op else {
            continue;
        };
        let Ok((args, ret)) = sig(code, *fun) else {
            continue;
        };
        if args.len() == 3
            && args[0] == game_t
            && args[1] == nc_t
            && matches!(code.types[args[2].0], Type::Enum { .. })
            && matches!(code.types[ret.0], Type::Void)
        {
            hits.push((*fun, args[2]));
        }
    }
    let [(h, msg_t)] = hits[..] else {
        bail!(
            "Game.startServer: expected one message handler closure, found {}",
            hits.len()
        );
    };
    Ok((fun_index(code, h)?, msg_t))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let void_t = prim("void", |t| matches!(t, Type::Void))?;
    let bool_t = prim("bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim("i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim("dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    if code.functions.iter().any(|f| {
        f.ops.iter().any(
            |op| matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(LOG_FORCED)),
        )
    }) {
        bail!("already applied");
    }

    let game_t = obj_type(code, "Game")?;
    let nc_t = obj_type(code, "hxbit.NetworkClient")?;
    let nh_t = obj_type(code, "hxbit.NetworkHost")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let game_auth = typed(game_t, "isAuth", bool_t)?;
    let game_started = typed(game_t, "hasGameplayStarted", bool_t)?;
    let game_reloading = typed(game_t, "reloading", bool_t)?;
    let game_ready = typed(game_t, "playersReady", i32_t)?;
    let game_host = field(code, game_t, "host")?;
    if !is_sub(code, game_host.1, nh_t) {
        bail!("Game.host is not an hxbit.NetworkHost");
    }
    let host_clients = typed(game_host.1, "clients", arr_t)?;
    let game_state = field(code, game_t, "state")?;
    let state_players = field(code, game_state.1, "players")?;
    let proxy_array = field(code, state_players.1, "array")?;
    let game_options = field(code, game_t, "options")?;
    let opt_multi = field_of_virtual(code, game_options.1, "multi")?;
    let multi_removed = field_of_virtual(code, opt_multi.1, "playersRemoved")?;
    if multi_removed.1 != arr_t {
        bail!("options.multi.playersRemoved is not an array");
    }
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let player_user = typed(player_t, "user", str_t)?;
    let arr_len = typed(arr_t, "length", i32_t)?;
    let arr_raw = field(code, arr_t, "array")?;
    let contains = proto(code, arr_t, "contains")?;
    let push = proto(code, arr_t, "push")?;
    let remove = proto(code, arr_t, "remove")?;
    if sig(code, contains)? != (vec![arr_t, dyn_t], bool_t)
        || sig(code, push)? != (vec![arr_t, dyn_t], i32_t)
        || sig(code, remove)? != (vec![arr_t, dyn_t], bool_t)
    {
        bail!("unexpected ArrayObj contains / push / remove signature");
    }
    let println = crate::diag::static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println.findex;
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t) {
        bail!("unexpected String.__add__ signature");
    }
    let new_arr = plan_new_arr(code, arr_t)?;

    // The handler: `client = ClientWT.check(msg) ? msg : null; client.get_user()`.
    let (join_fi, msg_t) = plan_handler(code, game_t, nc_t)?;
    let Type::Enum { constructs, .. } = &code.types[msg_t.0] else {
        unreachable!()
    };
    let names: Vec<&str> = constructs.iter().map(|c| s(code, c.name)).collect();
    if names != ["Join", "SyncDone", "ReadyToStart"] || !constructs[2].params.is_empty() {
        bail!("NetworkMessage is not Join / SyncDone / ReadyToStart");
    }
    let ready_ctor = RefEnumConstruct(2);
    let h = &code.functions[join_fi];
    let cwt_t = obj_type(code, "lib.ClientWT")?;
    let checks: Vec<(RefGlobal, RefFun)> = h
        .ops
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (
                Opcode::GetGlobal { dst, global },
                Opcode::Call2 {
                    fun,
                    arg0,
                    arg1: Reg(1),
                    ..
                },
            ) if dst == arg0 => Some((*global, *fun)),
            _ => None,
        })
        .collect();
    let [(cwt_cls, check)] = checks[..] else {
        bail!("the message handler: expected one ClientWT check of the client");
    };
    if sig(code, check)?.1 != bool_t || sig(code, check)?.0.len() != 2 {
        bail!("the message handler: the client check is not a type check");
    }
    let client_t = obj_type(code, "mpman.net.Client")?;
    if !is_sub(code, cwt_t, client_t) || !is_sub(code, cwt_t, nc_t) {
        bail!("lib.ClientWT is not an mpman.net.Client / hxbit.NetworkClient");
    }
    let get_user = method(code, client_t, "get_user")?;
    let (gu_args, user_t) = sig(code, get_user.findex)?;
    let get_user = get_user.findex;
    if gu_args != [client_t] {
        bail!("unexpected Client.get_user signature");
    }
    let user_id = typed(user_t, "id", str_t)?;
    if !h
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == get_user))
    {
        bail!("the message handler does not read the client's user");
    }
    // The handler never writes its client argument.
    if h.ops.iter().any(|o| {
        let d = format!("{o:?}");
        d.contains("dst: Reg(1),") || d.contains("dst: Reg(1) }")
    }) {
        bail!("the message handler writes its client register");
    }
    // `if (hasGameplayStarted) ... else { count }`: the one JFalse on the flag.
    let jumps: Vec<usize> = (1..h.ops.len())
        .filter(|&i| {
            matches!((&h.ops[i - 1], &h.ops[i]),
                (Opcode::Field { dst, obj: Reg(0), field }, Opcode::JFalse { cond, .. })
                    if *field == game_started && dst == cond)
        })
        .collect();
    let [branch_jump] = jumps[..] else {
        bail!("the message handler: expected one hasGameplayStarted test");
    };
    let [branch_at] = jump_targets(h, branch_jump)[..] else {
        unreachable!()
    };
    let into: Vec<usize> = (0..h.ops.len())
        .filter(|&i| jump_targets(h, i).contains(&branch_at))
        .collect();
    if into != [branch_jump] {
        bail!("the message handler: the counting branch has other entries");
    }
    let tail = &h.ops[branch_at..];
    let incr = tail.windows(3).any(|w| {
        matches!((&w[0], &w[1], &w[2]),
            (Opcode::Field { dst: a, obj: Reg(0), field: f1 }, Opcode::Incr { dst: b }, Opcode::SetField { obj: Reg(0), field: f2, src: c })
                if *f1 == game_ready && *f2 == game_ready && a == b && b == c)
    });
    let starts = tail.iter().any(|o| {
        matches!(o, Opcode::Call1 { fun, arg0: Reg(0), .. }
            if method(code, game_t, "gameplayStart").is_ok_and(|g| g.findex == *fun))
    });
    let reads = |f: RefField| {
        tail.iter()
            .any(|o| matches!(o, Opcode::Field { field, .. } if *field == f))
    };
    if !incr
        || !starts
        || !reads(state_players.0)
        || !reads(proxy_array.0)
        || !reads(multi_removed.0)
    {
        bail!("the message handler: the counting branch has an unexpected shape");
    }
    let only_entry = |at: usize, from: usize| {
        (0..h.ops.len())
            .filter(|&i| jump_targets(h, i).contains(&at))
            .eq([from])
    };
    let no_entry = |at: usize| (0..h.ops.len()).all(|i| !jump_targets(h, i).contains(&at));
    // `switch (msg)`: case 0 (Join) entered only from the switch.
    let switches: Vec<usize> = (0..h.ops.len())
        .filter(|&i| matches!(&h.ops[i], Opcode::Switch { offsets, .. } if offsets.len() == 3))
        .collect();
    let [switch_at] = switches[..] else {
        bail!("the message handler: expected one switch on the message");
    };
    let join_at = jump_targets(h, switch_at)[0];
    if join_at >= branch_jump || !only_entry(join_at, switch_at) {
        bail!("the message handler: the Join case has other entries");
    }
    // `if (playersReady == nbPlayers) { gameplayStart(); ...`: entered by fall-through.
    let gp_start = method(code, game_t, "gameplayStart")?.findex;
    let starts: Vec<usize> = (branch_at..h.ops.len())
        .filter(|&i| matches!(h.ops[i], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == gp_start))
        .collect();
    let [start_at] = starts[..] else {
        bail!("the message handler: expected one gameplayStart");
    };
    if !matches!(h.ops[start_at - 1], Opcode::JNotEq { .. }) || !no_entry(start_at) {
        bail!("the message handler: the start block has an unexpected shape");
    }
    // The late joiner's `client.sendMessage(ReadyToStart)` (gameplay started):
    // the message global and the send the broadcast must use too.
    let late: Vec<(RefGlobal, RefFun)> = h.ops[branch_jump + 1..branch_at]
        .windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (Opcode::GetGlobal { dst, global }, Opcode::Call2 { fun, arg1, .. })
                if arg1 == dst && h.regs[dst.0 as usize] == msg_t =>
            {
                Some((*global, *fun))
            }
            _ => None,
        })
        .collect();
    let [(ready_msg, send_fn)] = late[..] else {
        bail!("the message handler: expected one late-joiner ReadyToStart");
    };
    // `for (c in host.clients) c.sendMessage(ReadyToStart)`: the loop's last two ops.
    let sends: Vec<(usize, Reg)> = (start_at + 1..h.ops.len().saturating_sub(2))
        .filter_map(|i| match (&h.ops[i], &h.ops[i + 1], &h.ops[i + 2]) {
            (
                Opcode::GetGlobal { dst, global },
                Opcode::Call2 {
                    fun, arg0, arg1, ..
                },
                Opcode::JAlways { offset },
            ) if *global == ready_msg
                && *fun == send_fn
                && arg1 == dst
                && *offset < 0
                && h.regs[arg0.0 as usize] == nc_t =>
            {
                Some((i, *arg0))
            }
            _ => None,
        })
        .collect();
    let [(send_at, send_c)] = sends[..] else {
        bail!("the message handler: expected one start broadcast loop");
    };
    if !no_entry(send_at) {
        bail!("the message handler: the start broadcast has an unexpected shape");
    }

    // Game.update(dt) and Game.startServer(): a call at op 0, which no jump targets.
    let update = method(code, game_t, "update")?;
    let start = method(code, game_t, "startServer")?;
    if fun_args(code, update).first() != Some(&game_t) || fun_args(code, start) != [game_t] {
        bail!("unexpected Game.update / startServer signature");
    }
    if !start
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::SetThis { field, .. } if *field == game_ready))
    {
        bail!("Game.startServer does not reset playersReady");
    }
    for g in [update, start] {
        if (0..g.ops.len()).any(|i| jump_targets(g, i).contains(&0)) {
            bail!("fn@{}: a jump targets op 0", g.findex.0);
        }
    }

    Ok(Plan {
        void_t,
        bool_t,
        i32_t,
        dyn_t,
        str_t,
        game_t,
        nc_t,
        arr_t,
        msg_t,
        cwt_t,
        cwt_cls,
        check,
        get_user,
        user_t,
        user_id,
        player_t,
        player_user,
        game_auth,
        game_started,
        game_reloading,
        game_ready,
        game_host,
        host_clients,
        game_state,
        state_players,
        proxy_array,
        game_options,
        opt_multi,
        multi_removed,
        arr_len,
        arr_raw,
        contains,
        push,
        remove,
        println,
        str_add,
        new_arr,
        ready_ctor,
        join_fi,
        branch_jump,
        branch_at,
        switch_at,
        join_at,
        start_at,
        send_at,
        send_c,
        update_fi: fun_index(code, update.findex)?,
        start_fi: fun_index(code, start.findex)?,
        dbg_file: debug_file(code, "src/Game.hx")?,
    })
}

struct Globals {
    seen_c: RefGlobal,
    seen_u: RefGlobal,
    ready: RefGlobal,
    ready_c: RefGlobal,
    synced: RefGlobal,
    forced: RefGlobal,
    held: RefGlobal,
    /// The last lobby's options.multi.playersRemoved (never reset).
    removed: RefGlobal,
}

/// readyReset(): fresh lists, nothing forced or held.
fn add_reset(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let na = &p.new_arr;
    let mut r = Regs(vec![]);
    let (n, t, z, y, a, b, c, v) = (
        r.r(p.i32_t),
        r.r(na.type_t),
        r.r(na.raw_t),
        r.r(na.cast_t),
        r.r(p.arr_t),
        r.r(p.bool_t),
        r.r(p.nc_t),
        r.r(p.void_t),
    );
    let mut ops = vec![];
    for gl in [g.seen_c, g.seen_u, g.ready, g.ready_c, g.synced] {
        ops.extend([
            Opcode::Int {
                dst: n,
                ptr: na.zero,
            },
            Opcode::Type { dst: t, ty: na.ty },
            Opcode::Call2 {
                dst: z,
                fun: na.alloc,
                arg0: t,
                arg1: n,
            },
            Opcode::UnsafeCast { dst: y, src: z },
            Opcode::Call1 {
                dst: a,
                fun: na.wrap,
                arg0: y,
            },
            Opcode::SetGlobal { global: gl, src: a },
        ]);
    }
    ops.push(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    ops.push(Opcode::SetGlobal {
        global: g.forced,
        src: b,
    });
    ops.push(Opcode::Null { dst: c });
    ops.push(Opcode::SetGlobal {
        global: g.held,
        src: c,
    });
    ops.push(Opcode::Ret { ret: v });
    push_fn(code, vec![], p.void_t, r.0, ops, p.dbg_file)
}

/// readyWaiting(game): a client in host.clients, with a user, whose Join the
/// host has not handled.
fn add_waiting(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let cls_t = code.globals[p.cwt_cls.0];
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.game_t]);
    let game = Reg(0);
    let (b, host, clients, synced, i, len, raw, e) = (
        r.r(p.bool_t),
        r.r(p.game_host.1),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.arr_raw.1),
        r.r(p.dyn_t),
    );
    let (c, cls, cw, u, uid) = (
        r.r(p.nc_t),
        r.r(cls_t),
        r.r(p.cwt_t),
        r.r(p.user_t),
        r.r(p.str_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: host,
        obj: game,
        field: p.game_host.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: clients,
        obj: host,
        field: p.host_clients,
    });
    a.jmp(
        Opcode::JNull {
            reg: clients,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::GetGlobal {
        dst: synced,
        global: g.synced,
    });
    a.jmp(
        Opcode::JNull {
            reg: synced,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.cwt_cls,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("each");
    a.op(Opcode::Field {
        dst: len,
        obj: clients,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: clients,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "each");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: synced,
        arg1: c,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "each");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: cls,
        arg1: c,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "each");
    a.op(Opcode::UnsafeCast { dst: cw, src: c });
    a.op(Opcode::Call1 {
        dst: u,
        fun: p.get_user,
        arg0: cw,
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "each");
    a.op(Opcode::Field {
        dst: uid,
        obj: u,
        field: p.user_id,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "each",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("no");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![p.game_t], p.bool_t, r.0, a.finish(), p.dbg_file)
}

/// readyHold(game, c): true (the start is held) while readyWaiting; the count
/// of c's ReadyToStart is undone and c kept for the replay.
fn add_hold(code: &mut Bytecode, p: &Plan, g: &Globals, waiting: RefFun) -> Result<RefFun> {
    let log = str_global(code, p.str_t, LOG_HOLD);
    let mut r = Regs(vec![p.game_t, p.nc_t]);
    let (game, c) = (Reg(0), Reg(1));
    let (b, cnt, txt, v) = (r.r(p.bool_t), r.r(p.i32_t), r.r(p.str_t), r.r(p.void_t));
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: waiting,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "go");
    a.op(Opcode::SetGlobal {
        global: g.held,
        src: c,
    });
    a.op(Opcode::Field {
        dst: cnt,
        obj: game,
        field: p.game_ready,
    });
    a.op(Opcode::Decr { dst: cnt });
    a.op(Opcode::SetField {
        obj: game,
        field: p.game_ready,
        src: cnt,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: log,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: txt,
    });
    a.label("go");
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.game_t, p.nc_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// readyJoined(c): the host handled c's Join.
fn add_joined(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![p.nc_t]);
    let (synced, n, v) = (r.r(p.arr_t), r.r(p.i32_t), r.r(p.void_t));
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: synced,
        global: g.synced,
    });
    a.jmp(
        Opcode::JNull {
            reg: synced,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: synced,
        arg1: Reg(0),
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.nc_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// readySendTo(c): c's ReadyToStart was counted (always, before readyReset).
fn add_send_to(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![p.nc_t]);
    let (ready_c, b) = (r.r(p.arr_t), r.r(p.bool_t));
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: ready_c,
        global: g.ready_c,
    });
    a.jmp(
        Opcode::JNull {
            reg: ready_c,
            offset: 0,
        },
        "yes",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: ready_c,
        arg1: Reg(0),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("yes");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, vec![p.nc_t], p.bool_t, r.0, a.finish(), p.dbg_file)
}

/// readyFix(game): a user counted as ready whose connected client is not the
/// one that sent its counted ReadyToStart (forced after it left, or a new
/// connection after a reconnect) is counted no more: `ready.remove(uid);
/// playersReady--`. That client is still loading; the start broadcast
/// (vanilla: to every host.clients) must wait for its own ReadyToStart.
fn add_fix(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let cls_t = code.globals[p.cwt_cls.0];
    let mut r = Regs(vec![p.game_t]);
    let game = Reg(0);
    let (b, host, clients, ready, ready_c, i, len, cnt) = (
        r.r(p.bool_t),
        r.r(p.game_host.1),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let (raw, e, c, cls, cw, u, uid, v) = (
        r.r(p.arr_raw.1),
        r.r(p.dyn_t),
        r.r(p.nc_t),
        r.r(cls_t),
        r.r(p.cwt_t),
        r.r(p.user_t),
        r.r(p.str_t),
        r.r(p.void_t),
    );
    let i0 = int_const(code, 0);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: host,
        obj: game,
        field: p.game_host.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: clients,
        obj: host,
        field: p.host_clients,
    });
    a.jmp(
        Opcode::JNull {
            reg: clients,
            offset: 0,
        },
        "end",
    );
    for (dst, gl) in [(ready, g.ready), (ready_c, g.ready_c)] {
        a.op(Opcode::GetGlobal { dst, global: gl });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "end",
        );
    }
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.cwt_cls,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("each");
    a.op(Opcode::Field {
        dst: len,
        obj: clients,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: clients,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "each");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: ready_c,
        arg1: c,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "each");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: cls,
        arg1: c,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "each");
    a.op(Opcode::UnsafeCast { dst: cw, src: c });
    a.op(Opcode::Call1 {
        dst: u,
        fun: p.get_user,
        arg0: cw,
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "each");
    a.op(Opcode::Field {
        dst: uid,
        obj: u,
        field: p.user_id,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "each",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: ready,
        arg1: uid,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "each");
    a.op(Opcode::Field {
        dst: cnt,
        obj: game,
        field: p.game_ready,
    });
    a.op(Opcode::Decr { dst: cnt });
    a.op(Opcode::SetField {
        obj: game,
        field: p.game_ready,
        src: cnt,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "each");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// readyMark(game, c): false when c's user was already counted as ready
/// from this client; readyFix runs first.
/// readyRemoved(game, uid): vanilla's host reloads (in-game load, battle
/// restart, backup load) rebuild options.multi as {hostID, isServer, serverID}
/// without playersRemoved, so the ReadyToStart count waits for the save's
/// absent players forever. A list the game has is remembered; a game without
/// one gets the remembered one back; no options.multi (solo) forgets it. A
/// listed `uid` (not null) is taken off: that player is here and is counted.
fn add_removed(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![p.game_t, p.str_t]);
    let (game, uid) = (Reg(0), Reg(1));
    let (opt, multi, rem, b, v) = (
        r.r(p.game_options.1),
        r.r(p.opt_multi.1),
        r.r(p.arr_t),
        r.r(p.bool_t),
        r.r(p.void_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: opt,
        obj: game,
        field: p.game_options.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: opt,
            offset: 0,
        },
        "forget",
    );
    a.op(Opcode::Field {
        dst: multi,
        obj: opt,
        field: p.opt_multi.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: multi,
            offset: 0,
        },
        "forget",
    );
    a.op(Opcode::Field {
        dst: rem,
        obj: multi,
        field: p.multi_removed.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: rem,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::GetGlobal {
        dst: rem,
        global: g.removed,
    });
    a.jmp(
        Opcode::JNull {
            reg: rem,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetField {
        obj: multi,
        field: p.multi_removed.0,
        src: rem,
    });
    a.label("keep");
    a.op(Opcode::SetGlobal {
        global: g.removed,
        src: rem,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: rem,
        arg1: uid,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("forget");
    a.op(Opcode::Null { dst: rem });
    a.op(Opcode::SetGlobal {
        global: g.removed,
        src: rem,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.str_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn add_mark(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    fix: RefFun,
    removed: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.game_t, p.nc_t]);
    let c = Reg(1);
    let cls_t = code.globals[p.cwt_cls.0];
    let (ready, b, cls, cw, u, uid, n, v, ready_c) = (
        r.r(p.arr_t),
        r.r(p.bool_t),
        r.r(cls_t),
        r.r(p.cwt_t),
        r.r(p.user_t),
        r.r(p.str_t),
        r.r(p.i32_t),
        r.r(p.void_t),
        r.r(p.arr_t),
    );

    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: ready,
        global: g.ready,
    });
    a.jmp(
        Opcode::JNull {
            reg: ready,
            offset: 0,
        },
        "yes",
    );
    a.op(Opcode::GetGlobal {
        dst: b,
        global: g.forced,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "real");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.forced,
        src: b,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "yes");
    a.label("real");
    a.op(Opcode::Call1 {
        dst: v,
        fun: fix,
        arg0: Reg(0),
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.cwt_cls,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: cls,
        arg1: c,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "yes");
    a.op(Opcode::UnsafeCast { dst: cw, src: c });
    a.op(Opcode::Call1 {
        dst: u,
        fun: p.get_user,
        arg0: cw,
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "yes");
    a.op(Opcode::Field {
        dst: uid,
        obj: u,
        field: p.user_id,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "yes",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: ready,
        arg1: uid,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "push");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("push");
    // Before the vanilla count reads playersRemoved.
    a.op(Opcode::Call2 {
        dst: v,
        fun: removed,
        arg0: Reg(0),
        arg1: uid,
    });
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: ready,
        arg1: uid,
    });
    a.op(Opcode::GetGlobal {
        dst: ready_c,
        global: g.ready_c,
    });
    a.jmp(
        Opcode::JNull {
            reg: ready_c,
            offset: 0,
        },
        "yes",
    );
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: ready_c,
        arg1: c,
    });
    a.label("yes");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.game_t, p.nc_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// readyTick(game): see the header.
fn add_tick(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    handler: RefFun,
    fix: RefFun,
    waiting: RefFun,
    removed: RefFun,
) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let log = str_global(code, p.str_t, LOG_FORCED);
    let log_replay = str_global(code, p.str_t, LOG_REPLAY);
    let cls_t = code.globals[p.cwt_cls.0];
    let mut r = Regs(vec![p.game_t]);
    let game = Reg(0);
    let (b, host, clients, ready, seen_c, seen_u, i, j, len, zero, cnt) = (
        r.r(p.bool_t),
        r.r(p.game_host.1),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let (raw, e, c, c2, cls, cw, u, uid, uid2) = (
        r.r(p.arr_raw.1),
        r.r(p.dyn_t),
        r.r(p.nc_t),
        r.r(p.nc_t),
        r.r(cls_t),
        r.r(p.cwt_t),
        r.r(p.user_t),
        r.r(p.str_t),
        r.r(p.str_t),
    );
    let (st, prox, pa, players, pl, pu, opt, multi, rem, msg, txt, v, n, exc) = (
        r.r(p.game_state.1),
        r.r(p.state_players.1),
        r.r(p.proxy_array.1),
        r.r(p.arr_t),
        r.r(p.player_t),
        r.r(p.str_t),
        r.r(p.game_options.1),
        r.r(p.opt_multi.1),
        r.r(p.arr_t),
        r.r(p.msg_t),
        r.r(p.str_t),
        r.r(p.void_t),
        r.r(p.i32_t),
        r.r(p.dyn_t),
    );
    let mut a = Asm::new();
    let fld =
        |a: &mut Asm, dst: Reg, obj: Reg, field: RefField| a.op(Opcode::Field { dst, obj, field });
    // `user id of client c` into `out`, or jump to `skip` when it has none.
    let user_of = |a: &mut Asm, c: Reg, out: Reg, skip: &'static str| {
        a.op(Opcode::Call2 {
            dst: b,
            fun: p.check,
            arg0: cls,
            arg1: c,
        });
        a.jmp(Opcode::JFalse { cond: b, offset: 0 }, skip);
        a.op(Opcode::UnsafeCast { dst: cw, src: c });
        a.op(Opcode::Call1 {
            dst: u,
            fun: p.get_user,
            arg0: cw,
        });
        a.jmp(Opcode::JNull { reg: u, offset: 0 }, skip);
        a.op(Opcode::Field {
            dst: out,
            obj: u,
            field: p.user_id,
        });
        a.jmp(
            Opcode::JNull {
                reg: out,
                offset: 0,
            },
            skip,
        );
    };
    // `arr[idx++]` cast into `dst`.
    let next = |a: &mut Asm, arr: Reg, idx: Reg, dst: Reg, bump: bool| {
        a.op(Opcode::Field {
            dst: raw,
            obj: arr,
            field: p.arr_raw.0,
        });
        a.op(Opcode::GetArray {
            dst: e,
            array: raw,
            index: idx,
        });
        if bump {
            a.op(Opcode::Incr { dst: idx });
        }
        a.op(Opcode::UnsafeCast { dst, src: e });
    };
    // The vanilla handler run for client `c` as a ReadyToStart that readyMark
    // lets through uncounted (`forced`).
    let forced_ready = |a: &mut Asm, c: Reg, caught: &'static str| {
        a.op(Opcode::Bool {
            dst: b,
            value: ValBool(true),
        });
        a.op(Opcode::SetGlobal {
            global: g.forced,
            src: b,
        });
        a.op(Opcode::MakeEnum {
            dst: msg,
            construct: p.ready_ctor,
            args: vec![],
        });
        a.jmp(Opcode::Trap { exc, offset: 0 }, caught);
        a.op(Opcode::Call3 {
            dst: v,
            fun: handler,
            arg0: game,
            arg1: c,
            arg2: msg,
        });
        a.op(Opcode::EndTrap { exc });
        a.label(caught);
        a.op(Opcode::Bool {
            dst: b,
            value: ValBool(false),
        });
        a.op(Opcode::SetGlobal {
            global: g.forced,
            src: b,
        });
    };

    // Host only, while it waits for ReadyToStart.
    fld(&mut a, b, game, p.game_auth);
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    fld(&mut a, b, game, p.game_started);
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    fld(&mut a, b, game, p.game_reloading);
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    // 00. Absent players carry over a host reload.
    a.op(Opcode::Null { dst: uid });
    a.op(Opcode::Call2 {
        dst: v,
        fun: removed,
        arg0: game,
        arg1: uid,
    });
    fld(&mut a, host, game, p.game_host.0);
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "end",
    );
    fld(&mut a, clients, host, p.host_clients);
    a.jmp(
        Opcode::JNull {
            reg: clients,
            offset: 0,
        },
        "end",
    );
    for (dst, gl) in [(ready, g.ready), (seen_c, g.seen_c), (seen_u, g.seen_u)] {
        a.op(Opcode::GetGlobal { dst, global: gl });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "end",
        );
    }
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.cwt_cls,
    });
    // 0. A counted user back on a new connection waits for its own ReadyToStart.
    a.op(Opcode::Call1 {
        dst: v,
        fun: fix,
        arg0: game,
    });

    // 1. Remember every connected client with its user id (from the start of
    // the wait, before the host counted itself).
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("rec");
    fld(&mut a, len, clients, p.arr_len);
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "gone0",
    );
    next(&mut a, clients, i, c, true);
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "rec");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: seen_c,
        arg1: c,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "rec");
    user_of(&mut a, c, uid, "rec");
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: seen_c,
        arg1: c,
    });
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: seen_u,
        arg1: uid,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "rec");

    // 2. Users seen before, gone now, not counted: count them (once the host
    // counted itself).
    a.label("gone0");
    fld(&mut a, cnt, game, p.game_ready);
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: cnt,
            offset: 0,
        },
        "replay",
    );
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("gone");
    fld(&mut a, len, seen_c, p.arr_len);
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "replay",
    );
    next(&mut a, seen_c, i, c, false);
    next(&mut a, seen_u, i, uid, true);
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: ready,
        arg1: uid,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "gone");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: clients,
        arg1: c,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "gone");
    // Reconnected under another client object: not gone.
    a.op(Opcode::Int { dst: j, ptr: i0 });
    a.loop_head("same");
    fld(&mut a, len, clients, p.arr_len);
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: len,
            offset: 0,
        },
        "players",
    );
    next(&mut a, clients, j, c2, true);
    a.jmp(Opcode::JNull { reg: c2, offset: 0 }, "same");
    user_of(&mut a, c2, uid2, "same");
    a.jmp(
        Opcode::JNotEq {
            a: uid2,
            b: uid,
            offset: 0,
        },
        "same",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "gone");
    // Owns a player the vanilla count includes.
    a.label("players");
    fld(&mut a, st, game, p.game_state.0);
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "gone");
    fld(&mut a, prox, st, p.state_players.0);
    a.jmp(
        Opcode::JNull {
            reg: prox,
            offset: 0,
        },
        "gone",
    );
    fld(&mut a, pa, prox, p.proxy_array.0);
    a.op(Opcode::SafeCast {
        dst: players,
        src: pa,
    });
    a.jmp(
        Opcode::JNull {
            reg: players,
            offset: 0,
        },
        "gone",
    );
    a.op(Opcode::Int { dst: j, ptr: i0 });
    a.loop_head("pl");
    fld(&mut a, len, players, p.arr_len);
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: len,
            offset: 0,
        },
        "gone",
    );
    next(&mut a, players, j, pl, true);
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "pl");
    fld(&mut a, pu, pl, p.player_user);
    a.jmp(
        Opcode::JNotEq {
            a: pu,
            b: uid,
            offset: 0,
        },
        "pl",
    );
    fld(&mut a, opt, game, p.game_options.0);
    a.jmp(
        Opcode::JNull {
            reg: opt,
            offset: 0,
        },
        "force",
    );
    fld(&mut a, multi, opt, p.opt_multi.0);
    a.jmp(
        Opcode::JNull {
            reg: multi,
            offset: 0,
        },
        "force",
    );
    fld(&mut a, rem, multi, p.multi_removed.0);
    a.jmp(
        Opcode::JNull {
            reg: rem,
            offset: 0,
        },
        "force",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: rem,
        arg1: uid,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "gone");
    // Count it through the vanilla handler, as if it had sent ReadyToStart.
    a.label("force");
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: ready,
        arg1: uid,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: log,
    });
    a.op(Opcode::Call2 {
        dst: txt,
        fun: p.str_add,
        arg0: txt,
        arg1: uid,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: txt,
    });
    forced_ready(&mut a, c, "caught");
    fld(&mut a, b, game, p.game_started);
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.jmp(Opcode::JAlways { offset: 0 }, "gone");

    // 3. A held start with no connected client left unsynced: replay it.
    a.label("replay");
    a.op(Opcode::GetGlobal {
        dst: c,
        global: g.held,
    });
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: b,
        fun: waiting,
        arg0: game,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.op(Opcode::Null { dst: c2 });
    a.op(Opcode::SetGlobal {
        global: g.held,
        src: c2,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: log_replay,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: txt,
    });
    forced_ready(&mut a, c, "caught2");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let mut global = |t: RefType| {
        code.globals.push(t);
        RefGlobal(code.globals.len() - 1)
    };
    let g = Globals {
        seen_c: global(p.arr_t),
        seen_u: global(p.arr_t),
        ready: global(p.arr_t),
        ready_c: global(p.arr_t),
        synced: global(p.arr_t),
        forced: global(p.bool_t),
        held: global(p.nc_t),
        removed: global(p.arr_t),
    };
    let handler = code.functions[p.join_fi].findex;
    // Appended in this order (the tests' `k` indices).
    let fix = add_fix(code, &p, &g)?;
    let reset = add_reset(code, &p, &g)?;
    let removed = add_removed(code, &p, &g)?;
    let mark = add_mark(code, &p, &g, fix, removed)?;
    let waiting = add_waiting(code, &p, &g)?;
    let tick = add_tick(code, &p, &g, handler, fix, waiting, removed)?;
    let hold = add_hold(code, &p, &g, waiting)?;
    let joined = add_joined(code, &p, &g)?;
    let send_to = add_send_to(code, &p, &g)?;

    // Game.startServer: readyReset().
    let f = &mut code.functions[p.start_fi];
    f.regs.push(p.void_t);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(f, 0, vec![Opcode::Call0 { dst: v, fun: reset }]);
    // Game.update: readyTick(this).
    let f = &mut code.functions[p.update_fi];
    f.regs.push(p.void_t);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![Opcode::Call1 {
            dst: v,
            fun: tick,
            arg0: Reg(0),
        }],
    );
    // The message handler, last site first so the earlier indices hold.
    let f = &mut code.functions[p.join_fi];
    f.regs.push(p.bool_t);
    let b = Reg((f.regs.len() - 1) as u32);
    f.regs.push(p.void_t);
    let v = Reg((f.regs.len() - 1) as u32);
    // The broadcast: if (!readySendTo(c)) continue (skip GetGlobal + send).
    insert_ops(
        f,
        p.send_at,
        vec![
            Opcode::Call1 {
                dst: b,
                fun: send_to,
                arg0: p.send_c,
            },
            Opcode::JFalse { cond: b, offset: 2 },
        ],
    );
    // The start: if (readyHold(this, client)) return.
    insert_ops(
        f,
        p.start_at,
        vec![
            Opcode::Call2 {
                dst: b,
                fun: hold,
                arg0: Reg(0),
                arg1: Reg(1),
            },
            Opcode::JFalse { cond: b, offset: 1 },
            Opcode::Ret { ret: v },
        ],
    );
    // The counting branch: if (!readyMark(this, client)) return.
    let head = vec![
        Opcode::Call2 {
            dst: b,
            fun: mark,
            arg0: Reg(0),
            arg1: Reg(1),
        },
        Opcode::JTrue { cond: b, offset: 1 },
        Opcode::Ret { ret: v },
    ];
    let n = head.len() as i32;
    insert_ops(f, p.branch_at, head);
    // insert_ops moved the branch entry past the new head; point it back.
    let Opcode::JFalse { offset, .. } = &mut f.ops[p.branch_jump] else {
        bail!("the hasGameplayStarted test moved");
    };
    *offset -= n;
    // The Join case: readyJoined(client); the switch enters it at the call.
    insert_ops(
        f,
        p.join_at,
        vec![Opcode::Call1 {
            dst: v,
            fun: joined,
            arg0: Reg(1),
        }],
    );
    let Opcode::Switch { offsets, .. } = &mut f.ops[p.switch_at] else {
        bail!("the message switch moved");
    };
    offsets[0] -= 1;
    eprintln!(
        "patched ready start: a co-op load stops waiting for a player who left before ReadyToStart, \
         and starts no one who has not joined"
    );
    Ok(())
}

/// Lets a co-op load start without players who left before they were ready,
/// or leaves `code` untouched and logs why.
pub(crate) fn patch_ready_start(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("ready start skipped: {e:#}"));
            return;
        }
    };
    let snap = Snap::take(code);
    let touched = [p.start_fi, p.update_fi, p.join_fi];
    let saved: Vec<Function> = touched.iter().map(|&i| code.functions[i].clone()).collect();
    if let Err(e) = apply(code, p) {
        snap.restore(code);
        for (&i, f) in touched.iter().zip(saved) {
            code.functions[i] = f;
        }
        crate::skipped(format!("ready start skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;
    use std::collections::HashMap;

    /// The new functions, in the order `apply` appends them.
    mod k {
        pub(super) const RESET: usize = 1;
        pub(super) const TICK: usize = 5;
        pub(super) const JOINED: usize = 7;
        pub(super) const COUNT: usize = 9;
    }

    /// Patches a copy of the installed game (skipped when absent): only
    /// startServer, Game.update and the message handler change, the new
    /// functions are well typed, the image round-trips, and a second pass
    /// changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (start_fi, update_fi, join_fi, branch_jump) =
            (p.start_fi, p.update_fi, p.join_fi, p.branch_jump);
        // Handler insertions (original index, length), ascending.
        let sites = [
            (p.join_at, 1),
            (p.branch_at, 3),
            (p.start_at, 3),
            (p.send_at, 2),
        ];
        let m = |t: usize| {
            t + sites
                .iter()
                .filter(|s| s.0 <= t)
                .map(|s| s.1)
                .sum::<usize>()
        };
        let at = |t: usize| t + sites.iter().filter(|s| s.0 < t).map(|s| s.1).sum::<usize>();
        let mut code = read(&image);
        patch_ready_start(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + k::COUNT);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            let touched = [start_fi, update_fi, join_fi].contains(&i);
            assert_eq!(same, !touched, "function #{i} (fn@{})", a.findex.0);
        }
        for k in 0..k::COUNT {
            let f = &back.functions[nf + k];
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        for fi in [start_fi, update_fi] {
            shifted(&orig.functions[fi], &back.functions[fi], 0, 1);
            check_types(&back, &back.functions[fi], 0..1);
        }
        let (a, b) = (&orig.functions[join_fi], &back.functions[join_fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 9);
        for &(s, n) in &sites {
            check_types(&back, b, at(s)..at(s) + n);
        }
        check_flow(b);
        // Every original op kept; the hasGameplayStarted test and the Join
        // case enter their new heads, the start and the send fall into theirs.
        for i in 0..a.ops.len() {
            let want: Vec<usize> = if i == branch_jump {
                vec![at(p.branch_at)]
            } else if i == p.switch_at {
                let mut t: Vec<usize> = jump_targets(a, i).into_iter().map(m).collect();
                t[0] = at(p.join_at);
                t
            } else {
                jump_targets(a, i).into_iter().map(m).collect()
            };
            assert_eq!(jump_targets(b, m(i)), want, "op {i}");
        }
        // The send skip lands on the loop's back jump.
        assert_eq!(
            jump_targets(b, at(p.send_at) + 1),
            [m(p.send_at) + 2],
            "send skip"
        );
        for s in [p.branch_at, p.start_at] {
            assert!(matches!(
                b.ops[at(s)],
                Opcode::Call2 {
                    arg0: Reg(0),
                    arg1: Reg(1),
                    ..
                }
            ));
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_ready_start(&mut again);
        assert!(write(&again) == patched);
    }

    /// The broadcast filtered must send the late joiner's ReadyToStart with
    /// the late joiner's send: another message global or call is refused.
    #[test]
    fn refuses_another_broadcast() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        for k in 0..2 {
            let mut code = read(&image);
            let f = &mut code.functions[p.join_fi];
            match &mut f.ops[p.send_at + k] {
                Opcode::GetGlobal { global, .. } if k == 0 => global.0 += 1,
                Opcode::Call2 { fun, .. } if k == 1 => fun.0 += 1,
                o => panic!("unexpected {o:?}"),
            }
            assert!(plan(&code).is_err(), "variant {k}");
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        B(bool),
        I(i32),
        S(String),
        R(usize),
        E(usize),
    }

    enum H {
        Obj(HashMap<usize, V>),
        Arr(Vec<V>),
    }

    #[derive(Debug, PartialEq)]
    enum Ev {
        Start,
        Send(usize),
    }

    /// Executes the real handler and the new functions on a toy heap.
    struct Sim<'a> {
        code: &'a Bytecode,
        p: &'a Plan,
        /// Function count before the patch (Some when `code` is patched).
        nf: Option<usize>,
        heap: Vec<H>,
        globals: HashMap<usize, V>,
        users: HashMap<usize, usize>,
        events: Vec<Ev>,
        game: usize,
        clients: usize,
        players: usize,
        removed: usize,
    }

    impl<'a> Sim<'a> {
        fn new(code: &'a Bytecode, p: &'a Plan, players: &[&str], nf: Option<usize>) -> Self {
            let mut s = Sim {
                code,
                p,
                nf,
                heap: vec![],
                globals: HashMap::new(),
                users: HashMap::new(),
                events: vec![],
                game: 0,
                clients: 0,
                players: 0,
                removed: 0,
            };
            s.game = s.obj();
            let host = s.obj();
            s.clients = s.arr();
            let state = s.obj();
            let proxy = s.obj();
            s.players = s.arr();
            let opt = s.obj();
            let multi = s.obj();
            s.removed = s.arr();
            for u in players {
                let pl = s.obj();
                s.set(pl, p.player_user, V::S(u.to_string()));
                s.list(s.players).push(V::R(pl));
            }
            s.set(s.game, p.game_auth, V::B(true));
            s.set(s.game, p.game_started, V::B(false));
            s.set(s.game, p.game_reloading, V::B(false));
            s.set(s.game, p.game_host.0, V::R(host));
            s.set(s.game, p.game_state.0, V::R(state));
            s.set(s.game, p.game_options.0, V::R(opt));
            s.set(host, p.host_clients, V::R(s.clients));
            s.set(state, p.state_players.0, V::R(proxy));
            s.set(proxy, p.proxy_array.0, V::R(s.players));
            s.set(opt, p.opt_multi.0, V::R(multi));
            s.set(multi, p.multi_removed.0, V::R(s.removed));
            // Game.startServer: playersReady = 0, then the host counts itself.
            s.set(s.game, p.game_ready, V::I(0));
            if s.patched() {
                s.call(s.fun(k::RESET), vec![]);
            }
            s.set(s.game, p.game_ready, V::I(1));
            s
        }
        fn patched(&self) -> bool {
            self.nf.is_some()
        }
        /// New function `k` (see `k::*`).
        fn fun(&self, k: usize) -> RefFun {
            self.code.functions[self.nf.expect("patched") + k].findex
        }
        fn obj(&mut self) -> usize {
            self.heap.push(H::Obj(HashMap::new()));
            self.heap.len() - 1
        }
        fn arr(&mut self) -> usize {
            self.heap.push(H::Arr(vec![]));
            self.heap.len() - 1
        }
        fn set(&mut self, o: usize, f: RefField, v: V) {
            let H::Obj(m) = &mut self.heap[o] else {
                panic!("not an object")
            };
            m.insert(f.0, v);
        }
        fn get(&self, o: usize, f: RefField) -> V {
            match &self.heap[o] {
                H::Obj(m) => m.get(&f.0).cloned().unwrap_or(V::Null),
                H::Arr(a) if f == self.p.arr_len => V::I(a.len() as i32),
                H::Arr(_) if f == self.p.arr_raw.0 => V::R(o),
                H::Arr(_) => panic!("array field {}", f.0),
            }
        }
        fn list(&mut self, o: usize) -> &mut Vec<V> {
            let H::Arr(a) = &mut self.heap[o] else {
                panic!("not an array")
            };
            a
        }
        fn started(&self) -> bool {
            self.get(self.game, self.p.game_started) == V::B(true)
        }
        fn ready_count(&self) -> V {
            self.get(self.game, self.p.game_ready)
        }
        /// A client of user `uid` connects and the host handles its Join.
        fn connect(&mut self, uid: &str) -> usize {
            let c = self.arrive(uid);
            self.join(c);
            c
        }
        /// A client of user `uid` connects; its Join is not handled yet.
        fn arrive(&mut self, uid: &str) -> usize {
            let c = self.obj();
            let u = self.obj();
            self.set(u, self.p.user_id, V::S(uid.to_string()));
            self.users.insert(c, u);
            self.list(self.clients).push(V::R(c));
            c
        }
        /// The host handles c's Join (the handler's Join case: the new head;
        /// the vanilla rest gives a new user a player).
        fn join(&mut self, c: usize) {
            if self.patched() {
                self.call(self.fun(k::JOINED), vec![V::R(c)]);
            }
            let u = self.users[&c];
            let uid = self.get(u, self.p.user_id);
            let players = self.players;
            let known = (0..self.list(players).len()).any(|i| {
                let V::R(pl) = self.list(players)[i] else {
                    return false;
                };
                self.get(pl, self.p.player_user) == uid
            });
            if !known {
                let pl = self.obj();
                self.set(pl, self.p.player_user, uid);
                self.list(players).push(V::R(pl));
            }
        }
        /// The connection drops: hxbit takes it out of host.clients.
        fn drop_client(&mut self, c: usize) {
            self.list(self.clients).retain(|x| *x != V::R(c));
        }
        /// A ReadyToStart from `c` reaches the host's handler.
        fn ready(&mut self, c: usize) {
            let h = self.code.functions[self.p.join_fi].findex;
            self.call(h, vec![V::R(self.game), V::R(c), V::E(2)]);
        }
        /// One host frame (Game.update's new prologue).
        fn frame(&mut self) {
            if self.patched() {
                self.call(self.fun(k::TICK), vec![V::R(self.game)]);
            }
        }
        fn call(&mut self, f: RefFun, args: Vec<V>) -> V {
            let p = self.p;
            let code = self.code;
            if self.patched() && (0..k::COUNT).any(|k| self.fun(k) == f) {
                return self.exec(f, args);
            }
            if f == code.functions[p.join_fi].findex {
                return self.exec(f, args);
            }
            let arr = |v: &V| match v {
                V::R(a) => *a,
                _ => panic!("not an array: {v:?}"),
            };
            if f == p.check {
                return V::B(matches!(args[1], V::R(c) if self.users.contains_key(&c)));
            }
            if f == p.get_user {
                let V::R(c) = args[0] else { panic!() };
                return V::R(self.users[&c]);
            }
            if f == p.contains {
                let a = arr(&args[0]);
                return V::B(self.list(a).contains(&args[1]));
            }
            if f == p.push {
                let a = arr(&args[0]);
                self.list(a).push(args[1].clone());
                return V::I(self.list(a).len() as i32);
            }
            if f == p.remove {
                let a = arr(&args[0]);
                let l = self.list(a);
                return V::B(match l.iter().position(|x| *x == args[1]) {
                    Some(i) => {
                        l.remove(i);
                        true
                    }
                    None => false,
                });
            }
            if f == p.new_arr.alloc {
                return V::R(self.arr());
            }
            if f == p.new_arr.wrap {
                return args[0].clone();
            }
            if f == p.println {
                return V::Null;
            }
            if f == p.str_add {
                let (V::S(a), V::S(b)) = (&args[0], &args[1]) else {
                    panic!()
                };
                return V::S(format!("{a}{b}"));
            }
            let name = s(code, code.functions[fun_index(code, f).unwrap()].name);
            match name {
                "string" => V::S(format!("{:?}", args[0])),
                "gameplayStart" => {
                    self.set(self.game, p.game_started, V::B(true));
                    self.events.push(Ev::Start);
                    V::Null
                }
                "sendMessage" => {
                    let V::R(c) = args[0] else { panic!() };
                    self.events.push(Ev::Send(c));
                    V::Null
                }
                _ => panic!("unexpected call fn@{} {name}", f.0),
            }
        }
        fn exec(&mut self, f: RefFun, args: Vec<V>) -> V {
            let code = self.code;
            let func = &code.functions[fun_index(code, f).unwrap()];
            let mut r: Vec<V> = vec![V::Null; func.regs.len()];
            for (i, a) in args.into_iter().enumerate() {
                r[i] = a;
            }
            let int = |v: &V| match v {
                V::I(i) => *i,
                _ => panic!("not an int: {v:?}"),
            };
            let mut pc = 0usize;
            loop {
                let op = &func.ops[pc];
                pc += 1;
                let jump = |pc: &mut usize, off: i32| *pc = (*pc as i64 + off as i64) as usize;
                let rg = |x: &Reg| x.0 as usize;
                match op {
                    Opcode::Label | Opcode::EndTrap { .. } | Opcode::Trap { .. } => {}
                    Opcode::Ret { ret } => return r[rg(ret)].clone(),
                    Opcode::Mov { dst, src }
                    | Opcode::UnsafeCast { dst, src }
                    | Opcode::SafeCast { dst, src }
                    | Opcode::ToDyn { dst, src }
                    | Opcode::ToVirtual { dst, src } => r[rg(dst)] = r[rg(src)].clone(),
                    Opcode::NullCheck { reg } => {
                        assert!(
                            r[rg(reg)] != V::Null,
                            "null access fn@{} op {}",
                            f.0,
                            pc - 1
                        )
                    }
                    Opcode::Null { dst } | Opcode::Type { dst, .. } => r[rg(dst)] = V::Null,
                    Opcode::Bool { dst, value } => r[rg(dst)] = V::B(value.0),
                    Opcode::Int { dst, ptr } => r[rg(dst)] = V::I(code.ints[ptr.0]),
                    Opcode::Incr { dst } => r[rg(dst)] = V::I(int(&r[rg(dst)]) + 1),
                    Opcode::Decr { dst } => r[rg(dst)] = V::I(int(&r[rg(dst)]) - 1),
                    Opcode::Field { dst, obj, field } => {
                        let V::R(o) = r[rg(obj)] else {
                            panic!("null access fn@{} op {}", f.0, pc - 1)
                        };
                        r[rg(dst)] = self.get(o, *field);
                    }
                    Opcode::SetField { obj, field, src } => {
                        let V::R(o) = r[rg(obj)] else { panic!() };
                        let v = r[rg(src)].clone();
                        self.set(o, *field, v);
                    }
                    Opcode::GetGlobal { dst, global } => {
                        r[rg(dst)] = match self.globals.get(&global.0) {
                            Some(v) => v.clone(),
                            None => match code.globals_initializers.get(global) {
                                Some(&ci) => {
                                    let c = &code.constants.as_ref().unwrap()[ci];
                                    V::S(code.strings[c.fields[0]].to_string())
                                }
                                None => match &code.types[code.globals[global.0].0] {
                                    Type::Bool => V::B(false),
                                    _ => V::Null,
                                },
                            },
                        }
                    }
                    Opcode::SetGlobal { global, src } => {
                        self.globals.insert(global.0, r[rg(src)].clone());
                    }
                    Opcode::EnumIndex { dst, value } => {
                        let V::E(i) = r[rg(value)] else { panic!() };
                        r[rg(dst)] = V::I(i as i32);
                    }
                    Opcode::MakeEnum { dst, construct, .. } => r[rg(dst)] = V::E(construct.0),
                    Opcode::Switch { reg, offsets, end } => {
                        let i = int(&r[rg(reg)]);
                        match offsets.get(i as usize) {
                            Some(o) if i >= 0 => jump(&mut pc, *o),
                            _ => jump(&mut pc, *end),
                        }
                    }
                    Opcode::GetArray { dst, array, index } => {
                        let V::R(a) = r[rg(array)] else { panic!() };
                        let i = int(&r[rg(index)]) as usize;
                        r[rg(dst)] = self.list(a)[i].clone();
                    }
                    Opcode::JAlways { offset } => jump(&mut pc, *offset),
                    Opcode::JTrue { cond, offset } | Opcode::JFalse { cond, offset } => {
                        let want = matches!(op, Opcode::JTrue { .. });
                        if r[rg(cond)] == V::B(want) {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JNull { reg, offset } | Opcode::JNotNull { reg, offset } => {
                        let want = matches!(op, Opcode::JNull { .. });
                        if (r[rg(reg)] == V::Null) == want {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JEq { a, b, offset } | Opcode::JNotEq { a, b, offset } => {
                        let want = matches!(op, Opcode::JEq { .. });
                        if (r[rg(a)] == r[rg(b)]) == want {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JSGte { a, b, offset }
                    | Opcode::JSLt { a, b, offset }
                    | Opcode::JULt { a, b, offset } => {
                        let (x, y) = (int(&r[rg(a)]), int(&r[rg(b)]));
                        let hit = match op {
                            Opcode::JSGte { .. } => x >= y,
                            Opcode::JSLt { .. } => x < y,
                            _ => (x as u32) < (y as u32),
                        };
                        if hit {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::Call0 { dst, fun } => r[rg(dst)] = self.call(*fun, vec![]),
                    Opcode::Call1 { dst, fun, arg0 } => {
                        let a = vec![r[rg(arg0)].clone()];
                        r[rg(dst)] = self.call(*fun, a);
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } => {
                        let a = vec![r[rg(arg0)].clone(), r[rg(arg1)].clone()];
                        r[rg(dst)] = self.call(*fun, a);
                    }
                    Opcode::Call3 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                        arg2,
                    } => {
                        let a = [arg0, arg1, arg2]
                            .iter()
                            .map(|x| r[rg(x)].clone())
                            .collect();
                        r[rg(dst)] = self.call(*fun, a);
                    }
                    o => panic!("sim: unsupported op fn@{} {o:?}", f.0),
                }
            }
        }
    }

    fn images() -> Option<(Bytecode, Bytecode)> {
        let image = game()?;
        let orig = read(&image);
        let mut code = read(&image);
        patch_ready_start(&mut code);
        Some((orig, code))
    }

    /// Host + A + B load a save; B's client drops before ReadyToStart (a Join
    /// the barrier parked until JOIN_CAP). Vanilla waits forever; patched,
    /// the next frame counts B and starts gameplay for A.
    #[test]
    fn player_who_left_is_not_waited_for() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        for (img, fixed) in [(&orig, false), (&code, true)] {
            let nf = fixed.then_some(orig.functions.len());
            let mut s = Sim::new(img, &p, &["host", "a", "b"], nf);
            let a = s.connect("a");
            let b = s.connect("b");
            s.frame();
            s.ready(a);
            s.frame();
            assert!(!s.started(), "B is still connected and loading");
            s.drop_client(b);
            for _ in 0..3 {
                s.frame();
            }
            assert_eq!(s.started(), fixed);
            if fixed {
                assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
                assert_eq!(s.ready_count(), V::I(3));
                // B comes back: a late joiner, started on its own ReadyToStart.
                let b2 = s.connect("b");
                s.frame();
                s.ready(b2);
                assert_eq!(s.events[2..], [Ev::Send(b2)]);
            } else {
                assert!(s.events.is_empty());
            }
        }
    }

    /// B leaves first, then A gets ready: the count already includes B.
    #[test]
    fn left_before_the_others_were_ready() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        let b = s.connect("b");
        s.frame();
        s.drop_client(b);
        s.frame();
        assert!(!s.started());
        assert_eq!(s.ready_count(), V::I(2));
        s.ready(a);
        assert!(s.started());
        assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
    }

    /// Nobody leaves: the frames change nothing, the last ReadyToStart starts.
    #[test]
    fn normal_load_is_unchanged() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        s.frame();
        let b = s.connect("b");
        s.frame();
        s.ready(b);
        s.frame();
        assert!(!s.started());
        s.ready(a);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(b)]);
    }

    /// B reconnects during the wait: never forced (same user connected), and
    /// its second ReadyToStart is not counted twice (vanilla would start
    /// without A).
    #[test]
    fn reconnect_during_wait_counts_once() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        let b = s.connect("b");
        s.frame();
        s.ready(b);
        s.drop_client(b);
        let b2 = s.connect("b");
        s.frame();
        s.ready(b2);
        s.frame();
        assert!(!s.started(), "A is not ready yet");
        assert_eq!(s.ready_count(), V::I(2));
        s.ready(a);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(b2)]);
    }

    /// B leaves and is forced; B reconnects while A still loads. A's
    /// ReadyToStart must not start (and message) the loading B2: B2 is counted
    /// again only by its own ReadyToStart.
    #[test]
    fn forced_player_back_waits_for_its_own_ready() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        let b = s.connect("b");
        s.frame();
        s.drop_client(b);
        s.frame();
        assert_eq!(s.ready_count(), V::I(2), "B forced");
        let b2 = s.connect("b");
        s.ready(a);
        assert!(!s.started(), "B2 is still loading");
        assert_eq!(s.ready_count(), V::I(2));
        s.frame();
        s.ready(b2);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(b2)]);
    }

    /// B was ready, then reconnects: its new connection is not ready yet.
    #[test]
    fn ready_player_back_waits_for_its_own_ready() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        let b = s.connect("b");
        s.frame();
        s.ready(b);
        s.drop_client(b);
        let b2 = s.connect("b");
        s.frame();
        s.ready(a);
        assert!(!s.started(), "B2 is still loading");
        s.ready(b2);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(b2)]);
    }

    /// B connects and drops before the host counted itself: still forced.
    #[test]
    fn left_before_the_host_was_ready() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        s.set(s.game, p.game_ready, V::I(0));
        let a = s.connect("a");
        let b = s.connect("b");
        s.frame();
        s.drop_client(b);
        s.frame();
        assert_eq!(s.ready_count(), V::I(0));
        s.set(s.game, p.game_ready, V::I(1));
        s.frame();
        assert_eq!(s.ready_count(), V::I(2));
        s.ready(a);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
    }

    /// A user without a player in the count (removed in the lobby, or none at
    /// all) is never forced: forcing it would overshoot `==`.
    #[test]
    fn removed_or_unknown_user_is_not_counted() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "r"], Some(orig.functions.len()));
        let r = s.removed;
        s.list(r).push(V::S("r".into()));
        let a = s.connect("a");
        let rc = s.connect("r");
        let x = s.arrive("x");
        s.frame();
        s.drop_client(rc);
        s.drop_client(x);
        s.frame();
        assert_eq!(s.ready_count(), V::I(1));
        s.ready(a);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
    }

    /// A host reload (in-game load, battle restart) rebuilds options.multi
    /// without playersRemoved, so vanilla waits for the absent players of the
    /// save forever; the lobby's list is given back and A alone starts it.
    #[test]
    fn reload_keeps_absent_players() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(
            &code,
            &p,
            &["host", "a", "r1", "r2"],
            Some(orig.functions.len()),
        );
        let r = s.removed;
        s.list(r).push(V::S("r1".into()));
        s.list(r).push(V::S("r2".into()));
        s.frame();
        // The reload: new options.multi {hostID, isServer, serverID}, startServer.
        let opt = match s.get(s.game, p.game_options.0) {
            V::R(o) => o,
            v => panic!("options {v:?}"),
        };
        let multi = s.obj();
        s.set(opt, p.opt_multi.0, V::R(multi));
        s.set(s.game, p.game_ready, V::I(0));
        s.call(s.fun(k::RESET), vec![]);
        s.set(s.game, p.game_ready, V::I(1));
        // No frame between the reload and A's ReadyToStart.
        let a = s.connect("a");
        s.ready(a);
        assert_eq!(s.get(multi, p.multi_removed.0), V::R(r));
        assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
    }

    /// An absent player who came back (drop-in) before the reload is in the
    /// remembered list; its ReadyToStart takes it off and it is counted.
    #[test]
    fn reload_counts_a_returned_absent_player() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(
            &code,
            &p,
            &["host", "a", "r1", "r2"],
            Some(orig.functions.len()),
        );
        let r = s.removed;
        s.list(r).push(V::S("r1".into()));
        s.list(r).push(V::S("r2".into()));
        s.frame();
        let opt = match s.get(s.game, p.game_options.0) {
            V::R(o) => o,
            v => panic!("options {v:?}"),
        };
        let multi = s.obj();
        s.set(opt, p.opt_multi.0, V::R(multi));
        s.set(s.game, p.game_ready, V::I(0));
        s.call(s.fun(k::RESET), vec![]);
        s.set(s.game, p.game_ready, V::I(1));
        let a = s.connect("a");
        let c1 = s.connect("r1");
        s.ready(c1);
        assert_eq!(s.list(r).clone(), [V::S("r2".into())]);
        assert!(s.events.is_empty(), "started without A: {:?}", s.events);
        s.ready(a);
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(c1)]);
    }

    /// A solo game (no options.multi) forgets the remembered list.
    #[test]
    fn solo_forgets_absent_players() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "r1"], Some(orig.functions.len()));
        let r = s.removed;
        s.list(r).push(V::S("r1".into()));
        s.frame();
        let opt = match s.get(s.game, p.game_options.0) {
            V::R(o) => o,
            v => panic!("options {v:?}"),
        };
        s.set(opt, p.opt_multi.0, V::Null);
        s.frame();
        let multi = s.obj();
        s.set(opt, p.opt_multi.0, V::R(multi));
        s.frame();
        assert_eq!(s.get(multi, p.multi_removed.0), V::Null);
    }

    /// Host + A + B known, J1 + J2 still joining (connected, Join not handled:
    /// on their way, or parked by the barrier). Vanilla starts on B's
    /// ReadyToStart and sends the start to J1 and J2 before their SyncDone;
    /// patched, the start waits until both joined and are ready.
    #[test]
    fn joining_players_are_waited_for() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        for (img, fixed) in [(&orig, false), (&code, true)] {
            let nf = fixed.then_some(orig.functions.len());
            let mut s = Sim::new(img, &p, &["host", "a", "b"], nf);
            let a = s.connect("a");
            let b = s.connect("b");
            let j1 = s.arrive("j1");
            let j2 = s.arrive("j2");
            s.frame();
            s.ready(a);
            s.ready(b);
            s.frame();
            let all = [
                Ev::Start,
                Ev::Send(a),
                Ev::Send(b),
                Ev::Send(j1),
                Ev::Send(j2),
            ];
            if !fixed {
                assert_eq!(s.events, all, "vanilla starts the unsynced");
                continue;
            }
            assert!(!s.started(), "J1, J2 have not joined");
            assert_eq!(s.ready_count(), V::I(2), "B's count is held");
            s.join(j1);
            s.frame();
            assert!(!s.started(), "J2 has not joined");
            s.join(j2);
            s.frame();
            assert!(!s.started(), "J1, J2 joined, not ready");
            assert_eq!(s.ready_count(), V::I(3), "B counted again");
            s.ready(j1);
            s.frame();
            assert!(!s.started(), "J2 is not ready");
            s.ready(j2);
            assert_eq!(s.events, all);
            assert_eq!(s.ready_count(), V::I(5));
        }
    }

    /// The start is held for J, whose Join never comes: J drops (or the
    /// barrier's JOIN_CAP disconnects it), and the next frame starts.
    #[test]
    fn joining_player_who_drops_is_not_waited_for() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "b"], Some(orig.functions.len()));
        let a = s.connect("a");
        let b = s.connect("b");
        let j = s.arrive("j");
        s.frame();
        s.ready(a);
        s.ready(b);
        for _ in 0..3 {
            s.frame();
        }
        assert!(!s.started(), "J is still connected");
        s.drop_client(j);
        s.frame();
        assert_eq!(s.events, [Ev::Start, Ev::Send(a), Ev::Send(b)]);
        assert_eq!(s.ready_count(), V::I(3));
        // J comes back: a late joiner, started on its own ReadyToStart.
        let j2 = s.connect("j");
        s.ready(j2);
        assert_eq!(s.events[3..], [Ev::Send(j2)]);
    }

    /// The start goes only to clients whose ReadyToStart was counted: R
    /// (removed in the lobby, back and synced, outside the count) is still
    /// loading when A's ReadyToStart starts; R's own takes the late path.
    #[test]
    fn start_goes_only_to_ready_clients() {
        let Some((orig, code)) = images() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, &["host", "a", "r"], Some(orig.functions.len()));
        let removed = s.removed;
        s.list(removed).push(V::S("r".into()));
        let a = s.connect("a");
        let r = s.connect("r");
        s.frame();
        s.ready(a);
        assert!(s.started());
        assert_eq!(s.events, [Ev::Start, Ev::Send(a)]);
        s.ready(r);
        assert_eq!(s.events[2..], [Ev::Send(r)]);
    }
}
