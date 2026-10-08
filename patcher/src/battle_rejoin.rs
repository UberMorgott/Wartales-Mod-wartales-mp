// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A player who rejoins during a battle gets the battle, not an endless loading
// screen.
//
// Vanilla: the host starts a battle once, in Battle.onEndGeneration, with four
// one-shot RPCs to the clients connected at that moment (battle/State.hx,
// `@:rpc(clients)`: on the host they only send):
//
//   state.netInitGrid()        -> Battle.initGrid
//   state.netBeginBattle()     -> host beginBattle + state.clientBeginBattle()
//                                 -> Battle.beginBattle
//   state.initClients()        -> Battle.initClients: map, world, then initUI,
//                                 whose waitUntil(battleInit) stops the loading
//                                 screen
//   state.netAfterGen(false)   -> Battle.afterGen -> battleInit = true
//
// A client that reconnects while the battle runs is a late joiner: the host's
// message handler answers its ReadyToStart with `client.sendMessage(ReadyToStart)`,
// the client runs gameplayStart and enters the battle mode (music starts), and
// nothing else ever comes: Battle.initClients never runs there and the loading
// screen stays up for good. Vanilla bug (the in-game reconnect lobby exists in
// vanilla from the Pause menu; drop_in makes it exist from the start).
//
// The patch, on the host only: in the late-joiner branch, right after that send,
//
//   battleRejoin(game, client):
//     b = game.battle;  if (b == null || game.mode != b || !b.battleInit) return;
//     h = game.host;    st = b.state;
//     h.flush();                       // pending updates go to everyone first
//     prev = h.targetClient; h.targetClient = client;
//     try {
//       st.netInitGrid(); st.clientBeginBattle(); st.initClients(); st.netAfterGen(false);
//     } catch (_) {}
//     h.flushSend(); h.targetClient = prev;   // the replay reached `client` only
//     println(LOG)
//
// hxbit's NetworkHost.send writes to targetClient alone when it is set (the
// same mechanism sendMessage(msg, to) uses). The client handles the bytes after
// the ReadyToStart they follow: gameplayStart enters the battle synchronously
// (init sets game.battle, afterEnter sets lockAlives), so the replay finds the
// battle exactly as the vanilla RPCs do after a host save load mid-fight. A host
// still generating (battleInit false) sends the vanilla RPCs to every connected
// client itself, the late joiner included.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::diag::static_fn;
use crate::job_xp::{const_str, str_global};
use hlbc::types::ValBool;

const LOG: &str = "mp: battle rejoin: the running battle was replayed to a late joiner";

