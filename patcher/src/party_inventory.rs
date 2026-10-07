// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op party inventory: item costs paid from every player's inventory.
//
// Vanilla already has a "with chest" cost path: PlayerInventory.useList(inv,
// list, checkChest = true, cb) pays each `{item, count, quality}` entry from the
// player's own inventory, then the camp chest, then the boat chest (stolen
// stacks first), on the host (a client RPCs BasePlayer.useList when its own
// inventory is short). Crafting, brewing, repairs, healing, alter, boat work and
// Confession dialogs use it; their displays count with hasItemWithChest /
// countWithChest. Classic dialogs pay from the acting player's own inventory only.
//
// Consume (all checkChest callers): useList gets the prologue
//
//   if (checkChest) list = partyPrepare(this, list);
//
// partyPrepare (host only, else returns `list`) checks the WHOLE list first:
// global entries (`isGlobal`) must be held (PlayerInventory.hasItem, as vanilla
// will check them), and every other entry must be covered by own + chest + boat
// (vanilla's own sources) + the other players' inventories. Any shortfall (or a
// party-needing item listed twice) returns `list` untouched, so vanilla fails as
// before and nothing is consumed. Otherwise the part vanilla cannot cover is
// taken from the other players in `state.players` order (Inventory.use with
// stolen stacks first), and a new list asks vanilla for exactly own + chest +
// boat of those entries (entries are never mutated: callers may pass cdb data).
// Vanilla then pays and succeeds. Order: own -> chest -> boat -> other players.
//
// Dialogs (Dialog.hx): Classic dialogs are treated like Confession ones (vanilla
// type 11143): the choice is allowed when the acting player has the item or the
// party has it (GlobalInventory flags global|me|others|chest, never equipped),
// the cost labels count chest + other players (Fmt.formatItemCost flags
// 256 -> 260), the "same type as" substitution looks at the party too
// (getFormattedItem), and the cost is paid with checkChest = true. Tavern event
// dialogs keep their own tavern stock path.
//
// Displays: hasItemWithChest / countWithChest also count the other players
// (patch_party_counts); the injury heal panel's remedy list and the injury
// tooltip's "remedy available" test (inlined GlobalInventory iterations, flags
// player|chest) and with-chest cost buttons (Button.syncText, Debrief) do too
// (patch_party_lists). The heal itself pays with useList(checkChest = true).
// Recipe ingredient rows (ItemsRecipe: Grimoire cells, item tooltips) test with
// hasItemWithChest instead of the own inventory (patch_party_recipes); crafting
// already pays with useList(checkChest = true). Grimoire learn costs stay own-only:
// the host pays them per entry with tryUse on the acting player's inventory.
//
// Activities (patch_party_activities): fishing hooks and lockpicks were counted,
// required and consumed from the acting player's own inventory only. Their counters
// and gates now use countWithChest / hasItemWithChest, and the consuming call goes to
// partyTryUse / partyUse (same signatures as PlayerInventory.tryUse / use): the
// vanilla call when the own inventory holds enough, else a one-entry
// useList(inv, [{item, count}], checkChest = true, cb) — host: partyPrepare + vanilla
// (own -> chest -> boat -> other players); client: vanilla RPCs it to the host.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::*;
use super::*;
use hlbc::types::{RefGlobal, RefInt, ValBool};

/// GlobalInventory item-count flags (GlobalInventory.getItemCount).
const F_GLOBAL: i32 = 1;
const F_PLAYER: i32 = 2;
const F_OTHERS: i32 = 4;
const F_CHEST: i32 = 256;

struct T {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    dyn_: RefType,
    str_: RefType,
}

struct Consume {
    use_list_fi: usize,
    inv_t: RefType,
    list_t: RefType,
    entry_t: RefType,
    entry_count: RefField,
    entry_item: RefField,
    entry_quality: RefField,
    null_i32: RefType,
    ref_i32: RefType,
    ref_bool: RefType,
    null_bool: RefType,
    type_t: RefType,
    arr_t: RefType,
    alloc: RefFun,
    wrap: RefFun,
    push: RefFun,
    arr_len: RefField,
    arr_raw: (RefField, RefType),
    game_cls: (RefGlobal, RefType),
    game_inst: (RefField, RefType),
    game_auth: RefField,
    game_state: (RefField, RefType),
    get_camp: (RefFun, RefType),
    get_tool: (RefFun, RefType),
    chest_s: RefGlobal,
    light_s: RefGlobal,
    tool_inv: RefField,
    boat_inv: RefField,
    players: (RefField, RefType),
    proxy_array: (RefField, RefType),
    player_t: RefType,
    player_inv: RefField,
    count: RefFun,
    use_: RefFun,
    is_global: RefFun,
    has_item: RefFun,
    dbg_file: usize,
}

fn fun_t(code: &Bytecode, f: RefFun) -> Result<TypeFun> {
    let t = code
        .functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| g.t)
        .or_else(|| code.natives.iter().find(|n| n.findex == f).map(|n| n.t))
        .with_context(|| format!("function @{} not found", f.0))?;
    t.as_fun(code)
        .cloned()
        .with_context(|| format!("function @{} is not a function", f.0))
}

fn string_global(code: &Bytecode, str_t: RefType, value: &str) -> Result<RefGlobal> {
    code.constants
        .iter()
        .flatten()
        .find(|c| {
            code.globals.get(c.global.0) == Some(&str_t)
                && matches!(c.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value))
        })
        .map(|c| c.global)
        .with_context(|| format!("string global {value:?} not found"))
}

fn types(code: &Bytecode) -> Result<T> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    Ok(T {
        void: prim("void", |t| matches!(t, Type::Void))?,
        bool_: prim("Bool", |t| matches!(t, Type::Bool))?,
        i32_: prim("I32", |t| matches!(t, Type::I32))?,
        dyn_: prim("Dyn", |t| matches!(t, Type::Dyn))?,
        str_: obj_type(code, "String")?,
    })
}

/// The static `PlayerInventory.useList(inv, list, checkChest, cb)`.
fn find_use_list<'a>(code: &'a Bytecode, t: &T, inv_t: RefType) -> Result<&'a Function> {
    let arr_obj = obj_type(code, "hl.types.ArrayObj")?;
    let hits: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "useList"
                && f.t.as_fun(code).is_some_and(|ft| {
                    ft.args.len() == 4
                        && ft.args[0] == inv_t
                        && ft.args[1] == arr_obj
                        && ft.args[2] == t.bool_
                        && ft.ret == t.void
                })
        })
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one PlayerInventory.useList, found {}", hits.len()),
    }
}

fn plan_consume(code: &Bytecode, t: &T) -> Result<Consume> {
    let m = consume_refs(code, t)?;
    if matches!(
        code.functions[m.use_list_fi].ops.first(),
        Some(Opcode::JFalse { cond: Reg(2), .. })
    ) {
        bail!("already applied");
    }
    Ok(m)
}

