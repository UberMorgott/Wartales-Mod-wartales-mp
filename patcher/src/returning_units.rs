// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a player who was absent when a co-op save was loaded gets their units
// back when they drop in later in the same session.
//
// Vanilla: when a load starts without some save players, Game.gameplayStart
// calls `Game.removingPlayers(players)` (Game.hx:1029-1043) on the host: each
// absent BasePlayer's units are `swapOwner`ed to the host, its items moved to
// the host's inventory, and the BasePlayer is deleted. When that player then
// joins (drop_in.rs admits them), the hxbit Join handler (Game.startServer,
// closure at Game.hx:405-442) creates a fresh `ent.Player` with no units.
//
// The fix keeps a process-wide map uid -> units (a patch-added global
// haxe.ds.StringMap; nothing is saved, so the save format is untouched):
//   R1 removingPlayers, top of the per-player loop: `mpRemember(p)` stores
//      `p.allUnits.copy()` under `p.user` (before the units are moved).
//   R2 Join handler, right after `p.initContent()` (p.allUnits exists, the
//      player is already in state.players): `mpRestore(game, p)` takes the
//      entry of `p.user` out of the map and `swapOwner(p)`s every unit that
//      the host still owns and that is still in the troop (the same check as
//      the vanilla Transfer button); units the host gave away, dismissed or
//      dead units, and units of a previously loaded game (owned by another
//      game's player object) are left alone.
//   R3 During a battle R2 does nothing (swapOwner does not rebind the
//      battle's own unit objects); the entry stays and Battle.disposeBattle,
//      after `game.battle = null`, runs R2 for every player (mpRestoreAll).
// A map left from another GameState is dropped by R1 (no stale game graph).
// All of it runs on the host only. Log lines: `mp: drop-in: remembered units of
// <uid>` and `mp: drop-in: units returned to <uid>: <n>`.
// Limitation: after a host restart, or a reload of a save written while the
// player was absent, the map is empty; the units stay with the host, who can
// still hand them over with the vanilla Transfer button of the unit sheet.

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::diag::static_fn;
use crate::job_xp::str_global;

const TAG_KEEP: &str = "mp: drop-in: remembered units of ";
const TAG_BACK: &str = "mp: drop-in: units returned to ";
const SEP: &str = ": ";

struct Plan {
    game_t: RefType,
    bp_t: RefType,
    unit_t: RefType,
    arr_t: RefType,
    map_t: RefType,
    str_t: RefType,
    dyn_t: RefType,
    bool_t: RefType,
    nbool_t: RefType,
    i32_t: RefType,
    void_t: RefType,
    raw_t: RefType,
    player_t: RefType,
    gs_t: RefType,
    user_f: RefField,
    all_units_f: RefField,
    owner_f: RefField,
    state_f: RefField,
    player_f: RefField,
    arr_len_f: RefField,
    arr_raw_f: RefField,
    copy: RefFun,
    map_new: RefFun,
    map_get: RefFun,
    map_set: RefFun,
    map_remove: RefFun,
    in_troop: RefFun,
    swap_owner: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    println: RefFun,
    /// removingPlayers: function index and the insertion point (after `p` is read).
    rm_fi: usize,
    rm_at: usize,
    rm_p: Reg,
    /// Join closure: function index, the initContent call, the env read of the game.
    join_fi: usize,
    join_at: usize,
    join_p: Reg,
    game_read: Opcode,
    /// Game.isAuth / isDisposed / battle, GameState.players (proxy -> array).
    is_auth_f: RefField,
    disposed_f: RefField,
    battle_f: RefField,
    battle_t: RefType,
    players_f: RefField,
    proxy_t: RefType,
    proxy_arr_f: RefField,
    arrdyn_t: RefType,
    /// Battle.disposeBattle: function index, its final Ret, Battle.game.
    db_fi: usize,
    db_at: usize,
    db_game_f: RefField,
    dbg_file: usize,
}

