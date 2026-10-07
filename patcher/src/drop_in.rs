// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op drop-in: a player who was not in the session can join a running game.
//
// Vanilla path (Game.hx): the host's in-game "reconnect lobby" is built by
// `Game.createLobby` (data isReconnect, maxPlayers 4), which only
// `ui.win.Pause.init` calls (`if (game.isAuth && game.get_isMulti())
// game.createLobby(() -> {})`, Pause.hx:132-133). A joiner's
// `LobbyMessage.Join` reaches `GameLobby.getServerID`, the closure set in
// createLobby's create callback (Game.hx:3058-3061):
//
//     if (game.isDisposed) return null;
//     var p = first state.players entry with p.user == user.id;
//     if (p == null) return null;          // <- every new player is denied
//     return game.options.multi.serverID;
//
// Past that gate everything already supports a new player (the hxbit Join
// handler creates an ent.Player for an unknown user, Game.startServer).
//
// Two edits:
//   E1 gameplayStart: on the host of a co-op game the reconnect lobby is
//      created right away (same call as Pause, after the same guards plus
//      `!get_isLocalP2P()`, whose branch reads Lobby.inst), so the join code
//      and the Steam invite exist without opening the Pause menu first.
//      createLobby itself returns early when the lobby exists / is pending.
//   E2 getServerID: an unknown user is admitted while
//      `state.players.length < maxPlayers` (the reconnect lobby's own cap,
//      read from createLobby); a full party keeps the vanilla refusal. The
//      admission is logged: `mp: drop-in: new player admitted <uid>`.

use super::*;
use crate::diag::static_fn;
use crate::job_xp::str_global;

const LOG_TAG: &str = "mp: drop-in: new player admitted ";

