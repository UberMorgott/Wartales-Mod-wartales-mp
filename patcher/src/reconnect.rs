// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op guest that loses its game connection reconnects on its own.
//
// Vanilla client connect (Game.hx:482-519): `tryConnectToServer` stamps
// `tryConnectStart` once and calls `host.connect(options.multi.serverID, cb)`;
// the callback (the closure the function builds) on success sets
// `connectedToHost` and sends Join; on failure it
//   - returns if the game is `reloading` (the old game of a reload),
//   - with a `ui` (the game is running): shows "disconnected", then the title,
//   - without one (still loading): retries in 1 s until
//     tryConnectStart + CONNECTION_TRY_TIMEOUT (300 s), then join_error -> title.
// HostWT.connect closes the previous service first (Host.close -> the
// service's virtual `stop`, which calls none of its callbacks), and the
// service's onStop (a lost connection) calls the callback with false.
//
// Two holes: a connect that never resolves (no onConnect, no onStop: a Steam
// connect request lost while the host recreated its server on a save load,
// the link itself still up) leaves the guest on the loading screen for good;
// and a connection lost mid-game ends the session instead of rejoining. The
// host keeps the player's slot either way: its Join handler gives a returning
// user its own ent.Player back (barrier.rs rejoin, drop_in.rs).
//
// The pass, on the client paths only (the host never calls these):
//   C1 tryConnectToServer, at entry:
//        mpConnGame = this; mpConnAt = appTime(); this.connectedToHost = false;
//      (an attempt is not connected until its own callback says so; a retry
//      after a lost load connection had it true from the earlier attempt)
//   C2 Game.update, at entry (runs every frame, loading included):
//        if (mpConnGame == this) {
//          if (connectedToHost) mpConnGame = null;
//          else if (appTime() - mpConnAt >= WAIT) {
//            mpConnGame = null; println(LOG_HANG);
//            if (host != null) host.close();     // the pending service goes
//            cb(this, false);                    // the vanilla failure path
//          }
//        }
//      so a hung attempt counts as failed: retry in 1 s within the window,
//      or join_error -> title after it. A new attempt re-arms C1.
//   C3 the callback, right after the vanilla `reloading` return:
//        if (ui != null && ctrl != null && options.multi.serverID != null) {
//          println(LOG_REJOIN);
//          ctrl.reload__impl(options.saveFile, options.localSaveFile, serverID);
//          return;
//        }
//      the vanilla client reload (the host's ctrl.reload RPC calls the same
//      impl): dispose, start a new game connected to the id this game used,
//      Join; the new game has no ui while it loads, so it gets the vanilla
//      retry window and C2. A client starts without reading local saves
//      (Game.start), the state comes from the host.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::Asm;
use crate::job_xp::{const_str, str_global};

/// Seconds a connect attempt may stay unresolved before it counts as failed.
pub(crate) const WAIT: f64 = 20.0;
const LOG_HANG: &str = "mp: reconnect: no answer to the game connect in 20 s, retrying";
const LOG_REJOIN: &str = "mp: reconnect: connection to the host lost, rejoining";

struct Plan {
    game_t: RefType,
    bool_t: RefType,
    f64_t: RefType,
    str_t: RefType,
    void_t: RefType,
    try_fi: usize,
    update_fi: usize,
    cb_fi: usize,
    cb: RefFun,
    /// Index of the callback's `Ret` after the reloading test.
    cb_ret: usize,
    app_time: RefFun,
    println: RefFun,
    connected: RefField,
    host: (RefField, RefType),
    close: RefFun,
    ui: (RefField, RefType),
    ctrl: (RefField, RefType),
    opts: (RefField, RefType),
    multi: (RefField, RefType),
    sid: RefField,
    save: RefField,
    local: RefField,
    reload: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let str_t = obj_type(code, "String")?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let f64_t = prim_type(code, "f64", |t| matches!(t, Type::F64))?;