/// useList and the types / functions its host path uses (patched or not).
fn consume_refs(code: &Bytecode, t: &T) -> Result<Consume> {
    let inv_t = obj_type(code, "st.Inventory")?;
    let ul = find_use_list(code, t, inv_t)?;
    let use_list_fi = fun_index(code, ul.findex)?;
    let list_t = ul.regs[1];
    // The host path: count(this, item, quality, null) per entry, chest via getTool("Chest").
    let mut entry_t = None;
    let mut get_tool = None;
    let mut get_camp = None;
    let mut boat_inv = None;
    let mut tool_inv = None;
    let mut is_global = None;
    let mut has_item = None;
    let mut push = None;
    let mut alloc = None;
    let mut wrap = None;
    let mut light_s = None;
    let mut count = None;
    for (i, op) in ul.ops.iter().enumerate() {
        match op {
            Opcode::ToVirtual { dst, .. } if entry_t.is_none() => {
                entry_t = Some(ul.regs[dst.0 as usize])
            }
            Opcode::Call1 { fun, .. } if fname(code, *fun) == "get_camp" => {
                get_camp = Some((*fun, fun_t(code, *fun)?.ret))
            }
            Opcode::Call3 { fun, .. } if fname(code, *fun) == "getTool" => {
                get_tool = Some((*fun, fun_t(code, *fun)?.ret))
            }
            Opcode::Field { obj, field, .. }
                if field_name(code, ul.regs[obj.0 as usize], *field) == Some("boatInventory") =>
            {
                boat_inv = Some(*field)
            }
            Opcode::Field {
                obj: o_r,
                field,
                dst,
            } if field_name(code, ul.regs[o_r.0 as usize], *field) == Some("inventory")
                && ul.regs[dst.0 as usize] == inv_t
                && super::obj(code, ul.regs[o_r.0 as usize])
                    .map(|o| s(code, o.name))
                    .ok()
                    == Some("st.item.Tool") =>
            {
                tool_inv = Some(*field)
            }
            Opcode::Call2 { fun, .. } if fname(code, *fun) == "isGlobal" => is_global = Some(*fun),
            Opcode::Call4 { fun, .. } if fname(code, *fun) == "hasItem" && has_item.is_none() => {
                has_item = Some(*fun)
            }
            Opcode::Call4 { fun, .. } if fname(code, *fun) == "count" && count.is_none() => {
                count = Some(*fun)
            }
            Opcode::Call2 { fun, .. } if fname(code, *fun) == "push" && push.is_none() => {
                push = Some(*fun)
            }
            Opcode::Call2 { fun, arg0, .. }
                if alloc.is_none()
                    && matches!(code.types[ul.regs[arg0.0 as usize].0], Type::Type) =>
            {
                alloc = Some(*fun);
                // UnsafeCast; Call1 wrap(array) -> ArrayObj
                if let Some(Opcode::Call1 { fun: w, .. }) = ul.ops.get(i + 2) {
                    wrap = Some(*w);
                }
            }
            Opcode::JNotEq { b, .. } if light_s.is_none() => {
                if let Some(Opcode::GetGlobal { global, dst }) = ul.ops.get(i.wrapping_sub(1)) {
                    if dst == b && job_xp::const_str(code, *global) == Some("Light") {
                        light_s = Some(*global);
                    }
                }
            }
            _ => {}
        }
    }
    let entry_t = entry_t.context("useList: no entry virtual")?;
    let ef = |n: &str| field_of_virtual(code, entry_t, n);
    let (entry_count, ct) = ef("count")?;
    let (entry_item, it) = ef("item")?;
    let (entry_quality, null_i32) = ef("quality")?;
    if ct != t.i32_
        || it != t.str_
        || !matches!(code.types[null_i32.0], Type::Null(x) if x == t.i32_)
    {
        bail!("useList entry is not {{count: Int, item: String, quality: Null<Int>}}");
    }
    let (get_tool, get_camp) = (
        get_tool.context("useList: no getTool")?,
        get_camp.context("useList: no get_camp")?,
    );
    let count = count.context("useList: no Inventory.count")?;
    let cft = fun_t(code, count)?;
    if cft.args.len() != 4
        || cft.args[0] != inv_t
        || cft.args[1] != t.str_
        || cft.args[2] != null_i32
        || cft.ret != t.i32_
    {
        bail!("Inventory.count signature changed");
    }
    let ref_bool = cft.args[3];
    let has_item = has_item.context("useList: no hasItem")?;
    let ht = fun_t(code, has_item)?;
    if ht.args.len() != 4 || ht.args[0] != inv_t || ht.args[1] != t.str_ || ht.ret != t.bool_ {
        bail!("PlayerInventory.hasItem signature changed");
    }
    let ref_i32 = ht.args[2];
    if !matches!(code.types[ref_i32.0], Type::Ref(x) if x == t.i32_) || ht.args[3] != null_i32 {
        bail!("PlayerInventory.hasItem arguments changed");
    }
    let is_global = is_global.context("useList: no isGlobal")?;
    let gt = fun_t(code, is_global)?;
    if gt.args != [inv_t, t.str_] {
        bail!("PlayerInventory.isGlobal signature changed");
    }
    let null_bool = gt.ret;
    let use_ = proto(code, inv_t, "use")?;
    let ut = fun_t(code, use_)?;
    if ut.args != [inv_t, t.str_, t.bool_, null_i32, null_i32] {
        bail!("Inventory.use signature changed");
    }
    let alloc = alloc.context("useList: no alloc_array")?;
    let wrap = wrap.context("useList: no array wrap")?;
    let wt = fun_t(code, wrap)?;
    if wt.ret != list_t || wt.args.len() != 1 {
        bail!("array wrap is not (Array) -> ArrayObj");
    }
    let arr_t = wt.args[0];
    let at = fun_t(code, alloc)?;
    let type_t = at.args[0];
    let push = push.context("useList: no push")?;
    let (arr_len, _) = field(code, list_t, "length")?;
    let arr_raw = field(code, list_t, "array")?;
    let gs = game_refs(code)?;
    let (players, players_t) = field(code, gs.2 .1, "players")?;
    let proxy_array = field(code, players_t, "array")?;
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let (player_inv, pit) = field(code, player_t, "inventory")?;
    if pit != inv_t {
        bail!("BasePlayer.inventory is not st.Inventory");
    }
    let dbg_file = ul
        .debug_info
        .as_ref()
        .and_then(|d| d.first())
        .map(|x| x.0)
        .unwrap_or(0);
    Ok(Consume {
        use_list_fi,
        inv_t,
        list_t,
        entry_t,
        entry_count,
        entry_item,
        entry_quality,
        null_i32,
        ref_i32,
        ref_bool,
        null_bool,
        type_t,
        arr_t,
        alloc,
        wrap,
        push,
        arr_len,
        arr_raw,
        game_cls: gs.0,
        game_inst: gs.1,
        game_auth: gs.3,
        game_state: gs.2,
        get_camp,
        get_tool,
        chest_s: string_global(code, t.str_, "Chest")?,
        light_s: light_s.context("useList: no \"Light\" test")?,
        tool_inv: tool_inv.context("useList: no Tool.inventory")?,
        boat_inv: boat_inv.context("useList: no boatInventory")?,
        players: (players, players_t),
        proxy_array,
        player_t,
        player_inv,
        count,
        use_,
        is_global,
        has_item,
        dbg_file,
    })
}

type GameRefs = (
    (RefGlobal, RefType),
    (RefField, RefType),
    (RefField, RefType),
    RefField,
);

/// `$Game` class global, `inst`, `state`, `isAuth`.
fn game_refs(code: &Bytecode) -> Result<GameRefs> {
    let game_t = obj_type(code, "Game")?;
    let o = obj(code, game_t)?;
    let _ = o;
    let cls = obj(code, obj_type(code, "Game")?)?;
    let g = RefGlobal(
        cls.global
            .0
            .checked_sub(1)
            .context("Game: no class global")?,
    );
    let cls_t = code.globals[g.0];
    let (inst, inst_t) = field(code, cls_t, "inst")?;
    if inst_t != game_t {
        bail!("$Game.inst is not Game");
    }
    let state = field(code, game_t, "state")?;
    let (auth, at) = field(code, game_t, "isAuth")?;
    if !matches!(code.types[at.0], Type::Bool) {
        bail!("Game.isAuth is not Bool");
    }
    Ok(((g, cls_t), (inst, inst_t), state, auth))
}

/// Registers of partyPrepare.
struct R {
    gc: Reg,
    game: Reg,
    b: Reg,
    st: Reg,
    camp: Reg,
    name: Reg,
    rbn: Reg,
    tool: Reg,
    chest: Reg,
    boat: Reg,
    players: Reg,
    pad: Reg,
    parr: Reg,
    pn: Reg,
    j: Reg,
    raw: Reg,
    d: Reg,
    p: Reg,
    pinv: Reg,
    n: Reg,
    i: Reg,
    i2: Reg,
    zero: Reg,
    c: Reg,
    c2: Reg,
    item: Reg,
    item2: Reg,
    cnt: Reg,
    q: Reg,
    isg: Reg,
    tmp: Reg,
    rc: Reg,
    have: Reg,
    k: Reg,
    need: Reg,
    party: Reg,
    any: Reg,
    light: Reg,
    out: Reg,
    ty: Reg,
    arr: Reg,
    take: Reg,
    nq: Reg,
    e: Reg,
    v: Reg,
}

/// `d = list[idx]` as an entry `c`; null -> `skip`.
fn load_entry(a: &mut Asm, m: &Consume, r: &R, idx: Reg, c: Reg, skip: &'static str) {
    a.op(Opcode::Field {
        dst: r.raw,
        obj: Reg(1),
        field: m.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: r.d,
        array: r.raw,
        index: idx,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.d,
            offset: 0,
        },
        skip,
    );
    a.op(Opcode::ToVirtual { dst: c, src: r.d });
}

/// `pinv = players[j].inventory`; null or `this` -> `skip`.
fn load_player(a: &mut Asm, m: &Consume, r: &R, skip: &'static str) {
    a.op(Opcode::Field {
        dst: r.raw,
        obj: r.parr,
        field: m.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: r.d,
        array: r.raw,
        index: r.j,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.d,
            offset: 0,
        },
        skip,
    );
    a.op(Opcode::UnsafeCast { dst: r.p, src: r.d });
    a.op(Opcode::Field {
        dst: r.pinv,
        obj: r.p,
        field: m.player_inv,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.pinv,
            offset: 0,
        },
        skip,
    );
    a.jmp(
        Opcode::JEq {
            a: r.pinv,
            b: Reg(0),
            offset: 0,
        },
        skip,
    );
}

/// `x = inv.count(item, q, null)`.
fn count(a: &mut Asm, m: &Consume, r: &R, x: Reg, inv: Reg) {
    a.op(Opcode::Null { dst: r.rbn });
    a.op(Opcode::Call4 {
        dst: x,
        fun: m.count,
        arg0: inv,
        arg1: r.item,
        arg2: r.q,
        arg3: r.rbn,
    });
}

/// `have = this.count + chest?.count + boat?.count` (vanilla's own sources).
fn emit_have(a: &mut Asm, m: &Consume, r: &R, no_chest: &'static str, no_boat: &'static str) {
    count(a, m, r, r.have, Reg(0));
    a.jmp(
        Opcode::JNull {
            reg: r.chest,
            offset: 0,
        },
        no_chest,
    );
    count(a, m, r, r.k, r.chest);
    a.op(Opcode::Add {
        dst: r.have,
        a: r.have,
        b: r.k,
    });
    a.label(no_chest);
    a.jmp(
        Opcode::JNull {
            reg: r.boat,
            offset: 0,
        },
        no_boat,
    );
    count(a, m, r, r.k, r.boat);
    a.op(Opcode::Add {
        dst: r.have,
        a: r.have,
        b: r.k,
    });
    a.label(no_boat);
}

/// Entry `i` fields; entries vanilla handles without counting go to `pass`.
fn emit_entry(a: &mut Asm, m: &Consume, r: &R, pass: &'static str, global: &'static str) {
    a.op(Opcode::Field {
        dst: r.item,
        obj: r.c,
        field: m.entry_item,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.item,
            offset: 0,
        },
        pass,
    );
    a.op(Opcode::Field {
        dst: r.cnt,
        obj: r.c,
        field: m.entry_count,
    });
    a.jmp(
        Opcode::JSGte {
            a: r.zero,
            b: r.cnt,
            offset: 0,
        },
        pass,
    );
    a.jmp(
        Opcode::JEq {
            a: r.item,
            b: r.light,
            offset: 0,
        },
        pass,
    );
    a.op(Opcode::Field {
        dst: r.q,
        obj: r.c,
        field: m.entry_quality,
    });
    a.op(Opcode::Call2 {
        dst: r.isg,
        fun: m.is_global,
        arg0: Reg(0),
        arg1: r.item,
    });
    a.op(Opcode::SafeCast {
        dst: r.b,
        src: r.isg,
    });
    a.jmp(
        Opcode::JTrue {
            cond: r.b,
            offset: 0,
        },
        global,
    );
}

