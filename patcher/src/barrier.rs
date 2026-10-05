// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The co-op mode-switch barrier never waits forever.
//
// Every mode switch (leaving a town or the tavern, entering a place, starting a
// battle) runs a host barrier in st.Controller: `waitForClients` puts every
// client of `game.host.clients` into `waitLocks`, `waitForUnlock(cb)` parks the
// next step in `waitLockCallbs`, and each client's `onClientReady` RPC removes
// itself (`onClientReady__impl`); the last one runs the parked steps. Leaving
// takes two such phases, entering three or four, a battle start one. Nothing
// times out and nothing removes a client that is gone: a client that
// reconnects (`SteamService.onUserData` code 3 stops its old service, whose
// `NetworkClient.stop` takes it out of `host.clients`) stays in `waitLocks` as a
// dead object, and a client stuck before its answer (`waitAlive` never
// released, a refused `doLeaveMode`, a dropped fade callback) is waited for
// forever. Either way every screen stays black.
//
// This pass adds, on the host only:
//
//   waitForClients, at entry:         mpPhaseAt = sys_time()
//   Controller.update, at entry:      barrierTick(this)
//   the host's Join handler, at entry: if (joinGate(game, client, msg)) return
//
//   barrierTick(ctrl), every frame on the host (game.isAuth, game.host set):
//     a new controller (new game, reload) resets the state below;
//     soft-kicked clients still in host.clients TIMEOUT s later: c.stop();
//     deferred Joins: replayed (same handler, same client, still connected;
//       the gate lets a replay through) once no barrier is active; still
//       active JOIN_CAP s later: the parked clients are disconnected;
//     waitLocks entries no longer in host.clients (left, or replaced by their
//       reconnection), soft-kicked, or with a parked Join: removed. The last
//       ones connected before waitForClients took host.clients but have
//       nothing synced, so they can never answer; waiting for them kept the
//       barrier active and so their own Join parked (a save loaded into a
//       battle: Battle.onEndGeneration waits while clients reconnect);
//     waitLocks still not empty TIMEOUT s after the phase began: each late
//       client is removed and soft-kicked (rejoin; c.stop() if refused);
//     anything removed: release(ctrl).
//
//   rejoin(ctrl, game, host, c): the vanilla client reload, sent to c alone.
//     Host.loadGame sends every client `ctrl.reload(saveFile, localSaveFile,
//     serverID)`; the client's reload__impl disposes its game and starts a new
//     one connected to `serverID` that joins the running game: the host's Join
//     handler gives it its own ent.Player back (`set_ownerObject`, matched by
//     user id; the units hang off the player) and full-syncs the current state.
//     The RPC goes through the shared hxbit context, so pending property changes
//     and data are flushed to everyone first (`flushProps`, `flushSend`), then
//     `targetClient = c; ctrl.reload(..); flushSend(); targetClient = null`,
//     inside a trap that restores the target. Refused (false) without
//     game.options.multi.serverID, or while a target is already set.
//     The kicked object is quarantined (`mpKicked`): it is going away, so it is
//     never waited for again and its late answers count for nothing.
//
//   joinGate(game, c, msg): a Join (constructor 0) that arrives while a barrier
//     is active (lockSyncMode, waitLocks / waitLockCallbs / enterLeaveModeQueue
//     not empty, or a phase that began less than PHASE_GRACE s ago: the host's
//     own fade before its first parked step) is parked and true is returned.
//     A client full-synced in the middle of a switch would get the objects but
//     not the switch's earlier RPCs (doLeaveMode / doEnterMode) and end up in
//     the wrong mode. The joining client simply waits for its SyncDone longer.
//     Any Join (parked, replayed or let through) takes its client out of the
//     soft-kicked list: it is a full sync from scratch, and a client kicked
//     before it had synced anything ignored the rejoin RPC, so the tick would
//     otherwise disconnect a client that joined fine TIMEOUT s after the kick.
//     Kicked objects are not left out of `waitForClients` itself: their entry
//     goes on the next tick, and a stale answer removing it a frame earlier
//     releases nothing the tick would not release.
//
//   release(ctrl): the tail of onClientReady__impl, copied op for op: when
//     waitLocks is empty and callbacks are parked, swap the list and run them.
//
// TIMEOUT is per phase. A phase legitimately contains a client's mode load
// (seen up to 7.7 s for a town in shim.log); 30 s leaves room for slow disks.
// Clients that left are dropped at once; TIMEOUT only bounds clients stuck for
// good. JOIN_CAP bounds a Join parked behind a switch that never settles (the
// host's own fade callback lost, the host itself black): the parked client is
// disconnected rather than full-synced into a half-done switch.
//
// Not handled: the host's own fade callback lost before its first parked step
// (`Game.fadeTo` drops a fade): every list is empty, the host stays black, and
// after PHASE_GRACE a Join goes in. That host is broken with or without it.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use crate::job_xp::{const_str, str_global};
use hlbc::types::{RefGlobal, RefInt, ValBool};

/// Seconds a barrier phase waits for a client before it is asked to rejoin.
pub(crate) const TIMEOUT: f64 = 30.0;
/// Seconds a Join may be parked behind an active barrier.
pub(crate) const JOIN_CAP: f64 = 90.0;
/// Seconds after a phase began during which a switch counts as active (the host's fade).
pub(crate) const PHASE_GRACE: f64 = 3.0;
const LOG_GONE: &str = "mp: barrier: a client that left or rejoins is no longer waited for";
const LOG_REJOIN: &str = "mp: barrier: no answer in 30 s, asked to rejoin: ";
const LOG_STOP: &str = "mp: barrier: disconnected: ";
const LOG_JOINING: &str = "mp: barrier: a client whose Join waits is not waited for";
const LOG_PARK: &str = "mp: barrier: a Join waits for the mode switch to end";
const LOG_REPLAY: &str = "mp: barrier: a parked Join goes ahead";

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
    f64_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    ctrl_t: RefType,
    game_t: RefType,
    nc_t: RefType,
    nh_t: RefType,
    arr_t: RefType,
    msg_t: RefType,
    ctrl_game: RefField,
    wait_locks: RefField,
    callbs: RefField,
    mode_queue: RefField,
    lock_sync: RefField,
    game_auth: RefField,
    game_ctrl: RefField,
    game_host: F,
    game_options: F,
    opt_save: F,
    opt_local: F,
    opt_multi: F,
    multi_sid: F,
    host_clients: RefField,
    host_target: RefField,
    nc_host: F,
    arr_len: RefField,
    arr_raw: F,
    stop_slot: RefField,
    reload: RefFun,
    flush_props: RefFun,
    flush_send: RefFun,
    contains: RefFun,
    remove: RefFun,
    copy: RefFun,
    concat: RefFun,
    shift: RefFun,
    push: RefFun,
    sys_time: RefFun,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    new_arr: NewArr,
    update_fi: usize,
    clients_fi: usize,
    ready_fi: usize,
    join_fi: usize,
    /// First op of onClientReady__impl's tail (right after the waitLocks.remove).
    tail_at: usize,
    dbg_file: usize,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    if let Some(n) = code.natives.iter().find(|n| n.findex == f) {
        let t = n.t.as_fun(code).context("not a function type")?;
        return Ok((t.args.clone(), t.ret));
    }
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

/// Virtual-table slot of method `name` on `t` or one of its ancestors.
fn proto_slot(code: &Bytecode, t: RefType, name: &str) -> Result<RefField> {
    let mut cur = Some(t);
    while let Some(ct) = cur {
        let o = obj(code, ct)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == name) {
            if p.pindex < 0 {
                bail!("method {name} is not virtual");
            }
            return Ok(RefField(p.pindex as usize));
        }
        cur = o.super_;
    }
    bail!("method {name} not found on type {}", t.0)
}