struct Plan {
    void_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    game_t: RefType,
    nc_t: RefType,
    battle_t: RefType,
    mode_t: RefType,
    bstate_t: RefType,
    host_t: RefType,
    game_battle: RefField,
    game_mode: RefField,
    game_host: RefField,
    battle_init: RefField,
    battle_state: RefField,
    target: RefField,
    flush: RefFun,
    flush_send: RefFun,
    init_grid: RefFun,
    begin: RefFun,
    init_clients: RefFun,
    after_gen: RefFun,
    println: RefFun,
    /// The message handler and the op after the late joiner's send.
    join_fi: usize,
    at: usize,
    client: Reg,
    dbg_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let void_t = prim("void", |t| matches!(t, Type::Void))?;
    let bool_t = prim("bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim("dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    if code.functions.iter().any(|f| {
        f.ops.iter().any(
            |op| matches!(op, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(LOG)),
        )
    }) {
        bail!("already applied");
    }
    let game_t = obj_type(code, "Game")?;
    let nc_t = obj_type(code, "hxbit.NetworkClient")?;
    let nh_t = obj_type(code, "hxbit.NetworkHost")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let bstate_t = obj_type(code, "battle.State")?;
    let (game_battle, t) = field(code, game_t, "battle")?;
    if t != battle_t {
        bail!("Game.battle is not a battle.Battle");
    }
    let (game_mode, mode_t) = field(code, game_t, "mode")?;
    if !is_sub(code, battle_t, mode_t) {
        bail!("battle.Battle is not a Game.mode type");
    }
    let (game_host, host_t) = field(code, game_t, "host")?;
    if !is_sub(code, host_t, nh_t) {
        bail!("Game.host is not an hxbit.NetworkHost");
    }
    let battle_init = typed(code, battle_t, "battleInit", bool_t)?;
    let battle_state = typed(code, battle_t, "state", bstate_t)?;
    let target = typed(code, host_t, "targetClient", nc_t)?;
    let host_fn = |name: &str| -> Result<RefFun> {
        let f = method(code, nh_t, name)?.findex;
        if sig(code, f)? != (vec![nh_t], void_t) {
            bail!("NetworkHost.{name} is not (NetworkHost) -> void");
        }
        Ok(f)
    };
    let flush = host_fn("flush")?;
    let flush_send = host_fn("flushSend")?;
    // The four client RPCs: on the host (isAuth) they only send.
    let rpc = |name: &str, args: Vec<RefType>, impl_name: &str| -> Result<RefFun> {
        let f = method(code, bstate_t, name)?;
        if sig(code, f.findex)? != (args, void_t) {
            bail!("battle.State.{name}: unexpected signature");
        }
        let imp = method(code, bstate_t, impl_name)?.findex;
        let calls_impl = |o: &Opcode| match o {
            Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } => *fun == imp,
            _ => false,
        };
        // `if (__host != null && __host.isAuth) { send; return } impl(...)`.
        let n = f.ops.len();
        let ret_before_impl = n >= 3
            && calls_impl(&f.ops[n - 2])
            && matches!(f.ops[n - 3], Opcode::Ret { .. })
            && f.ops.iter().filter(|o| calls_impl(o)).count() == 1;
        if !ret_before_impl {
            bail!("battle.State.{name} is not a clients RPC");
        }
        Ok(f.findex)
    };
    let init_grid = rpc("netInitGrid", vec![bstate_t], "netInitGrid__impl")?;
    let begin = rpc(
        "clientBeginBattle",
        vec![bstate_t],
        "clientBeginBattle__impl",
    )?;
    let init_clients = rpc("initClients", vec![bstate_t], "initClients__impl")?;
    let after_gen = rpc("netAfterGen", vec![bstate_t, bool_t], "netAfterGen__impl")?;
    let println = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println.findex;

    // The handler; the ReadyToStart global is the one the start broadcast
    // sends in its loop (`GetGlobal g; Call2 send(c, g); JAlways back`).
    let (join_fi, msg_t) = crate::ready_start::plan_handler(code, game_t, nc_t)?;
    let h = &code.functions[join_fi];
    if h.regs.first() != Some(&game_t) {
        bail!("the message handler is not (Game, NetworkClient, msg)");
    }
    let sends = |i: usize| match (&h.ops[i], &h.ops[i + 1]) {
        (Opcode::GetGlobal { dst, global }, Opcode::Call2 { arg0, arg1, .. })
            if arg1 == dst && h.regs[dst.0 as usize] == msg_t =>
        {
            Some((*global, *arg0))
        }
        _ => None,
    };
    let n = h.ops.len();
    let loop_globals: Vec<RefGlobal> = (0..n.saturating_sub(2))
        .filter(|&i| matches!(h.ops[i + 2], Opcode::JAlways { offset } if offset < 0))
        .filter_map(|i| sends(i).map(|(g, _)| g))
        .collect();
    let [ready] = loop_globals[..] else {
        bail!("the message handler: expected one start broadcast loop");
    };
    // The late joiner's `client.sendMessage(ReadyToStart)`: the other send of it.
    let late: Vec<(usize, Reg)> = (0..n.saturating_sub(2))
        .filter(|&i| !matches!(h.ops[i + 2], Opcode::JAlways { offset } if offset < 0))
        .filter_map(|i| match sends(i) {
            Some((g, c)) if g == ready && is_sub(code, h.regs[c.0 as usize], nc_t) => Some((i, c)),
            _ => None,
        })
        .collect();
    let [(late, client)] = late[..] else {
        bail!(
            "the message handler: {} late-joiner ReadyToStart sends {late:?}, want 1",
            late.len()
        );
    };
    let at = late + 2;
    if at >= n || (0..n).any(|i| jump_targets(h, i).contains(&at)) {
        bail!("the message handler: the late-joiner send has an unexpected shape");
    }
    Ok(Plan {
        void_t,
        bool_t,
        dyn_t,
        str_t,
        game_t,
        nc_t,
        battle_t,
        mode_t,
        bstate_t,
        host_t,
        game_battle,
        game_mode,
        game_host,
        battle_init,
        battle_state,
        target,
        flush,
        flush_send,
        init_grid,
        begin,
        init_clients,
        after_gen,
        println,
        join_fi,
        at,
        client,
        dbg_file: debug_file(code, "src/Game.hx")?,
    })
}

