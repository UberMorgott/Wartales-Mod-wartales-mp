// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A guest's reload connects to the server id it joined by (issue #9).
//
// Every co-op load in the running game (Game.loadGame, the pause menu's
// backup, the game-over reload, our barrier rejoin) sends the clients
// `ctrl.reload(saveFile, localSaveFile, host.options.multi.serverID)`; the
// client's `reload__impl` disposes its game and starts a new one connected to
// that id. On our direct relay the host's id is "W127.0.0.1:<port>:<pw>": the
// host reaches its relay on loopback. The guests got the same id at the start
// in a lobby message, which their master rewrites to the route they joined over
// (internal/master/relayaddr.go); the reload RPC rides the game connection and
// is never rewritten, so a guest on another PC dialed its own machine and
// stayed on the loading screen while the host waited for it.
//
// The client's own `game.options.multi.serverID` is the id it is connected to
// (the rewritten one, or the one its previous reload used; over Steam it is the
// host's Steam id, the same as the host's). The pass prepends to reload__impl:
//
//   var g = this.game;
//   if (g != null && !g.isAuth && g.options != null && g.options.multi != null
//       && g.options.multi.serverID != null && sid != null) {
//     sid = g.options.multi.serverID; Sys.println(LOG);
//   }
//
// The host (isAuth) is left alone: vanilla returns for it right after. A null
// sid (a reload without multi) is kept. Codex review: every client path
// (StartClients, drop-in, a previous reload, barrier rejoin) stores the id it
// connected with in options.multi.serverID, and the incoming RPC
// (Controller.networkRPC) calls reload__impl directly, so this covers both.
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::Asm;
use crate::job_xp::{const_str, str_global};

const LOG: &str = "mp: reload: connecting to the server id this game joined by, not the host's own";

struct Plan {
    fi: usize,
    ctrl_game: RefField,
    game_t: RefType,
    auth_f: RefField,
    bool_t: RefType,
    opts: (RefField, RefType),
    multi: (RefField, RefType),
    sid_f: RefField,
    str_t: RefType,
    void_t: RefType,
    println: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let ctrl_t = obj_type(code, "st.Controller")?;
    let game_t = obj_type(code, "Game")?;
    let str_t = obj_type(code, "String")?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let f = method(code, ctrl_t, "reload__impl")?;
    let fi = fun_index(code, f.findex)?;
    let (args, ret) = sig(code, f.findex)?;
    if args != [ctrl_t, str_t, str_t, str_t] || ret != void_t {
        bail!("reload__impl: unexpected signature");
    }
    let ctrl_game = typed(code, ctrl_t, "game", game_t)?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let auth_f = typed(code, game_t, "isAuth", bool_t)?;
    let opts = field(code, game_t, "options")?;
    let multi = field_of_virtual(code, opts.1, "multi")?;
    let sid = field_of_virtual(code, multi.1, "serverID")?;
    if sid.1 != str_t {
        bail!("Game.options.multi.serverID is not a String");
    }
    // Vanilla: GetThis game = this.game; NullCheck; Field isAuth; JFalse ...
    if !matches!(f.ops.first(), Some(Opcode::GetThis { field, .. }) if *field == ctrl_game) {
        bail!(
            "reload__impl fn@{}: does not start with this.game",
            f.findex.0
        );
    }
    if f.ops.iter().any(
        |o| matches!(o, Opcode::GetGlobal { global, .. } if const_str(code, *global) == Some(LOG)),
    ) {
        bail!("reload__impl fn@{}: already applied", f.findex.0);
    }
    let println = crate::diag::static_fn(code, "$Sys", "println")?.findex;
    Ok(Plan {
        fi,
        ctrl_game,
        game_t,
        auth_f,
        bool_t,
        opts,
        multi,
        sid_f: sid.0,
        str_t,
        void_t,
        println,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let log = str_global(code, p.str_t, LOG);
    let f = &mut code.functions[p.fi];
    let game = new_reg(f, p.game_t);
    let auth = new_reg(f, p.bool_t);
    let opts = new_reg(f, p.opts.1);
    let multi = new_reg(f, p.multi.1);
    let own = new_reg(f, p.str_t);
    let msg = new_reg(f, p.str_t);
    let v = new_reg(f, p.void_t);
    let sid = Reg(3);
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: game,
        field: p.ctrl_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Field {
        dst: auth,
        obj: game,
        field: p.auth_f,
    });
    a.jmp(
        Opcode::JTrue {
            cond: auth,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Field {
        dst: opts,
        obj: game,
        field: p.opts.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: opts,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Field {
        dst: multi,
        obj: opts,
        field: p.multi.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: multi,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Field {
        dst: own,
        obj: multi,
        field: p.sid_f,
    });
    a.jmp(
        Opcode::JNull {
            reg: own,
            offset: 0,
        },
        "body",
    );
    a.jmp(
        Opcode::JNull {
            reg: sid,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Mov { dst: sid, src: own });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: log,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("body");
    let ops = a.finish();
    insert_ops(f, 0, ops);
    eprintln!(
        "patched reload route fn@{}: a client's reload keeps the server id it joined by",
        f.findex.0
    );
}

/// Makes a client's `Controller.reload__impl` connect to its own server id, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_reload_route(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("reload route skipped: {e:#}")),
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
        let fi = plan(&orig).expect("plan").fi;
        let mut code = read(&image);
        patch_reload_route(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let n = b.ops.len() - a.ops.len();
        assert_eq!(n, 14);
        shifted(a, b, 0, n);
        check_types(&back, b, 0..n);
        check_flow(b);
        assert!(matches!(b.ops[n - 3], Opcode::Mov { dst: Reg(3), .. }));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_reload_route(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