/// onClientReady__impl: the op after its only `waitLocks.remove(cl)`, and `cl`.
fn ready_tail(f: &Function, wait_locks: RefField, remove: RefFun) -> Result<(usize, Reg)> {
    let removes: Vec<usize> = (0..f.ops.len())
        .filter(|&j| {
            matches!(f.ops[j], Opcode::Call2 { fun, arg0, .. } if fun == remove
                && f.ops[j.saturating_sub(2)..j].iter().any(
                    |o| matches!(o, Opcode::GetThis { dst, field } if *dst == arg0 && *field == wait_locks)))
        })
        .collect();
    let [rm] = removes[..] else {
        bail!(
            "onClientReady__impl: expected one waitLocks.remove, found {}",
            removes.len()
        );
    };
    let Opcode::Call2 { arg1, .. } = f.ops[rm] else {
        unreachable!()
    };
    Ok((rm + 1, arg1))
}

/// The empty-array allocation stored into `waitLockCallbs` by the tail.
fn plan_new_arr(code: &Bytecode, f: &Function, tail_at: usize, callbs: RefField) -> Result<NewArr> {
    let ops = &f.ops;
    let rt = |r: Reg| f.regs[r.0 as usize];
    let at = (tail_at..ops.len())
        .find(|&i| matches!(ops[i], Opcode::SetThis { field, .. } if field == callbs))
        .context("onClientReady__impl: the tail does not reset waitLockCallbs")?;
    let shape = || -> Option<NewArr> {
        let Opcode::SetThis { src, .. } = ops[at] else {
            return None;
        };
        let Opcode::Call1 {
            dst,
            fun: wrap,
            arg0: y,
        } = ops[at - 1]
        else {
            return None;
        };
        let Opcode::UnsafeCast { dst: y2, src: z } = ops[at - 2] else {
            return None;
        };
        let Opcode::Call2 {
            dst: z2,
            fun: alloc,
            arg0: t,
            arg1: n,
        } = ops[at - 3]
        else {
            return None;
        };
        let Opcode::Type { dst: t2, ty } = ops[at - 4] else {
            return None;
        };
        let Opcode::Int { dst: n2, ptr } = ops[at - 5] else {
            return None;
        };
        (dst == src && y2 == y && z2 == z && t2 == t && n2 == n && code.ints[ptr.0] == 0).then_some(
            NewArr {
                ty,
                type_t: rt(t),
                raw_t: rt(z),
                cast_t: rt(y),
                alloc,
                wrap,
                zero: ptr,
            },
        )
    };
    shape().context("onClientReady__impl: waitLockCallbs is not reset to a new empty array")
}

/// The host's Join handler: the `(Game, NetworkClient, NetworkMessage) -> Void`
/// closure that Game.startServer binds as `host.onMessage`.
fn plan_join(code: &Bytecode, game_t: RefType, nc_t: RefType) -> Result<(usize, RefType)> {
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
    let [(join, msg_t)] = hits[..] else {
        bail!(
            "Game.startServer: expected one message handler closure, found {}",
            hits.len()
        );
    };
    let Type::Enum { constructs, .. } = &code.types[msg_t.0] else {
        unreachable!()
    };
    if constructs.first().map(|c| s(code, c.name)) != Some("Join") {
        bail!("NetworkMessage constructor 0 is not Join");
    }
    let fi = fun_index(code, join)?;
    let f = &code.functions[fi];
    if !f
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::EnumIndex { value: Reg(2), .. }))
    {
        bail!("the message handler does not switch on the message");
    }
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
        bail!("the message handler: a jump targets op 0");
    }
    Ok((fi, msg_t))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let void_t = prim("void", |t| matches!(t, Type::Void))?;
    let bool_t = prim("bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim("i32", |t| matches!(t, Type::I32))?;
    let f64_t = prim("f64", |t| matches!(t, Type::F64))?;
    let dyn_t = prim("dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    if code.functions.iter().any(|f| {
        f.ops.iter().any(
            |op| matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(LOG_GONE)),
        )
    }) {
        bail!("already applied");
    }

    let ctrl_t = obj_type(code, "st.Controller")?;
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
    let ctrl_game = typed(ctrl_t, "game", game_t)?;
    let wait_locks = typed(ctrl_t, "waitLocks", arr_t)?;
    let callbs = typed(ctrl_t, "waitLockCallbs", arr_t)?;
    let mode_queue = typed(ctrl_t, "enterLeaveModeQueue", arr_t)?;
    let lock_sync = typed(ctrl_t, "lockSyncMode", bool_t)?;
    let game_auth = typed(game_t, "isAuth", bool_t)?;
    let game_ctrl = typed(game_t, "ctrl", ctrl_t)?;
    let game_host = field(code, game_t, "host")?;
    if !is_subclass(code, game_host.1, nh_t)? {
        bail!("Game.host is not an hxbit.NetworkHost");
    }
    let host_clients = typed(game_host.1, "clients", arr_t)?;
    let host_target = typed(game_host.1, "targetClient", nc_t)?;
    let nc_host = field(code, nc_t, "host")?;
    if nc_host.1 != nh_t {
        bail!("NetworkClient.host is not an hxbit.NetworkHost");
    }
    let game_options = field(code, game_t, "options")?;
    let opt_save = field_of_virtual(code, game_options.1, "saveFile")?;
    let opt_local = field_of_virtual(code, game_options.1, "localSaveFile")?;
    let opt_multi = field_of_virtual(code, game_options.1, "multi")?;
    let multi_sid = field_of_virtual(code, opt_multi.1, "serverID")?;
    if opt_save.1 != str_t || opt_local.1 != str_t || multi_sid.1 != str_t {
        bail!("Game.options save files / multi.serverID are not Strings");
    }
    let arr_len = typed(arr_t, "length", i32_t)?;
    let arr_raw = field(code, arr_t, "array")?;
    let stop_slot = proto_slot(code, nc_t, "stop")?;
    if sig(code, method(code, nc_t, "stop")?.findex)? != (vec![nc_t], void_t) {
        bail!("unexpected NetworkClient.stop signature");
    }

    let reload = method(code, ctrl_t, "reload")?.findex;
    if sig(code, reload)? != (vec![ctrl_t, str_t, str_t, str_t], void_t) {
        bail!("unexpected Controller.reload signature");
    }
    let flush_props = method(code, nh_t, "flushProps")?.findex;
    let flush_send = method(code, nh_t, "flushSend")?.findex;
    for f in [flush_props, flush_send] {
        if sig(code, f)? != (vec![nh_t], void_t) {
            bail!("unexpected NetworkHost fn@{} signature", f.0);
        }
    }
    let contains = proto(code, arr_t, "contains")?;
    let remove = proto(code, arr_t, "remove")?;
    let copy = proto(code, arr_t, "copy")?;
    let concat = proto(code, arr_t, "concat")?;
    let push = proto(code, arr_t, "push")?;
    let shift = proto(code, arr_t, "shift")?;
    if sig(code, contains)? != (vec![arr_t, dyn_t], bool_t)
        || sig(code, remove)? != (vec![arr_t, dyn_t], bool_t)
        || sig(code, copy)? != (vec![arr_t], arr_t)
        || sig(code, concat)? != (vec![arr_t, arr_t], arr_t)
        || sig(code, push)? != (vec![arr_t, dyn_t], i32_t)
        || sig(code, shift)? != (vec![arr_t], dyn_t)
    {
        bail!("unexpected ArrayObj contains / remove / copy / concat / push signature");
    }
    let sys_time = native(code, "sys_time", &[], f64_t)?;
    let println = crate::diag::static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println.findex;
    let std_string = crate::diag::static_fn(code, "$Std", "string")?.findex;
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, std_string)? != (vec![dyn_t], str_t)
        || sig(code, str_add)? != (vec![str_t, str_t], str_t)
    {
        bail!("unexpected Std.string / String.__add__ signature");
    }

    // Controller.update(dt) and waitForClients(): a call at op 0, which no jump targets.
    let update = method(code, ctrl_t, "update")?;
    if fun_args(code, update) != [ctrl_t, f64_t] {
        bail!("unexpected Controller.update signature");
    }
    let wfc = method(code, ctrl_t, "waitForClients")?;
    if fun_args(code, wfc) != [ctrl_t] {
        bail!("unexpected Controller.waitForClients signature");
    }
    for g in [update, wfc] {
        if (0..g.ops.len()).any(|i| jump_targets(g, i).contains(&0)) {
            bail!("fn@{}: a jump targets op 0", g.findex.0);
        }
    }
    // waitForClients pushes host.clients into waitLocks: the set the tick prunes against.
    let fills = wfc
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Field { field, .. } if *field == host_clients))
        && wfc
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::GetThis { field, .. } if *field == wait_locks))
        && wfc
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == push));
    if !fills {
        bail!("waitForClients does not fill waitLocks from host.clients");
    }

    // onClientReady__impl: `cl = host.rpcClient; if (cl == null) return;
    // waitLocks.remove(cl);` then the tail this pass reuses as release(ctrl).
    let ready = method(code, ctrl_t, "onClientReady__impl")?;
    if fun_args(code, ready) != [ctrl_t] {
        bail!("unexpected onClientReady__impl signature");
    }
    let (tail_at, cl) = ready_tail(ready, wait_locks, remove)?;
    let tail = &ready.ops[tail_at..];
    let closure_calls = tail
        .iter()
        .filter(|o| matches!(o, Opcode::CallClosure { args, .. } if args.is_empty()))
        .count();
    let reads_callbs = tail
        .iter()
        .any(|o| matches!(o, Opcode::GetThis { field, .. } if *field == callbs));
    if closure_calls != 1 || !reads_callbs || !matches!(tail.last(), Some(Opcode::Ret { .. })) {
        bail!("onClientReady__impl: the tail does not run the parked callbacks");
    }
    // Self-contained: no jump between head and tail, and the tail never reads the client.
    for i in 0..ready.ops.len() {
        for t in jump_targets(ready, i) {
            if (i < tail_at) != (t < tail_at) || (i < tail_at && t == tail_at) {
                bail!("onClientReady__impl: op {i} jumps across the tail boundary");
            }
        }
    }
    if tail.iter().any(|o| crate::chest_buttons::uses_reg(o, cl.0)) {
        bail!("onClientReady__impl: the tail reads the client register");
    }
    let new_arr = plan_new_arr(code, ready, tail_at, callbs)?;
    let (join_fi, msg_t) = plan_join(code, game_t, nc_t)?;

    Ok(Plan {
        void_t,
        bool_t,
        i32_t,
        f64_t,
        dyn_t,
        str_t,
        ctrl_t,
        game_t,
        nc_t,
        nh_t,
        arr_t,
        msg_t,
        ctrl_game,
        wait_locks,
        callbs,
        mode_queue,
        lock_sync,
        game_auth,
        game_ctrl,
        game_host,
        game_options,
        opt_save,
        opt_local,
        opt_multi,
        multi_sid,
        host_clients,
        host_target,
        nc_host,
        arr_len,
        arr_raw,
        stop_slot,
        reload,
        flush_props,
        flush_send,
        contains,
        remove,
        copy,
        concat,
        shift,
        push,
        sys_time,
        println,
        std_string,
        str_add,
        new_arr,
        update_fi: fun_index(code, update.findex)?,
        clients_fi: fun_index(code, wfc.findex)?,
        ready_fi: fun_index(code, ready.findex)?,
        join_fi,
        tail_at,
        dbg_file: debug_file(code, "src/st/Controller.hx")?,
    })
}