fn add_party_prepare(code: &mut Bytecode, t: &T, m: &Consume) -> Result<RefFun> {
    let mut g = Regs(vec![m.inv_t, m.list_t]);
    let r = R {
        gc: g.r(m.game_cls.1),
        game: g.r(m.game_inst.1),
        b: g.r(t.bool_),
        st: g.r(m.game_state.1),
        camp: g.r(m.get_camp.1),
        name: g.r(t.str_),
        rbn: g.r(m.ref_bool),
        tool: g.r(m.get_tool.1),
        chest: g.r(m.inv_t),
        boat: g.r(m.inv_t),
        players: g.r(m.players.1),
        pad: g.r(m.proxy_array.1),
        parr: g.r(m.list_t),
        pn: g.r(t.i32_),
        j: g.r(t.i32_),
        raw: g.r(m.arr_raw.1),
        d: g.r(t.dyn_),
        p: g.r(m.player_t),
        pinv: g.r(m.inv_t),
        n: g.r(t.i32_),
        i: g.r(t.i32_),
        i2: g.r(t.i32_),
        zero: g.r(t.i32_),
        c: g.r(m.entry_t),
        c2: g.r(m.entry_t),
        item: g.r(t.str_),
        item2: g.r(t.str_),
        cnt: g.r(t.i32_),
        q: g.r(m.null_i32),
        isg: g.r(m.null_bool),
        tmp: g.r(t.i32_),
        rc: g.r(m.ref_i32),
        have: g.r(t.i32_),
        k: g.r(t.i32_),
        need: g.r(t.i32_),
        party: g.r(t.i32_),
        any: g.r(t.bool_),
        light: g.r(t.str_),
        out: g.r(m.list_t),
        ty: g.r(m.type_t),
        arr: g.r(m.arr_t),
        take: g.r(t.i32_),
        nq: g.r(m.null_i32),
        e: g.r(m.entry_t),
        v: g.r(t.void),
    };
    let zc = int_const(code, 0);
    let mut a = Asm::new();
    // host only; chest / boat exactly as useList resolves them with checkChest
    a.op(Opcode::GetGlobal {
        dst: r.gc,
        global: m.game_cls.0,
    });
    a.op(Opcode::Field {
        dst: r.game,
        obj: r.gc,
        field: m.game_inst.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.game,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Field {
        dst: r.b,
        obj: r.game,
        field: m.game_auth,
    });
    a.jmp(
        Opcode::JFalse {
            cond: r.b,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Field {
        dst: r.st,
        obj: r.game,
        field: m.game_state.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.st,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Call1 {
        dst: r.camp,
        fun: m.get_camp.0,
        arg0: r.st,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.camp,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::GetGlobal {
        dst: r.name,
        global: m.chest_s,
    });
    a.op(Opcode::Null { dst: r.rbn });
    a.op(Opcode::Call3 {
        dst: r.tool,
        fun: m.get_tool.0,
        arg0: r.camp,
        arg1: r.name,
        arg2: r.rbn,
    });
    a.op(Opcode::Null { dst: r.chest });
    a.jmp(
        Opcode::JNull {
            reg: r.tool,
            offset: 0,
        },
        "nochest",
    );
    a.op(Opcode::Field {
        dst: r.chest,
        obj: r.tool,
        field: m.tool_inv,
    });
    a.label("nochest");
    a.op(Opcode::Field {
        dst: r.boat,
        obj: r.camp,
        field: m.boat_inv,
    });
    a.op(Opcode::Field {
        dst: r.players,
        obj: r.st,
        field: m.players.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.players,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Field {
        dst: r.pad,
        obj: r.players,
        field: m.proxy_array.0,
    });
    a.op(Opcode::SafeCast {
        dst: r.parr,
        src: r.pad,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.parr,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Field {
        dst: r.pn,
        obj: r.parr,
        field: m.arr_len,
    });
    a.op(Opcode::GetGlobal {
        dst: r.light,
        global: m.light_s,
    });
    a.op(Opcode::Int {
        dst: r.zero,
        ptr: zc,
    });
    a.op(Opcode::Bool {
        dst: r.any,
        value: ValBool(false),
    });
    a.op(Opcode::Field {
        dst: r.n,
        obj: Reg(1),
        field: m.arr_len,
    });

    // ---- pass 1: the whole list must be payable, else leave it to vanilla
    a.op(Opcode::Int { dst: r.i, ptr: zc });
    a.loop_head("l1");
    a.jmp(
        Opcode::JSGte {
            a: r.i,
            b: r.n,
            offset: 0,
        },
        "l1done",
    );
    load_entry(&mut a, m, &r, r.i, r.c, "n1");
    emit_entry(&mut a, m, &r, "n1", "g1");
    emit_have(&mut a, m, &r, "h1c", "h1b");
    a.jmp(
        Opcode::JSGte {
            a: r.have,
            b: r.cnt,
            offset: 0,
        },
        "n1",
    );
    a.op(Opcode::Sub {
        dst: r.need,
        a: r.cnt,
        b: r.have,
    });
    // the same item twice: leave it to vanilla
    a.op(Opcode::Int { dst: r.i2, ptr: zc });
    a.loop_head("dl");
    a.jmp(
        Opcode::JSGte {
            a: r.i2,
            b: r.n,
            offset: 0,
        },
        "ddone",
    );
    a.jmp(
        Opcode::JEq {
            a: r.i2,
            b: r.i,
            offset: 0,
        },
        "dn",
    );
    load_entry(&mut a, m, &r, r.i2, r.c2, "dn");
    a.op(Opcode::Field {
        dst: r.item2,
        obj: r.c2,
        field: m.entry_item,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.item2,
            offset: 0,
        },
        "dn",
    );
    a.jmp(
        Opcode::JEq {
            a: r.item2,
            b: r.item,
            offset: 0,
        },
        "keep",
    );
    a.label("dn");
    a.op(Opcode::Incr { dst: r.i2 });
    a.jmp(Opcode::JAlways { offset: 0 }, "dl");
    a.label("ddone");
    // party = sum of the other players' counts
    a.op(Opcode::Int {
        dst: r.party,
        ptr: zc,
    });
    a.op(Opcode::Int { dst: r.j, ptr: zc });
    a.loop_head("pl");
    a.jmp(
        Opcode::JSGte {
            a: r.j,
            b: r.pn,
            offset: 0,
        },
        "pdone",
    );
    load_player(&mut a, m, &r, "pn");
    count(&mut a, m, &r, r.k, r.pinv);
    a.op(Opcode::Add {
        dst: r.party,
        a: r.party,
        b: r.k,
    });
    a.label("pn");
    a.op(Opcode::Incr { dst: r.j });
    a.jmp(Opcode::JAlways { offset: 0 }, "pl");
    a.label("pdone");
    a.jmp(
        Opcode::JSLt {
            a: r.party,
            b: r.need,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Bool {
        dst: r.any,
        value: ValBool(true),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "n1");
    // global entry: vanilla pays it from the global inventory; it must be there
    a.label("g1");
    a.op(Opcode::Mov {
        dst: r.tmp,
        src: r.cnt,
    });
    a.op(Opcode::Ref {
        dst: r.rc,
        src: r.tmp,
    });
    a.op(Opcode::Call4 {
        dst: r.b,
        fun: m.has_item,
        arg0: Reg(0),
        arg1: r.item,
        arg2: r.rc,
        arg3: r.q,
    });
    a.jmp(
        Opcode::JFalse {
            cond: r.b,
            offset: 0,
        },
        "keep",
    );
    a.label("n1");
    a.op(Opcode::Incr { dst: r.i });
    a.jmp(Opcode::JAlways { offset: 0 }, "l1");
    a.label("l1done");
    a.jmp(
        Opcode::JFalse {
            cond: r.any,
            offset: 0,
        },
        "keep",
    );

    // ---- pass 2: take the shortfall from the other players, ask vanilla for the rest
    a.op(Opcode::Type {
        dst: r.ty,
        ty: m.entry_t,
    });
    a.op(Opcode::Call2 {
        dst: r.arr,
        fun: m.alloc,
        arg0: r.ty,
        arg1: r.zero,
    });
    a.op(Opcode::Call1 {
        dst: r.out,
        fun: m.wrap,
        arg0: r.arr,
    });
    a.op(Opcode::Int { dst: r.i, ptr: zc });
    a.loop_head("l2");
    a.jmp(
        Opcode::JSGte {
            a: r.i,
            b: r.n,
            offset: 0,
        },
        "l2done",
    );
    a.op(Opcode::Field {
        dst: r.raw,
        obj: Reg(1),
        field: m.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: r.d,
        array: r.raw,
        index: r.i,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.d,
            offset: 0,
        },
        "same",
    );
    a.op(Opcode::ToVirtual { dst: r.c, src: r.d });
    emit_entry(&mut a, m, &r, "same", "same");
    emit_have(&mut a, m, &r, "h2c", "h2b");
    a.jmp(
        Opcode::JSGte {
            a: r.have,
            b: r.cnt,
            offset: 0,
        },
        "same",
    );
    a.op(Opcode::Sub {
        dst: r.need,
        a: r.cnt,
        b: r.have,
    });
    a.op(Opcode::Int { dst: r.j, ptr: zc });
    a.loop_head("cl");
    a.jmp(
        Opcode::JSGte {
            a: r.j,
            b: r.pn,
            offset: 0,
        },
        "cdone",
    );
    a.jmp(
        Opcode::JSGte {
            a: r.zero,
            b: r.need,
            offset: 0,
        },
        "cdone",
    );
    load_player(&mut a, m, &r, "cn");
    count(&mut a, m, &r, r.k, r.pinv);
    a.jmp(
        Opcode::JSGte {
            a: r.zero,
            b: r.k,
            offset: 0,
        },
        "cn",
    );
    a.op(Opcode::Mov {
        dst: r.take,
        src: r.need,
    });
    a.jmp(
        Opcode::JSGte {
            a: r.k,
            b: r.need,
            offset: 0,
        },
        "ctake",
    );
    a.op(Opcode::Mov {
        dst: r.take,
        src: r.k,
    });
    a.label("ctake");
    a.op(Opcode::ToDyn {
        dst: r.nq,
        src: r.take,
    });
    a.op(Opcode::Bool {
        dst: r.b,
        value: ValBool(true),
    });
    a.op(Opcode::CallN {
        dst: r.v,
        fun: m.use_,
        args: vec![r.pinv, r.item, r.b, r.nq, r.q],
    });
    a.op(Opcode::Sub {
        dst: r.need,
        a: r.need,
        b: r.take,
    });
    a.label("cn");
    a.op(Opcode::Incr { dst: r.j });
    a.jmp(Opcode::JAlways { offset: 0 }, "cl");
    a.label("cdone");
    a.op(Opcode::New { dst: r.e });
    a.op(Opcode::SetField {
        obj: r.e,
        field: m.entry_item,
        src: r.item,
    });
    a.op(Opcode::SetField {
        obj: r.e,
        field: m.entry_count,
        src: r.have,
    });
    a.op(Opcode::SetField {
        obj: r.e,
        field: m.entry_quality,
        src: r.q,
    });
    a.op(Opcode::Call2 {
        dst: r.k,
        fun: m.push,
        arg0: r.out,
        arg1: r.e,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "n2");
    a.label("same");
    a.op(Opcode::Call2 {
        dst: r.k,
        fun: m.push,
        arg0: r.out,
        arg1: r.d,
    });
    a.label("n2");
    a.op(Opcode::Incr { dst: r.i });
    a.jmp(Opcode::JAlways { offset: 0 }, "l2");
    a.label("l2done");
    a.op(Opcode::Ret { ret: r.out });
    a.label("keep");
    a.op(Opcode::Ret { ret: Reg(1) });
    push_fn(
        code,
        vec![m.inv_t, m.list_t],
        m.list_t,
        g.0,
        a.finish(),
        m.dbg_file,
    )
}

fn apply_consume(code: &mut Bytecode, t: &T, m: &Consume) -> Result<RefFun> {
    let prep = add_party_prepare(code, t, m)?;
    let f = &mut code.functions[m.use_list_fi];
    insert_ops(
        f,
        0,
        vec![
            Opcode::JFalse {
                cond: Reg(2),
                offset: 1,
            },
            Opcode::Call2 {
                dst: Reg(1),
                fun: prep,
                arg0: Reg(0),
                arg1: Reg(1),
            },
        ],
    );
    eprintln!(
        "patched party inventory fn@{}: with-chest costs also paid by the other players (partyPrepare fn@{})",
        f.findex.0, prep.0
    );
    Ok(prep)
}

/// A no-argument constructor value stored in a global: the init code does
/// `SetGlobal(g, v); SetField($Enum.<name>, v)`.
fn enum_value_global(code: &Bytecode, enum_t: RefType, name: &str) -> Result<RefGlobal> {
    for f in &code.functions {
        for w in f.ops.windows(2) {
            if let [Opcode::SetGlobal { global, src }, Opcode::SetField {
                obj,
                field,
                src: s2,
            }] = w
            {
                if src == s2
                    && code.globals.get(global.0) == Some(&enum_t)
                    && field_name(code, f.regs[obj.0 as usize], *field) == Some(name)
                {
                    return Ok(*global);
                }
            }
        }
    }
    bail!("enum value {name} not found")
}

/// One op edit: replace op `at` of function `fi` by `op`.
struct Edit {
    fi: usize,
    at: usize,
    op: Opcode,
    what: &'static str,
}

struct Dialog {
    edits: Vec<Edit>,
    /// (function index, op index) of the Int ops whose value becomes `value`.
    ints: Vec<(usize, usize, i32)>,
}

fn int_value(code: &Bytecode, op: &Opcode) -> Option<(Reg, i32)> {
    match op {
        Opcode::Int { dst, ptr } => code.ints.get(ptr.0).map(|v| (*dst, *v)),
        _ => None,
    }
}

/// `Int` op setting `reg` most recently before `at`.
fn int_set_before(code: &Bytecode, f: &Function, reg: Reg, at: usize) -> Option<(usize, i32)> {
    (0..at)
        .rev()
        .find_map(|i| match int_value(code, &f.ops[i]) {
            Some((d, v)) if d == reg => Some((i, v)),
            _ => {
                if crate::chest_buttons::uses_reg(&f.ops[i], reg.0)
                    && !matches!(f.ops[i], Opcode::NullCheck { .. })
                {
                    // another writer of `reg` in between: stop looking
                    Some((usize::MAX, 0))
                } else {
                    None
                }
            }
        })
        .filter(|(i, _)| *i != usize::MAX)
}

fn call_target(op: &Opcode) -> Option<(RefFun, Vec<Reg>)> {
    match op {
        Opcode::Call1 { fun, arg0, .. } => Some((*fun, vec![*arg0])),
        Opcode::Call2 {
            fun, arg0, arg1, ..
        } => Some((*fun, vec![*arg0, *arg1])),
        Opcode::Call3 {
            fun,
            arg0,
            arg1,
            arg2,
            ..
        } => Some((*fun, vec![*arg0, *arg1, *arg2])),
        Opcode::Call4 {
            fun,
            arg0,
            arg1,
            arg2,
            arg3,
            ..
        } => Some((*fun, vec![*arg0, *arg1, *arg2, *arg3])),
        Opcode::CallN { fun, args, .. } => Some((*fun, args.clone())),
        _ => None,
    }
}

fn plan_dialog(code: &Bytecode, use_list: RefFun) -> Result<Dialog> {
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    let (type_f, type_t) = field(code, dlg_t, "type")?;
    if !matches!(code.types[type_t.0], Type::Enum { .. }) {
        bail!("Dialog.type is not an enum");
    }
    let confession = enum_value_global(code, type_t, "Confession")?;
    let tavern = enum_value_global(code, type_t, "TavernEvent")?;
    let gi_t = obj_type(code, "st.player.GlobalInventory")?;
    let has_count = method(code, gi_t, "hasItemCount")?.findex;
    let get_count = method(code, gi_t, "getItemCount")?.findex;
    let fmt_cost = code
        .functions
        .iter()
        .find(|f| s(code, f.name) == "formatItemCost")
        .context("Fmt.formatItemCost not found")?;
    let fmt_item = code
        .functions
        .iter()
        .find(|f| s(code, f.name) == "getFormattedItem")
        .context("Dialog.getFormattedItem not found")?;
    let cost_list = code
        .functions
        .iter()
        .find(|f| s(code, f.name) == "getChoiceItemsCost")
        .context("Dialog.getChoiceItemsCost not found")?;
    let cost_fns = [fmt_cost.findex, fmt_item.findex, cost_list.findex];

    let mut edits = vec![];
    let mut ints = vec![];

    // displayChoices: the Confession tests deciding chest/party scope -> "not a tavern event".
    let dc = method(code, dlg_t, "displayChoices")?;
    let dci = fun_index(code, dc.findex)?;
    let o = &dc.ops;
    let (mut allowed, mut scopes) = (0, 0);
    for i in 0..o.len().saturating_sub(6) {
        let (Opcode::GetThis { dst: a, field }, Opcode::GetGlobal { dst: b, global }) =
            (&o[i], &o[i + 1])
        else {
            continue;
        };
        if *field != type_f {
            continue;
        }
        if *global == tavern {
            if matches!(o[i + 2], Opcode::JEq { .. }) {
                bail!("already applied");
            }
            continue;
        }
        if *global != confession {
            continue;
        }
        let Opcode::JNotEq {
            a: ja,
            b: jb,
            offset,
        } = o[i + 2]
        else {
            continue;
        };
        if ja != *a || jb != *b {
            continue;
        }
        // (a) `allowed` fallback: the skipped block asks GlobalInventory.hasItemCount
        if offset > 2 {
            let end = i + 3 + offset as usize;
            let calls_count = o[i + 3..end.min(o.len())]
                .iter()
                .any(|x| matches!(call_target(x), Some((f, _)) if f == has_count));
            if calls_count {
                edits.push(Edit {
                    fi: dci,
                    at: i + 1,
                    op: Opcode::GetGlobal {
                        dst: *b,
                        global: tavern,
                    },
                    what: "allowed scope",
                });
                edits.push(Edit {
                    fi: dci,
                    at: i + 2,
                    op: Opcode::JEq {
                        a: ja,
                        b: jb,
                        offset,
                    },
                    what: "allowed scope",
                });
                allowed += 1;
            }
            continue;
        }
        // (b) checkChest argument: `type == Confession` -> `type != TavernEvent`
        if let (
            2,
            Opcode::Bool {
                dst: x,
                value: ValBool(true),
            },
            Opcode::JAlways { offset: 1 },
            Opcode::Bool {
                dst: y,
                value: ValBool(false),
            },
            Opcode::ToDyn { dst: dd, src: z },
        ) = (offset, &o[i + 3], &o[i + 4], &o[i + 5], &o[i + 6])
        {
            if x != y || y != z {
                continue;
            }
            let fed = o[i + 7..(i + 16).min(o.len())]
                .iter()
                .any(|op| matches!(call_target(op), Some((f, args)) if cost_fns.contains(&f) && args.contains(dd)));
            if !fed {
                continue;
            }
            edits.push(Edit {
                fi: dci,
                at: i + 1,
                op: Opcode::GetGlobal {
                    dst: *b,
                    global: tavern,
                },
                what: "checkChest scope",
            });
            edits.push(Edit {
                fi: dci,
                at: i + 3,
                op: Opcode::Bool {
                    dst: *x,
                    value: ValBool(false),
                },
                what: "checkChest scope",
            });
            edits.push(Edit {
                fi: dci,
                at: i + 5,
                op: Opcode::Bool {
                    dst: *x,
                    value: ValBool(true),
                },
                what: "checkChest scope",
            });
            scopes += 1;
        }
    }
    if allowed != 1 || scopes != 3 {
        bail!("displayChoices: {allowed} allowed / {scopes} checkChest sites (want 1 / 3)");
    }

    // Choice cost payment: useList(player.inventory, items, type == Confession, cb) -> checkChest = true.
    let mut pays = 0;
    for (fi, f) in code.functions.iter().enumerate() {
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Call4 { fun, arg2, .. } = op else {
                continue;
            };
            if *fun != use_list || i < 5 {
                continue;
            }
            if let (
                Opcode::GetGlobal { global, .. },
                Opcode::JNotEq { offset: 2, .. },
                Opcode::Bool {
                    dst: x,
                    value: ValBool(true),
                },
                Opcode::JAlways { offset: 1 },
                Opcode::Bool {
                    dst: y,
                    value: ValBool(false),
                },
            ) = (
                &f.ops[i - 5],
                &f.ops[i - 4],
                &f.ops[i - 3],
                &f.ops[i - 2],
                &f.ops[i - 1],
            ) {
                if *global == confession && x == arg2 && y == arg2 {
                    edits.push(Edit {
                        fi,
                        at: i - 1,
                        op: Opcode::Bool {
                            dst: *y,
                            value: ValBool(true),
                        },
                        what: "cost payment",
                    });
                    pays += 1;
                }
            }
        }
    }
    if pays != 1 {
        bail!("{pays} dialog cost payments found (want 1)");
    }

    // getActionCost (host, picks the item a "same type as" cost really takes):
    // checkChest = (dialogType == Confession) -> (dialogType != TavernEvent), as in
    // displayChoices, so display and payment resolve the same item.
    let gac = code
        .functions
        .iter()
        .find(|f| s(code, f.name) == "getActionCost")
        .context("Dialog.getActionCost not found")?;
    let gaci = fun_index(code, gac.findex)?;
    let go = &gac.ops;
    let mut acs = 0;
    for i in 0..go.len().saturating_sub(6) {
        if let (
            Opcode::GetGlobal { dst: b, global },
            Opcode::JNotEq {
                a: ja,
                b: jb,
                offset: 2,
            },
            Opcode::Bool {
                dst: x,
                value: ValBool(true),
            },
            Opcode::JAlways { offset: 1 },
            Opcode::Bool {
                dst: y,
                value: ValBool(false),
            },
            Opcode::ToDyn { dst: dd, src: z },
        ) = (
            &go[i],
            &go[i + 1],
            &go[i + 2],
            &go[i + 3],
            &go[i + 4],
            &go[i + 5],
        ) {
            if *global != confession || jb != b || x != y || y != z {
                continue;
            }
            let fed = go[i + 6..(i + 8).min(go.len())]
                .iter()
                .any(|op| matches!(call_target(op), Some((f, args)) if f == fmt_item.findex && args.contains(dd)));
            if !fed {
                continue;
            }
            let _ = ja;
            edits.push(Edit {
                fi: gaci,
                at: i,
                op: Opcode::GetGlobal {
                    dst: *b,
                    global: tavern,
                },
                what: "action cost scope",
            });
            edits.push(Edit {
                fi: gaci,
                at: i + 2,
                op: Opcode::Bool {
                    dst: *x,
                    value: ValBool(false),
                },
                what: "action cost scope",
            });
            edits.push(Edit {
                fi: gaci,
                at: i + 4,
                op: Opcode::Bool {
                    dst: *x,
                    value: ValBool(true),
                },
                what: "action cost scope",
            });
            acs += 1;
        }
    }
    if acs != 1 {
        bail!("getActionCost: {acs} checkChest sites (want 1)");
    }

    // getFormattedItem, when the caller passes checkChest: the item is "found" when
    // the party holds it (global|player|others|chest, counted around the passed
    // player, not the local one), and the same-type match also looks at the other
    // players. Without checkChest (tavern events) it stays vanilla.
    let gfi = fun_index(code, fmt_item.findex)?;
    let fmt_ops = &fmt_item.ops;
    let first_count = fmt_ops
        .iter()
        .position(|op| matches!(call_target(op), Some((f, _)) if f == has_count))
        .context("getFormattedItem: no hasItemCount")?;
    let Some((_, args)) = call_target(&fmt_ops[first_count]) else {
        unreachable!()
    };
    let (ii, v) = int_set_before(code, fmt_item, args[4], first_count)
        .context("getFormattedItem: flags not constant")?;
    if v != F_CHEST {
        bail!("getFormattedItem: chest flags {v}");
    }
    ints.push((gfi, ii, F_GLOBAL | F_PLAYER | F_OTHERS | F_CHEST));
    let pl = args[5];
    let pi = (0..first_count)
        .rev()
        .find(|&i| matches!(fmt_ops[i], Opcode::Null { dst } if dst == pl))
        .context("getFormattedItem: player argument is not null")?;
    if fmt_item.regs[pl.0 as usize] != fmt_item.regs[0] {
        bail!("getFormattedItem: player argument type");
    }
    edits.push(Edit {
        fi: gfi,
        at: pi,
        op: Opcode::Mov {
            dst: pl,
            src: Reg(0),
        },
        what: "found around the passed player",
    });
    // same-type filter: `filter |= 256` when checkChest -> `|= 260`
    let second = fmt_ops
        .iter()
        .enumerate()
        .skip(first_count + 1)
        .filter_map(|(i, op)| match int_value(code, op) {
            Some((r, F_CHEST)) => match fmt_ops.get(i + 1) {
                Some(Opcode::Or { b, .. }) if *b == r => Some(i),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [or_at] = second[..] else {
        bail!(
            "getFormattedItem: {} same-type chest flags (want 1)",
            second.len()
        );
    };
    ints.push((gfi, or_at, F_CHEST | F_OTHERS));
    // formatItemCost: the chest count also counts the other players.
    let fci = fun_index(code, fmt_cost.findex)?;
    let c_at = fmt_cost
        .ops
        .iter()
        .position(|op| matches!(call_target(op), Some((f, _)) if f == get_count))
        .context("formatItemCost: no getItemCount")?;
    let Some((_, args)) = call_target(&fmt_cost.ops[c_at]) else {
        unreachable!()
    };
    let (ii, v) = int_set_before(code, fmt_cost, args[3], c_at)
        .context("formatItemCost: flags not constant")?;
    if v != F_CHEST {
        bail!("formatItemCost: chest flags {v}");
    }
    ints.push((fci, ii, F_CHEST | F_OTHERS));
    Ok(Dialog { edits, ints })
}

fn apply_dialog(code: &mut Bytecode, d: Dialog) {
    for (fi, at, value) in &d.ints {
        let ptr: RefInt = int_const(code, *value);
        if let Opcode::Int { ptr: p, .. } = &mut code.functions[*fi].ops[*at] {
            *p = ptr;
        }
    }
    let n = d.edits.len();
    for e in d.edits {
        let _ = e.what;
        code.functions[e.fi].ops[e.at] = e.op;
    }
    eprintln!(
        "patched party inventory dialogs: classic dialogs use the party scope ({n} op edits, {} flag constants)",
        d.ints.len()
    );
}

/// Pays with-chest costs from the other players too and gives Classic dialogs the
/// party scope, or leaves `code` untouched and logs why.
pub(crate) fn patch_party_inventory(code: &mut Bytecode) {
    let t = match types(code) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("party inventory skipped: {e:#}");
            return;
        }
    };
    let m = match plan_consume(code, &t) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("party inventory skipped: {e:#}");
            return;
        }
    };
    let use_list = code.functions[m.use_list_fi].findex;
    let dialog = plan_dialog(code, use_list);
    let snap = Snap::take(code);
    let before = code.functions[m.use_list_fi].clone();
    if let Err(e) = apply_consume(code, &t, &m) {
        eprintln!("party inventory skipped: {e:#}");
        code.functions[m.use_list_fi] = before;
        snap.restore(code);
        return;
    }
    match dialog {
        Ok(d) => apply_dialog(code, d),
        Err(e) => eprintln!("party inventory dialogs skipped: {e:#}"),
    }
}

/// `(fi, at)` of the `Int 2` (player) in the flags of `name`'s GlobalInventory call.
fn plan_count_flags(
    code: &Bytecode,
    name: &str,
    target: RefFun,
    flags_arg: usize,
) -> Result<(usize, usize)> {
    let hits: Vec<usize> = code
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| s(code, f.name) == name)
        .map(|(i, _)| i)
        .collect();
    let [fi] = hits[..] else {
        bail!("expected one {name}, found {}", hits.len());
    };
    let f = &code.functions[fi];
    let at = f
        .ops
        .iter()
        .position(|op| matches!(call_target(op), Some((g, _)) if g == target))
        .with_context(|| format!("{name}: no GlobalInventory call"))?;
    let Some((_, args)) = call_target(&f.ops[at]) else {
        unreachable!()
    };
    let flags = *args
        .get(flags_arg)
        .with_context(|| format!("{name}: flags argument"))?;
    // flags = 1 | 2 | 256 built as Int/Or pairs
    for i in (1..at).rev() {
        if let (Some((x, v)), Opcode::Or { dst, a, b }) =
            (int_value(code, &f.ops[i]), &f.ops[i + 1])
        {
            if *dst == flags && *a == flags && *b == x {
                match v {
                    2 => return Ok((fi, i)),
                    v if v == 2 | F_OTHERS => bail!("already applied"),
                    _ => {}
                }
            }
        }
    }
    bail!("{name}: player flag not found")
}

/// hasItemWithChest / countWithChest (crafting, repair, healing, boat displays) also
/// count the other players' inventories, matching what partyPrepare lets them pay
/// with. Skipped (logged) on mismatch.
pub(crate) fn patch_party_counts(code: &mut Bytecode) {
    let plan = || -> Result<Vec<(usize, usize)>> {
        let gi_t = obj_type(code, "st.player.GlobalInventory")?;
        let has_count = method(code, gi_t, "hasItemCount")?.findex;
        let get_count = method(code, gi_t, "getItemCount")?.findex;
        Ok(vec![
            plan_count_flags(code, "hasItemWithChest", has_count, 4)?,
            plan_count_flags(code, "countWithChest", get_count, 3)?,
        ])
    };
    match plan() {
        Ok(sites) => {
            let ptr = int_const(code, 2 | F_OTHERS);
            for (fi, at) in &sites {
                if let Opcode::Int { ptr: p, .. } = &mut code.functions[*fi].ops[*at] {
                    *p = ptr;
                }
            }
            eprintln!(
                "patched party counts fn@{} fn@{}: with-chest counts include the other players",
                code.functions[sites[0].0].findex.0, code.functions[sites[1].0].findex.0
            );
        }
        Err(e) => eprintln!("party counts skipped: {e:#}"),
    }
}

/// `(fi, at)` of the `Int 2` (player) in `this.name`'s inlined GlobalInventory item
/// iteration `flags = 2 | 256` (player + chest). The iteration must also have the
/// other-players branch (`flags & 4`), so adding 4 lists their inventories (never
/// equipped: that is 8 / 16).
fn plan_iter_flags(code: &Bytecode, class: &str, name: &str) -> Result<(usize, usize)> {
    let f = method(code, obj_type(code, class)?, name)?;
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    let mut sites = vec![];
    for i in 0..o.len().saturating_sub(2) {
        let (Some((ra, va)), Some((rb, vb)), Opcode::Or { dst, a, b }) = (
            int_value(code, &o[i]),
            int_value(code, &o[i + 1]),
            &o[i + 2],
        ) else {
            continue;
        };
        if *dst != ra || *a != ra || *b != rb || vb != F_CHEST {
            continue;
        }
        match va {
            F_PLAYER => sites.push((i, ra)),
            v if v == F_PLAYER | F_OTHERS => bail!("{class}.{name}: already applied"),
            _ => {}
        }
    }
    let [(at, flags)] = sites[..] else {
        bail!(
            "{class}.{name}: {} player|chest flag sites (want 1)",
            sites.len()
        );
    };
    let others = o.windows(2).skip(at + 3).any(|w| {
        matches!(
            (int_value(code, &w[0]), &w[1]),
            (Some((c, F_OTHERS)), Opcode::And { a, b, .. }) if *a == flags && *b == c
        )
    });
    if !others {
        bail!("{class}.{name}: no other-players branch");
    }
    Ok((fi, at))
}

/// `(fi, at)` of the `Int 256` flags of `Button.syncText`'s with-chest
/// `GlobalInventory.getItemCount` (cost buttons with `checkChest`, e.g. Debrief).
fn plan_button_flags(code: &Bytecode) -> Result<(usize, usize)> {
    let gi_t = obj_type(code, "st.player.GlobalInventory")?;
    let get_count = method(code, gi_t, "getItemCount")?.findex;
    let f = method(code, obj_type(code, "ui.comp.Button")?, "syncText")?;
    let fi = fun_index(code, f.findex)?;
    let calls: Vec<usize> = f
        .ops
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(call_target(op), Some((g, _)) if g == get_count))
        .map(|(i, _)| i)
        .collect();
    let [c_at] = calls[..] else {
        bail!(
            "Button.syncText: {} getItemCount calls (want 1)",
            calls.len()
        );
    };
    let Some((_, args)) = call_target(&f.ops[c_at]) else {
        unreachable!()
    };
    let (ii, v) =
        int_set_before(code, f, args[3], c_at).context("Button.syncText: flags not constant")?;
    match v {
        F_CHEST => Ok((fi, ii)),
        v if v == F_CHEST | F_OTHERS => bail!("Button.syncText: already applied"),
        v => bail!("Button.syncText: chest flags {v}"),
    }
}

/// The injury heal panel's remedy list and the injury tooltip's "remedy available"
/// test (both own + chest) also look at the other players' inventories, and with-chest
/// cost buttons (Debrief heal / repair) count them too. Paying is partyPrepare's job
/// (InjuryHealPanel.heal and Debrief use useList with checkChest). Skipped (logged)
/// on mismatch.
pub(crate) fn patch_party_lists(code: &mut Bytecode) {
    // (function index, op index, new flags value)
    let plan = || -> Result<Vec<(usize, usize, i32)>> {
        let heal = plan_iter_flags(code, "ui.win.InjuryHealPanel", "init")?;
        let tip = plan_iter_flags(code, "battle.ui.comp.StatusIcon", "getTipContent")?;
        let button = plan_button_flags(code)?;
        Ok(vec![
            (heal.0, heal.1, F_PLAYER | F_OTHERS),
            (tip.0, tip.1, F_PLAYER | F_OTHERS),
            (button.0, button.1, F_CHEST | F_OTHERS),
        ])
    };
    match plan() {
        Ok(sites) => {
            for (fi, at, value) in &sites {
                let ptr = int_const(code, *value);
                if let Opcode::Int { ptr: p, .. } = &mut code.functions[*fi].ops[*at] {
                    *p = ptr;
                }
            }
            eprintln!(
                "patched party lists fn@{} fn@{} fn@{}: remedies and cost buttons include the other players",
                code.functions[sites[0].0].findex.0,
                code.functions[sites[1].0].findex.0,
                code.functions[sites[2].0].findex.0
            );
        }
        Err(e) => eprintln!("party lists skipped: {e:#}"),
    }
}

/// `(fi, at, with_chest)`: the own-inventory `PlayerInventory.hasItem` call in
/// `ItemsRecipe.hasItem`'s `checkChest = false` branch, and the `hasItemWithChest`
/// (same signature) to call instead.
fn plan_recipe_has(code: &Bytecode) -> Result<(usize, usize, RefFun)> {
    let wc: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| s(code, f.name) == "hasItemWithChest")
        .collect();
    let [wc] = wc[..] else {
        bail!("expected one hasItemWithChest, found {}", wc.len());
    };
    let wc_t = fun_t(code, wc.findex)?;
    let f = method(code, obj_type(code, "ui.win.ItemsRecipe")?, "hasItem")?;
    let fi = fun_index(code, f.findex)?;
    let mut own = vec![];
    let mut chest = 0;
    for (i, op) in f.ops.iter().enumerate() {
        let Opcode::Call4 { fun, .. } = op else {
            continue;
        };
        if *fun == wc.findex {
            chest += 1;
        } else if fname(code, *fun) == "hasItem" && fun_t(code, *fun)? == wc_t {
            own.push(i);
        }
    }
    match (&own[..], chest) {
        ([at], 1) => Ok((fi, *at, wc.findex)),
        ([], 2) => bail!("ItemsRecipe.hasItem: already applied"),
        _ => bail!(
            "ItemsRecipe.hasItem: {} own / {chest} with-chest calls (want 1 / 1)",
            own.len()
        ),
    }
}

/// Recipe ingredient rows (`ItemsRecipe`: Grimoire recipe cells and their learn
/// tooltip, item tooltips' recipe) mark an ingredient missing from the player's own
/// inventory only; they now count like `hasItemWithChest` (global + every player +
/// chest + boat), matching what crafting pays with (useList checkChest + partyPrepare).
/// Display only. Skipped (logged) on mismatch.
pub(crate) fn patch_party_recipes(code: &mut Bytecode) {
    match plan_recipe_has(code) {
        Ok((fi, at, with_chest)) => {
            if let Opcode::Call4 { fun, .. } = &mut code.functions[fi].ops[at] {
                *fun = with_chest;
            }
            eprintln!(
                "patched party recipes fn@{}: recipe ingredient rows include chest and the other players",
                code.functions[fi].findex.0
            );
        }
        Err(e) => eprintln!("party recipes skipped: {e:#}"),
    }
}

/// PlayerInventory statics used by the activity pass, matched by name and signature.
struct InvFns {
    count: RefFun,
    count_wc: RefFun,
    has: RefFun,
    has_wc: RefFun,
    try_use: RefFun,
    use_: RefFun,
    cb_t: RefType,
}

/// The one function named `name` with exactly these argument types.
fn static_fn(code: &Bytecode, name: &str, args: &[RefType]) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == name && f.t.as_fun(code).is_some_and(|ft| ft.args[..] == args[..])
        })
        .map(|f| f.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one {name}{args:?}, found {}", hits.len()),
    }
}

fn inv_fns(code: &Bytecode, t: &T, m: &Consume) -> Result<InvFns> {
    let cb_t = fun_t(code, code.functions[m.use_list_fi].findex)?.args[3];
    let (inv, st) = (m.inv_t, t.str_);
    Ok(InvFns {
        count: static_fn(code, "count", &[inv, st])?,
        count_wc: static_fn(code, "countWithChest", &[inv, st])?,
        has: static_fn(code, "hasItem", &[inv, st, m.ref_i32, m.null_i32])?,
        has_wc: static_fn(code, "hasItemWithChest", &[inv, st, m.ref_i32, m.null_i32])?,
        try_use: static_fn(code, "tryUse", &[inv, st, m.ref_i32, cb_t])?,
        use_: static_fn(code, "use", &[inv, st, t.bool_, m.ref_i32])?,
        cb_t,
    })
}

/// Op indices of `f`'s direct calls to `target`.
fn calls_to(f: &Function, target: RefFun) -> Vec<usize> {
    f.ops
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(call_target(op), Some((g, _)) if g == target))
        .map(|(i, _)| i)
        .collect()
}