    let tc = method(code, game_t, "tryConnectToServer")?;
    if fun_args(code, tc) != [game_t] {
        bail!("tryConnectToServer: unexpected signature");
    }
    if (0..tc.ops.len()).any(|i| jump_targets(tc, i).contains(&0)) {
        bail!("tryConnectToServer: a jump targets op 0");
    }
    let cbs: Vec<RefFun> = tc
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::InstanceClosure {
                fun, obj: Reg(0), ..
            } => Some(*fun),
            _ => None,
        })
        .collect();
    let [cb] = cbs[..] else {
        bail!("tryConnectToServer: expected one callback closure");
    };
    let (args, ret) = sig(code, cb)?;
    if args != [game_t, bool_t] || ret != void_t {
        bail!("connect callback fn@{}: unexpected signature", cb.0);
    }
    let cb_fi = fun_index(code, cb)?;
    let f = &code.functions[cb_fi];
    if f.ops.iter().any(
        |o| matches!(o, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(LOG_REJOIN)),
    ) {
        bail!("connect callback fn@{}: already applied", cb.0);
    }

    let connected = typed(code, game_t, "connectedToHost", bool_t)?;
    let reloading = typed(code, game_t, "reloading", bool_t)?;
    // Vanilla: JFalse ok; ...; Field r = this.reloading; JFalse r +1; Ret
    let hits: Vec<usize> = (0..f.ops.len().saturating_sub(2))
        .filter(|&k| {
            matches!(f.ops[k], Opcode::Field { obj: Reg(0), field, dst } if field == reloading
                && matches!(f.ops[k + 1], Opcode::JFalse { cond, offset: 1 } if cond == dst))
                && matches!(f.ops[k + 2], Opcode::Ret { .. })
        })
        .collect();
    let [k] = hits[..] else {
        bail!("connect callback fn@{}: no single reloading return", cb.0);
    };
    let cb_ret = k + 2;
    if (0..f.ops.len()).any(|i| i != k + 1 && jump_targets(f, i).contains(&(cb_ret + 1))) {
        bail!(
            "connect callback fn@{}: the op after the reloading return is a join",
            cb.0
        );
    }
    let times: Vec<RefFun> = f
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call0 { fun, dst } if f.regs[dst.0 as usize] == f64_t => Some(*fun),
            _ => None,
        })
        .collect();
    let [app_time] = times[..] else {
        bail!("connect callback fn@{}: expected one appTime call", cb.0);
    };
    if sig(code, app_time)? != (vec![], f64_t) {
        bail!("appTime: unexpected signature");
    }

    let update = method(code, game_t, "update")?;
    if fun_args(code, update).first() != Some(&game_t) {
        bail!("Game.update: unexpected signature");
    }
    if (0..update.ops.len()).any(|i| jump_targets(update, i).contains(&0)) {
        bail!("Game.update: a jump targets op 0");
    }

    let host = field(code, game_t, "host")?;
    let net_host = obj_type(code, "mpman.net.Host")?;
    if !is_sub(code, host.1, net_host) {
        bail!("Game.host is not an mpman.net.Host");
    }
    let close = method(code, net_host, "close")?;
    if fun_args(code, close) != [net_host] {
        bail!("Host.close: unexpected signature");
    }
    let ui = field(code, game_t, "ui")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let ctrl = (typed(code, game_t, "ctrl", ctrl_t)?, ctrl_t);
    let reload = method(code, ctrl_t, "reload__impl")?.findex;
    if sig(code, reload)? != (vec![ctrl_t, str_t, str_t, str_t], void_t) {
        bail!("reload__impl: unexpected signature");
    }
    let opts = field(code, game_t, "options")?;
    let multi = field_of_virtual(code, opts.1, "multi")?;
    let sid = field_of_virtual(code, multi.1, "serverID")?;
    let save = field_of_virtual(code, opts.1, "saveFile")?;
    let local = field_of_virtual(code, opts.1, "localSaveFile")?;
    if sid.1 != str_t || save.1 != str_t || local.1 != str_t {
        bail!("options.saveFile / localSaveFile / multi.serverID are not Strings");
    }
    let println = crate::diag::static_fn(code, "$Sys", "println")?.findex;
    Ok(Plan {
        game_t,
        bool_t,
        f64_t,
        str_t,
        void_t,
        try_fi: fun_index(code, tc.findex)?,
        update_fi: fun_index(code, update.findex)?,
        cb_fi,
        cb,
        cb_ret,
        app_time,
        println,
        connected,
        host,
        close: close.findex,
        ui,
        ctrl,
        opts,
        multi,
        sid: sid.0,
        save: save.0,
        local: local.0,
        reload,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let g_game = add_global(code, p.game_t);
    let g_at = add_global(code, p.f64_t);
    let log_hang = str_global(code, p.str_t, LOG_HANG);
    let log_rejoin = str_global(code, p.str_t, LOG_REJOIN);
    let wait = float_const(code, WAIT);
    let this = Reg(0);

    // C1
    let f = &mut code.functions[p.try_fi];
    let t = new_reg(f, p.f64_t);
    let no = new_reg(f, p.bool_t);
    let c1 = vec![
        Opcode::SetGlobal {
            global: g_game,
            src: this,
        },
        Opcode::Call0 {
            dst: t,
            fun: p.app_time,
        },
        Opcode::SetGlobal {
            global: g_at,
            src: t,
        },
        Opcode::Bool {
            dst: no,
            value: hlbc::types::ValBool(false),
        },
        Opcode::SetField {
            obj: this,
            field: p.connected,
            src: no,
        },
    ];
    insert_ops(f, 0, c1);

    // C2
    let f = &mut code.functions[p.update_fi];
    let g = new_reg(f, p.game_t);
    let b = new_reg(f, p.bool_t);
    let t = new_reg(f, p.f64_t);
    let at = new_reg(f, p.f64_t);
    let w = new_reg(f, p.f64_t);
    let h = new_reg(f, p.host.1);
    let msg = new_reg(f, p.str_t);
    let v = new_reg(f, p.void_t);
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: g,
        global: g_game,
    });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, "end");
    a.jmp(
        Opcode::JNotEq {
            a: g,
            b: this,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: this,
        field: p.connected,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "check");
    a.op(Opcode::Null { dst: g });
    a.op(Opcode::SetGlobal {
        global: g_game,
        src: g,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("check");
    a.op(Opcode::Call0 {
        dst: t,
        fun: p.app_time,
    });
    a.op(Opcode::GetGlobal {
        dst: at,
        global: g_at,
    });
    a.op(Opcode::Sub {
        dst: t,
        a: t,
        b: at,
    });
    a.op(Opcode::Float { dst: w, ptr: wait });
    a.jmp(
        Opcode::JSLt {
            a: t,
            b: w,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Null { dst: g });
    a.op(Opcode::SetGlobal {
        global: g_game,
        src: g,
    });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: log_hang,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.op(Opcode::Field {
        dst: h,
        obj: this,
        field: p.host.0,
    });
    a.jmp(Opcode::JNull { reg: h, offset: 0 }, "fail");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.close,
        arg0: h,
    });
    a.label("fail");
    a.op(Opcode::Bool {
        dst: b,
        value: hlbc::types::ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.cb,
        arg0: this,
        arg1: b,
    });
    a.label("end");
    insert_ops(f, 0, a.finish());

    // C3
    let f = &mut code.functions[p.cb_fi];
    let ui = new_reg(f, p.ui.1);
    let c = new_reg(f, p.ctrl.1);
    let o = new_reg(f, p.opts.1);
    let m = new_reg(f, p.multi.1);
    let sid = new_reg(f, p.str_t);
    let save = new_reg(f, p.str_t);
    let local = new_reg(f, p.str_t);
    let msg = new_reg(f, p.str_t);
    let v = new_reg(f, p.void_t);
    let mut a = Asm::new();
    for (dst, obj, field) in [
        (ui, this, p.ui.0),
        (c, this, p.ctrl.0),
        (o, this, p.opts.0),
        (m, o, p.multi.0),
        (sid, m, p.sid),
    ] {
        a.op(Opcode::Field { dst, obj, field });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "vanilla",
        );
    }
    a.op(Opcode::Field {
        dst: save,
        obj: o,
        field: p.save,
    });
    a.op(Opcode::Field {
        dst: local,
        obj: o,
        field: p.local,
    });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: log_rejoin,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.op(Opcode::Call4 {
        dst: v,
        fun: p.reload,
        arg0: c,
        arg1: save,
        arg2: local,
        arg3: sid,
    });
    a.op(Opcode::Ret { ret: v });
    a.label("vanilla");
    let at = p.cb_ret + 1;
    insert_ops(f, at, a.finish());
    // The reloading test jumped to `at`; insert_ops moved it past the new block.
    if let Opcode::JFalse { offset, .. } = &mut f.ops[p.cb_ret - 1] {
        *offset = 1;
    }
    eprintln!(
        "patched reconnect: tryConnectToServer fn@{}, Game.update fn@{}, connect callback fn@{}: a hung connect retries after {WAIT} s, a lost connection rejoins",
        code.functions[p.try_fi].findex.0, code.functions[p.update_fi].findex.0, p.cb.0
    );
}