struct Globals {
    phase_at: RefGlobal,
    kicked: RefGlobal,
    kicked_at: RefGlobal,
    pending: RefGlobal,
    pending_at: RefGlobal,
    join_msg: RefGlobal,
    ctrl: RefGlobal,
    /// True while the tick replays a parked Join (the gate lets it through).
    replaying: RefGlobal,
}

/// `release(ctrl)`: onClientReady__impl's tail, with its registers.
fn add_release(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let ready = &code.functions[p.ready_fi];
    let regs = ready.regs.clone();
    let ops = ready.ops[p.tail_at..].to_vec();
    push_fn(code, vec![p.ctrl_t], p.void_t, regs, ops, p.dbg_file)
}

/// `rejoin(ctrl, game, host, c) -> Bool`: the reload RPC to `c` alone.
fn add_rejoin(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let host_t = p.game_host.1;
    let mut r = Regs(vec![p.ctrl_t, p.game_t, host_t, p.nc_t]);
    let (opts, multi, sid, save, local, prev, b, v, exc) = (
        r.r(p.game_options.1),
        r.r(p.opt_multi.1),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.nc_t),
        r.r(p.bool_t),
        r.r(p.void_t),
        r.r(p.dyn_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: opts,
        obj: Reg(1),
        field: p.game_options.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: opts,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: multi,
        obj: opts,
        field: p.opt_multi.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: multi,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: sid,
        obj: multi,
        field: p.multi_sid.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: sid,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: prev,
        obj: Reg(2),
        field: p.host_target,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: prev,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: save,
        obj: opts,
        field: p.opt_save.0,
    });
    a.op(Opcode::Field {
        dst: local,
        obj: opts,
        field: p.opt_local.0,
    });
    // Everything pending goes to everyone first; then the RPC to c alone.
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.flush_props,
        arg0: Reg(2),
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.flush_send,
        arg0: Reg(2),
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "caught");
    a.op(Opcode::SetField {
        obj: Reg(2),
        field: p.host_target,
        src: Reg(3),
    });
    a.op(Opcode::Call4 {
        dst: v,
        fun: p.reload,
        arg0: Reg(0),
        arg1: save,
        arg2: local,
        arg3: sid,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.flush_send,
        arg0: Reg(2),
    });
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::SetField {
        obj: Reg(2),
        field: p.host_target,
        src: prev,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("caught");
    a.op(Opcode::SetField {
        obj: Reg(2),
        field: p.host_target,
        src: prev,
    });
    a.label("no");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.ctrl_t, p.game_t, host_t, p.nc_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `active(ctrl, game) -> Bool`: a mode switch is under way on the host.
/// The host's own fade between the clients' answers and its first parked step
/// leaves every list empty; a phase that began less than PHASE_GRACE s ago
/// counts as active to cover it (not `game.fading`, which can stay set).
fn add_active(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let fgrace = float_const(code, PHASE_GRACE);
    let mut r = Regs(vec![p.ctrl_t, p.game_t]);
    let (b, arr, len, zero, now, at, grace) = (
        r.r(p.bool_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::Field {
        dst: b,
        obj: Reg(0),
        field: p.lock_sync,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "yes");
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::GetGlobal {
        dst: at,
        global: g.phase_at,
    });
    a.op(Opcode::Sub {
        dst: now,
        a: now,
        b: at,
    });
    a.op(Opcode::Float {
        dst: grace,
        ptr: fgrace,
    });
    a.jmp(
        Opcode::JSGt {
            a: grace,
            b: now,
            offset: 0,
        },
        "yes",
    );
    for (k, f) in [p.wait_locks, p.callbs, p.mode_queue]
        .into_iter()
        .enumerate()
    {
        let next = ["n0", "n1", "n2"][k];
        a.op(Opcode::Field {
            dst: arr,
            obj: Reg(0),
            field: f,
        });
        a.jmp(
            Opcode::JNull {
                reg: arr,
                offset: 0,
            },
            next,
        );
        a.op(Opcode::Field {
            dst: len,
            obj: arr,
            field: p.arr_len,
        });
        a.jmp(
            Opcode::JSGt {
                a: len,
                b: zero,
                offset: 0,
            },
            "yes",
        );
        a.label(next);
    }
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
    push_fn(
        code,
        vec![p.ctrl_t, p.game_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `own(ctrl)`: the state below belongs to one controller; a new one (new game,
/// reload) starts from nothing.
fn add_own(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![p.ctrl_t]);
    let (cur, arr, b, v) = (r.r(p.ctrl_t), r.r(p.arr_t), r.r(p.bool_t), r.r(p.void_t));
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: cur,
        global: g.ctrl,
    });
    a.jmp(
        Opcode::JEq {
            a: cur,
            b: Reg(0),
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: g.ctrl,
        src: Reg(0),
    });
    a.op(Opcode::Null { dst: arr });
    a.op(Opcode::SetGlobal {
        global: g.kicked,
        src: arr,
    });
    a.op(Opcode::SetGlobal {
        global: g.pending,
        src: arr,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.replaying,
        src: b,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.ctrl_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `joinGate(game, c, msg) -> Bool`: park a Join that arrives during a switch.
fn add_join_gate(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    active: RefFun,
    own: RefFun,
) -> Result<RefFun> {
    let log_park = str_global(code, p.str_t, LOG_PARK);
    let na = &p.new_arr;
    let mut r = Regs(vec![p.game_t, p.nc_t, p.msg_t]);
    let replay = r.r(p.bool_t);
    let (b, idx, zero, ctrl, pend, now, n, ty, raw, cast, msg, v) = (
        r.r(p.bool_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.ctrl_t),
        r.r(p.arr_t),
        r.r(p.f64_t),
        r.r(p.i32_t),
        r.r(na.type_t),
        r.r(na.raw_t),
        r.r(na.cast_t),
        r.r(p.str_t),
        r.r(p.void_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: b,
        obj: Reg(0),
        field: p.game_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "no");
    // Read before own(), which clears it for a new controller.
    a.op(Opcode::GetGlobal {
        dst: replay,
        global: g.replaying,
    });
    a.jmp(
        Opcode::JNull {
            reg: Reg(2),
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::EnumIndex {
        dst: idx,
        value: Reg(2),
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: na.zero,
    });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: zero,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Field {
        dst: ctrl,
        obj: Reg(0),
        field: p.game_ctrl,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: own,
        arg0: ctrl,
    });
    // A Join is a full sync from scratch: a client soft-kicked before it had
    // anything synced (the rejoin RPC found no controller there) is no longer
    // quarantined, or the tick would disconnect it TIMEOUT s after the kick.
    a.op(Opcode::GetGlobal {
        dst: pend,
        global: g.kicked,
    });
    a.jmp(
        Opcode::JNull {
            reg: pend,
            offset: 0,
        },
        "fresh",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: pend,
        arg1: Reg(1),
    });
    a.label("fresh");
    // A replay from the tick goes through (it was parked once already).
    a.jmp(
        Opcode::JTrue {
            cond: replay,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: active,
        arg0: ctrl,
        arg1: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "no");
    a.op(Opcode::GetGlobal {
        dst: pend,
        global: g.pending,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: pend,
            offset: 0,
        },
        "have",
    );
    a.op(Opcode::Int {
        dst: n,
        ptr: na.zero,
    });
    a.op(Opcode::Type { dst: ty, ty: na.ty });
    a.op(Opcode::Call2 {
        dst: raw,
        fun: na.alloc,
        arg0: ty,
        arg1: n,
    });
    a.op(Opcode::UnsafeCast {
        dst: cast,
        src: raw,
    });
    a.op(Opcode::Call1 {
        dst: pend,
        fun: na.wrap,
        arg0: cast,
    });
    a.op(Opcode::SetGlobal {
        global: g.pending,
        src: pend,
    });
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::SetGlobal {
        global: g.pending_at,
        src: now,
    });
    a.label("have");
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.push,
        arg0: pend,
        arg1: Reg(1),
    });
    a.op(Opcode::SetGlobal {
        global: g.join_msg,
        src: Reg(2),
    });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: log_park,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
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
    push_fn(
        code,
        vec![p.game_t, p.nc_t, p.msg_t],
        p.bool_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `barrierTick(ctrl)`, see the module comment.
#[allow(clippy::too_many_arguments)]
fn add_tick(
    code: &mut Bytecode,
    p: &Plan,
    g: &Globals,
    release: RefFun,
    rejoin: RefFun,
    active: RefFun,
    own: RefFun,
    join: RefFun,
) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let i1 = int_const(code, 1);
    let flim = float_const(code, TIMEOUT);
    let fcap = float_const(code, JOIN_CAP);
    let log_gone = str_global(code, p.str_t, LOG_GONE);
    let log_rejoin = str_global(code, p.str_t, LOG_REJOIN);
    let log_stop = str_global(code, p.str_t, LOG_STOP);
    let log_replay = str_global(code, p.str_t, LOG_REPLAY);
    let host_t = p.game_host.1;
    let mut r = Regs(vec![p.ctrl_t]);
    let (game, b, host, clients, wl, len, zero, one, i, raw, e, removed) = (
        r.r(p.game_t),
        r.r(p.bool_t),
        r.r(host_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.arr_raw.1),
        r.r(p.dyn_t),
        r.r(p.bool_t),
    );
    let (now, at, d, lim, late, kicked, c, msg, text, v, jm, ch) = (
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.nc_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.void_t),
        r.r(p.msg_t),
        r.r(p.nh_t),
    );
    let exc = r.r(p.dyn_t);
    let pend = r.r(p.arr_t);
    let log_joining = str_global(code, p.str_t, LOG_JOINING);
    let mut a = Asm::new();
    let log = |a: &mut Asm, g: RefGlobal, tail: Option<Reg>| {
        a.op(Opcode::GetGlobal {
            dst: msg,
            global: g,
        });
        if let Some(t) = tail {
            a.op(Opcode::Call2 {
                dst: msg,
                fun: p.str_add,
                arg0: msg,
                arg1: t,
            });
        }
        a.op(Opcode::Call1 {
            dst: v,
            fun: p.println,
            arg0: msg,
        });
    };
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(0),
        field: p.ctrl_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.game_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
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
    a.op(Opcode::Call1 {
        dst: v,
        fun: own,
        arg0: Reg(0),
    });
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.op(Opcode::Bool {
        dst: removed,
        value: ValBool(false),
    });
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: flim,
    });

    // 1. Soft-kicked clients that are still here TIMEOUT s later: drop the connection.
    a.op(Opcode::GetGlobal {
        dst: kicked,
        global: g.kicked,
    });
    a.jmp(
        Opcode::JNull {
            reg: kicked,
            offset: 0,
        },
        "joins",
    );
    a.op(Opcode::GetGlobal {
        dst: at,
        global: g.kicked_at,
    });
    a.op(Opcode::Sub {
        dst: d,
        a: now,
        b: at,
    });
    a.jmp(
        Opcode::JSGte {
            a: lim,
            b: d,
            offset: 0,
        },
        "joins",
    );
    a.op(Opcode::Null { dst: late });
    a.op(Opcode::SetGlobal {
        global: g.kicked,
        src: late,
    });
    a.op(Opcode::Field {
        dst: i,
        obj: kicked,
        field: p.arr_len,
    });
    a.loop_head("stale");
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: i,
            offset: 0,
        },
        "joins",
    );
    a.op(Opcode::Sub {
        dst: i,
        a: i,
        b: one,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: kicked,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: clients,
        arg1: e,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "stale");
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    a.op(Opcode::Call1 {
        dst: text,
        fun: p.std_string,
        arg0: e,
    });
    log(&mut a, log_stop, Some(text));
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.stop_slot,
        args: vec![c],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "stale");

    // 2. Parked Joins: replay them once no switch is under way; a switch still
    //    under way JOIN_CAP s later never settles: disconnect the parked clients.
    a.label("joins");
    a.op(Opcode::GetGlobal {
        dst: late,
        global: g.pending,
    });
    a.jmp(
        Opcode::JNull {
            reg: late,
            offset: 0,
        },
        "barrier",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: active,
        arg0: Reg(0),
        arg1: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "replay");
    a.op(Opcode::GetGlobal {
        dst: at,
        global: g.pending_at,
    });
    a.op(Opcode::Sub {
        dst: d,
        a: now,
        b: at,
    });
    a.op(Opcode::Float { dst: at, ptr: fcap });
    a.jmp(
        Opcode::JSGte {
            a: at,
            b: d,
            offset: 0,
        },
        "barrier",
    );
    a.op(Opcode::Null { dst: kicked });
    a.op(Opcode::SetGlobal {
        global: g.pending,
        src: kicked,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("drops");
    a.op(Opcode::Field {
        dst: len,
        obj: late,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "barrier",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: late,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Add {
        dst: i,
        a: i,
        b: one,
    });
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    a.op(Opcode::Field {
        dst: ch,
        obj: c,
        field: p.nc_host.0,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "drops");
    a.op(Opcode::Call1 {
        dst: text,
        fun: p.std_string,
        arg0: e,
    });
    log(&mut a, log_stop, Some(text));
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.stop_slot,
        args: vec![c],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "drops");

    // Each entry leaves the parked list before its handler runs, so a handler that
    // throws leaves the rest parked for the next tick.
    a.label("replay");
    a.op(Opcode::GetGlobal {
        dst: jm,
        global: g.join_msg,
    });
    a.loop_head("replays");
    a.op(Opcode::Field {
        dst: len,
        obj: late,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: len,
            offset: 0,
        },
        "replayed",
    );
    a.op(Opcode::Call1 {
        dst: e,
        fun: p.shift,
        arg0: late,
    });
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    // Still connected (NetworkClient.stop clears `host`)?
    a.op(Opcode::Field {
        dst: ch,
        obj: c,
        field: p.nc_host.0,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "replays");
    log(&mut a, log_replay, None);
    // The gate lets the replay through; the flag is cleared on both exits.
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::SetGlobal {
        global: g.replaying,
        src: b,
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "rethrow");
    a.op(Opcode::Call3 {
        dst: v,
        fun: join,
        arg0: game,
        arg1: c,
        arg2: jm,
    });
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.replaying,
        src: b,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "replays");
    a.label("rethrow");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetGlobal {
        global: g.replaying,
        src: b,
    });
    a.op(Opcode::Rethrow { exc });
    a.label("replayed");
    a.op(Opcode::Null { dst: kicked });
    a.op(Opcode::SetGlobal {
        global: g.pending,
        src: kicked,
    });

    // 3. The barrier itself.
    a.label("barrier");
    a.op(Opcode::Field {
        dst: wl,
        obj: Reg(0),
        field: p.wait_locks,
    });
    a.jmp(Opcode::JNull { reg: wl, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: len,
        obj: wl,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: len,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: kicked,
        global: g.kicked,
    });
    a.op(Opcode::GetGlobal {
        dst: pend,
        global: g.pending,
    });
    // Drop the entries that left host.clients, are being kicked, or whose Join is
    // parked (connected before waitForClients took host.clients, nothing synced:
    // they cannot answer, and waiting for them keeps their own Join parked).
    a.op(Opcode::Mov { dst: i, src: len });
    a.loop_head("prune");
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: i,
            offset: 0,
        },
        "pruned",
    );
    a.op(Opcode::Sub {
        dst: i,
        a: i,
        b: one,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: wl,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: clients,
        arg1: e,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "drop");
    a.jmp(
        Opcode::JNull {
            reg: kicked,
            offset: 0,
        },
        "parked",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: kicked,
        arg1: e,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "drop");
    a.label("parked");
    a.jmp(
        Opcode::JNull {
            reg: pend,
            offset: 0,
        },
        "prune",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.contains,
        arg0: pend,
        arg1: e,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "prune");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: wl,
        arg1: e,
    });
    a.op(Opcode::Bool {
        dst: removed,
        value: ValBool(true),
    });
    log(&mut a, log_joining, None);
    a.jmp(Opcode::JAlways { offset: 0 }, "prune");
    a.label("drop");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: wl,
        arg1: e,
    });
    a.op(Opcode::Bool {
        dst: removed,
        value: ValBool(true),
    });
    log(&mut a, log_gone, None);
    a.jmp(Opcode::JAlways { offset: 0 }, "prune");
    a.label("pruned");

    // Phase timeout: every late client is asked to rejoin (or disconnected).
    a.op(Opcode::Field {
        dst: len,
        obj: wl,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: len,
            offset: 0,
        },
        "release",
    );
    a.op(Opcode::GetGlobal {
        dst: at,
        global: g.phase_at,
    });
    a.op(Opcode::Sub {
        dst: d,
        a: now,
        b: at,
    });
    a.jmp(
        Opcode::JSGte {
            a: lim,
            b: d,
            offset: 0,
        },
        "release",
    );
    a.op(Opcode::Call1 {
        dst: late,
        fun: p.copy,
        arg0: wl,
    });
    a.jmp(
        Opcode::JNull {
            reg: kicked,
            offset: 0,
        },
        "first",
    );
    a.op(Opcode::Call2 {
        dst: kicked,
        fun: p.concat,
        arg0: kicked,
        arg1: late,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "keep");
    a.label("first");
    a.op(Opcode::Mov {
        dst: kicked,
        src: late,
    });
    a.label("keep");
    a.op(Opcode::SetGlobal {
        global: g.kicked,
        src: kicked,
    });
    a.op(Opcode::SetGlobal {
        global: g.kicked_at,
        src: now,
    });
    a.op(Opcode::Field {
        dst: i,
        obj: late,
        field: p.arr_len,
    });
    a.loop_head("kick");
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: i,
            offset: 0,
        },
        "release",
    );
    a.op(Opcode::Sub {
        dst: i,
        a: i,
        b: one,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: late,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: e,
        array: raw,
        index: i,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: wl,
        arg1: e,
    });
    a.op(Opcode::Bool {
        dst: removed,
        value: ValBool(true),
    });
    a.op(Opcode::UnsafeCast { dst: c, src: e });
    a.op(Opcode::Call1 {
        dst: text,
        fun: p.std_string,
        arg0: e,
    });
    a.op(Opcode::CallN {
        dst: b,
        fun: rejoin,
        args: vec![Reg(0), game, host, c],
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "hard");
    log(&mut a, log_rejoin, Some(text));
    a.jmp(Opcode::JAlways { offset: 0 }, "kick");
    a.label("hard");
    log(&mut a, log_stop, Some(text));
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.stop_slot,
        args: vec![c],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "kick");

    a.label("release");
    a.jmp(
        Opcode::JFalse {
            cond: removed,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: release,
        arg0: Reg(0),
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.ctrl_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let mut global = |t: RefType| {
        code.globals.push(t);
        RefGlobal(code.globals.len() - 1)
    };
    let g = Globals {
        phase_at: global(p.f64_t),
        kicked: global(p.arr_t),
        kicked_at: global(p.f64_t),
        pending: global(p.arr_t),
        pending_at: global(p.f64_t),
        join_msg: global(p.msg_t),
        ctrl: global(p.ctrl_t),
        replaying: global(p.bool_t),
    };
    let join = code.functions[p.join_fi].findex;
    let release = add_release(code, &p)?;
    let rejoin = add_rejoin(code, &p)?;
    let active = add_active(code, &p, &g)?;
    let own = add_own(code, &p, &g)?;
    let gate = add_join_gate(code, &p, &g, active, own)?;
    let tick = add_tick(code, &p, &g, release, rejoin, active, own, join)?;

    // waitForClients: mpPhaseAt = sys_time().
    let f = &mut code.functions[p.clients_fi];
    f.regs.push(p.f64_t);
    let now = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Call0 {
                dst: now,
                fun: p.sys_time,
            },
            Opcode::SetGlobal {
                global: g.phase_at,
                src: now,
            },
        ],
    );
    // Controller.update: barrierTick(this).
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
    // The Join handler: if (joinGate(game, client, msg)) return.
    let f = &mut code.functions[p.join_fi];
    f.regs.push(p.bool_t);
    let b = Reg((f.regs.len() - 1) as u32);
    f.regs.push(p.void_t);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Call3 {
                dst: b,
                fun: gate,
                arg0: Reg(0),
                arg1: Reg(1),
                arg2: Reg(2),
            },
            Opcode::JFalse { cond: b, offset: 1 },
            Opcode::Ret { ret: v },
        ],
    );
    eprintln!(
        "patched barrier: a co-op mode switch drops clients that left, asks a client silent for {TIMEOUT} s to rejoin, and holds Joins until it ends"
    );
    Ok(())
}