fn want_sig(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("{what}: unexpected signature");
    }
    Ok(())
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let player_t = obj_type(code, "ent.Player")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let map_t = obj_type(code, "haxe.ds.StringMap")?;
    let str_t = obj_type(code, "String")?;
    let gs_t = obj_type(code, "st.GameState")?;
    let dyn_t = prim_type(code, "dyn", |t| matches!(t, Type::Dyn))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;

    let user_f = typed(code, bp_t, "user", str_t)?;
    let all_units_f = typed(code, bp_t, "allUnits", arr_t)?;
    let owner_f = typed(code, unit_t, "owner", bp_t)?;
    let state_f = typed(code, game_t, "state", gs_t)?;
    let player_f = typed(code, gs_t, "player", player_t)?;
    if !is_sub(code, player_t, bp_t) {
        bail!("ent.Player is not an ent.BasePlayer");
    }
    let (arr_len_f, t) = field(code, arr_t, "length")?;
    if t != i32_t {
        bail!("ArrayObj.length is not an i32");
    }
    let (arr_raw_f, raw_t) = field(code, arr_t, "array")?;

    let copy = method(code, arr_t, "copy")?.findex;
    want_sig(code, copy, "ArrayObj.copy", &[arr_t], arr_t)?;
    let map_get = proto(code, map_t, "get")?;
    want_sig(code, map_get, "StringMap.get", &[map_t, str_t], dyn_t)?;
    let map_set = proto(code, map_t, "set")?;
    want_sig(
        code,
        map_set,
        "StringMap.set",
        &[map_t, str_t, dyn_t],
        void_t,
    )?;
    let map_remove = proto(code, map_t, "remove")?;
    want_sig(
        code,
        map_remove,
        "StringMap.remove",
        &[map_t, str_t],
        bool_t,
    )?;
    // The constructor: what vanilla calls right after `new haxe.ds.StringMap`.
    let mut ctors: Vec<RefFun> = vec![];
    for f in &code.functions {
        for w in f.ops.windows(2) {
            if let [Opcode::New { dst }, Opcode::Call1 { fun, arg0, .. }] = w {
                if arg0 == dst && f.regs[dst.0 as usize] == map_t && !ctors.contains(fun) {
                    ctors.push(*fun);
                }
            }
        }
    }
    let [map_new] = ctors[..] else {
        bail!("{} StringMap constructors, want 1", ctors.len());
    };
    want_sig(code, map_new, "StringMap constructor", &[map_t], void_t)?;

    let in_troop = method(code, unit_t, "isInTroop")?.findex;
    let (a, nbool_t) = sig(code, in_troop)?;
    if a != [unit_t] || !matches!(code.types[nbool_t.0], Type::Null(b) if b == bool_t) {
        bail!("st.Unit.isInTroop is not (st.Unit) -> Null<Bool>");
    }
    let swap_owner = method(code, unit_t, "swapOwner")?.findex;
    want_sig(
        code,
        swap_owner,
        "st.Unit.swapOwner",
        &[unit_t, bp_t],
        void_t,
    )?;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want_sig(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want_sig(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;
    let println = static_fn(code, "$Sys", "println")?.findex;
    want_sig(code, println, "Sys.println", &[dyn_t], void_t)?;

    // R1: removingPlayers. The per-player loop reads `p` with
    // `UnsafeCast p = cast el; Incr i` and then calls `p.allUnits.copy()` and
    // swapOwner; the hook goes right after the Incr.
    let rm = method(code, game_t, "removingPlayers")?;
    if fun_args(code, rm) != [game_t, arr_t] {
        bail!("Game.removingPlayers: unexpected shape (already patched?)");
    }
    let rm_fi = fun_index(code, rm.findex)?;
    let reads: Vec<(usize, Reg)> = (0..rm.ops.len().saturating_sub(4))
        .filter_map(
            |i| match (&rm.ops[i], &rm.ops[i + 1], &rm.ops[i + 3], &rm.ops[i + 4]) {
                (
                    Opcode::UnsafeCast { dst, .. },
                    Opcode::Incr { .. },
                    Opcode::NullCheck { reg },
                    Opcode::Field { obj, field, .. },
                ) if rm.regs[dst.0 as usize] == bp_t
                    && matches!(rm.ops[i + 2], Opcode::Int { .. })
                    && reg == dst
                    && obj == dst
                    && *field == all_units_f =>
                {
                    Some((i + 2, *dst))
                }
                _ => None,
            },
        )
        .collect();
    let [(rm_at, rm_p)] = reads[..] else {
        bail!(
            "removingPlayers: {} player reads before allUnits, want 1",
            reads.len()
        );
    };
    if !calls(rm, swap_owner) {
        bail!("removingPlayers does not call swapOwner");
    }

    // R2: the Join closure: the only function that runs `new ent.Player` and
    // then `initContent(p)` on it, inside the startServer handler (Game.hx).
    let init = method(code, bp_t, "initContent")?.findex;
    let game_file = debug_file(code, "src/Game.hx")?;
    let mut sites = vec![];
    for (fi, f) in code.functions.iter().enumerate() {
        if !matches!(f.ops.first(), Some(Opcode::New { dst }) if f.regs[dst.0 as usize] == player_t)
        {
            continue;
        }
        if f.debug_info.as_ref().and_then(|d| d.first()).map(|d| d.0) != Some(game_file) {
            continue;
        }
        for (i, op) in f.ops.iter().enumerate() {
            if let Opcode::Call1 { fun, arg0, .. } = op {
                if *fun == init && *arg0 == Reg(1) && f.regs[1] == player_t {
                    sites.push((fi, i));
                }
            }
        }
    }
    let [(join_fi, init_at)] = sites[..] else {
        bail!("{} Join-closure initContent calls, want 1", sites.len());
    };
    let jf = &code.functions[join_fi];
    // The game comes from the closure environment: `EnumField g = env.0` (Game).
    let game_read = jf
        .ops
        .iter()
        .find(|o| matches!(o, Opcode::EnumField { dst, value, .. } if *value == Reg(0) && jf.regs[dst.0 as usize] == game_t))
        .cloned()
        .context("Join closure: no read of the game from its environment")?;

    let is_auth_f = typed(code, game_t, "isAuth", bool_t)?;
    let disposed_f = typed(code, game_t, "isDisposed", bool_t)?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let battle_f = typed(code, game_t, "battle", battle_t)?;
    let proxy_t = obj_type(code, "hxbit.ArrayProxyData")?;
    let players_f = typed(code, gs_t, "players", proxy_t)?;
    let arrdyn_t = obj_type(code, "hl.types.ArrayDyn")?;
    let proxy_arr_f = typed(code, proxy_t, "array", arrdyn_t)?;

    // R3: Battle.disposeBattle ends with `game.battle = null; return`: the
    // units of a player who dropped in during a battle are returned there.
    let db = method(code, battle_t, "disposeBattle")?;
    let db_game_f = typed(code, battle_t, "game", game_t)?;
    let n = db.ops.len();
    let tail_ok = n >= 2
        && matches!(db.ops[n - 1], Opcode::Ret { .. })
        && matches!(db.ops[n - 2], Opcode::SetField { field, obj, .. }
            if field == battle_f && db.regs[obj.0 as usize] == game_t);
    if !tail_ok {
        bail!("Battle.disposeBattle: does not end with `game.battle = null; return` (already patched?)");
    }
    let db_fi = fun_index(code, db.findex)?;
    let dbg_file = game_file;
    Ok(Plan {
        game_t,
        bp_t,
        unit_t,
        arr_t,
        map_t,
        str_t,
        dyn_t,
        bool_t,
        nbool_t,
        i32_t,
        void_t,
        raw_t,
        player_t,
        gs_t,
        user_f,
        all_units_f,
        owner_f,
        state_f,
        player_f,
        arr_len_f,
        arr_raw_f,
        copy,
        map_new,
        map_get,
        map_set,
        map_remove,
        in_troop,
        swap_owner,
        std_string,
        str_add,
        println,
        rm_fi,
        rm_at,
        rm_p,
        join_fi,
        join_at: init_at + 1,
        join_p: Reg(1),
        game_read,
        is_auth_f,
        disposed_f,
        battle_f,
        battle_t,
        players_f,
        proxy_t,
        proxy_arr_f,
        arrdyn_t,
        db_fi,
        db_at: n - 1,
        db_game_f,
        dbg_file,
    })
}

/// Patch-added globals: the uid -> units map and the GameState it belongs to.
struct Globals {
    map: RefGlobal,
    state: RefGlobal,
}

/// `mpRemember(game, p)`: G[p.user] = p.allUnits.copy(); a map left from
/// another game state is dropped first (it only holds that game's objects).
fn add_remember(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let tag = str_global(code, p.str_t, TAG_KEEP);
    let mut r = Regs(vec![p.game_t, p.bp_t]);
    let (uid, units, m, v, msg, st, gs) = (
        r.r(p.str_t),
        r.r(p.arr_t),
        r.r(p.map_t),
        r.r(p.void_t),
        r.r(p.str_t),
        r.r(p.gs_t),
        r.r(p.gs_t),
    );
    let (game, pl) = (Reg(0), Reg(1));
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: uid,
        obj: pl,
        field: p.user_f,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: units,
        obj: pl,
        field: p.all_units_f,
    });
    a.jmp(
        Opcode::JNull {
            reg: units,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call1 {
        dst: units,
        fun: p.copy,
        arg0: units,
    });
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.state_f,
    });
    a.op(Opcode::GetGlobal {
        dst: gs,
        global: g.state,
    });
    a.op(Opcode::GetGlobal {
        dst: m,
        global: g.map,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "fresh");
    a.jmp(
        Opcode::JEq {
            a: gs,
            b: st,
            offset: 0,
        },
        "have",
    );
    a.label("fresh");
    a.op(Opcode::New { dst: m });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.map_new,
        arg0: m,
    });
    a.op(Opcode::SetGlobal {
        global: g.map,
        src: m,
    });
    a.op(Opcode::SetGlobal {
        global: g.state,
        src: st,
    });
    a.label("have");
    // An ArrayObj goes to set's Dyn argument as is (self-describing).
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.map_set,
        arg0: m,
        arg1: uid,
        arg2: units,
    });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: tag,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: uid,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.bp_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `mpRestore(game, p)`: hands the units remembered for `p.user` back to `p`.