struct Plan {
    /// gameplayStart (function index).
    start_fi: usize,
    is_auth: RefField,
    is_multi: RefFun,
    is_local_p2p: RefFun,
    create_lobby: RefFun,
    noop: RefFun,
    noop_t: RefType,
    bool_t: RefType,
    void_t: RefType,
    /// getServerID closure (function index) and its deny tail `k`
    /// (`JNotNull p; Null r; Ret r`).
    gate_fi: usize,
    k: usize,
    /// The players array register and its `length` field, from the loop head.
    arr: Reg,
    len_f: RefField,
    user_id: RefField,
    max_players: i32,
    i32_t: RefType,
    str_t: RefType,
    str_add: RefFun,
    println: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let str_t = obj_type(code, "String")?;
    let (is_auth, t) = field(code, game_t, "isAuth")?;
    if t != bool_t {
        bail!("Game.isAuth is not a bool");
    }
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], bool_t) {
        bail!("Game.get_isMulti is not (Game) -> bool");
    }
    let lp: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| s(code, f.name) == "get_isLocalP2P")
        .map(|f| f.findex)
        .collect();
    let [is_local_p2p] = lp[..] else {
        bail!("{} get_isLocalP2P functions, want 1", lp.len());
    };
    if sig(code, is_local_p2p)? != (vec![], bool_t) {
        bail!("get_isLocalP2P is not () -> bool");
    }
    let cl = method(code, game_t, "createLobby")?;
    let create_lobby = cl.findex;
    let (cl_args, cl_ret) = sig(code, create_lobby)?;
    if cl_args.len() != 2 || cl_args[0] != game_t || cl_ret != void_t {
        bail!("Game.createLobby is not (Game, () -> void) -> void");
    }
    let noop_t = cl_args[1];
    let calls_is_local = cl
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call0 { fun, .. } if *fun == is_local_p2p));
    if !calls_is_local {
        bail!("createLobby does not branch on get_isLocalP2P");
    }
    // maxPlayers of the reconnect lobby: `Int r = N; ToDyn d = r; DynSet o["maxPlayers"] = d`.
    let mp_s = string_index(code, "maxPlayers")?;
    let mut max = None;
    for w in cl.ops.windows(3) {
        if let [Opcode::Int { dst, ptr }, Opcode::ToDyn { dst: d, src }, Opcode::DynSet { field, src: v, .. }] =
            w
        {
            if src == dst && v == d && *field == mp_s {
                max = Some(code.ints[ptr.0]);
            }
        }
    }
    let max_players = max.context("createLobby: maxPlayers literal not found")?;
    if !(2..=8).contains(&max_players) {
        bail!("createLobby: maxPlayers {max_players} out of range");
    }

    // The no-op callback Pause passes: `StaticClosure c = noop; Call2 createLobby(game, c)`.
    let pause_t = obj_type(code, "ui.win.Pause")?;
    let pause = method(code, pause_t, "init")?;
    let sites: Vec<RefFun> = pause
        .ops
        .windows(2)
        .filter_map(|w| match w {
            [Opcode::StaticClosure { dst, fun }, Opcode::Call2 { fun: f, arg1, .. }]
                if *f == create_lobby && arg1 == dst =>
            {
                Some(*fun)
            }
            _ => None,
        })
        .collect();
    let [noop] = sites[..] else {
        bail!(
            "Pause.init: {} createLobby(noop) calls, want 1",
            sites.len()
        );
    };
    let nf = &code.functions[fun_index(code, noop)?];
    if nf.ops.len() != 1 || !matches!(nf.ops[0], Opcode::Ret { .. }) || nf.t != noop_t {
        bail!("Pause.init: the createLobby callback is not a no-op () -> void");
    }

    let start = method(code, game_t, "gameplayStart")?;
    if fun_args(code, start) != [game_t] || calls(start, create_lobby) {
        bail!("Game.gameplayStart: unexpected shape (already patched?)");
    }
    let start_fi = fun_index(code, start.findex)?;

    // getServerID: the closure stored into GameLobby.getServerID by the
    // create callback of createLobby.
    let user_t = obj_type(code, "mpman.User")?;
    let (user_id, t) = field(code, user_t, "id")?;
    if t != str_t {
        bail!("mpman.User.id is not a String");
    }
    let gl_t = obj_type(code, "GameLobby")?;
    let (gsid, _) = field(code, gl_t, "getServerID")?;
    let mut gates = vec![];
    for op in &cl.ops {
        let Opcode::InstanceClosure { fun, .. } = op else {
            continue;
        };
        let cb = &code.functions[fun_index(code, *fun)?];
        for w in cb.ops.windows(2) {
            if let [Opcode::InstanceClosure { dst, fun: g, .. }, Opcode::SetField { obj, field, src }] =
                w
            {
                if src == dst && *field == gsid && cb.regs[obj.0 as usize] == gl_t {
                    gates.push(*g);
                }
            }
        }
    }
    let [gate] = gates[..] else {
        bail!(
            "{} getServerID closures in createLobby's callback, want 1",
            gates.len()
        );
    };
    let gate_fi = fun_index(code, gate)?;
    let g = &code.functions[gate_fi];
    let (gargs, gret) = sig(code, gate)?;
    if gargs.len() != 2 || gargs[1] != user_t || gret != str_t {
        bail!("getServerID closure is not (env, mpman.User) -> String");
    }
    let rt = |r: Reg| g.regs[r.0 as usize];
    let tails: Vec<usize> = (0..g.ops.len().saturating_sub(2))
        .filter(|&k| {
            matches!(
                (&g.ops[k], &g.ops[k + 1], &g.ops[k + 2]),
                (Opcode::JNotNull { offset: 2, .. }, Opcode::Null { dst: a }, Opcode::Ret { ret: b })
                    if a == b
            )
        })
        .collect();
    let [k] = tails[..] else {
        bail!(
            "getServerID: {} deny tails, want 1 (already patched?)",
            tails.len()
        );
    };
    // The loop exit jumps to k: `Field len = arr.length; JSGte i >= len -> k`.
    let heads: Vec<(Reg, RefField)> = (1..k)
        .filter(|&i| jump_targets(g, i) == [k] && matches!(g.ops[i], Opcode::JSGte { .. }))
        .filter_map(|i| match (&g.ops[i - 1], &g.ops[i]) {
            (Opcode::Field { dst, obj, field }, Opcode::JSGte { b, .. }) if dst == b => {
                Some((*obj, *field))
            }
            _ => None,
        })
        .collect();
    let [(arr, len_f)] = heads[..] else {
        bail!(
            "getServerID: {} loop heads exiting to the deny tail, want 1",
            heads.len()
        );
    };
    if field_name(code, rt(arr), len_f) != Some("length") {
        bail!("getServerID: the loop bound is not array.length");
    }
    // The admitted value is options.multi.serverID.
    let reads_sid = g.ops[k + 3..].iter().any(|o| {
        matches!(o, Opcode::Field { obj, field, .. } if field_name(code, rt(*obj), *field) == Some("serverID"))
    });
    if !reads_sid {
        bail!("getServerID: no serverID read after the deny tail");
    }

    let str_add = static_fn(code, "$String", "__add__")?.findex;
    let println = static_fn(code, "$Sys", "println")?.findex;
    let dyn_t = prim_type(code, "dyn", |t| matches!(t, Type::Dyn))?;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t)
        || sig(code, println)? != (vec![dyn_t], void_t)
    {
        bail!("String.__add__ / Sys.println: unexpected signatures");
    }
    Ok(Plan {
        start_fi,
        is_auth,
        is_multi,
        is_local_p2p,
        create_lobby,
        noop,
        noop_t,
        bool_t,
        void_t,
        gate_fi,
        k,
        arr,
        len_f,
        user_id,
        max_players,
        i32_t,
        str_t,
        str_add,
        println,
    })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    // E1: if (isAuth && get_isMulti() && !get_isLocalP2P()) createLobby(noop);
    let max = int_const(code, p.max_players);
    let tag = str_global(code, p.str_t, LOG_TAG);
    let f = &mut code.functions[p.start_fi];
    let b = new_reg(f, p.bool_t);
    let c = new_reg(f, p.noop_t);
    let v = new_reg(f, p.void_t);
    let this = Reg(0);
    insert_ops(
        f,
        0,
        vec![
            Opcode::GetThis {
                dst: b,
                field: p.is_auth,
            },
            Opcode::JFalse { cond: b, offset: 6 },
            Opcode::Call1 {
                dst: b,
                fun: p.is_multi,
                arg0: this,
            },
            Opcode::JFalse { cond: b, offset: 4 },
            Opcode::Call0 {
                dst: b,
                fun: p.is_local_p2p,
            },
            Opcode::JTrue { cond: b, offset: 2 },
            Opcode::StaticClosure {
                dst: c,
                fun: p.noop,
            },
            Opcode::Call2 {
                dst: v,
                fun: p.create_lobby,
                arg0: this,
                arg1: c,
            },
        ],
    );
    eprintln!(
        "patched drop-in fn@{} (gameplayStart): co-op host creates the reconnect lobby at start",
        f.findex.0
    );

    // E2: deny tail `JNotNull p -> ok; Null r; Ret r` gains, before `Null r`:
    //   if (arr.length < max) { println(tag + user.id); goto ok; }
    let f = &mut code.functions[p.gate_fi];
    let len = new_reg(f, p.i32_t);
    let cap = new_reg(f, p.i32_t);
    let msg = new_reg(f, p.str_t);
    let uid = new_reg(f, p.str_t);
    let v = new_reg(f, p.void_t);
    let user = Reg(1);
    // After insertion: k JNotNull -> k+13; k+1..k+3 load; k+4 JSGte -> k+11
    // (vanilla `Null r; Ret r`); k+5..k+9 log; k+10 JAlways -> k+13 (serverID).
    let ops = vec![
        Opcode::NullCheck { reg: p.arr },
        Opcode::Field {
            dst: len,
            obj: p.arr,
            field: p.len_f,
        },
        Opcode::Int { dst: cap, ptr: max },
        Opcode::JSGte {
            a: len,
            b: cap,
            offset: 6,
        },
        Opcode::GetGlobal {
            dst: msg,
            global: tag,
        },
        Opcode::NullCheck { reg: user },
        Opcode::Field {
            dst: uid,
            obj: user,
            field: p.user_id,
        },
        Opcode::Call2 {
            dst: msg,
            fun: p.str_add,
            arg0: msg,
            arg1: uid,
        },
        Opcode::Call1 {
            dst: v,
            fun: p.println,
            arg0: msg,
        },
        // jumps over the original `Null r; Ret r` to the serverID read
        Opcode::JAlways { offset: 2 },
    ];
    let n = ops.len();
    insert_ops(f, p.k + 1, ops);
    debug_assert_eq!(jump_targets(f, p.k + n), vec![p.k + n + 3]);
    eprintln!(
        "patched drop-in fn@{} (getServerID): a new player is admitted while players < {}",
        f.findex.0, p.max_players
    );
}