/// `(fi, at)` of the single call of `class.name` to `own`; with no such call and a
/// call to `party` instead, the pass was already applied.
fn one_call(
    code: &Bytecode,
    class: &str,
    name: &str,
    own: RefFun,
    party: RefFun,
) -> Result<(usize, usize)> {
    let f = method(code, obj_type(code, class)?, name)?;
    let fi = fun_index(code, f.findex)?;
    match calls_to(f, own)[..] {
        [at] => Ok((fi, at)),
        [] if !calls_to(f, party).is_empty() => bail!("{class}.{name}: already applied"),
        ref v => bail!("{class}.{name}: {} calls to fn@{} (want 1)", v.len(), own.0),
    }
}

/// Calls of `f` to `target` whose item argument (`item_arg`) was just loaded from
/// the string global `g` (a `GetGlobal` into that register at most 4 ops before).
fn calls_with_item(f: &Function, target: RefFun, item_arg: usize, g: RefGlobal) -> Vec<usize> {
    calls_to(f, target)
        .into_iter()
        .filter(|&at| {
            let Some((_, args)) = call_target(&f.ops[at]) else {
                return false;
            };
            let r = args[item_arg];
            f.ops[at.saturating_sub(4)..at]
                .iter()
                .rev()
                .find_map(|op| match op {
                    Opcode::GetGlobal { dst, global } if *dst == r => Some(*global == g),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .collect()
}

/// The planned activity edits: `(fi, at, new callee)` swaps, and the `tryUse` / `use`
/// sites that get partyTryUse / partyUse (appended when applying).
struct Acts {
    swaps: Vec<(usize, usize, RefFun)>,
    try_use: (usize, usize),
    use_: (usize, usize),
}

fn plan_activities(code: &Bytecode, t: &T, f: &InvFns) -> Result<Acts> {
    const FISH: &str = "ui.win.FishingAction";
    const PICK: &str = "ui.win.LockPick";
    let mut swaps = vec![
        // fishing: hook counter, cast gate, hook kind choice
        one_call(code, FISH, "updateCounters", f.count, f.count_wc).map(|x| (x, f.count_wc))?,
        one_call(code, FISH, "canFishing", f.has, f.has_wc).map(|x| (x, f.has_wc))?,
        one_call(code, FISH, "setFishhook", f.has, f.has_wc).map(|x| (x, f.has_wc))?,
        // lock picking: lockpick counter, the count deciding use / last-pick break
        one_call(code, PICK, "updateLockPickCount", f.count, f.count_wc)
            .map(|x| (x, f.count_wc))?,
        one_call(code, PICK, "startState", f.count, f.count_wc).map(|x| (x, f.count_wc))?,
    ]
    .into_iter()
    .map(|((fi, at), g)| (fi, at, g))
    .collect::<Vec<_>>();
    let try_use = one_call(code, FISH, "startState", f.try_use, f.try_use)?;
    let use_ = one_call(code, PICK, "startState", f.use_, f.use_)?;
    let lp = string_global(code, t.str_, "LockPick")?;
    // Chest.tryUnlock's lockpick closure: "no lockpick -> sound, stop" tests
    let tu = method(code, obj_type(code, "ent.p.Chest")?, "tryUnlock")?;
    let mut picks = vec![];
    for op in &tu.ops {
        if let Opcode::InstanceClosure { fun, .. } | Opcode::StaticClosure { fun, .. } = op {
            let ci = fun_index(code, *fun)?;
            let c = &code.functions[ci];
            let own = calls_with_item(c, f.has, 1, lp);
            if !own.is_empty() {
                picks.push((ci, own));
            } else if !calls_with_item(c, f.has_wc, 1, lp).is_empty() {
                bail!("Chest.tryUnlock: already applied");
            }
        }
    }
    let [(ci, ref own)] = picks[..] else {
        bail!(
            "Chest.tryUnlock: {} lockpick closures (want 1)",
            picks.len()
        );
    };
    if own.len() != 2 {
        bail!("Chest.tryUnlock: {} lockpick tests (want 2)", own.len());
    }
    swaps.extend(own.iter().map(|&at| (ci, at, f.has_wc)));
    // crime cave HUD lockpick counter
    let cc = method(code, obj_type(code, "world.CrimeCaveContent")?, "update")?;
    let ci = fun_index(code, cc.findex)?;
    match calls_with_item(cc, f.count, 1, lp)[..] {
        [at] => swaps.push((ci, at, f.count_wc)),
        ref v => bail!(
            "CrimeCaveContent.update: {} lockpick counts (want 1)",
            v.len()
        ),
    }
    Ok(Acts {
        swaps,
        try_use,
        use_,
    })
}

/// `partyTryUse(inv, item, count, cb)` (PlayerInventory.tryUse's signature) or
/// `partyUse(inv, item, stolenFirst, count)` (PlayerInventory.use's): the vanilla
/// call when `inv` alone holds `count` (default 1), else
/// `useList(inv, [{item, count, quality: null}], true, cb or noop)`, which pays
/// own -> chest -> boat -> other players (partyPrepare) on the host, or RPCs it
/// there from a client.
fn add_party_use(
    code: &mut Bytecode,
    t: &T,
    m: &Consume,
    f: &InvFns,
    with_cb: bool,
    noop: RefFun,
) -> Result<RefFun> {
    let args = if with_cb {
        vec![m.inv_t, t.str_, m.ref_i32, f.cb_t]
    } else {
        vec![m.inv_t, t.str_, t.bool_, m.ref_i32]
    };
    let cnt = Reg(if with_cb { 2 } else { 3 });
    let mut g = Regs(args.clone());
    let n = g.r(t.i32_);
    let own = g.r(t.i32_);
    let v = g.r(t.void);
    let ty = g.r(m.type_t);
    let arr = g.r(m.arr_t);
    let list = g.r(m.list_t);
    let e = g.r(m.entry_t);
    let q = g.r(m.null_i32);
    let k = g.r(t.i32_);
    let b = g.r(t.bool_);
    let cb = if with_cb { Reg(3) } else { g.r(f.cb_t) };
    let one = int_const(code, 1);
    let zero = int_const(code, 0);
    let use_list = code.functions[m.use_list_fi].findex;
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNotNull {
            reg: cnt,
            offset: 0,
        },
        "deref",
    );
    a.op(Opcode::Int { dst: n, ptr: one });
    a.jmp(Opcode::JAlways { offset: 0 }, "have");
    a.label("deref");
    a.op(Opcode::Unref { dst: n, src: cnt });
    a.label("have");
    a.op(Opcode::Call2 {
        dst: own,
        fun: f.count,
        arg0: Reg(0),
        arg1: Reg(1),
    });
    a.jmp(
        Opcode::JSLt {
            a: own,
            b: n,
            offset: 0,
        },
        "party",
    );
    a.op(Opcode::Call4 {
        dst: v,
        fun: if with_cb { f.try_use } else { f.use_ },
        arg0: Reg(0),
        arg1: Reg(1),
        arg2: Reg(2),
        arg3: Reg(3),
    });
    a.op(Opcode::Ret { ret: v });
    a.label("party");
    a.op(Opcode::Type {
        dst: ty,
        ty: m.entry_t,
    });
    a.op(Opcode::Int { dst: k, ptr: zero });
    a.op(Opcode::Call2 {
        dst: arr,
        fun: m.alloc,
        arg0: ty,
        arg1: k,
    });
    a.op(Opcode::Call1 {
        dst: list,
        fun: m.wrap,
        arg0: arr,
    });
    a.op(Opcode::New { dst: e });
    a.op(Opcode::SetField {
        obj: e,
        field: m.entry_item,
        src: Reg(1),
    });
    a.op(Opcode::SetField {
        obj: e,
        field: m.entry_count,
        src: n,
    });
    a.op(Opcode::Null { dst: q });
    a.op(Opcode::SetField {
        obj: e,
        field: m.entry_quality,
        src: q,
    });
    a.op(Opcode::Call2 {
        dst: k,
        fun: m.push,
        arg0: list,
        arg1: e,
    });
    if !with_cb {
        a.op(Opcode::StaticClosure { dst: cb, fun: noop });
    }
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call4 {
        dst: v,
        fun: use_list,
        arg0: Reg(0),
        arg1: list,
        arg2: b,
        arg3: cb,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, args, t.void, g.0, a.finish(), m.dbg_file)
}

/// `(Bool) -> Void` that does nothing (useList's callback for partyUse).
fn add_noop_cb(code: &mut Bytecode, t: &T, f: &InvFns, dbg_file: usize) -> Result<RefFun> {
    let ret = Reg(1);
    let noop = push_fn(
        code,
        vec![t.bool_],
        t.void,
        vec![t.bool_, t.void],
        vec![Opcode::Ret { ret }],
        dbg_file,
    )?;
    // closures must carry the callback's own function type
    let fi = fun_index(code, noop)?;
    code.functions[fi].t = f.cb_t;
    code.types.pop();
    Ok(noop)
}

/// Fishing hooks and lockpicks are counted, required and consumed from the party
/// (own inventory + camp chest + boat chest + the other players), like the with-chest
/// costs above. Fishing: the hook counter (`updateCounters`), the cast gate
/// (`canFishing`) and the hook kind (`setFishhook`) use countWithChest /
/// hasItemWithChest; the host's hook wear-out `tryUse` becomes partyTryUse. Lock
/// picking: the "pick the lock" gate in Chest.tryUnlock, the lockpick counters
/// (LockPick, crime cave HUD) and the count deciding the use use the party; the
/// per-attempt `use` becomes partyUse. Own inventory first (vanilla call), the rest
/// through useList(checkChest = true) + partyPrepare. Skipped (logged) on mismatch.
pub(crate) fn patch_party_activities(code: &mut Bytecode) {
    let plan = || -> Result<(T, Consume, InvFns, Acts)> {
        let t = types(code)?;
        let m = consume_refs(code, &t)?;
        let f = inv_fns(code, &t, &m)?;
        let acts = plan_activities(code, &t, &f)?;
        Ok((t, m, f, acts))
    };
    let (t, m, f, acts) = match plan() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("party activities skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    let added = (|| -> Result<(RefFun, RefFun)> {
        let noop = add_noop_cb(code, &t, &f, m.dbg_file)?;
        let tu = add_party_use(code, &t, &m, &f, true, noop)?;
        let us = add_party_use(code, &t, &m, &f, false, noop)?;
        Ok((tu, us))
    })();
    let (tu, us) = match added {
        Ok(x) => x,
        Err(e) => {
            snap.restore(code);
            eprintln!("party activities skipped: {e:#}");
            return;
        }
    };
    let edits = acts.swaps.iter().copied().chain([
        (acts.try_use.0, acts.try_use.1, tu),
        (acts.use_.0, acts.use_.1, us),
    ]);
    for (fi, at, g) in edits {
        match &mut code.functions[fi].ops[at] {
            Opcode::Call2 { fun, .. } | Opcode::Call4 { fun, .. } => *fun = g,
            _ => unreachable!("planned call site"),
        }
    }
    eprintln!(
        "patched party activities: fishing hooks and lockpicks counted and used from the party ({} call sites, partyTryUse fn@{} partyUse fn@{})",
        acts.swaps.len() + 2,
        tu.0,
        us.0
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Patches a copy of the installed game (skipped when absent): useList only gains
    /// its two-op prologue, partyPrepare is well typed, the dialog functions change
    /// only at the planned ops, nothing else changes, the image round-trips, and a
    /// second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let t = types(&orig).expect("types");
        let m = plan_consume(&orig, &t).expect("plan consume");
        let ul = orig.functions[m.use_list_fi].findex;
        let d = plan_dialog(&orig, ul).expect("plan dialog");
        assert_eq!(d.edits.len(), 16);
        assert_eq!(d.ints.len(), 3);
        let edited: Vec<(usize, usize)> = d
            .edits
            .iter()
            .map(|e| (e.fi, e.at))
            .chain(d.ints.iter().map(|(fi, at, _)| (*fi, *at)))
            .collect();
        let mut code = read(&image);
        patch_party_inventory(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == m.use_list_fi {
                shifted(a, b, 0, 2);
                assert!(matches!(
                    b.ops[0],
                    Opcode::JFalse {
                        cond: Reg(2),
                        offset: 1
                    }
                ));
                assert!(
                    matches!(b.ops[1], Opcode::Call2 { dst: Reg(1), arg0: Reg(0), arg1: Reg(1), fun } if fun.0 == nf_findex(&back, nf))
                );
                check_types(&back, b, 0..2);
                continue;
            }
            assert_eq!(a.regs, b.regs, "fn#{i}");
            assert_eq!(a.ops.len(), b.ops.len(), "fn#{i}");
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                let same = format!("{x:?}") == format!("{y:?}");
                assert_eq!(
                    same,
                    !edited.contains(&(i, k)),
                    "fn#{i} op {k}: {x:?} -> {y:?}"
                );
            }
            if edited.iter().any(|(fi, _)| *fi == i) {
                check_types(&back, b, 0..b.ops.len());
            }
        }
        let prep = &back.functions[nf];
        check_types(&back, prep, 0..prep.ops.len());
        check_flow(prep);
        // 256 -> 260 and 2 -> 6 flag constants
        for (fi, at, v) in &d.ints {
            let Opcode::Int { ptr, .. } = back.functions[*fi].ops[*at] else {
                panic!("not an Int");
            };
            assert_eq!(back.ints[ptr.0], *v);
        }

        let mut again = read(&patched);
        assert!(plan_consume(&again, &t).is_err());
        assert!(plan_dialog(&again, ul).is_err());
        patch_party_inventory(&mut again);
        assert!(write(&again) == patched);
    }

    fn nf_findex(code: &Bytecode, i: usize) -> usize {
        code.functions[i].findex.0
    }

    /// hasItemWithChest / countWithChest: only their `Int 2` flag becomes 6; idempotent.
    #[test]
    fn patches_counts() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut code = read(&image);
        patch_party_counts(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        let mut changed = 0;
        for (a, b) in orig.functions.iter().zip(&back.functions) {
            assert_eq!(a.ops.len(), b.ops.len());
            for (x, y) in a.ops.iter().zip(&b.ops) {
                if format!("{x:?}") != format!("{y:?}") {
                    let (Opcode::Int { dst: d1, ptr: p1 }, Opcode::Int { dst: d2, ptr: p2 }) =
                        (x, y)
                    else {
                        panic!("unexpected edit {x:?} -> {y:?}");
                    };
                    assert_eq!(d1, d2);
                    assert_eq!((orig.ints[p1.0], back.ints[p2.0]), (2, 6));
                    assert!(["hasItemWithChest", "countWithChest"].contains(&s(&orig, a.name)));
                    changed += 1;
                }
            }
        }
        assert_eq!(changed, 2);
        let mut again = read(&patched);
        patch_party_counts(&mut again);
        assert!(write(&again) == patched);
    }

    /// Heal panel / injury tooltip `2 -> 6` and Button.syncText `256 -> 260` only; idempotent.
    #[test]
    fn patches_lists() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut code = read(&image);
        patch_party_lists(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        let mut changed = vec![];
        for (a, b) in orig.functions.iter().zip(&back.functions) {
            assert_eq!(a.ops.len(), b.ops.len());
            for (x, y) in a.ops.iter().zip(&b.ops) {
                if format!("{x:?}") != format!("{y:?}") {
                    let (Opcode::Int { dst: d1, ptr: p1 }, Opcode::Int { dst: d2, ptr: p2 }) =
                        (x, y)
                    else {
                        panic!("unexpected edit {x:?} -> {y:?}");
                    };
                    assert_eq!(d1, d2);
                    changed.push((
                        s(&orig, a.name).to_string(),
                        orig.ints[p1.0],
                        back.ints[p2.0],
                    ));
                }
            }
        }
        changed.sort();
        assert_eq!(
            changed,
            [
                ("getTipContent".to_string(), 2, 6),
                ("init".to_string(), 2, 6),
                ("syncText".to_string(), 256, 260),
            ]
        );
        let mut again = read(&patched);
        assert!(plan_iter_flags(&again, "ui.win.InjuryHealPanel", "init").is_err());
        assert!(plan_button_flags(&again).is_err());
        patch_party_lists(&mut again);
        assert!(write(&again) == patched);
    }

    /// ItemsRecipe.hasItem: only its own-inventory hasItem call becomes
    /// hasItemWithChest; idempotent.
    #[test]
    fn patches_recipes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let (fi, at, wc) = plan_recipe_has(&orig).expect("plan");
        let mut code = read(&image);
        patch_party_recipes(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len());
        let mut changed = vec![];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(a.regs, b.regs);
            assert_eq!(a.ops.len(), b.ops.len());
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                if format!("{x:?}") != format!("{y:?}") {
                    changed.push((i, k));
                }
            }
        }
        assert_eq!(changed, [(fi, at)]);
        assert!(matches!(back.functions[fi].ops[at], Opcode::Call4 { fun, .. } if fun == wc));
        check_types(&back, &back.functions[fi], 0..back.functions[fi].ops.len());
        let mut again = read(&patched);
        assert!(plan_recipe_has(&again).is_err());
        patch_party_recipes(&mut again);
        assert!(write(&again) == patched);
    }

    /// Activities: only the planned call sites change (same-signature callees), three
    /// well-typed functions are appended (noop, partyTryUse, partyUse), the image
    /// round-trips, a second pass changes nothing; also after the useList prologue.
    #[test]
    fn patches_activities() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let t = types(&orig).expect("types");
        let m = consume_refs(&orig, &t).expect("refs");
        let f = inv_fns(&orig, &t, &m).expect("inv fns");
        let acts = plan_activities(&orig, &t, &f).expect("plan");
        assert_eq!(acts.swaps.len(), 8);
        let mut planned: Vec<(usize, usize)> = acts
            .swaps
            .iter()
            .map(|(fi, at, _)| (*fi, *at))
            .chain([acts.try_use, acts.use_])
            .collect();
        planned.sort();
        let mut code = read(&image);
        patch_party_activities(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 3);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        let mut changed = vec![];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(a.regs, b.regs);
            assert_eq!(a.ops.len(), b.ops.len());
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                if format!("{x:?}") != format!("{y:?}") {
                    changed.push((i, k));
                }
            }
            if planned.iter().any(|(fi, _)| *fi == i) {
                check_types(&back, b, 0..b.ops.len());
            }
        }
        assert_eq!(changed, planned);
        let (tu, us) = (back.functions[nf + 1].findex, back.functions[nf + 2].findex);
        let at = |(fi, k): (usize, usize)| call_target(&back.functions[fi].ops[k]).unwrap().0;
        assert_eq!(at(acts.try_use), tu);
        assert_eq!(at(acts.use_), us);
        assert_eq!(fun_t(&back, tu).unwrap(), fun_t(&orig, f.try_use).unwrap());
        assert_eq!(fun_t(&back, us).unwrap(), fun_t(&orig, f.use_).unwrap());
        assert_eq!(back.functions[nf].t, f.cb_t);
        for g in &back.functions[nf..] {
            check_types(&back, g, 0..g.ops.len());
            check_flow(g);
        }
        let mut again = read(&patched);
        assert!(plan_activities(&again, &t, &f).is_err());
        patch_party_activities(&mut again);
        assert!(write(&again) == patched);

        // after the consume pass (useList prologue): same plan, same edits
        let mut code = read(&image);
        patch_party_inventory(&mut code);
        let m2 = consume_refs(&code, &t).expect("refs after consume");
        let f2 = inv_fns(&code, &t, &m2).expect("inv fns after consume");
        let acts2 = plan_activities(&code, &t, &f2).expect("plan after consume");
        assert_eq!(acts2.swaps, acts.swaps);
        patch_party_activities(&mut code);
        let _ = read(&write(&code));
    }
}