/// During a battle nothing happens (the battle's own unit objects would keep
/// the old owner); the entry stays and `mpRestoreAll` runs it when the battle
/// is disposed.
fn add_restore(code: &mut Bytecode, p: &Plan, g: &Globals) -> Result<RefFun> {
    let tag = str_global(code, p.str_t, TAG_BACK);
    let sep = str_global(code, p.str_t, SEP);
    let zero = int_const(code, 0);
    let mut r = Regs(vec![p.game_t, p.bp_t]);
    let (m, uid, d, arr, st, host, bt) = (
        r.r(p.map_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.arr_t),
        r.r(p.gs_t),
        r.r(p.player_t),
        r.r(p.battle_t),
    );
    let (i, n, cnt, raw, el, u, own) = (
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.raw_t),
        r.r(p.dyn_t),
        r.r(p.unit_t),
        r.r(p.bp_t),
    );
    let (nb, b, v, msg, t, dc) = (
        r.r(p.nbool_t),
        r.r(p.bool_t),
        r.r(p.void_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
    );
    let (game, pl) = (Reg(0), Reg(1));
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.disposed_f,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: bt,
        obj: game,
        field: p.battle_f,
    });
    a.jmp(Opcode::JNotNull { reg: bt, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: m,
        global: g.map,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: uid,
        obj: pl,
        field: p.user_f,
    });
    a.jmp(
        Opcode::JNull {
            reg: uid,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.map_get,
        arg0: m,
        arg1: uid,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "end");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.map_remove,
        arg0: m,
        arg1: uid,
    });
    a.op(Opcode::SafeCast { dst: arr, src: d });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.state_f,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: host,
        obj: st,
        field: p.player_f,
    });
    a.jmp(
        Opcode::JNull {
            reg: host,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: i, ptr: zero });
    a.op(Opcode::Int {
        dst: cnt,
        ptr: zero,
    });
    a.loop_head("loop");
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.arr_len_f,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.arr_raw_f,
    });
    a.op(Opcode::GetArray {
        dst: el,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: u, src: el });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: own,
        obj: u,
        field: p.owner_f,
    });
    a.jmp(
        Opcode::JNotEq {
            a: own,
            b: host,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Call1 {
        dst: nb,
        fun: p.in_troop,
        arg0: u,
    });
    a.jmp(Opcode::JNull { reg: nb, offset: 0 }, "loop");
    a.op(Opcode::SafeCast { dst: b, src: nb });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "loop");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.swap_owner,
        arg0: u,
        arg1: pl,
    });
    // count only what actually moved
    a.op(Opcode::Field {
        dst: own,
        obj: u,
        field: p.owner_f,
    });
    a.jmp(
        Opcode::JNotEq {
            a: own,
            b: pl,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Incr { dst: cnt });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("done");
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: tag,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: uid,
    });
    a.op(Opcode::GetGlobal {
        dst: t,
        global: sep,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: t,
    });
    a.op(Opcode::ToDyn { dst: dc, src: cnt });
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.std_string,
        arg0: dc,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: t,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.bp_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `mpRestoreAll(game)`: on the host, `mpRestore(game, p)` for every player