/// Lets the co-op barrier go on without clients that left or stopped answering,
/// or leaves `code` untouched and logs why.
pub(crate) fn patch_barrier(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("barrier skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    let touched = [p.clients_fi, p.update_fi, p.join_fi];
    let saved: Vec<Function> = touched.iter().map(|&i| code.functions[i].clone()).collect();
    if let Err(e) = apply(code, p) {
        snap.restore(code);
        for (&i, f) in touched.iter().zip(saved) {
            code.functions[i] = f;
        }
        eprintln!("barrier skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Patches a copy of the installed game (skipped when absent): only
    /// waitForClients, Controller.update and the Join handler gain their
    /// prologue, the new functions are well typed, release is
    /// onClientReady__impl's tail op for op, the image round-trips, and a second
    /// pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (cf, uf, jf, rf, tail_at) =
            (p.clients_fi, p.update_fi, p.join_fi, p.ready_fi, p.tail_at);
        let mut code = read(&image);
        patch_barrier(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 6);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let n = match i {
                _ if i == cf => 2,
                _ if i == uf => 1,
                _ if i == jf => 3,
                _ => {
                    assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops), "fn#{i}");
                    assert_eq!(a.regs, b.regs, "fn#{i}");
                    continue;
                }
            };
            shifted(a, b, 0, n);
            check_types(&back, b, 0..n);
        }
        // The Join gate returns before anything else and falls through otherwise.
        let j = &back.functions[jf];
        assert!(matches!(j.ops[1], Opcode::JFalse { offset: 1, .. }));
        assert!(matches!(j.ops[2], Opcode::Ret { .. }));
        let release = &back.functions[nf];
        let ready = &orig.functions[rf];
        assert_eq!(release.regs, ready.regs);
        assert_eq!(
            format!("{:?}", release.ops),
            format!("{:?}", &ready.ops[tail_at..])
        );
        for f in &back.functions[nf..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        let tick = back.functions[nf + 5].findex;
        assert!(
            matches!(back.functions[uf].ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == tick)
        );

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_barrier(&mut again);
        assert!(write(&again) == patched);
    }

    /// A tail that is entered from the head, or reads the client register, is
    /// refused and the image is left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (rf, tail_at) = (p.ready_fi, p.tail_at);
        let jump_in = |f: &mut Function| {
            // op 0 jumps to the op after the tail's first one.
            f.ops[0] = Opcode::JAlways {
                offset: tail_at as i32,
            };
        };
        let reads_client = |f: &mut Function| {
            let Opcode::Call2 { arg1, .. } = f.ops[tail_at - 1] else {
                panic!("no remove before the tail");
            };
            f.ops[tail_at] = Opcode::Mov {
                dst: Reg(5),
                src: arg1,
            };
        };
        for (what, mutate) in [
            ("jump in", &jump_in as &dyn Fn(&mut Function)),
            ("reads client", &reads_client),
        ] {
            let mut code = read(&image);
            mutate(&mut code.functions[rf]);
            let before = write(&code);
            assert!(plan(&code).is_err(), "{what}");
            patch_barrier(&mut code);
            assert!(write(&code) == before, "{what}");
        }
    }

    // A small interpreter for the functions this pass adds (gate, tick, active,
    // own), run against the patched installed game. The game's own code is
    // stubbed: release (runs the parked steps once waitLocks is empty), rejoin
    // (sent, nothing happens: the client has no synced controller yet), the Join
    // handler (records a full sync), NetworkClient.stop (leaves host.clients).

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        B(bool),
        I(i32),
        F(f64),
        R(usize),
        E(usize),
        S(String),
    }

    enum H {
        Obj(std::collections::HashMap<usize, V>),
        Arr(Vec<V>),
    }

    #[derive(Debug, PartialEq)]
    enum Ev {
        Release,
        Rejoin(usize),
        Join(usize),
        Stop(usize),
    }

    struct Sim<'a> {
        code: &'a Bytecode,
        p: &'a Plan,
        nf: usize,
        heap: Vec<H>,
        globals: std::collections::HashMap<usize, V>,
        now: f64,
        events: Vec<Ev>,
        log: Vec<String>,
        game: usize,
        ctrl: usize,
        host: usize,
        g0: usize,
    }

    impl<'a> Sim<'a> {
        fn new(code: &'a Bytecode, p: &'a Plan, nf: usize, g0: usize) -> Self {
            let mut s = Sim {
                code,
                p,
                nf,
                g0,
                heap: vec![],
                globals: Default::default(),
                now: 1000.0,
                events: vec![],
                log: vec![],
                game: 0,
                ctrl: 0,
                host: 0,
            };
            let (wl, cb, mq, cl) = (s.arr(), s.arr(), s.arr(), s.arr());
            s.game = s.obj();
            s.ctrl = s.obj();
            s.host = s.obj();
            s.set(s.game, p.game_auth, V::B(true));
            s.set(s.game, p.game_ctrl, V::R(s.ctrl));
            s.set(s.game, p.game_host.0, V::R(s.host));
            s.set(s.ctrl, p.ctrl_game, V::R(s.game));
            s.set(s.ctrl, p.wait_locks, V::R(wl));
            s.set(s.ctrl, p.callbs, V::R(cb));
            s.set(s.ctrl, p.mode_queue, V::R(mq));
            s.set(s.ctrl, p.lock_sync, V::B(false));
            s.set(s.host, p.host_clients, V::R(cl));
            s
        }
        fn obj(&mut self) -> usize {
            self.heap.push(H::Obj(Default::default()));
            self.heap.len() - 1
        }
        fn arr(&mut self) -> usize {
            self.heap.push(H::Arr(vec![]));
            self.heap.len() - 1
        }
        fn set(&mut self, o: usize, f: RefField, v: V) {
            let H::Obj(m) = &mut self.heap[o] else { panic!("not an object") };
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
            let H::Arr(a) = &mut self.heap[o] else { panic!("not an array") };
            a
        }
        fn field_arr(&self, o: usize, f: RefField) -> usize {
            let V::R(a) = self.get(o, f) else { panic!("no array") };
            a
        }
        fn wait_locks(&mut self) -> &mut Vec<V> {
            let a = self.field_arr(self.ctrl, self.p.wait_locks);
            self.list(a)
        }
        fn clients(&mut self) -> &mut Vec<V> {
            let a = self.field_arr(self.host, self.p.host_clients);
            self.list(a)
        }
        /// A client connects: in host.clients, its Join still on the way.
        fn connect(&mut self) -> usize {
            let c = self.obj();
            self.set(c, self.p.nc_host.0, V::R(self.host));
            self.clients().push(V::R(c));
            c
        }
        /// waitForClients + waitForUnlock(cb): a new barrier phase.
        fn phase(&mut self) {
            let g = self.phase_global();
            self.globals.insert(g, V::F(self.now));
            let cl = self.clients().clone();
            self.wait_locks().extend(cl);
            let cb = self.field_arr(self.ctrl, self.p.callbs);
            self.list(cb).push(V::Null);
        }
        /// apply() adds phase_at first.
        fn phase_global(&self) -> usize {
            self.g0
        }
        fn fun(&self, k: usize) -> RefFun {
            self.code.functions[self.nf + k].findex
        }
        fn gate(&mut self, c: usize) -> bool {
            let ctor0 = V::E(0);
            let r = self.call(self.fun(4), vec![V::R(self.game), V::R(c), ctor0]);
            r == V::B(true)
        }
        /// A Join reaches the patched handler.
        fn join_arrives(&mut self, c: usize) {
            let j = self.code.functions[self.p.join_fi].findex;
            self.call(j, vec![V::R(self.game), V::R(c), V::E(0)]);
        }
        /// onClientReady__impl: the head removes the client, the tail is release.
        fn ready(&mut self, c: usize) {
            self.wait_locks().retain(|x| *x != V::R(c));
            self.call(self.fun(0), vec![V::R(self.ctrl)]);
        }
        fn at(&self, e: &Ev) -> Option<usize> {
            self.events.iter().position(|x| x == e)
        }
        fn tick(&mut self) {
            self.call(self.fun(5), vec![V::R(self.ctrl)]);
        }
        /// Ticks every 0.25 s up to `t` s from the start.
        fn run_to(&mut self, t: f64) {
            while self.now < 1000.0 + t {
                self.now += 0.25;
                self.tick();
            }
        }
        fn call(&mut self, f: RefFun, args: Vec<V>) -> V {
            let p = self.p;
            if let Some(k) = (0..6).find(|&k| self.fun(k) == f) {
                match k {
                    0 => {
                        let cb = self.field_arr(self.ctrl, p.callbs);
                        if self.wait_locks().is_empty() && !self.list(cb).is_empty() {
                            self.list(cb).clear();
                            self.events.push(Ev::Release);
                        }
                        return V::Null;
                    }
                    1 => {
                        let V::R(c) = args[3] else { panic!() };
                        self.events.push(Ev::Rejoin(c));
                        return V::B(true);
                    }
                    _ => return self.exec(f, args),
                }
            }
            // The patched Join handler: the gate first, then the full sync.
            if f == self.code.functions[p.join_fi].findex {
                let V::R(c) = args[1] else { panic!() };
                if !self.gate(c) {
                    self.events.push(Ev::Join(c));
                }
                return V::Null;
            }
            let arr = |_: &Self, v: &V| match v {
                V::R(a) => *a,
                _ => panic!("not an array: {v:?}"),
            };
            if f == p.contains {
                let a = arr(self, &args[0]);
                return V::B(self.list(a).contains(&args[1]));
            }
            if f == p.remove {
                let a = arr(self, &args[0]);
                let l = self.list(a);
                let at = l.iter().position(|x| *x == args[1]);
                return V::B(at.map(|i| l.remove(i)).is_some());
            }
            if f == p.push {
                let a = arr(self, &args[0]);
                self.list(a).push(args[1].clone());
                return V::I(self.list(a).len() as i32);
            }
            if f == p.shift {
                let a = arr(self, &args[0]);
                let l = self.list(a);
                return if l.is_empty() { V::Null } else { l.remove(0) };
            }
            if f == p.copy {
                let a = arr(self, &args[0]);
                let v = self.list(a).clone();
                let n = self.arr();
                *self.list(n) = v;
                return V::R(n);
            }
            if f == p.concat {
                let (a, b) = (arr(self, &args[0]), arr(self, &args[1]));
                let mut v = self.list(a).clone();
                v.extend(self.list(b).clone());
                let n = self.arr();
                *self.list(n) = v;
                return V::R(n);
            }
            if f == p.new_arr.alloc {
                return V::R(self.arr());
            }
            if f == p.new_arr.wrap {
                return args[0].clone();
            }
            if f == p.sys_time {
                return V::F(self.now);
            }
            if f == p.println {
                let V::S(s) = &args[0] else { panic!() };
                self.log.push(s.clone());
                return V::Null;
            }
            if f == p.std_string {
                return V::S(format!("{:?}", args[0]));
            }
            if f == p.str_add {
                let (V::S(a), V::S(b)) = (&args[0], &args[1]) else { panic!() };
                return V::S(format!("{a}{b}"));
            }
            panic!("unexpected call fn@{}", f.0);
        }
        fn exec(&mut self, f: RefFun, args: Vec<V>) -> V {
            let code = self.code;
            let func = &code.functions[fun_index(code, f).unwrap()];
            let mut r: Vec<V> = vec![V::Null; func.regs.len()];
            for (i, a) in args.into_iter().enumerate() {
                r[i] = a;
            }
            let num = |v: &V| match v {
                V::I(i) => *i as f64,
                V::F(x) => *x,
                _ => panic!("not a number: {v:?}"),
            };
            let mut pc = 0usize;
            loop {
                let op = &func.ops[pc];
                pc += 1;
                let jump = |pc: &mut usize, off: i32| *pc = (*pc as i64 + off as i64) as usize;
                match op {
                    Opcode::Label | Opcode::EndTrap { .. } | Opcode::Trap { .. } => {}
                    Opcode::Ret { ret } => return r[ret.0 as usize].clone(),
                    Opcode::Mov { dst, src } | Opcode::UnsafeCast { dst, src } => {
                        r[dst.0 as usize] = r[src.0 as usize].clone()
                    }
                    Opcode::Null { dst } | Opcode::Type { dst, .. } => r[dst.0 as usize] = V::Null,
                    Opcode::Bool { dst, value } => r[dst.0 as usize] = V::B(value.0),
                    Opcode::Int { dst, ptr } => r[dst.0 as usize] = V::I(code.ints[ptr.0]),
                    Opcode::Float { dst, ptr } => r[dst.0 as usize] = V::F(code.floats[ptr.0]),
                    Opcode::Field { dst, obj, field } => {
                        let V::R(o) = r[obj.0 as usize] else { panic!("null access op {}", pc - 1) };
                        r[dst.0 as usize] = self.get(o, *field);
                    }
                    Opcode::GetGlobal { dst, global } => {
                        let v = match self.globals.get(&global.0) {
                            Some(v) => v.clone(),
                            None => match code.globals_initializers.get(global) {
                                Some(&ci) => {
                                    let c = &code.constants.as_ref().unwrap()[ci];
                                    V::S(code.strings[c.fields[0]].to_string())
                                }
                                None => match &code.types[code.globals[global.0].0] {
                                    Type::F64 => V::F(0.0),
                                    Type::Bool => V::B(false),
                                    _ => V::Null,
                                },
                            },
                        };
                        r[dst.0 as usize] = v;
                    }
                    Opcode::SetGlobal { global, src } => {
                        self.globals.insert(global.0, r[src.0 as usize].clone());
                    }
                    Opcode::EnumIndex { dst, value } => {
                        let V::E(i) = r[value.0 as usize] else { panic!() };
                        r[dst.0 as usize] = V::I(i as i32);
                    }
                    Opcode::Add { dst, a, b } | Opcode::Sub { dst, a, b } => {
                        let sub = matches!(op, Opcode::Sub { .. });
                        r[dst.0 as usize] = match (&r[a.0 as usize], &r[b.0 as usize]) {
                            (V::I(x), V::I(y)) => V::I(if sub { x - y } else { x + y }),
                            (x, y) => {
                                let (x, y) = (num(x), num(y));
                                V::F(if sub { x - y } else { x + y })
                            }
                        };
                    }
                    Opcode::GetArray { dst, array, index } => {
                        let (V::R(a), V::I(i)) = (&r[array.0 as usize], &r[index.0 as usize]) else {
                            panic!()
                        };
                        let (a, i) = (*a, *i as usize);
                        r[dst.0 as usize] = self.list(a)[i].clone();
                    }
                    Opcode::JAlways { offset } => jump(&mut pc, *offset),
                    Opcode::JTrue { cond, offset } | Opcode::JFalse { cond, offset } => {
                        let want = matches!(op, Opcode::JTrue { .. });
                        if r[cond.0 as usize] == V::B(want) {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JNull { reg, offset } | Opcode::JNotNull { reg, offset } => {
                        let want = matches!(op, Opcode::JNull { .. });
                        if (r[reg.0 as usize] == V::Null) == want {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JEq { a, b, offset } | Opcode::JNotEq { a, b, offset } => {
                        let want = matches!(op, Opcode::JEq { .. });
                        if (r[a.0 as usize] == r[b.0 as usize]) == want {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::JSGte { a, b, offset } | Opcode::JSGt { a, b, offset } => {
                        let (x, y) = (num(&r[a.0 as usize]), num(&r[b.0 as usize]));
                        let hit = if matches!(op, Opcode::JSGte { .. }) { x >= y } else { x > y };
                        if hit {
                            jump(&mut pc, *offset);
                        }
                    }
                    Opcode::Call0 { dst, fun } => r[dst.0 as usize] = self.call(*fun, vec![]),
                    Opcode::Call1 { dst, fun, arg0 } => {
                        let a = vec![r[arg0.0 as usize].clone()];
                        r[dst.0 as usize] = self.call(*fun, a);
                    }
                    Opcode::Call2 { dst, fun, arg0, arg1 } => {
                        let a = vec![r[arg0.0 as usize].clone(), r[arg1.0 as usize].clone()];
                        r[dst.0 as usize] = self.call(*fun, a);
                    }
                    Opcode::Call3 { dst, fun, arg0, arg1, arg2 } => {
                        let a = [arg0, arg1, arg2].iter().map(|x| r[x.0 as usize].clone()).collect();
                        r[dst.0 as usize] = self.call(*fun, a);
                    }
                    Opcode::CallN { dst, fun, args } => {
                        let a = args.iter().map(|x| r[x.0 as usize].clone()).collect();
                        r[dst.0 as usize] = self.call(*fun, a);
                    }
                    Opcode::CallMethod { dst, field, args } => {
                        assert_eq!(*field, self.p.stop_slot);
                        let V::R(c) = r[args[0].0 as usize] else { panic!() };
                        self.events.push(Ev::Stop(c));
                        self.clients().retain(|x| *x != V::R(c));
                        self.set(c, self.p.nc_host.0, V::Null);
                        r[dst.0 as usize] = V::Null;
                    }
                    o => panic!("sim: unsupported op {o:?}"),
                }
            }
        }
    }

    fn sim_image() -> Option<(Bytecode, Bytecode)> {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return None;
        };
        let orig = read(&image);
        let mut code = read(&image);
        patch_barrier(&mut code);
        Some((orig, code))
    }

    /// Loading a save into a battle: A is synced, B has reconnected but its
    /// Join is still on the way when Battle.onEndGeneration's waitForClients
    /// takes host.clients. B's Join is parked, so B can never answer; the
    /// barrier must not wait for it (the phase goes on without it, then the
    /// Join is replayed) and B, now synced, is never disconnected.
    #[test]
    fn parked_join_is_not_waited_for() {
        let Some((orig, code)) = sim_image() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, orig.functions.len(), orig.globals.len());
        s.tick();
        let a = s.connect();
        s.join_arrives(a);
        assert_eq!(s.events, [Ev::Join(a)]);
        let b = s.connect();
        s.phase();
        s.now += 0.5;
        s.join_arrives(b);
        assert_eq!(s.at(&Ev::Join(b)), None, "a Join during a switch is parked");
        s.ready(a);
        s.run_to(TIMEOUT - 1.0);
        let rel = s.at(&Ev::Release).expect("the phase goes on without the joining client");
        let join = s.at(&Ev::Join(b)).expect("the parked Join is replayed");
        assert!(rel < join, "{:?}", s.events);
        assert!(s.wait_locks().is_empty());
        s.run_to(4.0 * JOIN_CAP);
        assert!(
            !s.events.iter().any(|e| matches!(e, Ev::Rejoin(_) | Ev::Stop(_))),
            "{:?}\n{:?}",
            s.events,
            s.log
        );
    }

    /// A client soft-kicked before its Join arrived (the rejoin RPC reaches a
    /// client with nothing synced, so nothing happens) joins afterwards: that
    /// Join is a full sync, and the quarantine must not disconnect it later.
    #[test]
    fn join_after_kick_is_not_disconnected() {
        let Some((orig, code)) = sim_image() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, orig.functions.len(), orig.globals.len());
        s.tick();
        let b = s.connect();
        s.phase();
        s.run_to(TIMEOUT + 1.0);
        assert!(s.at(&Ev::Rejoin(b)).is_some(), "{:?}", s.events);
        assert!(s.at(&Ev::Release).is_some());
        s.now += 1.0;
        s.join_arrives(b);
        assert!(s.at(&Ev::Join(b)).is_some(), "{:?}", s.events);
        s.run_to(4.0 * JOIN_CAP);
        assert_eq!(s.at(&Ev::Stop(b)), None, "{:?}\n{:?}", s.events, s.log);
    }

    /// Unchanged: a synced client that never answers is asked to rejoin after
    /// TIMEOUT and disconnected TIMEOUT later if it is still there.
    #[test]
    fn silent_client_is_still_dropped() {
        let Some((orig, code)) = sim_image() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, orig.functions.len(), orig.globals.len());
        s.tick();
        let a = s.connect();
        s.join_arrives(a);
        s.phase();
        s.run_to(TIMEOUT - 1.0);
        assert_eq!(s.at(&Ev::Release), None);
        s.run_to(TIMEOUT + 1.0);
        assert!(s.at(&Ev::Rejoin(a)).is_some() && s.at(&Ev::Release).is_some());
        s.run_to(2.0 * TIMEOUT + 1.0);
        assert!(s.at(&Ev::Stop(a)).is_some(), "{:?}", s.events);
    }

    /// A switch that never settles (lockSyncMode held): the parked client is
    /// disconnected after JOIN_CAP and nothing is left to wait for.
    #[test]
    fn join_cap_drops_and_wait_set_recovers() {
        let Some((orig, code)) = sim_image() else { return };
        let p = plan(&orig).expect("plan");
        let mut s = Sim::new(&code, &p, orig.functions.len(), orig.globals.len());
        s.tick();
        let b = s.connect();
        s.set(s.ctrl, p.lock_sync, V::B(true));
        s.phase();
        s.join_arrives(b);
        s.run_to(JOIN_CAP - 1.0);
        assert_eq!(s.at(&Ev::Stop(b)), None);
        assert!(s.wait_locks().is_empty(), "the parked client is not waited for");
        s.run_to(JOIN_CAP + 1.0);
        assert!(s.at(&Ev::Stop(b)).is_some(), "{:?}", s.events);
        assert_eq!(s.at(&Ev::Join(b)), None);
        assert!(s.clients().is_empty() && s.wait_locks().is_empty());
    }
}