/// battleRejoin(game, client), as in the header.
fn add_rejoin(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let log = str_global(code, p.str_t, LOG);
    let mut r = Regs(vec![p.game_t, p.nc_t]);
    let (game, c) = (Reg(0), Reg(1));
    let b = r.r(p.battle_t);
    let m = r.r(p.mode_t);
    let ok = r.r(p.bool_t);
    let st = r.r(p.bstate_t);
    let h = r.r(p.host_t);
    let prev = r.r(p.nc_t);
    let v = r.r(p.void_t);
    let e = r.r(p.dyn_t);
    let txt = r.r(p.str_t);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.game_battle,
    });
    a.jmp(Opcode::JNull { reg: b, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: m,
        obj: game,
        field: p.game_mode,
    });
    a.jmp(Opcode::JNotEq { a: m, b, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: ok,
        obj: b,
        field: p.battle_init,
    });
    a.jmp(
        Opcode::JFalse {
            cond: ok,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: st,
        obj: b,
        field: p.battle_state,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: h,
        obj: game,
        field: p.game_host,
    });
    a.jmp(Opcode::JNull { reg: h, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.flush,
        arg0: h,
    });
    a.op(Opcode::Field {
        dst: prev,
        obj: h,
        field: p.target,
    });
    a.op(Opcode::SetField {
        obj: h,
        field: p.target,
        src: c,
    });
    a.jmp(Opcode::Trap { exc: e, offset: 0 }, "caught");
    for fun in [p.init_grid, p.begin, p.init_clients] {
        a.op(Opcode::Call1 {
            dst: v,
            fun,
            arg0: st,
        });
    }
    a.op(Opcode::Bool {
        dst: ok,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.after_gen,
        arg0: st,
        arg1: ok,
    });
    a.op(Opcode::EndTrap { exc: e });
    // Both paths: what was written goes to `c` only, then the target is reset.
    let restore = |a: &mut Asm| {
        a.op(Opcode::Call1 {
            dst: v,
            fun: p.flush_send,
            arg0: h,
        });
        a.op(Opcode::SetField {
            obj: h,
            field: p.target,
            src: prev,
        });
    };
    restore(&mut a);
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: log,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: txt,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    a.label("caught");
    restore(&mut a);
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.nc_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let rejoin = add_rejoin(code, p)?;
    let f = &mut code.functions[p.join_fi];
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.at,
        vec![Opcode::Call2 {
            dst: v,
            fun: rejoin,
            arg0: Reg(0),
            arg1: p.client,
        }],
    );
    eprintln!(
        "patched battle rejoin fn@{} (message handler) op {}: a late joiner during a battle gets \
         the battle's start RPCs (battleRejoin fn@{})",
        f.findex.0, p.at, rejoin.0
    );
    Ok(())
}

/// Replays a running battle's start to a player who rejoins during it, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_battle_rejoin(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("battle rejoin skipped: {e:#}"));
            return;
        }
    };
    let snap = crate::asm::Snap::take(code);
    let saved = code.functions[p.join_fi].clone();
    if let Err(e) = apply(code, &p) {
        snap.restore(code);
        code.functions[p.join_fi] = saved;
        crate::skipped(format!("battle rejoin skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// On the installed game, after ready_start as in patch_image: only the
    /// handler changes (one call after the late send), the new function is well
    /// typed with a closed trap, and a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let mut orig = read(&image);
        crate::ready_start::patch_ready_start(&mut orig);
        let base = write(&orig);
        let orig = read(&base);
        let p = plan(&orig).expect("plan");
        let mut code = read(&base);
        patch_battle_rejoin(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != p.join_fi, "function #{i}");
        }
        let f = &back.functions[nf];
        check_types(&back, f, 0..f.ops.len());
        check_flow(f);
        assert_eq!(traps_ok(f), 1);
        let (a, b) = (&orig.functions[p.join_fi], &back.functions[p.join_fi]);
        shifted(a, b, p.at, 1);
        check_types(&back, b, p.at..p.at + 1);
        check_flow(b);
        assert!(matches!(a.ops[p.at - 1], Opcode::Call2 { .. }));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_battle_rejoin(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass also finds its site on the vanilla handler.
    #[test]
    fn plans_on_vanilla_handler() {
        let Some(image) = game() else { return };
        plan(&read(&image)).expect("plan");
    }
}