/// (only those with a pending entry change anything).
fn add_restore_all(code: &mut Bytecode, p: &Plan, g: &Globals, restore: RefFun) -> Result<RefFun> {
    let zero = int_const(code, 0);
    let mut r = Regs(vec![p.game_t]);
    let (b, m, st, px, ad, arr, i, n, raw, el, pl, v) = (
        r.r(p.bool_t),
        r.r(p.map_t),
        r.r(p.gs_t),
        r.r(p.proxy_t),
        r.r(p.arrdyn_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.raw_t),
        r.r(p.dyn_t),
        r.r(p.bp_t),
        r.r(p.void_t),
    );
    let game = Reg(0);
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: m,
        global: g.map,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "end");
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
        field: p.is_auth_f,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.state_f,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: px,
        obj: st,
        field: p.players_f,
    });
    a.jmp(Opcode::JNull { reg: px, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: ad,
        obj: px,
        field: p.proxy_arr_f,
    });
    a.op(Opcode::SafeCast { dst: arr, src: ad });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: i, ptr: zero });
    a.loop_head("loop");
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.arr_len_f,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.arr_raw_f,
    });
    a.op(Opcode::GetArray {
        dst: el,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: pl, src: el });
    a.op(Opcode::Call2 {
        dst: v,
        fun: restore,
        arg0: game,
        arg1: pl,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.game_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let g = Globals {
        map: add_global(code, p.map_t),
        state: add_global(code, p.gs_t),
    };
    let remember = add_remember(code, p, &g)?;
    let restore = add_restore(code, p, &g)?;
    let restore_all = add_restore_all(code, p, &g, restore)?;

    let f = &mut code.functions[p.rm_fi];
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.rm_at,
        vec![Opcode::Call2 {
            dst: v,
            fun: remember,
            arg0: Reg(0),
            arg1: p.rm_p,
        }],
    );
    eprintln!(
        "patched returning units fn@{} (removingPlayers) op {}: remember each absent player's units",
        f.findex.0, p.rm_at
    );

    let f = &mut code.functions[p.join_fi];
    let gr = new_reg(f, p.game_t);
    let v = new_reg(f, p.void_t);
    let Opcode::EnumField {
        value,
        construct,
        field,
        ..
    } = p.game_read.clone()
    else {
        unreachable!()
    };
    insert_ops(
        f,
        p.join_at,
        vec![
            Opcode::EnumField {
                dst: gr,
                value,
                construct,
                field,
            },
            Opcode::Call2 {
                dst: v,
                fun: restore,
                arg0: gr,
                arg1: p.join_p,
            },
        ],
    );
    eprintln!(
        "patched returning units fn@{} (Join, after initContent) op {}: give a returning player their units back",
        f.findex.0, p.join_at
    );

    let f = &mut code.functions[p.db_fi];
    let gr = new_reg(f, p.game_t);
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.db_at,
        vec![
            Opcode::GetThis {
                dst: gr,
                field: p.db_game_f,
            },
            Opcode::Call1 {
                dst: v,
                fun: restore_all,
                arg0: gr,
            },
        ],
    );
    eprintln!(
        "patched returning units fn@{} (disposeBattle) op {}: units of a player who dropped in during the battle",
        f.findex.0, p.db_at
    );
    Ok(())
}