/// Makes a co-op client retry a hung connect and rejoin after a lost
/// connection, or leaves `code` untouched and logs why.
pub(crate) fn patch_reconnect(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("reconnect skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (try_fi, update_fi, cb_fi, cb_ret) = (p.try_fi, p.update_fi, p.cb_fi, p.cb_ret);
        let mut code = read(&image);
        patch_reconnect(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                ![try_fi, update_fi, cb_fi].contains(&i),
                "function #{i}"
            );
        }
        for fi in [try_fi, update_fi] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let n = b.ops.len() - a.ops.len();
            shifted(a, b, 0, n);
            check_types(&back, b, 0..n);
            check_flow(b);
        }
        let (a, b) = (&orig.functions[cb_fi], &back.functions[cb_fi]);
        let n = b.ops.len() - a.ops.len();
        check_types(&back, b, cb_ret + 1..cb_ret + 1 + n);
        check_flow(b);
        // Not reloading: the test falls into the new block, not past it.
        assert!(matches!(
            b.ops[cb_ret - 1],
            Opcode::JFalse { offset: 1, .. }
        ));
        assert!(matches!(b.ops[cb_ret + 1 + n - 1], Opcode::Ret { .. }));
        assert_eq!(
            format!("{:?}", &a.ops[cb_ret + 1..]),
            format!("{:?}", &b.ops[cb_ret + 1 + n..]),
            "vanilla tail kept"
        );

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_reconnect(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