/// Lets a new player drop into a running co-op game, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_drop_in(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => eprintln!("drop-in skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(p.max_players, 4);
        let mut code = read(&image);
        patch_drop_in(&mut code);
        let back = read(&write(&code));

        let changed: Vec<usize> = orig
            .functions
            .iter()
            .zip(&back.functions)
            .enumerate()
            .filter(|(_, (a, b))| !same(a, b))
            .map(|(i, _)| i)
            .collect();
        let mut want = vec![p.start_fi, p.gate_fi];
        want.sort();
        assert_eq!(changed, want);

        let (a, b) = (&orig.functions[p.start_fi], &back.functions[p.start_fi]);
        shifted(a, b, 0, 8);
        check_flow(b);
        check_types(&back, b, 0..8);
        assert_eq!(jump_targets(b, 1), vec![8]);
        assert_eq!(jump_targets(b, 3), vec![8]);
        assert_eq!(jump_targets(b, 5), vec![8]);

        let (a, b) = (&orig.functions[p.gate_fi], &back.functions[p.gate_fi]);
        let k = p.k;
        shifted(a, b, k + 1, 10);
        check_flow(b);
        check_types(&back, b, k..k + 14);
        // full party: falls to the vanilla `Null; Ret`
        assert_eq!(jump_targets(b, k + 4), vec![k + 11]);
        assert!(matches!(b.ops[k + 11], Opcode::Null { .. }));
        assert!(matches!(b.ops[k + 12], Opcode::Ret { .. }));
        // admitted: same target as a found player
        assert_eq!(jump_targets(b, k + 10), jump_targets(b, k));

        // A second pass refuses (already patched) and changes nothing.
        let patched = write(&code);
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_drop_in(&mut again);
        assert!(write(&again) == patched);
    }

    /// A changed deny tail refuses the whole pass.
    #[test]
    fn refuses_unknown_shape() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        code.functions[p.gate_fi].ops[p.k + 1] = Opcode::Nop;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_drop_in(&mut code);
        assert!(write(&code) == before);
    }
}