/// Gives a player who was absent at load their units back when they drop in,
/// or leaves `code` untouched and logs why.
pub(crate) fn patch_returning_units(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("returning units skipped: {e:#}"));
            return;
        }
    };
    let snap = crate::asm::Snap::take(code);
    let rm = code.functions[p.rm_fi].clone();
    let join = code.functions[p.join_fi].clone();
    if let Err(e) = apply(code, &p) {
        snap.restore(code);
        code.functions[p.rm_fi] = rm;
        code.functions[p.join_fi] = join;
        crate::skipped(format!("returning units skipped: {e:#}"));
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
        let mut code = read(&image);
        patch_returning_units(&mut code);
        let back = read(&write(&code));

        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 3);
        let changed: Vec<usize> = orig
            .functions
            .iter()
            .zip(&back.functions)
            .enumerate()
            .filter(|(_, (a, b))| !same(a, b))
            .map(|(i, _)| i)
            .collect();
        let mut want = vec![p.rm_fi, p.join_fi, p.db_fi];
        want.sort();
        assert_eq!(changed, want);

        let (a, b) = (&orig.functions[p.rm_fi], &back.functions[p.rm_fi]);
        shifted(a, b, p.rm_at, 1);
        check_flow(b);
        check_types(&back, b, p.rm_at..p.rm_at + 1);
        let (a, b) = (&orig.functions[p.join_fi], &back.functions[p.join_fi]);
        shifted(a, b, p.join_at, 2);
        check_flow(b);
        check_types(&back, b, p.join_at..p.join_at + 2);

        for f in &back.functions[n..] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }

        let patched = write(&code);
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_returning_units(&mut again);
        assert!(write(&again) == patched);
    }
}
