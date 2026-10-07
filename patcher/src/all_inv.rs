// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op "all inventories" button: a bottom-bar button (multiplayer only) that
// opens one inventory panel per OTHER connected player, side by side at the
// left edge of the screen (saved drag positions win). Full access: any player can take from and give to
// any other player's inventory.
//
// Authority (vanilla): a player's st.Inventory is owned by that player's client
// (`hasAuthority` = owner == Game.me); the owner mutates it and field-syncs it to
// the host. The st.Inventory RPCs are owner-routed: the host forwards a call to
// the owner's client only, which runs it once. A transfer that removes an item
// from player B's inventory therefore has to run ON B. This pass uses exactly one
// path for every cross-player move: SlotOperation.MoveTo executed by the owner of
// the source inventory (consume + `target.addItem` on one thread; the add to a
// foreign target is the owner-routed netAddItem RPC). No hand (Pick/Drop/Swap),
// no Remove + local add.
//
//   N. st.Inventory.networkAllow: the dead `networkGetName` block (ops 41-50)
//      becomes, for an owned inventory, "RPC permission (mode 0 host receive,
//      mode 6 caller pre-check) is granted to any BasePlayer client for the
//      networkOperation and netAddItem RPCs". Modes 1/2 (routing: who executes)
//      and 3-5 (field sync, registration) stay owner-only, so a call still runs
//      once, on the owner. Spectator / non-player clients (no BasePlayer) and
//      the other RPCs (use, tryUse, cancelPick, setStackCount, ...) are refused
//      as before. Same op count, no retarget.
//   S. getSlotApi's `networkOperation` closure: without authority vanilla turns a
//      MoveTo into Remove(n) on the source plus a local add when the result comes
//      back. The result of an owner-routed RPC never comes back (loss), so for an
//      owned source inventory (`owner != null`) the MoveTo is sent as is; the
//      owner moves the stack itself. Containers (owner == null: chests, loot)
//      keep the vanilla conversion.
//   U. UI (local only):
//      - WorldButtonsBar constructor (end): `mpAllInvButton(this)` adds a
//        "button" flow with an icon (ICON_ID, the inventory chest, with a
//        BADGE_ID companions badge on it; tooltip TIP) whose click is
//        `mpAllInvToggle(bar)`; visible only in multiplayer, enabled
//        like btInventory (`mpAllInvTick`). It is moved right after
//        btInventory's flow in the bar's row (`addChildAt`): appended, it
//        would follow the pause / settings icons (game-option-icons, created
//        last by the constructor).
//      - Toggle: in GameUI's dom (HUD root) an absolute horizontal flow at the
//        left edge, past the HUD docked there (BOX_ATTRS, BOX_MARGIN) gets, per other
//        connected player j, a panel shaped like the vanilla #inventory one
//        (`element#inventory` > `.title` (getName: nickname in the player
//        colour; Close icon) + (styled inline: PANEL_ATTRS / TITLE_ATTRS / ...,
//        the game-inventory CSS does not reach the HUD root; the title is
//        the drag handle) +
//        `inventory-content` with `new ui.comp.Inventory(FoundItems,
//        p.inventory, 6, content)`), made draggable with
//        `mpDragPanel(panel, "AllInv#" + j)` (window_drag; positions kept).
//      - GameUI.update (op 0): `mpAllInvTick(this)`: button visibility /
//        enable; panels close on battle, loading, arena, solo, HUD rebuild; a
//        panel whose player disconnected is removed.
//      - Take: the panels are FoundItems grids, so a right click (shift: amount)
//        is the vanilla loot take `MoveTo(getActionPlayer().inventory, n)`; with
//        S it is sent raw and B's client moves it.
//      - ItemSlot.allowPick / allowDrop / doPick / drop (op 0): refused on a
//        foreign slot (`mpAllInvForeign`: FoundItems slot inside a
//        ui.comp.Inventory whose inventory has an owner that is not Game.me), so
//        no hand drag / drop / swap on another player's inventory.
//      - Give: ItemSlot.onRightClick (op 0) `if (mpAllInvGive(this)) return;`:
//        while panels are open, ctrl + right click on a slot of the own
//        inventory moves the whole stack (ctrl + shift: amount box) to the
//        target panel's inventory through the slot API (own inventory: local
//        MoveTo, the add is netAddItem to the target's owner). Target = the
//        panel last hovered (ItemSlot.getTipContent op 0: `mpAllInvHover`) or
//        right-clicked, else the first open panel; only a connected player.
//      - Equip from another inventory = take, then equip as usual.
// All players need this build: an unpatched owner refuses the forwarded calls
// (a give would then lose the item on the giver's side).
//
// Every appended function that vanilla calls runs under a trap; an exception is
// logged ("mp: allinv: ...", LOG_CAP lines) and the vanilla path continues.
// Validated before editing; on any mismatch the whole pass is skipped (logged).

use super::asm::{push_fn, string_ref, Asm, Regs, Snap};
use super::chest_buttons::{plan_icon_block, IconBlock};
use super::diag::{index_of, static_fn};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefEnumConstruct, RefGlobal, RefString, ValBool};

const LOG_CAP: i32 = 20;
const S_ERR: &str = "mp: allinv: ";
pub(crate) const KEY_PREFIX: &str = "AllInv#";
/// The bottom-bar inventory icon (TXT_OW_UI_ICONS_48PX 1,3) ...
const ICON_ID: &str = "Inventory";
/// ... with the companions group (TXT_OW_UI_ICONS_48PX 11,2) as a badge.
const BADGE_ID: &str = "Companions";
const BADGE_ATTRS: &[(&str, &str)] = &[
    ("class", "mpAllInvBadge"),
    ("position", "absolute"),
    ("align", "right bottom"),
    ("offset", "6 4"),
    ("scale", "0.5"),
    ("networkable", "false"),
];
/// Panels open at the screen's left edge, bottom aligned like the own inventory.
const BOX_ATTRS: &[(&str, &str)] = &[
    ("layout", "horizontal"),
    ("hspacing", "8"),
    ("content-valign", "bottom"),
    ("position", "absolute"),
    ("align", "left bottom"),
];
/// The box's offset: x = BOX_MARGIN past the right edge of the HUD docked at
/// the left edge (GameUI root children that are visible flows starting within
/// LEFT_X of it and narrower than LEFT_W: #gameInfo with the place name and the
/// craft / units rows, ...), so the panels open beside it; y = BOX_Y.
const BOX_MARGIN: i32 = 10;
const BOX_Y: &str = " -70";
const LEFT_X: f64 = 50.0;
const LEFT_W: f64 = 600.0;
const TIP: &str = "Co-op inventories";
// The panels live on the HUD root, outside game-inventory, so none of the
// `game-inventory #inventory ...` rules (style.css) reach them: the look of
// the vanilla inventory panel is given inline (domkit parses attributes as
// CSS values).
const PANEL_ATTRS: &[(&str, &str)] = &[
    ("class", "inventory mpAllInvPanel"),
    ("id", "inventory"),
    ("layout", "vertical"),
    ("padding", "0 0 5 5"),
    ("cursor", "default"),
    ("background", "url(\"ui/elements/InventoryBg.png\") 50 50"),
];
/// The header row (the drag handle): nickname left, close button right.
const TITLE_ATTRS: &[(&str, &str)] = &[
    ("class", "title"),
    ("height", "50"),
    ("min-width", "220"),
    ("padding-left", "20"),
    ("padding-right", "44"),
    ("content-valign", "middle"),
];
const NAME_ATTRS: &[(&str, &str)] = &[
    ("font", "'ui/fonts/eb_garamond_medium.fnt' 19 multi 0.5 0.5"),
    ("color", "#969696"),
];
/// `.window icon.windowClose` + the inventory panel's offset.
const CLOSE_ATTRS: &[(&str, &str)] = &[
    ("class", "windowClose"),
    ("networkable", "false"),
    ("position", "absolute"),
    ("align", "top right"),
    ("offset", "-10 15"),
    ("scale", "0.5"),
    ("cursor", "button"),
];
const ROWS: i32 = 6;
/// Ancestors searched from a slot for its ui.comp.Inventory.
const DEPTH: i32 = 8;
const N_BUTTON: &str = "mpAllInvButton";
const N_TICK: &str = "mpAllInvTick";
const N_TOGGLE: &str = "mpAllInvToggle";
const N_FOREIGN: &str = "mpAllInvForeign";
const N_GIVE: &str = "mpAllInvGive";
const N_HOVER: &str = "mpAllInvHover";

// ---------- N: st.Inventory.networkAllow ----------

pub(crate) struct NetPlan {
    pub(crate) fi: usize,
    /// RPC ids of networkOperation / netAddItem.
    pub(crate) id_op: i32,
    pub(crate) id_add: i32,
}

/// The RPC id a generated hxbit caller stub passes to `networkAllow(mode, id, ..)`.
fn rpc_id(code: &Bytecode, stub: RefFun, allow: RefFun) -> Result<i32> {
    let f = &code.functions[index_of(code, stub)?];
    let int_at = |i: usize, r: Reg| {
        f.ops[..i].iter().rev().find_map(|o| match o {
            Opcode::Int { dst, ptr } if *dst == r => Some(code.ints[ptr.0]),
            _ => None,
        })
    };
    let ids: Vec<i32> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call4 { fun, arg2, .. } if *fun == allow => int_at(i, *arg2),
            _ => None,
        })
        .collect();
    match ids[..] {
        [id, ..] if ids.iter().all(|x| *x == id) => Ok(id),
        _ => bail!("fn@{}: no single networkAllow RPC id", stub.0),
    }
}

pub(crate) fn net_plan(code: &Bytecode) -> Result<NetPlan> {
    let inv_t = obj_type(code, "st.Inventory")?;
    let allow = proto(code, inv_t, "networkAllow")?;
    let fi = index_of(code, allow)?;
    let f = &code.functions[fi];
    let (owner, _) = field(code, inv_t, "owner")?;
    let id_op = rpc_id(code, proto(code, inv_t, "networkOperation")?, allow)?;
    let id_add = rpc_id(code, proto(code, inv_t, "netAddItem")?, allow)?;
    if id_op == id_add {
        bail!("networkOperation and netAddItem share an RPC id");
    }
    if f.ops.len() != 56 {
        bail!("networkAllow: {} ops (want 56)", f.ops.len());
    }
    if matches!(f.ops[50], Opcode::JAlways { .. }) && matches!(f.ops[45], Opcode::JNull { .. }) {
        bail!("already applied");
    }
    let r = |i: u32| Reg(i);
    let ok = matches!(f.ops[0], Opcode::SafeCast { dst, src } if dst == r(4) && src == r(3))
        && matches!(f.ops[31], Opcode::GetThis { field, .. } if field == owner)
        && matches!(f.ops[32], Opcode::JNotEq { b, .. } if b == r(4))
        && matches!(f.ops[36], Opcode::Int { dst, .. } if dst == r(6))
        && matches!(f.ops[41], Opcode::Int { dst, .. } if dst == r(6))
        && matches!(f.ops[50], Opcode::Call3 { fun, arg0, arg1, .. }
            if arg0 == r(0) && arg1 == r(2)
                && code.functions.iter().find(|g| g.findex == fun)
                    .is_some_and(|g| s(code, g.name) == "networkGetName"))
        && matches!(f.ops[51], Opcode::JFalse { cond, .. } if cond == r(9))
        && matches!(
            f.ops[52],
            Opcode::Bool {
                value: ValBool(true),
                ..
            }
        )
        && matches!(f.ops[55], Opcode::Ret { .. });
    if !ok {
        bail!("networkAllow: unexpected op shape");
    }
    // No other op jumps into the replaced block.
    for i in (0..41).chain(51..56) {
        if jump_targets(f, i).iter().any(|t| (42..=50).contains(t)) {
            bail!("networkAllow: op {i} jumps into the dead block");
        }
    }
    let i32_t = f.regs[6];
    if !matches!(code.types[i32_t.0], Type::I32)
        || f.regs[1] != i32_t
        || f.regs[2] != i32_t
        || s(code, obj(code, f.regs[4])?.name) != "ent.BasePlayer"
    {
        bail!("networkAllow: unexpected registers");
    }
    Ok(NetPlan { fi, id_op, id_add })
}

fn net_apply(code: &mut Bytecode, p: &NetPlan) {
    let k = |code: &mut Bytecode, v| int_const(code, v);
    let (c0, c6, cop, cadd) = (k(code, 0), k(code, 6), k(code, p.id_op), k(code, p.id_add));
    let (mode, id, client, t) = (Reg(1), Reg(2), Reg(4), Reg(6));
    let f = &mut code.functions[p.fi];
    let new = [
        Opcode::Int { dst: t, ptr: c0 },
        Opcode::JEq {
            a: mode,
            b: t,
            offset: 2,
        }, // 42 -> 45
        Opcode::Int { dst: t, ptr: c6 },
        Opcode::JNotEq {
            a: mode,
            b: t,
            offset: 6,
        }, // 44 -> 51
        Opcode::JNull {
            reg: client,
            offset: 5,
        }, // 45 -> 51
        Opcode::Int { dst: t, ptr: cop },
        Opcode::JEq {
            a: id,
            b: t,
            offset: 4,
        }, // 47 -> 52
        Opcode::Int { dst: t, ptr: cadd },
        Opcode::JEq {
            a: id,
            b: t,
            offset: 2,
        }, // 49 -> 52
        Opcode::JAlways { offset: 0 }, // 50 -> 51
    ];
    for (i, op) in new.into_iter().enumerate() {
        f.ops[41 + i] = op;
    }
    eprintln!(
        "patched all inventories fn@{}: networkAllow lets any player call networkOperation (id {}) / netAddItem (id {}) on an owned inventory",
        f.findex.0, p.id_op, p.id_add
    );
}

// ---------- S: getSlotApi's networkOperation closure ----------

pub(crate) struct ApiPlan {
    pub(crate) fi: usize,
    /// First op after `if (op.index != MoveTo) goto raw`.
    pub(crate) at: usize,
    /// The raw-send block (vanilla's non-MoveTo branch).
    pub(crate) raw: usize,
    ctx: (RefEnumConstruct, RefField),
    inv_t: RefType,
    bp_t: RefType,
    owner: RefField,
}

pub(crate) fn api_plan(code: &Bytecode) -> Result<ApiPlan> {
    let inv_t = obj_type(code, "st.Inventory")?;
    let (owner, bp_t) = field(code, inv_t, "owner")?;
    let net_op = proto(code, inv_t, "networkOperation")?;
    let nt = code.functions[index_of(code, net_op)?]
        .t
        .as_fun(code)
        .context("networkOperation type")?
        .clone();
    let op_t = *nt.args.get(2).context("networkOperation args")?;
    let Type::Enum { constructs, .. } = &code.types[op_t.0] else {
        bail!("slot operation is not an enum");
    };
    let move_i = constructs
        .iter()
        .position(|c| s(code, c.name) == "MoveTo")
        .context("SlotOperation.MoveTo not found")?;
    let has_auth = proto(code, inv_t, "hasAuthority")?;
    let get_api = method(code, inv_t, "getSlotApi")?;
    let mut hits = vec![];
    for o in &get_api.ops {
        let Opcode::InstanceClosure { fun, .. } = o else {
            continue;
        };
        let g = &code.functions[index_of(code, *fun)?];
        let gt = g.t.as_fun(code).context("closure type")?;
        if gt.args.len() == 4
            && gt.args[1] == op_t
            && g.ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == has_auth))
        {
            hits.push(index_of(code, *fun)?);
        }
    }
    let [fi] = hits[..] else {
        bail!(
            "getSlotApi: {} networkOperation closures (want 1)",
            hits.len()
        );
    };
    let f = &code.functions[fi];
    let mut site = None;
    for i in 0..f.ops.len().saturating_sub(3) {
        let (
            Opcode::EnumIndex { dst: d, value },
            Opcode::Int { dst: k, ptr },
            Opcode::JNotEq { a, b, offset },
        ) = (&f.ops[i], &f.ops[i + 1], &f.ops[i + 2])
        else {
            continue;
        };
        if *value == Reg(1) && a == d && b == k && code.ints[ptr.0] == move_i as i32 {
            site = Some((i + 3, (i as i64 + 3 + *offset as i64) as usize));
        }
    }
    let (at, raw) = site.context("networkOperation closure: no MoveTo test")?;
    match &f.ops[at] {
        Opcode::EnumField {
            value: Reg(1),
            construct,
            field: RefField(0),
            ..
        } if construct.0 == move_i => {}
        Opcode::EnumField { value: Reg(0), .. } => bail!("already applied"),
        _ => bail!("networkOperation closure: MoveTo branch shape"),
    }
    if raw <= at || raw >= f.ops.len() {
        bail!("networkOperation closure: raw branch out of range");
    }
    // The raw branch sends the operation itself: networkOperation(inv, i, op, .., cb).
    let sends_raw = f.ops[raw..].iter().take(16).any(|o| {
        matches!(o, Opcode::CallN { fun, args, .. } if *fun == net_op && args.get(2) == Some(&Reg(1)))
    });
    if !sends_raw {
        bail!("networkOperation closure: raw branch does not send the operation");
    }
    let ctx = f
        .ops
        .iter()
        .find_map(|o| match o {
            Opcode::EnumField {
                dst,
                value: Reg(0),
                construct,
                field,
            } if f.regs[dst.0 as usize] == inv_t => Some((*construct, *field)),
            _ => None,
        })
        .context("networkOperation closure: no captured inventory")?;
    Ok(ApiPlan {
        fi,
        at,
        raw,
        ctx,
        inv_t,
        bp_t,
        owner,
    })
}

fn api_apply(code: &mut Bytecode, p: &ApiPlan) {
    let f = &mut code.functions[p.fi];
    f.regs.push(p.inv_t);
    let inv = Reg((f.regs.len() - 1) as u32);
    f.regs.push(p.bp_t);
    let own = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::EnumField {
                dst: inv,
                value: Reg(0),
                construct: p.ctx.0,
                field: p.ctx.1,
            },
            Opcode::NullCheck { reg: inv },
            Opcode::Field {
                dst: own,
                obj: inv,
                field: p.owner,
            },
            // inserted op at+3 -> raw + 4
            Opcode::JNotNull {
                reg: own,
                offset: (p.raw - p.at) as i32,
            },
        ],
    );
    eprintln!(
        "patched all inventories fn@{} op {}: MoveTo on an owned inventory without authority is sent to its owner as is",
        f.findex.0, p.at
    );
}

// ---------- U: UI, slot gates, give ----------

pub(crate) struct UiPlan {
    dbg_file: usize,
    // types
    void_t: RefType,
    bool_t: RefType,
    i32_t: RefType,
    f64_t: RefType,
    dyn_t: RefType,
    dynobj_t: RefType,
    str_t: RefType,
    type_t: RefType,
    ref_bool_t: RefType,
    arr_dyn_t: RefType,
    obj_t: RefType,
    flow_t: RefType,
    arr_t: RefType,
    raw_t: RefType,
    null_i32_t: RefType,
    game_t: RefType,
    me_t: RefType,
    state_t: RefType,
    players_t: RefType,
    pad_t: RefType,
    ui_t: RefType,
    gi_t: RefType,
    bar_t: RefType,
    icon_t: RefType,
    text_t: RefType,
    ic_t: RefType,
    uinv_t: RefType,
    inv_t: RefType,
    bp_t: RefType,
    slot_t: RefType,
    mode_t: RefType,
    battle_t: RefType,
    item_v_t: RefType,
    item_nv_t: RefType,
    item_t: RefType,
    api_t: RefType,
    api_v_t: RefType,
    op_t: RefType,
    bool_cb_t: RefType,
    int_cb_t: RefType,
    // fields
    parent: RefField,
    children: RefField,
    visible: RefField,
    obj_x: RefField,
    flow_calc_w: RefField,
    dom: RefField,
    arr_len: RefField,
    arr_raw: RefField,
    game_inst: RefField,
    game_me: RefField,
    game_state: RefField,
    game_ui: RefField,
    game_battle: RefField,
    game_loading: RefField,
    state_players: RefField,
    players_arr: RefField,
    ui_game: RefField,
    ui_gi: RefField,
    ui_bar: RefField,
    bar_game: RefField,
    bar_bt: RefField,
    enable: RefField,
    title_tip: RefField,
    uinv_inv: RefField,
    inv_owner: RefField,
    bp_inv: RefField,
    bp_connected: RefField,
    slot_mode: RefField,
    slot_api: RefField,
    item_k: RefField,
    api_net_op: RefField,
    move_to: RefEnumConstruct,
    found: i32,
    // globals
    game_cls: RefGlobal,
    uinv_cls: RefGlobal,
    ic_cls: RefGlobal,
    found_g: RefGlobal,
    close_g: RefGlobal,
    // functions
    is_of_type: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    println: RefFun,
    remove: RefFun,
    child_index: RefFun,
    bar_add_at: RefFun,
    flow_set_visible: RefFun,
    set_enable: RefFun,
    set_tip: RefFun,
    set_text: RefFun,
    user_name: RefFun,
    is_multi: RefFun,
    inv_allowed: RefFun,
    get_inventory: RefFun,
    uinv_ctor: RefFun,
    ctrl_down: RefFun,
    is_down: RefFun,
    shift_key: i32,
    select_amount: RefFun,
    get_item: RefFun,
    get_locked: RefFun,
    get_count: RefFun,
    blk: IconBlock,
    // sites
    bar_ctor_fi: usize,
    bar_ret: usize,
    ui_update_fi: usize,
    allow_pick_fi: usize,
    allow_drop_fi: usize,
    do_pick_fi: usize,
    drop_fi: usize,
    right_click_fi: usize,
    tip_fi: usize,
}

fn class_global(code: &Bytecode, t: RefType) -> Result<RefGlobal> {
    let o = obj(code, t)?;
    // HL stores an object's class global 1-based (0 = none).
    let g = o
        .global
        .0
        .checked_sub(1)
        .with_context(|| format!("{}: no class global", s(code, o.name)))?;
    if g >= code.globals.len() {
        bail!("{}: class global out of range", s(code, o.name));
    }
    Ok(RefGlobal(g))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<TypeFun> {
    let t = match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => n.t,
        None => code.functions[index_of(code, f)?].t,
    };
    t.as_fun(code).cloned().context("not a function type")
}

fn want(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    let t = sig(code, f)?;
    if t.args != args || t.ret != ret {
        bail!("{what}: unexpected signature");
    }
    Ok(())
}

fn typed(code: &Bytecode, t: RefType, name: &str, want: RefType) -> Result<RefField> {
    let (f, ft) = field(code, t, name)?;
    if ft != want {
        bail!("field {name} has an unexpected type");
    }
    Ok(f)
}

/// `name` from the prototype of `t` or its nearest ancestor.
fn proto_up(code: &Bytecode, t: RefType, name: &str) -> Result<RefFun> {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if let Ok(f) = proto(code, c, name) {
            return Ok(f);
        }
        cur = obj(code, c)?.super_;
    }
    bail!("{name} not found on type {} or its parents", t.0)
}

fn find_named(code: &Bytecode, name: &str) -> Option<RefFun> {
    code.functions
        .iter()
        .find(|f| f.name != RefString(0) && s(code, f.name) == name)
        .map(|f| f.findex)
}

fn name_fn(code: &mut Bytecode, f: RefFun, name: &str) {
    let n = string_ref(code, name);
    let i = code.functions.iter().position(|x| x.findex == f).unwrap();
    code.functions[i].name = n;
}

/// The global vanilla initialises with the argument-less enum value `construct`
/// (the init code: `g = SafeCast($Enum.__evalues__[construct])`).
fn enum_value_global(code: &Bytecode, enum_t: RefType, construct: usize) -> Result<RefGlobal> {
    let mut hits = vec![];
    for f in &code.functions {
        for w in f.ops.windows(4) {
            if let [Opcode::Int { dst: k, ptr }, Opcode::GetArray { dst: x, index, .. }, Opcode::SafeCast { dst: y, src }, Opcode::SetGlobal { global, src: y2 }] =
                w
            {
                if code.ints[ptr.0] == construct as i32
                    && index == k
                    && src == x
                    && y2 == y
                    && code.globals[global.0] == enum_t
                    && !hits.contains(global)
                {
                    hits.push(*global);
                }
            }
        }
    }
    match hits[..] {
        [g] => Ok(g),
        _ => bail!("enum value global: {} candidates (want 1)", hits.len()),
    }
}

/// Insert position 0 must be a plain fall-through start.
fn plain_start(f: &Function, what: &str) -> Result<()> {
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
        bail!("{what}: op 0 is a jump target");
    }
    Ok(())
}

pub(crate) fn ui_plan(code: &Bytecode) -> Result<UiPlan> {
    if find_named(code, N_BUTTON).is_some() || ui_applied(code) {
        bail!("already applied");
    }
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let void_t = prim("void", |t| matches!(t, Type::Void))?;
    let bool_t = prim("bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim("i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim("dynamic", |t| matches!(t, Type::Dyn))?;
    let dynobj_t = prim("dynobj", |t| matches!(t, Type::DynObj))?;
    let type_t = prim("type", |t| matches!(t, Type::Type))?;
    let null_i32_t = prim_type(
        code,
        "null<i32>",
        |t| matches!(t, Type::Null(x) if *x == i32_t),
    )?;
    let str_t = obj_type(code, "String")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let (arr_raw, raw_t) = field(code, arr_t, "array")?;
    let arr_len = typed(code, arr_t, "length", i32_t)?;
    let (parent, pt) = field(code, obj_t, "parent")?;
    if pt != obj_t {
        bail!("h2d.Object.parent type");
    }
    let children = typed(code, obj_t, "children", arr_t)?;
    let visible = typed(code, obj_t, "visible", bool_t)?;
    let f64_t = field(code, obj_t, "x")?.1;
    if !matches!(code.types[f64_t.0], Type::F64) {
        bail!("h2d.Object.x is not an f64");
    }
    let obj_x = field(code, obj_t, "x")?.0;
    let flow_calc_w = typed(code, flow_t, "calculatedWidth", f64_t)?;
    let (dom, dk_t) = field(code, obj_t, "dom")?;

    let game_t = obj_type(code, "Game")?;
    let game_cls = class_global(code, game_t)?;
    let game_inst = typed(code, code.globals[game_cls.0], "inst", game_t)?;
    let (game_me, me_t) = field(code, game_t, "me")?;
    let (game_state, state_t) = field(code, game_t, "state")?;
    let (game_ui, ui_t) = field(code, game_t, "ui")?;
    let (game_battle, battle_t) = field(code, game_t, "battle")?;
    let game_loading = typed(code, game_t, "isLoading", bool_t)?;
    let (state_players, players_t) = field(code, state_t, "players")?;
    let (players_arr, pad_t) = field(code, players_t, "array")?;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    want(code, is_multi, "Game.get_isMulti", &[game_t], bool_t)?;

    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let bar_t = obj_type(code, "ui.comp.gameUIComp.WorldButtonsBar")?;
    if s(code, obj(code, ui_t)?.name) != "ui.GameUI" {
        bail!("Game.ui is not ui.GameUI");
    }
    let ui_game = typed(code, ui_t, "game", game_t)?;
    let ui_gi = typed(code, ui_t, "gameInventory", gi_t)?;
    let ui_bar = typed(code, ui_t, "worldButtonsBar", bar_t)?;
    let bar_game = typed(code, bar_t, "game", game_t)?;
    let (bar_bt, icon_t) = field(code, bar_t, "btInventory")?;
    if s(code, obj(code, icon_t)?.name) != "ui.comp.Icon" {
        bail!("btInventory is not a ui.comp.Icon");
    }
    let enable = typed(code, icon_t, "enable", bool_t)?;
    let title_tip = typed(code, icon_t, "titleTip", bool_t)?;
    let text_t = field(code, gi_t, "invTitle")?.1;
    let ic_t = field(code, gi_t, "inventoryContent")?.1;
    let inv_allowed = method(code, gi_t, "isInventoryAllowed")?.findex;
    want(code, inv_allowed, "isInventoryAllowed", &[gi_t], bool_t)?;

    let uinv_t = obj_type(code, "ui.comp.Inventory")?;
    let inv_t = obj_type(code, "st.Inventory")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let uinv_inv = typed(code, uinv_t, "inventory", inv_t)?;
    let (_, mode_t) = field(code, uinv_t, "mode")?;
    let inv_owner = typed(code, inv_t, "owner", bp_t)?;
    let bp_inv = typed(code, bp_t, "inventory", inv_t)?;
    let bp_connected = typed(code, bp_t, "connected", bool_t)?;
    let uinv_cls = class_global(code, uinv_t)?;
    let ic_cls = class_global(code, ic_t)?;
    let get_inventory = method(code, ic_t, "getInventory")?.findex;
    want(
        code,
        get_inventory,
        "InventoryContent.getInventory",
        &[ic_t],
        uinv_t,
    )?;
    let uinv_ctor = method(code, uinv_t, "__constructor__")?.findex;
    want(
        code,
        uinv_ctor,
        "ui.comp.Inventory constructor",
        &[uinv_t, mode_t, inv_t, null_i32_t, obj_t],
        void_t,
    )?;
    let found = window_found(code, mode_t)?;
    let found_g = enum_value_global(code, mode_t, found as usize)?;

    let is_of_type = static_fn(code, "$Std", "isOfType")?.findex;
    want(code, is_of_type, "Std.isOfType", &[dyn_t, dyn_t], bool_t)?;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;
    let println = static_fn(code, "$Sys", "println")?.findex;
    want(code, println, "Sys.println", &[dyn_t], void_t)?;
    let remove = method(code, obj_t, "remove")?.findex;
    want(code, remove, "Object.remove", &[obj_t], void_t)?;
    let child_index = method(code, obj_t, "getChildIndex")?.findex;
    want(
        code,
        child_index,
        "Object.getChildIndex",
        &[obj_t, obj_t],
        i32_t,
    )?;
    // The bar's own addChildAt (Flow's: it keeps the FlowProperties in step).
    let bar_add_at = proto_up(code, bar_t, "addChildAt")?;
    let baa = sig(code, bar_add_at)?;
    if baa.args.len() != 3 || baa.args[1] != obj_t || baa.args[2] != i32_t || baa.ret != void_t {
        bail!("WorldButtonsBar.addChildAt signature");
    }
    let flow_set_visible = proto_up(code, flow_t, "set_visible")?;
    let fsv = sig(code, flow_set_visible)?;
    if fsv.args.len() != 2 || fsv.args[1] != bool_t {
        bail!("Flow.set_visible signature");
    }
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let set_enable = method(code, elem_t, "set_enable")?.findex;
    want(
        code,
        set_enable,
        "Element.set_enable",
        &[elem_t, bool_t],
        bool_t,
    )?;
    let set_tip = method(code, elem_t, "set_tipText")?.findex;
    want(
        code,
        set_tip,
        "Element.set_tipText",
        &[elem_t, str_t],
        str_t,
    )?;
    let set_text = proto_up(code, text_t, "set_text")?;
    let stt = sig(code, set_text)?;
    if stt.args.len() != 2 || stt.args[1] != str_t {
        bail!("set_text signature");
    }
    // getName: the nickname in <font color=...> (player colour), as the players
    // panel and timeline_list show it; the title is a TextFixed (an HtmlText).
    let user_name = method(code, bp_t, "getName")?.findex;
    want(code, user_name, "BasePlayer.getName", &[bp_t], str_t)?;

    // ItemSlot
    let slot_t = obj_type(code, "ui.comp.ItemSlot")?;
    let slot_mode = typed(code, slot_t, "mode", mode_t)?;
    let (slot_api, api_t) = field(code, slot_t, "api")?;
    let ctrl_down = method(code, slot_t, "ctrlKeyDown")?.findex;
    want(code, ctrl_down, "ItemSlot.ctrlKeyDown", &[slot_t], bool_t)?;
    let select_amount = method(code, slot_t, "selectAmount")?.findex;
    let sa = sig(code, select_amount)?;
    if sa.args.len() != 4 || sa.args[1] != str_t || sa.args[2] != i32_t || sa.ret != void_t {
        bail!("ItemSlot.selectAmount signature");
    }
    let int_cb_t = sa.args[3];
    if !matches!(int_cb_t.as_fun(code), Some(t) if t.args == [i32_t] && t.ret == void_t) {
        bail!("selectAmount callback is not (Int) -> Void");
    }
    let on_click = method(code, slot_t, "onClick")?;
    let (shift_key, is_down) = match &on_click.ops[..] {
        [Opcode::Int { dst, ptr }, Opcode::Call1 { fun, arg0, .. }, ..] if arg0 == dst => {
            (code.ints[ptr.0], *fun)
        }
        _ => bail!("ItemSlot.onClick: no key test"),
    };
    want(code, is_down, "Key.isDown", &[i32_t], bool_t)?;
    let rc = method(code, slot_t, "onRightClick")?;
    // var it = get_item(); (ToVirtual) ... it.k ... k.get_locked()
    let (get_item, item_v_t, item_nv_t) = match &rc.ops[..] {
        [Opcode::Call1 {
            fun,
            dst,
            arg0: Reg(0),
        }, Opcode::ToVirtual { dst: v, src }, ..]
            if src == dst =>
        {
            (*fun, rc.regs[dst.0 as usize], rc.regs[v.0 as usize])
        }
        _ => bail!("onRightClick does not start with get_item"),
    };
    let (item_k, item_t, get_locked) = match &rc.ops[4..8] {
        [Opcode::NullCheck { .. }, Opcode::Field { dst: k, field, .. }, Opcode::NullCheck { .. }, Opcode::Call1 { fun, arg0, .. }]
            if arg0 == k =>
        {
            (*field, rc.regs[k.0 as usize], *fun)
        }
        _ => bail!("onRightClick: no item.k.locked test"),
    };
    if s(code, obj(code, item_t)?.name) != "st.Item" {
        bail!("item.k is not st.Item");
    }
    want(code, get_locked, "Item.get_locked", &[item_t], bool_t)?;
    let get_count = code
        .functions
        .iter()
        .find(|f| {
            s(code, f.name) == "get_count"
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == [item_v_t] && t.ret == i32_t)
        })
        .context("get_count(slot item) not found")?
        .findex;
    // slot.api as the virtual the FoundItems take calls networkOperation on.
    let (api_v_t, api_net_op, bool_cb_t) = slot_api_call(code, slot_t, slot_api)?;
    let Some(Type::Fun(nt) | Type::Method(nt)) = virtual_field_t(code, api_v_t, api_net_op) else {
        bail!(
            "slot api networkOperation is not a function: {:?}",
            virtual_field_t(code, api_v_t, api_net_op)
        );
    };
    let op_t = nt.args[0];
    let Type::Enum { constructs, .. } = &code.types[op_t.0] else {
        bail!("slot operation is not an enum");
    };
    let mi = constructs
        .iter()
        .position(|c| s(code, c.name) == "MoveTo")
        .context("SlotOperation.MoveTo not found")?;
    if constructs[mi].params != [inv_t, i32_t] {
        bail!("SlotOperation.MoveTo is not (Inventory, Int)");
    }
    if nt.args.len() != 3 || nt.args[2] != bool_cb_t {
        bail!("slot api networkOperation arguments");
    }

    // icon creation, as WorldButtonsBar builds btInventory
    let bar_ctor = method(code, bar_t, "__constructor__")?;
    let bar_ctor_fi = index_of(code, bar_ctor.findex)?;
    let blk = plan_icon_block(code, bar_ctor, bar_bt, bar_t)?;
    if blk.props_t != dk_t {
        bail!("createNew does not return domkit.Properties");
    }
    let ds = sig(code, blk.dyn_alloc)?;
    let (ref_bool_t, arr_dyn_t) = (ds.args[1], ds.ret);
    if !matches!(code.types[ref_bool_t.0], Type::Ref(b) if b == bool_t) {
        bail!("ArrayDyn.alloc second argument is not Ref<Bool>");
    }
    let cs = sig(code, blk.create)?;
    if cs.args != [str_t, dk_t, arr_dyn_t, dyn_t] || cs.ret != dk_t {
        bail!("createNew signature");
    }
    if !matches!(blk.arr_elem_t_op, Opcode::Type { .. }) {
        bail!("array element type op");
    }
    let bar_ret = bar_ctor.ops.len() - 1;
    if !matches!(bar_ctor.ops[bar_ret], Opcode::Ret { .. }) {
        bail!("WorldButtonsBar constructor does not end with Ret");
    }
    if (0..bar_ret).any(|i| jump_targets(bar_ctor, i).contains(&bar_ret)) {
        bail!("WorldButtonsBar constructor: a jump lands on the final Ret");
    }
    let close_g = existing_str(code, str_t, "Close")?;

    let ui_update = method(code, ui_t, "update")?;
    plain_start(ui_update, "GameUI.update")?;
    let sites = [
        "allowPick",
        "allowDrop",
        "doPick",
        "drop",
        "onRightClick",
        "getTipContent",
    ];
    let mut fis = vec![];
    for n in sites {
        let f = method(code, slot_t, n)?;
        plain_start(f, n)?;
        fis.push(index_of(code, f.findex)?);
    }
    for (n, ret) in [
        ("allowPick", bool_t),
        ("allowDrop", bool_t),
        ("doPick", void_t),
        ("drop", bool_t),
        ("onRightClick", void_t),
    ] {
        if method(code, slot_t, n)?.t.as_fun(code).map(|t| t.ret) != Some(ret) {
            bail!("ItemSlot.{n}: unexpected return type");
        }
    }
    let dbg_file = bar_ctor
        .debug_info
        .as_ref()
        .and_then(|d| d.first())
        .map(|x| x.0)
        .unwrap_or(0);
    Ok(UiPlan {
        dbg_file,
        void_t,
        bool_t,
        i32_t,
        dyn_t,
        dynobj_t,
        str_t,
        type_t,
        ref_bool_t,
        arr_dyn_t,
        obj_t,
        flow_t,
        arr_t,
        raw_t,
        null_i32_t,
        game_t,
        me_t,
        state_t,
        players_t,
        pad_t,
        ui_t,
        gi_t,
        bar_t,
        icon_t,
        text_t,
        ic_t,
        uinv_t,
        inv_t,
        bp_t,
        slot_t,
        mode_t,
        battle_t,
        item_v_t,
        item_nv_t,
        item_t,
        api_t,
        api_v_t,
        op_t,
        bool_cb_t,
        int_cb_t,
        parent,
        children,
        visible,
        obj_x,
        flow_calc_w,
        f64_t,
        dom,
        arr_len,
        arr_raw,
        game_inst,
        game_me,
        game_state,
        game_ui,
        game_battle,
        game_loading,
        state_players,
        players_arr,
        ui_game,
        ui_gi,
        ui_bar,
        bar_game,
        bar_bt,
        enable,
        title_tip,
        uinv_inv,
        inv_owner,
        bp_inv,
        bp_connected,
        slot_mode,
        slot_api,
        item_k,
        api_net_op,
        move_to: RefEnumConstruct(mi),
        found,
        game_cls,
        uinv_cls,
        ic_cls,
        found_g,
        close_g,
        is_of_type,
        std_string,
        str_add,
        println,
        remove,
        child_index,
        bar_add_at,
        flow_set_visible,
        set_enable,
        set_tip,
        set_text,
        user_name,
        is_multi,
        inv_allowed,
        get_inventory,
        uinv_ctor,
        ctrl_down,
        is_down,
        shift_key,
        select_amount,
        get_item,
        get_locked,
        get_count,
        blk,
        bar_ctor_fi,
        bar_ret,
        ui_update_fi: index_of(code, ui_update.findex)?,
        allow_pick_fi: fis[0],
        allow_drop_fi: fis[1],
        do_pick_fi: fis[2],
        drop_fi: fis[3],
        right_click_fi: fis[4],
        tip_fi: fis[5],
    })
}

/// Function names do not survive serialization: an image patched before is
/// recognised by ItemSlot.allowPick starting with a call that returns an
/// st.Inventory (vanilla starts with `get_item`).
fn ui_applied(code: &Bytecode) -> bool {
    let (Ok(slot_t), Ok(inv_t)) = (
        obj_type(code, "ui.comp.ItemSlot"),
        obj_type(code, "st.Inventory"),
    ) else {
        return false;
    };
    let Ok(f) = method(code, slot_t, "allowPick") else {
        return false;
    };
    matches!(f.ops.first(), Some(Opcode::Call1 { fun, arg0: Reg(0), .. })
        if sig(code, *fun).is_ok_and(|t| t.ret == inv_t))
}

fn window_found(code: &Bytecode, mode_t: RefType) -> Result<i32> {
    match &code.types[mode_t.0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|c| s(code, c.name) == "FoundItems" && c.params.is_empty())
            .map(|i| i as i32)
            .context("ItemSlotMode.FoundItems not found"),
        _ => bail!("ItemSlotMode is not an enum"),
    }
}

fn existing_str(code: &Bytecode, str_t: RefType, value: &str) -> Result<RefGlobal> {
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

fn virtual_field_t(code: &Bytecode, t: RefType, f: RefField) -> Option<&Type> {
    match &code.types[t.0] {
        Type::Virtual { fields } => fields.get(f.0).map(|x| &code.types[x.t.0]),
        _ => None,
    }
}

/// `slot.api` cast to a virtual and its `networkOperation(op, slot, cb)` call,
/// as vanilla's FoundItems take does: (virtual type, field, callback type).
fn slot_api_call(
    code: &Bytecode,
    slot_t: RefType,
    api: RefField,
) -> Result<(RefType, RefField, RefType)> {
    for f in &code.functions {
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Field { dst, obj: o, field } = op else {
                continue;
            };
            if *field != api || f.regs[o.0 as usize] != slot_t {
                continue;
            }
            let Some(Opcode::ToVirtual { dst: v, src }) = f.ops.get(i + 1) else {
                continue;
            };
            if src != dst {
                continue;
            }
            let vt = f.regs[v.0 as usize];
            for o2 in &f.ops[i + 2..] {
                if let Opcode::CallMethod { field, args, .. } = o2 {
                    if args.len() == 4 && args[0] == *v {
                        let Type::Virtual { fields } = &code.types[vt.0] else {
                            break;
                        };
                        if fields.get(field.0).map(|x| s(code, x.name)) == Some("networkOperation")
                        {
                            return Ok((vt, *field, f.regs[args[3].0 as usize]));
                        }
                    }
                }
            }
        }
    }
    bail!("no slot.api.networkOperation call found")
}

struct Globals {
    box_: RefGlobal,
    gi: RefGlobal,
    target: RefGlobal,
    btn: RefGlobal,
    icon: RefGlobal,
    logs: RefGlobal,
    err: RefGlobal,
}

fn new_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

/// Registers of a `createNew(comp, parent, [arg], {attrs})` sequence.
struct CRegs {
    comp: Reg,
    n: Reg,
    ty: Reg,
    raw: Reg,
    arr: Reg,
    wrapped: Reg,
    b: Reg,
    rb: Reg,
    args: Reg,
    attrs: Reg,
    sv: Reg,
}

fn cregs(p: &UiPlan, r: &mut Regs) -> CRegs {
    CRegs {
        comp: r.r(p.str_t),
        n: r.r(p.i32_t),
        ty: r.r(p.type_t),
        raw: r.r(p.blk.arr_t),
        arr: r.r(p.blk.arr_t),
        wrapped: r.r(p.blk.arr_obj_t),
        b: r.r(p.bool_t),
        rb: r.r(p.ref_bool_t),
        args: r.r(p.arr_dyn_t),
        attrs: r.r(p.dynobj_t),
        sv: r.r(p.str_t),
    }
}

/// `dst = createNew(comp, parent, [arg], attrs)`, the way domkit-generated code
/// builds a component (no attrs: null).
#[allow(clippy::too_many_arguments)]
fn emit_create(
    a: &mut Asm,
    code: &mut Bytecode,
    p: &UiPlan,
    r: &CRegs,
    dst: Reg,
    parent: Reg,
    comp: &'static str,
    arg: Option<RefGlobal>,
    attrs: &[(&str, &'static str)],
) {
    emit_create_dyn(a, code, p, r, dst, parent, comp, arg, attrs, None)
}

/// emit_create with one more attribute whose String value is in a register.
#[allow(clippy::too_many_arguments)]
fn emit_create_dyn(
    a: &mut Asm,
    code: &mut Bytecode,
    p: &UiPlan,
    r: &CRegs,
    dst: Reg,
    parent: Reg,
    comp: &'static str,
    arg: Option<RefGlobal>,
    attrs: &[(&str, &'static str)],
    dyn_attr: Option<(&str, Reg)>,
) {
    let b = &p.blk;
    let comp_g = str_global(code, p.str_t, comp);
    a.op(Opcode::GetGlobal {
        dst: r.comp,
        global: comp_g,
    });
    a.op(Opcode::Int {
        dst: r.n,
        ptr: int_const(code, arg.is_some() as i32),
    });
    let Opcode::Type { ty, .. } = b.arr_elem_t_op else {
        unreachable!("checked in the plan")
    };
    a.op(Opcode::Type { dst: r.ty, ty });
    a.op(Opcode::Call2 {
        dst: r.raw,
        fun: b.alloc,
        arg0: r.ty,
        arg1: r.n,
    });
    a.op(Opcode::UnsafeCast {
        dst: r.arr,
        src: r.raw,
    });
    if let Some(g) = arg {
        a.op(Opcode::GetGlobal {
            dst: r.sv,
            global: g,
        });
        a.op(Opcode::Int {
            dst: r.n,
            ptr: int_const(code, 0),
        });
        a.op(Opcode::SetArray {
            array: r.arr,
            index: r.n,
            src: r.sv,
        });
    }
    a.op(Opcode::Call1 {
        dst: r.wrapped,
        fun: b.wrap,
        arg0: r.arr,
    });
    a.op(Opcode::Bool {
        dst: r.b,
        value: ValBool(true),
    });
    a.op(Opcode::Ref {
        dst: r.rb,
        src: r.b,
    });
    a.op(Opcode::Call2 {
        dst: r.args,
        fun: b.dyn_alloc,
        arg0: r.wrapped,
        arg1: r.rb,
    });
    if attrs.is_empty() && dyn_attr.is_none() {
        a.op(Opcode::Null { dst: r.attrs });
    } else {
        a.op(Opcode::New { dst: r.attrs });
        for (k, v) in attrs {
            let ks = string_ref(code, k);
            let vg = str_global(code, p.str_t, v);
            a.op(Opcode::GetGlobal {
                dst: r.sv,
                global: vg,
            });
            a.op(Opcode::DynSet {
                obj: r.attrs,
                field: ks,
                src: r.sv,
            });
        }
        if let Some((k, src)) = dyn_attr {
            let ks = string_ref(code, k);
            a.op(Opcode::DynSet {
                obj: r.attrs,
                field: ks,
                src,
            });
        }
    }
    a.op(Opcode::Call4 {
        dst,
        fun: b.create,
        arg0: r.comp,
        arg1: parent,
        arg2: r.args,
        arg3: r.attrs,
    });
}

/// `Game.inst` into `game` (jumps to `none` when null).
fn game_inst(a: &mut Asm, p: &UiPlan, gc: Reg, game: Reg, none: &'static str) {
    a.op(Opcode::GetGlobal {
        dst: gc,
        global: p.game_cls,
    });
    a.op(Opcode::Field {
        dst: game,
        obj: gc,
        field: p.game_inst,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        none,
    );
}

/// `out: EndTrap; Ret res` / `catch: report(exc); <fallback>; Ret res`.
fn close_trap(a: &mut Asm, exc: Reg, res: Reg, v: Reg, report: RefFun, fallback: Option<Opcode>) {
    a.label("out");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: res });
    a.label("catch");
    a.op(Opcode::Call1 {
        dst: v,
        fun: report,
        arg0: exc,
    });
    if let Some(o) = fallback {
        a.op(o);
    }
    a.op(Opcode::Ret { ret: res });
}

fn open_trap(a: &mut Asm, exc: Reg) {
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
}

/// `report(exc)`: `if (logs < LOG_CAP) { logs++; Sys.println("mp: allinv: " + Std.string(exc)); }`
fn add_report(code: &mut Bytecode, p: &UiPlan, g: &Globals) -> Result<RefFun> {
    let cap = int_const(code, LOG_CAP);
    let mut r = Regs(vec![p.dyn_t]);
    let (v, n, k, s1, s2) = (
        r.r(p.void_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.str_t),
        r.r(p.str_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: n,
        global: g.logs,
    });
    a.op(Opcode::Int { dst: k, ptr: cap });
    a.jmp(
        Opcode::JSGte {
            a: n,
            b: k,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Incr { dst: n });
    a.op(Opcode::SetGlobal {
        global: g.logs,
        src: n,
    });
    a.op(Opcode::GetGlobal {
        dst: s1,
        global: g.err,
    });
    a.op(Opcode::Call1 {
        dst: s2,
        fun: p.std_string,
        arg0: Reg(0),
    });
    a.op(Opcode::Call2 {
        dst: s1,
        fun: p.str_add,
        arg0: s1,
        arg1: s2,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s1,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.dyn_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `noop(ok)`: the slot API's result callback (it must not be null).
fn add_noop(code: &mut Bytecode, p: &UiPlan) -> Result<RefFun> {
    let regs = vec![p.bool_t, p.void_t];
    push_fn(
        code,
        vec![p.bool_t],
        p.void_t,
        regs,
        vec![Opcode::Ret { ret: Reg(1) }],
        p.dbg_file,
    )
}

/// `slotInv(slot)`: the st.Inventory of the nearest ui.comp.Inventory ancestor.
fn add_slot_inv(code: &mut Bytecode, p: &UiPlan) -> Result<RefFun> {
    let depth = int_const(code, DEPTH);
    let mut r = Regs(vec![p.slot_t]);
    let (o, d, k, cls, b, ui, inv) = (
        r.r(p.obj_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(code.globals[p.uinv_cls.0]),
        r.r(p.bool_t),
        r.r(p.uinv_t),
        r.r(p.inv_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: o,
        obj: Reg(0),
        field: p.parent,
    });
    a.op(Opcode::Int {
        dst: d,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::Int { dst: k, ptr: depth });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.uinv_cls,
    });
    a.loop_head("l");
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "none");
    a.jmp(
        Opcode::JSGte {
            a: d,
            b: k,
            offset: 0,
        },
        "none",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.is_of_type,
        arg0: o,
        arg1: cls,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "hit");
    a.op(Opcode::Field {
        dst: o,
        obj: o,
        field: p.parent,
    });
    a.op(Opcode::Incr { dst: d });
    a.jmp(Opcode::JAlways { offset: 0 }, "l");
    a.label("hit");
    a.op(Opcode::UnsafeCast { dst: ui, src: o });
    a.op(Opcode::Field {
        dst: inv,
        obj: ui,
        field: p.uinv_inv,
    });
    a.op(Opcode::Ret { ret: inv });
    a.label("none");
    a.op(Opcode::Null { dst: inv });
    a.op(Opcode::Ret { ret: inv });
    push_fn(code, vec![p.slot_t], p.inv_t, r.0, a.finish(), p.dbg_file)
}

/// `foreign(slot)`: while panels are open, the inventory of a FoundItems slot
/// that belongs to another player (owner set and not Game.me), else null.
fn add_foreign(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    slot_inv: RefFun,
) -> Result<RefFun> {
    let fc = int_const(code, p.found);
    let mut r = Regs(vec![p.slot_t]);
    let (exc, res, v, bx, mode, idx, k, ow, gc, game, me) = (
        r.r(p.dyn_t),
        r.r(p.inv_t),
        r.r(p.void_t),
        r.r(p.obj_t),
        r.r(p.mode_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.bp_t),
        r.r(code.globals[p.game_cls.0]),
        r.r(p.game_t),
        r.r(p.me_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: res });
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "fast");
    open_trap(&mut a, exc);
    a.op(Opcode::Field {
        dst: mode,
        obj: Reg(0),
        field: p.slot_mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::EnumIndex {
        dst: idx,
        value: mode,
    });
    a.op(Opcode::Int { dst: k, ptr: fc });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: k,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: res,
        fun: slot_inv,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JNull {
            reg: res,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: ow,
        obj: res,
        field: p.inv_owner,
    });
    a.jmp(Opcode::JNull { reg: ow, offset: 0 }, "clear");
    game_inst(&mut a, p, gc, game, "clear");
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.game_me,
    });
    a.jmp(
        Opcode::JEq {
            a: ow,
            b: me,
            offset: 0,
        },
        "clear",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("clear");
    a.op(Opcode::Null { dst: res });
    close_trap(&mut a, exc, res, v, report, Some(Opcode::Null { dst: res }));
    a.label("fast");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.slot_t], p.inv_t, r.0, a.finish(), p.dbg_file)
}

/// `alive(inv)`: inv's owner is a connected player other than Game.me.
fn add_alive(code: &mut Bytecode, p: &UiPlan) -> Result<RefFun> {
    let mut r = Regs(vec![p.inv_t]);
    let (res, ow, b, gc, game, me) = (
        r.r(p.bool_t),
        r.r(p.bp_t),
        r.r(p.bool_t),
        r.r(code.globals[p.game_cls.0]),
        r.r(p.game_t),
        r.r(p.me_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: ow,
        obj: Reg(0),
        field: p.inv_owner,
    });
    a.jmp(Opcode::JNull { reg: ow, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: b,
        obj: ow,
        field: p.bp_connected,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    game_inst(&mut a, p, gc, game, "ret");
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.game_me,
    });
    a.jmp(
        Opcode::JEq {
            a: ow,
            b: me,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.inv_t], p.bool_t, r.0, a.finish(), p.dbg_file)
}

/// Loop over `obj.children` (`ch`, `n`, index `i`), `c` = current child.
struct ChildLoop {
    ch: Reg,
    n: Reg,
    raw: Reg,
    i: Reg,
    d: Reg,
    c: Reg,
}

fn child_loop(p: &UiPlan, r: &mut Regs) -> ChildLoop {
    ChildLoop {
        ch: r.r(p.arr_t),
        n: r.r(p.i32_t),
        raw: r.r(p.raw_t),
        i: r.r(p.i32_t),
        d: r.r(p.dyn_t),
        c: r.r(p.obj_t),
    }
}

/// `panelInv(panel)`: the inventory shown by the panel's inventory-content.
fn add_panel_inv(code: &mut Bytecode, p: &UiPlan) -> Result<RefFun> {
    let mut r = Regs(vec![p.obj_t]);
    let res = r.r(p.inv_t);
    let l = child_loop(p, &mut r);
    let (cls, b, icr, ui) = (
        r.r(code.globals[p.ic_cls.0]),
        r.r(p.bool_t),
        r.r(p.ic_t),
        r.r(p.uinv_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: res });
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: l.ch,
        obj: Reg(0),
        field: p.children,
    });
    a.jmp(
        Opcode::JNull {
            reg: l.ch,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: l.n,
        obj: l.ch,
        field: p.arr_len,
    });
    a.op(Opcode::Field {
        dst: l.raw,
        obj: l.ch,
        field: p.arr_raw,
    });
    a.op(Opcode::Int {
        dst: l.i,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.ic_cls,
    });
    a.loop_head("l");
    a.jmp(
        Opcode::JSGte {
            a: l.i,
            b: l.n,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::GetArray {
        dst: l.d,
        array: l.raw,
        index: l.i,
    });
    a.op(Opcode::Incr { dst: l.i });
    a.op(Opcode::UnsafeCast { dst: l.c, src: l.d });
    a.jmp(
        Opcode::JNull {
            reg: l.c,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.is_of_type,
        arg0: l.c,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "l");
    a.op(Opcode::UnsafeCast { dst: icr, src: l.c });
    a.op(Opcode::Call1 {
        dst: ui,
        fun: p.get_inventory,
        arg0: icr,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: res,
        obj: ui,
        field: p.uinv_inv,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.obj_t], p.inv_t, r.0, a.finish(), p.dbg_file)
}

/// `target()`: the give target: the remembered panel inventory if still open and
/// alive, else the first alive panel's, else null.
fn add_target(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    alive: RefFun,
    panel_inv: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![]);
    let (res, first, bx, want, inv, b) = (
        r.r(p.inv_t),
        r.r(p.inv_t),
        r.r(p.obj_t),
        r.r(p.inv_t),
        r.r(p.inv_t),
        r.r(p.bool_t),
    );
    let l = child_loop(p, &mut r);
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: res });
    a.op(Opcode::Null { dst: first });
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: l.ch,
        obj: bx,
        field: p.children,
    });
    a.jmp(
        Opcode::JNull {
            reg: l.ch,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: l.n,
        obj: l.ch,
        field: p.arr_len,
    });
    a.op(Opcode::Field {
        dst: l.raw,
        obj: l.ch,
        field: p.arr_raw,
    });
    a.op(Opcode::Int {
        dst: l.i,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::GetGlobal {
        dst: want,
        global: g.target,
    });
    a.loop_head("l");
    a.jmp(
        Opcode::JSGte {
            a: l.i,
            b: l.n,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::GetArray {
        dst: l.d,
        array: l.raw,
        index: l.i,
    });
    a.op(Opcode::Incr { dst: l.i });
    a.op(Opcode::UnsafeCast { dst: l.c, src: l.d });
    a.op(Opcode::Call1 {
        dst: inv,
        fun: panel_inv,
        arg0: l.c,
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: alive,
        arg0: inv,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "l");
    a.jmp(
        Opcode::JEq {
            a: inv,
            b: want,
            offset: 0,
        },
        "hit",
    );
    a.jmp(
        Opcode::JNotNull {
            reg: first,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Mov {
        dst: first,
        src: inv,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "l");
    a.label("hit");
    a.op(Opcode::Mov { dst: res, src: inv });
    a.jmp(Opcode::JAlways { offset: 0 }, "ret");
    a.label("done");
    a.op(Opcode::Mov {
        dst: res,
        src: first,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![], p.inv_t, r.0, a.finish(), p.dbg_file)
}

/// `close()`: removes the panels' container and forgets it.
fn add_close(code: &mut Bytecode, p: &UiPlan, g: &Globals) -> Result<RefFun> {
    let mut r = Regs(vec![]);
    let (v, bx, nb, ninv, ngi) = (
        r.r(p.void_t),
        r.r(p.obj_t),
        r.r(p.obj_t),
        r.r(p.inv_t),
        r.r(p.gi_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.op(Opcode::Null { dst: nb });
    a.op(Opcode::SetGlobal {
        global: g.box_,
        src: nb,
    });
    a.op(Opcode::Null { dst: ninv });
    a.op(Opcode::SetGlobal {
        global: g.target,
        src: ninv,
    });
    a.op(Opcode::Null { dst: ngi });
    a.op(Opcode::SetGlobal {
        global: g.gi,
        src: ngi,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "ret");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: bx,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `closePanel(panel)` (a panel's Close icon): removes it; the last one closes all.
fn add_close_panel(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    close: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.obj_t]);
    let (exc, v, bx, ch, n, z) = (
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(p.obj_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let mut a = Asm::new();
    open_trap(&mut a, exc);
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: Reg(0),
    });
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: ch,
        obj: bx,
        field: p.children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "all");
    a.op(Opcode::Field {
        dst: n,
        obj: ch,
        field: p.arr_len,
    });
    a.op(Opcode::Int {
        dst: z,
        ptr: int_const(code, 0),
    });
    a.jmp(
        Opcode::JSGt {
            a: n,
            b: z,
            offset: 0,
        },
        "out",
    );
    a.label("all");
    a.op(Opcode::Call0 { dst: v, fun: close });
    close_trap(&mut a, exc, v, v, report, None);
    push_fn(code, vec![p.obj_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `giveN(slot, n)`: `slot.api.networkOperation(MoveTo(target(), n), slot, noop)`.
fn add_give_n(
    code: &mut Bytecode,
    p: &UiPlan,
    report: RefFun,
    target: RefFun,
    noop: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.slot_t, p.i32_t]);
    let (exc, v, z, t, api, va, op, cb) = (
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(p.i32_t),
        r.r(p.inv_t),
        r.r(p.api_t),
        r.r(p.api_v_t),
        r.r(p.op_t),
        r.r(p.bool_cb_t),
    );
    let mut a = Asm::new();
    open_trap(&mut a, exc);
    a.op(Opcode::Int {
        dst: z,
        ptr: int_const(code, 0),
    });
    a.jmp(
        Opcode::JSLte {
            a: Reg(1),
            b: z,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call0 {
        dst: t,
        fun: target,
    });
    a.jmp(Opcode::JNull { reg: t, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: api,
        obj: Reg(0),
        field: p.slot_api,
    });
    a.jmp(
        Opcode::JNull {
            reg: api,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::ToVirtual { dst: va, src: api });
    a.op(Opcode::MakeEnum {
        dst: op,
        construct: p.move_to,
        args: vec![t, Reg(1)],
    });
    a.op(Opcode::StaticClosure { dst: cb, fun: noop });
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.api_net_op,
        args: vec![va, op, Reg(0), cb],
    });
    close_trap(&mut a, exc, v, v, report, None);
    push_fn(
        code,
        vec![p.slot_t, p.i32_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `give(slot)` (ItemSlot.onRightClick, true = handled): a foreign slot becomes
/// the give target (vanilla take goes on); ctrl + right click on the own
/// inventory gives the stack (ctrl + shift: amount box) to the target.
#[allow(clippy::too_many_arguments)]
fn add_give(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    foreign: RefFun,
    slot_inv: RefFun,
    target: RefFun,
    give_n: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.slot_t]);
    let (exc, res, v, bx, fi, b, inv, gc, game, me, mi, it, iv, k, t, key, cur, m, cl, n) = (
        r.r(p.dyn_t),
        r.r(p.bool_t),
        r.r(p.void_t),
        r.r(p.obj_t),
        r.r(p.inv_t),
        r.r(p.bool_t),
        r.r(p.inv_t),
        r.r(code.globals[p.game_cls.0]),
        r.r(p.game_t),
        r.r(p.me_t),
        r.r(p.inv_t),
        r.r(p.item_v_t),
        r.r(p.item_nv_t),
        r.r(p.item_t),
        r.r(p.inv_t),
        r.r(p.i32_t),
        r.r(p.str_t),
        r.r(p.i32_t),
        r.r(p.int_cb_t),
        r.r(p.i32_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "fast");
    open_trap(&mut a, exc);
    a.op(Opcode::Call1 {
        dst: fi,
        fun: foreign,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JNull { reg: fi, offset: 0 }, "own");
    a.op(Opcode::SetGlobal {
        global: g.target,
        src: fi,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("own");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.ctrl_down,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: inv,
        fun: slot_inv,
        arg0: Reg(0),
    });
    a.jmp(
        Opcode::JNull {
            reg: inv,
            offset: 0,
        },
        "out",
    );
    game_inst(&mut a, p, gc, game, "out");
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.game_me,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: mi,
        obj: me,
        field: p.bp_inv,
    });
    a.jmp(
        Opcode::JNotEq {
            a: inv,
            b: mi,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: it,
        fun: p.get_item,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "out");
    a.op(Opcode::ToVirtual { dst: iv, src: it });
    a.op(Opcode::Field {
        dst: k,
        obj: iv,
        field: p.item_k,
    });
    a.jmp(Opcode::JNull { reg: k, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.get_locked,
        arg0: k,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "out");
    a.op(Opcode::Call0 {
        dst: t,
        fun: target,
    });
    a.jmp(Opcode::JNull { reg: t, offset: 0 }, "out");
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.op(Opcode::Int {
        dst: key,
        ptr: int_const(code, p.shift_key),
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_down,
        arg0: key,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "whole");
    a.op(Opcode::Null { dst: cur });
    a.op(Opcode::Int {
        dst: m,
        ptr: int_const(code, -1),
    });
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: give_n,
        obj: Reg(0),
    });
    a.op(Opcode::Call4 {
        dst: v,
        fun: p.select_amount,
        arg0: Reg(0),
        arg1: cur,
        arg2: m,
        arg3: cl,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("whole");
    a.op(Opcode::Call1 {
        dst: n,
        fun: p.get_count,
        arg0: it,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: give_n,
        arg0: Reg(0),
        arg1: n,
    });
    close_trap(
        &mut a,
        exc,
        res,
        v,
        report,
        Some(Opcode::Bool {
            dst: res,
            value: ValBool(false),
        }),
    );
    a.label("fast");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.slot_t], p.bool_t, r.0, a.finish(), p.dbg_file)
}

/// `hover(slot)` (ItemSlot.getTipContent): a hovered foreign slot's inventory
/// becomes the give target.
fn add_hover(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    foreign: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.slot_t]);
    let (exc, v, bx, fi) = (r.r(p.dyn_t), r.r(p.void_t), r.r(p.obj_t), r.r(p.inv_t));
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "fast");
    open_trap(&mut a, exc);
    a.op(Opcode::Call1 {
        dst: fi,
        fun: foreign,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JNull { reg: fi, offset: 0 }, "out");
    a.op(Opcode::SetGlobal {
        global: g.target,
        src: fi,
    });
    close_trap(&mut a, exc, v, v, report, None);
    a.label("fast");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.slot_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `addPanel(box, p, j)`: one vanilla-styled `#inventory` panel for player p.
fn add_add_panel(
    code: &mut Bytecode,
    p: &UiPlan,
    close_panel: RefFun,
    drag_panel: RefFun,
) -> Result<RefFun> {
    let dk_t = p.blk.props_t;
    let mut r = Regs(vec![dk_t, p.bp_t, p.i32_t]);
    let c = cregs(p, &mut r);
    let (
        v,
        pp,
        po,
        tp,
        xp,
        xo,
        tx,
        name,
        ip,
        io,
        icon,
        cl,
        cp,
        co,
        ui,
        mode,
        inv,
        rows,
        rn,
        pre,
        dj,
        js,
        key,
        pf,
        sv,
    ) = (
        r.r(p.void_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(dk_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(p.text_t),
        r.r(p.str_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(p.icon_t),
        r.r(p.blk.closure_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(p.uinv_t),
        r.r(p.mode_t),
        r.r(p.inv_t),
        r.r(p.i32_t),
        r.r(p.null_i32_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.flow_t),
        r.r(p.str_t),
    );
    let pre_g = str_global(code, p.str_t, KEY_PREFIX);
    let rows_c = int_const(code, ROWS);
    let props_obj = p.blk.props_obj;
    let mut a = Asm::new();
    emit_create(
        &mut a,
        code,
        p,
        &c,
        pp,
        Reg(0),
        "element",
        None,
        PANEL_ATTRS,
    );
    a.op(Opcode::Field {
        dst: po,
        obj: pp,
        field: props_obj,
    });
    a.jmp(Opcode::JNull { reg: po, offset: 0 }, "ret");
    emit_create(&mut a, code, p, &c, tp, pp, "flow", None, TITLE_ATTRS);
    emit_create(&mut a, code, p, &c, xp, tp, "text-fixed", None, NAME_ATTRS);
    a.op(Opcode::Field {
        dst: xo,
        obj: xp,
        field: props_obj,
    });
    a.op(Opcode::SafeCast { dst: tx, src: xo });
    a.op(Opcode::Call1 {
        dst: name,
        fun: p.user_name,
        arg0: Reg(1),
    });
    a.jmp(
        Opcode::JNull {
            reg: name,
            offset: 0,
        },
        "noname",
    );
    a.jmp(Opcode::JNull { reg: tx, offset: 0 }, "noname");
    a.op(Opcode::Call2 {
        dst: sv,
        fun: p.set_text,
        arg0: tx,
        arg1: name,
    });
    a.label("noname");
    emit_create(
        &mut a,
        code,
        p,
        &c,
        ip,
        tp,
        "icon",
        Some(p.close_g),
        CLOSE_ATTRS,
    );
    a.op(Opcode::Field {
        dst: io,
        obj: ip,
        field: props_obj,
    });
    a.op(Opcode::SafeCast { dst: icon, src: io });
    a.jmp(
        Opcode::JNull {
            reg: icon,
            offset: 0,
        },
        "noclose",
    );
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: close_panel,
        obj: po,
    });
    a.op(Opcode::SetField {
        obj: icon,
        field: p.blk.onclick,
        src: cl,
    });
    a.label("noclose");
    emit_create(&mut a, code, p, &c, cp, pp, "inventory-content", None, &[]);
    a.op(Opcode::Field {
        dst: co,
        obj: cp,
        field: props_obj,
    });
    a.op(Opcode::New { dst: ui });
    a.op(Opcode::GetGlobal {
        dst: mode,
        global: p.found_g,
    });
    a.op(Opcode::Field {
        dst: inv,
        obj: Reg(1),
        field: p.bp_inv,
    });
    a.op(Opcode::Int {
        dst: rows,
        ptr: rows_c,
    });
    a.op(Opcode::ToDyn { dst: rn, src: rows });
    a.op(Opcode::CallN {
        dst: v,
        fun: p.uinv_ctor,
        args: vec![ui, mode, inv, rn, co],
    });
    a.op(Opcode::GetGlobal {
        dst: pre,
        global: pre_g,
    });
    a.op(Opcode::ToDyn {
        dst: dj,
        src: Reg(2),
    });
    a.op(Opcode::Call1 {
        dst: js,
        fun: p.std_string,
        arg0: dj,
    });
    a.op(Opcode::Call2 {
        dst: key,
        fun: p.str_add,
        arg0: pre,
        arg1: js,
    });
    a.op(Opcode::SafeCast { dst: pf, src: po });
    a.op(Opcode::Call2 {
        dst: v,
        fun: drag_panel,
        arg0: pf,
        arg1: key,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![dk_t, p.bp_t, p.i32_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `toggle(bar)`: the bottom-bar button's click.
fn add_toggle(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    close: RefFun,
    add_panel: RefFun,
) -> Result<RefFun> {
    let dk_t = p.blk.props_t;
    let mut r = Regs(vec![p.bar_t]);
    let c = cregs(p, &mut r);
    let (
        exc,
        v,
        game,
        b,
        ui,
        gi,
        bx,
        ogi,
        pa,
        bt,
        dom,
        bp,
        bo,
        me,
        st,
        pl,
        pad,
        parr,
        n,
        raw,
        j,
        jj,
        d,
        pv,
        pc,
        pinv,
        cnt,
        z,
        ninv,
    ) = (
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(p.game_t),
        r.r(p.bool_t),
        r.r(p.ui_t),
        r.r(p.gi_t),
        r.r(p.obj_t),
        r.r(p.gi_t),
        r.r(p.obj_t),
        r.r(p.battle_t),
        r.r(dk_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(p.me_t),
        r.r(p.state_t),
        r.r(p.players_t),
        r.r(p.pad_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.raw_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.dyn_t),
        r.r(p.bp_t),
        r.r(p.bool_t),
        r.r(p.inv_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.inv_t),
    );
    let (best, uo, ka, kn, kraw, ki, kd, ko, kb, kf) = (
        r.r(p.i32_t),
        r.r(p.obj_t),
        r.r(p.arr_t),
        r.r(p.i32_t),
        r.r(p.raw_t),
        r.r(p.i32_t),
        r.r(p.dyn_t),
        r.r(p.obj_t),
        r.r(p.bool_t),
        r.r(p.flow_t),
    );
    let (kx, lim, kw, cur, kc, kdy, koff, ky) = (
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.f64_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.dyn_t),
        r.r(p.str_t),
        r.r(p.str_t),
    );
    let mut a = Asm::new();
    open_trap(&mut a, exc);
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(0),
        field: p.bar_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.game_ui,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: gi,
        obj: ui,
        field: p.ui_gi,
    });
    a.jmp(Opcode::JNull { reg: gi, offset: 0 }, "out");
    // open: close (and stop) when the panels are this HUD's and still attached
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "open");
    a.op(Opcode::GetGlobal {
        dst: ogi,
        global: g.gi,
    });
    a.op(Opcode::Field {
        dst: pa,
        obj: bx,
        field: p.parent,
    });
    a.op(Opcode::Call0 { dst: v, fun: close });
    a.jmp(
        Opcode::JNotEq {
            a: ogi,
            b: gi,
            offset: 0,
        },
        "open",
    );
    a.jmp(Opcode::JNotNull { reg: pa, offset: 0 }, "out");
    a.label("open");
    a.op(Opcode::Field {
        dst: bt,
        obj: game,
        field: p.game_battle,
    });
    a.jmp(Opcode::JNotNull { reg: bt, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.inv_allowed,
        arg0: gi,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    // On the HUD root (GameUI is full screen), not in GameInventory (right
    // docked): the box sits at the left edge (BOX_ATTRS).
    a.op(Opcode::Field {
        dst: dom,
        obj: ui,
        field: p.dom,
    });
    a.jmp(
        Opcode::JNull {
            reg: dom,
            offset: 0,
        },
        "out",
    );
    // Left-docked HUD: the box starts past its right edge.
    a.op(Opcode::Int {
        dst: best,
        ptr: int_const(code, BOX_MARGIN),
    });
    a.op(Opcode::Field {
        dst: uo,
        obj: dom,
        field: p.blk.props_obj,
    });
    a.jmp(Opcode::JNull { reg: uo, offset: 0 }, "placed");
    a.op(Opcode::Field {
        dst: ka,
        obj: uo,
        field: p.children,
    });
    a.jmp(Opcode::JNull { reg: ka, offset: 0 }, "placed");
    a.op(Opcode::Field {
        dst: kn,
        obj: ka,
        field: p.arr_len,
    });
    a.op(Opcode::Field {
        dst: kraw,
        obj: ka,
        field: p.arr_raw,
    });
    a.op(Opcode::Int {
        dst: ki,
        ptr: int_const(code, 0),
    });
    a.loop_head("kl");
    a.jmp(
        Opcode::JSGte {
            a: ki,
            b: kn,
            offset: 0,
        },
        "placed",
    );
    a.op(Opcode::GetArray {
        dst: kd,
        array: kraw,
        index: ki,
    });
    a.op(Opcode::Incr { dst: ki });
    a.op(Opcode::UnsafeCast { dst: ko, src: kd });
    a.jmp(Opcode::JNull { reg: ko, offset: 0 }, "kl");
    a.op(Opcode::Field {
        dst: kb,
        obj: ko,
        field: p.visible,
    });
    a.jmp(
        Opcode::JFalse {
            cond: kb,
            offset: 0,
        },
        "kl",
    );
    a.op(Opcode::SafeCast { dst: kf, src: ko });
    a.jmp(Opcode::JNull { reg: kf, offset: 0 }, "kl");
    a.op(Opcode::Field {
        dst: kx,
        obj: ko,
        field: p.obj_x,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: float_const(code, LEFT_X),
    });
    a.jmp(
        Opcode::JSGte {
            a: kx,
            b: lim,
            offset: 0,
        },
        "kl",
    );
    a.op(Opcode::Field {
        dst: kw,
        obj: kf,
        field: p.flow_calc_w,
    });
    a.op(Opcode::Float {
        dst: lim,
        ptr: float_const(code, LEFT_W),
    });
    a.jmp(
        Opcode::JSGte {
            a: kw,
            b: lim,
            offset: 0,
        },
        "kl",
    );
    a.op(Opcode::Add {
        dst: kx,
        a: kx,
        b: kw,
    });
    a.op(Opcode::ToInt { dst: cur, src: kx });
    a.op(Opcode::Int {
        dst: kc,
        ptr: int_const(code, BOX_MARGIN),
    });
    a.op(Opcode::Add {
        dst: cur,
        a: cur,
        b: kc,
    });
    a.jmp(
        Opcode::JSLte {
            a: cur,
            b: best,
            offset: 0,
        },
        "kl",
    );
    a.op(Opcode::Mov {
        dst: best,
        src: cur,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "kl");
    a.label("placed");
    a.op(Opcode::ToDyn {
        dst: kdy,
        src: best,
    });
    a.op(Opcode::Call1 {
        dst: koff,
        fun: p.std_string,
        arg0: kdy,
    });
    a.op(Opcode::GetGlobal {
        dst: ky,
        global: str_global(code, p.str_t, BOX_Y),
    });
    a.op(Opcode::Call2 {
        dst: koff,
        fun: p.str_add,
        arg0: koff,
        arg1: ky,
    });
    emit_create_dyn(
        &mut a,
        code,
        p,
        &c,
        bp,
        dom,
        "flow",
        None,
        BOX_ATTRS,
        Some(("offset", koff)),
    );
    a.op(Opcode::Field {
        dst: bo,
        obj: bp,
        field: p.blk.props_obj,
    });
    a.jmp(Opcode::JNull { reg: bo, offset: 0 }, "out");
    a.op(Opcode::Int {
        dst: cnt,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::Mov { dst: z, src: cnt });
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.game_me,
    });
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.game_state,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "done");
    a.op(Opcode::Field {
        dst: pl,
        obj: st,
        field: p.state_players,
    });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "done");
    a.op(Opcode::Field {
        dst: pad,
        obj: pl,
        field: p.players_arr,
    });
    a.op(Opcode::SafeCast {
        dst: parr,
        src: pad,
    });
    a.jmp(
        Opcode::JNull {
            reg: parr,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: parr,
        field: p.arr_len,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: parr,
        field: p.arr_raw,
    });
    a.op(Opcode::Mov { dst: j, src: z });
    a.loop_head("l");
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: n,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: j,
    });
    a.op(Opcode::Mov { dst: jj, src: j });
    a.op(Opcode::Incr { dst: j });
    a.op(Opcode::UnsafeCast { dst: pv, src: d });
    a.jmp(Opcode::JNull { reg: pv, offset: 0 }, "l");
    a.jmp(
        Opcode::JEq {
            a: pv,
            b: me,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Field {
        dst: pc,
        obj: pv,
        field: p.bp_connected,
    });
    a.jmp(
        Opcode::JFalse {
            cond: pc,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Field {
        dst: pinv,
        obj: pv,
        field: p.bp_inv,
    });
    a.jmp(
        Opcode::JNull {
            reg: pinv,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Call3 {
        dst: v,
        fun: add_panel,
        arg0: bp,
        arg1: pv,
        arg2: jj,
    });
    a.op(Opcode::Incr { dst: cnt });
    a.jmp(Opcode::JAlways { offset: 0 }, "l");
    a.label("done");
    a.jmp(
        Opcode::JSGt {
            a: cnt,
            b: z,
            offset: 0,
        },
        "keep",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: bo,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "out");
    a.label("keep");
    a.op(Opcode::SetGlobal {
        global: g.box_,
        src: bo,
    });
    a.op(Opcode::SetGlobal {
        global: g.gi,
        src: gi,
    });
    a.op(Opcode::Null { dst: ninv });
    a.op(Opcode::SetGlobal {
        global: g.target,
        src: ninv,
    });
    close_trap(&mut a, exc, v, v, report, None);
    push_fn(code, vec![p.bar_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `button(bar)` (WorldButtonsBar constructor end): the bar button.
fn add_button(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    toggle: RefFun,
) -> Result<RefFun> {
    let dk_t = p.blk.props_t;
    let mut r = Regs(vec![p.bar_t]);
    let c = cregs(p, &mut r);
    let (exc, v, dom, cp, ip, co, io, icon, cl, tip, sv, f, game, m, b, cf) = (
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(dk_t),
        r.r(dk_t),
        r.r(dk_t),
        r.r(p.obj_t),
        r.r(p.obj_t),
        r.r(p.icon_t),
        r.r(p.blk.closure_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.bool_t),
        r.r(p.game_t),
        r.r(p.bool_t),
        r.r(p.bool_t),
        r.r(p.flow_t),
    );
    let (bp, bo, badge) = (r.r(dk_t), r.r(p.obj_t), r.r(p.icon_t));
    let (bt, btp, root, rb, idx, one) = (
        r.r(p.icon_t),
        r.r(p.obj_t),
        r.r(p.obj_t),
        r.r(p.bar_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let k1 = int_const(code, 1);
    let icon_g = str_global(code, p.str_t, ICON_ID);
    let badge_g = str_global(code, p.str_t, BADGE_ID);
    let tip_g = str_global(code, p.str_t, TIP);
    let mut a = Asm::new();
    open_trap(&mut a, exc);
    a.op(Opcode::Field {
        dst: dom,
        obj: Reg(0),
        field: p.dom,
    });
    a.jmp(
        Opcode::JNull {
            reg: dom,
            offset: 0,
        },
        "out",
    );
    emit_create(
        &mut a,
        code,
        p,
        &c,
        cp,
        dom,
        "flow",
        None,
        &[("class", "button mpAllInv")],
    );
    emit_create(
        &mut a,
        code,
        p,
        &c,
        ip,
        cp,
        "icon",
        Some(icon_g),
        &[("id", "mpAllInv"), ("networkable", "false")],
    );
    a.op(Opcode::Field {
        dst: co,
        obj: cp,
        field: p.blk.props_obj,
    });
    // Right after btInventory's "button inventory" flow, in the bar's own row:
    // created last, the flow would follow the game-option-icons (pause /
    // settings) the constructor appends at its end (WorldButtonsBar.hx:59).
    a.op(Opcode::Field {
        dst: bt,
        obj: Reg(0),
        field: p.bar_bt,
    });
    a.jmp(Opcode::JNull { reg: bt, offset: 0 }, "placed");
    a.op(Opcode::Field {
        dst: btp,
        obj: bt,
        field: p.parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: btp,
            offset: 0,
        },
        "placed",
    );
    a.op(Opcode::Field {
        dst: root,
        obj: btp,
        field: p.parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "placed",
    );
    a.jmp(Opcode::JNull { reg: co, offset: 0 }, "placed");
    a.op(Opcode::Field {
        dst: btp,
        obj: co,
        field: p.parent,
    });
    a.jmp(
        Opcode::JNotEq {
            a: btp,
            b: root,
            offset: 0,
        },
        "placed",
    );
    a.op(Opcode::Field {
        dst: btp,
        obj: bt,
        field: p.parent,
    });
    a.op(Opcode::SafeCast { dst: rb, src: root });
    a.jmp(Opcode::JNull { reg: rb, offset: 0 }, "placed");
    a.op(Opcode::Call2 {
        dst: idx,
        fun: p.child_index,
        arg0: root,
        arg1: btp,
    });
    a.op(Opcode::Int { dst: one, ptr: k1 });
    a.op(Opcode::Add {
        dst: idx,
        a: idx,
        b: one,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.bar_add_at,
        arg0: rb,
        arg1: co,
        arg2: idx,
    });
    a.label("placed");
    a.op(Opcode::Field {
        dst: io,
        obj: ip,
        field: p.blk.props_obj,
    });
    a.op(Opcode::SafeCast { dst: icon, src: io });
    a.jmp(
        Opcode::JNull {
            reg: icon,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: toggle,
        obj: Reg(0),
    });
    a.op(Opcode::SetField {
        obj: icon,
        field: p.blk.onclick,
        src: cl,
    });
    a.op(Opcode::GetGlobal {
        dst: tip,
        global: tip_g,
    });
    a.op(Opcode::Call2 {
        dst: sv,
        fun: p.set_tip,
        arg0: icon,
        arg1: tip,
    });
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    a.op(Opcode::SetField {
        obj: icon,
        field: p.title_tip,
        src: f,
    });
    // Badge over the icon (its child: fades with it when disabled). It has an
    // interactive of its own (`world-buttons-bar icon { cursor: button }`), so
    // it clicks and tips like the icon.
    emit_create(
        &mut a,
        code,
        p,
        &c,
        bp,
        ip,
        "icon",
        Some(badge_g),
        BADGE_ATTRS,
    );
    a.op(Opcode::Field {
        dst: bo,
        obj: bp,
        field: p.blk.props_obj,
    });
    a.op(Opcode::SafeCast {
        dst: badge,
        src: bo,
    });
    a.jmp(
        Opcode::JNull {
            reg: badge,
            offset: 0,
        },
        "nobadge",
    );
    a.op(Opcode::SetField {
        obj: badge,
        field: p.blk.onclick,
        src: cl,
    });
    a.op(Opcode::Call2 {
        dst: sv,
        fun: p.set_tip,
        arg0: badge,
        arg1: tip,
    });
    a.op(Opcode::SetField {
        obj: badge,
        field: p.title_tip,
        src: f,
    });
    a.label("nobadge");
    a.op(Opcode::SetGlobal {
        global: g.btn,
        src: co,
    });
    a.op(Opcode::SetGlobal {
        global: g.icon,
        src: icon,
    });
    a.op(Opcode::Mov { dst: m, src: f });
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(0),
        field: p.bar_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "vis",
    );
    a.op(Opcode::Call1 {
        dst: m,
        fun: p.is_multi,
        arg0: game,
    });
    a.label("vis");
    a.op(Opcode::SafeCast { dst: cf, src: co });
    a.jmp(Opcode::JNull { reg: cf, offset: 0 }, "out");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.flow_set_visible,
        arg0: cf,
        arg1: m,
    });
    close_trap(&mut a, exc, v, v, report, None);
    push_fn(code, vec![p.bar_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// `tick(ui)` (GameUI.update, every frame, battle included).
#[allow(clippy::too_many_arguments)]
fn add_tick(
    code: &mut Bytecode,
    p: &UiPlan,
    g: &Globals,
    report: RefFun,
    close: RefFun,
    panel_inv: RefFun,
    alive: RefFun,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.ui_t]);
    let (
        exc,
        v,
        game,
        multi,
        btn,
        vis,
        cf,
        b,
        icon,
        bar,
        bt,
        e,
        ie,
        bx,
        gi,
        ogi,
        pa,
        bat,
        ld,
        inv,
        z,
        one,
    ) = (
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(p.game_t),
        r.r(p.bool_t),
        r.r(p.obj_t),
        r.r(p.bool_t),
        r.r(p.flow_t),
        r.r(p.bool_t),
        r.r(p.icon_t),
        r.r(p.bar_t),
        r.r(p.icon_t),
        r.r(p.bool_t),
        r.r(p.bool_t),
        r.r(p.obj_t),
        r.r(p.gi_t),
        r.r(p.gi_t),
        r.r(p.obj_t),
        r.r(p.battle_t),
        r.r(p.bool_t),
        r.r(p.inv_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let l = child_loop(p, &mut r);
    let mut a = Asm::new();
    open_trap(&mut a, exc);
    a.op(Opcode::Field {
        dst: game,
        obj: Reg(0),
        field: p.ui_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: multi,
        fun: p.is_multi,
        arg0: game,
    });
    // button: visible in multiplayer only, enabled like btInventory
    a.op(Opcode::GetGlobal {
        dst: btn,
        global: g.btn,
    });
    a.jmp(
        Opcode::JNull {
            reg: btn,
            offset: 0,
        },
        "panels",
    );
    a.op(Opcode::Field {
        dst: vis,
        obj: btn,
        field: p.visible,
    });
    a.jmp(
        Opcode::JEq {
            a: vis,
            b: multi,
            offset: 0,
        },
        "en",
    );
    a.op(Opcode::SafeCast { dst: cf, src: btn });
    a.jmp(Opcode::JNull { reg: cf, offset: 0 }, "en");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.flow_set_visible,
        arg0: cf,
        arg1: multi,
    });
    a.label("en");
    a.op(Opcode::GetGlobal {
        dst: icon,
        global: g.icon,
    });
    a.jmp(
        Opcode::JNull {
            reg: icon,
            offset: 0,
        },
        "panels",
    );
    a.op(Opcode::Field {
        dst: bar,
        obj: Reg(0),
        field: p.ui_bar,
    });
    a.jmp(
        Opcode::JNull {
            reg: bar,
            offset: 0,
        },
        "panels",
    );
    a.op(Opcode::Field {
        dst: bt,
        obj: bar,
        field: p.bar_bt,
    });
    a.jmp(Opcode::JNull { reg: bt, offset: 0 }, "panels");
    a.op(Opcode::Field {
        dst: e,
        obj: bt,
        field: p.enable,
    });
    a.op(Opcode::Field {
        dst: ie,
        obj: icon,
        field: p.enable,
    });
    a.jmp(
        Opcode::JEq {
            a: e,
            b: ie,
            offset: 0,
        },
        "panels",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_enable,
        arg0: icon,
        arg1: e,
    });
    // panels: closed when no longer usable, a disconnected player's removed
    a.label("panels");
    a.op(Opcode::GetGlobal {
        dst: bx,
        global: g.box_,
    });
    a.jmp(Opcode::JNull { reg: bx, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: gi,
        obj: Reg(0),
        field: p.ui_gi,
    });
    a.jmp(Opcode::JNull { reg: gi, offset: 0 }, "shut");
    a.op(Opcode::GetGlobal {
        dst: ogi,
        global: g.gi,
    });
    a.jmp(
        Opcode::JNotEq {
            a: gi,
            b: ogi,
            offset: 0,
        },
        "shut",
    );
    a.op(Opcode::Field {
        dst: pa,
        obj: bx,
        field: p.parent,
    });
    a.jmp(Opcode::JNull { reg: pa, offset: 0 }, "shut");
    a.jmp(
        Opcode::JFalse {
            cond: multi,
            offset: 0,
        },
        "shut",
    );
    a.op(Opcode::Field {
        dst: bat,
        obj: game,
        field: p.game_battle,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: bat,
            offset: 0,
        },
        "shut",
    );
    a.op(Opcode::Field {
        dst: ld,
        obj: game,
        field: p.game_loading,
    });
    a.jmp(
        Opcode::JTrue {
            cond: ld,
            offset: 0,
        },
        "shut",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.inv_allowed,
        arg0: gi,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "shut");
    a.op(Opcode::Field {
        dst: l.ch,
        obj: bx,
        field: p.children,
    });
    a.jmp(
        Opcode::JNull {
            reg: l.ch,
            offset: 0,
        },
        "shut",
    );
    a.op(Opcode::Int {
        dst: z,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::Int {
        dst: one,
        ptr: int_const(code, 1),
    });
    a.op(Opcode::Field {
        dst: l.i,
        obj: l.ch,
        field: p.arr_len,
    });
    a.loop_head("l");
    a.jmp(
        Opcode::JSLte {
            a: l.i,
            b: z,
            offset: 0,
        },
        "after",
    );
    a.op(Opcode::Sub {
        dst: l.i,
        a: l.i,
        b: one,
    });
    a.op(Opcode::Field {
        dst: l.raw,
        obj: l.ch,
        field: p.arr_raw,
    });
    a.op(Opcode::GetArray {
        dst: l.d,
        array: l.raw,
        index: l.i,
    });
    a.op(Opcode::UnsafeCast { dst: l.c, src: l.d });
    a.jmp(
        Opcode::JNull {
            reg: l.c,
            offset: 0,
        },
        "l",
    );
    a.op(Opcode::Call1 {
        dst: inv,
        fun: panel_inv,
        arg0: l.c,
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: alive,
        arg0: inv,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "l");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: l.c,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "l");
    a.label("after");
    a.op(Opcode::Field {
        dst: l.n,
        obj: l.ch,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGt {
            a: l.n,
            b: z,
            offset: 0,
        },
        "out",
    );
    a.label("shut");
    a.op(Opcode::Call0 { dst: v, fun: close });
    close_trap(&mut a, exc, v, v, report, None);
    push_fn(code, vec![p.ui_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

/// Appended functions, by role.
pub(crate) struct UiFns {
    pub(crate) foreign: RefFun,
    pub(crate) give: RefFun,
    pub(crate) hover: RefFun,
    pub(crate) toggle: RefFun,
    pub(crate) button: RefFun,
    pub(crate) tick: RefFun,
}

fn build(code: &mut Bytecode, p: &UiPlan) -> Result<UiFns> {
    let drag = window_drag::api(code)?;
    let g = Globals {
        box_: new_global(code, p.obj_t),
        gi: new_global(code, p.gi_t),
        target: new_global(code, p.inv_t),
        btn: new_global(code, p.obj_t),
        icon: new_global(code, p.icon_t),
        logs: new_global(code, p.i32_t),
        err: str_global(code, p.str_t, S_ERR),
    };
    let report = add_report(code, p, &g)?;
    let noop = add_noop(code, p)?;
    let slot_inv = add_slot_inv(code, p)?;
    let foreign = add_foreign(code, p, &g, report, slot_inv)?;
    let alive = add_alive(code, p)?;
    let panel_inv = add_panel_inv(code, p)?;
    let target = add_target(code, p, &g, alive, panel_inv)?;
    let close = add_close(code, p, &g)?;
    let close_panel = add_close_panel(code, p, &g, report, close)?;
    let give_n = add_give_n(code, p, report, target, noop)?;
    let give = add_give(code, p, &g, report, foreign, slot_inv, target, give_n)?;
    let hover = add_hover(code, p, &g, report, foreign)?;
    let add_panel = add_add_panel(code, p, close_panel, drag.panel)?;
    let toggle = add_toggle(code, p, &g, report, close, add_panel)?;
    let button = add_button(code, p, &g, report, toggle)?;
    let tick = add_tick(code, p, &g, report, close, panel_inv, alive)?;
    for (f, n) in [
        (report, "mpAllInvReport"),
        (slot_inv, "mpAllInvSlotInv"),
        (foreign, N_FOREIGN),
        (alive, "mpAllInvAlive"),
        (panel_inv, "mpAllInvPanelInv"),
        (target, "mpAllInvTarget"),
        (close, "mpAllInvClose"),
        (close_panel, "mpAllInvClosePanel"),
        (give_n, "mpAllInvGiveN"),
        (give, N_GIVE),
        (hover, N_HOVER),
        (add_panel, "mpAllInvAddPanel"),
        (toggle, N_TOGGLE),
        (button, N_BUTTON),
        (tick, N_TICK),
    ] {
        name_fn(code, f, n);
    }
    Ok(UiFns {
        foreign,
        give,
        hover,
        toggle,
        button,
        tick,
    })
}

fn new_reg(f: &mut Function, t: RefType) -> Reg {
    f.regs.push(t);
    Reg((f.regs.len() - 1) as u32)
}

fn ui_apply(code: &mut Bytecode, p: &UiPlan) -> Result<UiFns> {
    let fns = build(code, p)?;
    for fi in [p.allow_pick_fi, p.allow_drop_fi, p.do_pick_fi, p.drop_fi] {
        let ret = code.functions[fi].t.as_fun(code).context("gate type")?.ret;
        gate(code, p, fi, fns.foreign, ret);
    }
    // onRightClick: if (give(this)) return;
    {
        let f = &mut code.functions[p.right_click_fi];
        let b = new_reg(f, p.bool_t);
        let v = new_reg(f, p.void_t);
        insert_ops(
            f,
            0,
            vec![
                Opcode::Call1 {
                    dst: b,
                    fun: fns.give,
                    arg0: Reg(0),
                },
                Opcode::JFalse { cond: b, offset: 1 },
                Opcode::Ret { ret: v },
            ],
        );
    }
    // getTipContent: hover(this);
    {
        let f = &mut code.functions[p.tip_fi];
        let v = new_reg(f, p.void_t);
        insert_ops(
            f,
            0,
            vec![Opcode::Call1 {
                dst: v,
                fun: fns.hover,
                arg0: Reg(0),
            }],
        );
    }
    // GameUI.update: tick(this);
    {
        let f = &mut code.functions[p.ui_update_fi];
        let v = new_reg(f, p.void_t);
        insert_ops(
            f,
            0,
            vec![Opcode::Call1 {
                dst: v,
                fun: fns.tick,
                arg0: Reg(0),
            }],
        );
    }
    // WorldButtonsBar constructor: button(this) before the Ret.
    {
        let f = &mut code.functions[p.bar_ctor_fi];
        let v = new_reg(f, p.void_t);
        insert_ops(
            f,
            p.bar_ret,
            vec![Opcode::Call1 {
                dst: v,
                fun: fns.button,
                arg0: Reg(0),
            }],
        );
    }
    eprintln!(
        "patched all inventories: bar button fn@{} (toggle fn@{}), tick fn@{} in GameUI.update, slot gates fn@{}, give fn@{}, hover fn@{}",
        fns.button.0, fns.toggle.0, fns.tick.0, fns.foreign.0, fns.give.0, fns.hover.0
    );
    Ok(fns)
}

/// `if (foreign(this) != null) return <false | void>;` at op 0.
fn gate(code: &mut Bytecode, p: &UiPlan, fi: usize, foreign: RefFun, ret_t: RefType) {
    let f = &mut code.functions[fi];
    let inv = new_reg(f, p.inv_t);
    let rv = new_reg(f, ret_t);
    let mut ops = vec![Opcode::Call1 {
        dst: inv,
        fun: foreign,
        arg0: Reg(0),
    }];
    if ret_t == p.bool_t {
        ops.push(Opcode::JNull {
            reg: inv,
            offset: 2,
        });
        ops.push(Opcode::Bool {
            dst: rv,
            value: ValBool(false),
        });
    } else {
        ops.push(Opcode::JNull {
            reg: inv,
            offset: 1,
        });
    }
    ops.push(Opcode::Ret { ret: rv });
    insert_ops(f, 0, ops);
}

/// All three parts, or none (each needs the others: without N a give loses
/// the item, without S a take loses it, without U nothing uses them).
pub(crate) fn patch_all_inv(code: &mut Bytecode) {
    let np = net_plan(code);
    let sp = api_plan(code);
    let up = ui_plan(code);
    let (np, sp, up) = match (np, sp, up) {
        (Ok(n), Ok(s), Ok(u)) => (n, s, u),
        (n, s, u) => {
            for (what, e) in [
                ("permission", n.err()),
                ("slot api", s.err()),
                ("ui", u.err()),
            ] {
                if let Some(e) = e {
                    eprintln!("all inventories skipped ({what}): {e:#}");
                }
            }
            return;
        }
    };
    let snap = Snap::take(code);
    let before: Vec<(usize, Function)> = [
        np.fi,
        sp.fi,
        up.bar_ctor_fi,
        up.ui_update_fi,
        up.allow_pick_fi,
        up.allow_drop_fi,
        up.do_pick_fi,
        up.drop_fi,
        up.right_click_fi,
        up.tip_fi,
    ]
    .iter()
    .map(|&i| (i, code.functions[i].clone()))
    .collect();
    if let Err(e) = ui_apply(code, &up) {
        eprintln!("all inventories skipped: {e:#}");
        for (i, f) in before {
            code.functions[i] = f;
        }
        snap.restore(code);
        return;
    }
    net_apply(code, &np);
    api_apply(code, &sp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, shifted, write, HLBOOT};
    use std::collections::HashMap;

    /// Ops of a gate: call, jump, (Bool false,) Ret.
    const GATE_BOOL_OPS: usize = 4;
    const GATE_VOID_OPS: usize = 3;

    fn same(a: &Function, b: &Function) -> bool {
        format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs
    }

    /// Each Trap is closed by an EndTrap on its register, nothing jumps out of
    /// the protected block, and the handler follows a Ret.
    fn traps_ok(f: &Function) -> usize {
        let n = f.ops.len();
        let mut traps = 0;
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Trap { exc, .. } = *op else {
                continue;
            };
            traps += 1;
            let [handler] = jump_targets(f, i)[..] else {
                unreachable!()
            };
            let end = (i + 1..n)
                .find(|&j| matches!(f.ops[j], Opcode::EndTrap { exc: e } if e == exc))
                .expect("EndTrap");
            assert!(handler > end);
            assert!(matches!(f.ops[handler - 1], Opcode::Ret { .. }));
            for j in i + 1..end {
                assert!(!matches!(
                    f.ops[j],
                    Opcode::Ret { .. } | Opcode::Trap { .. }
                ));
                for t in jump_targets(f, j) {
                    assert!(
                        t > i && t <= end,
                        "fn@{} op {j} leaves the trap",
                        f.findex.0
                    );
                }
            }
        }
        traps
    }

    struct Plans {
        n: NetPlan,
        s: ApiPlan,
        u: UiPlan,
    }

    fn plans(code: &Bytecode) -> Plans {
        Plans {
            n: net_plan(code).expect("permission plan"),
            s: api_plan(code).expect("slot api plan"),
            u: ui_plan(code).expect("ui plan"),
        }
    }

    /// Sites found; only the ten site functions change; window_drag's API_FNS and
    /// this pass's 16 functions are appended, type-check and trap where vanilla
    /// calls them; a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plans(&orig);
        assert_eq!(
            (p.n.id_op, p.n.id_add),
            (7, 0),
            "hxbit RPC ids in 1.0.48274"
        );
        assert_eq!(orig.functions[p.s.fi].findex.0, 39420);
        assert_eq!((p.s.at, p.s.raw), (45, 84));
        assert_eq!(p.u.found, 4);
        let mut code = read(&image);
        patch_all_inv(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + window_drag::API_FNS + 16);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        let u = &p.u;
        let sites = [
            p.n.fi,
            p.s.fi,
            u.bar_ctor_fi,
            u.ui_update_fi,
            u.allow_pick_fi,
            u.allow_drop_fi,
            u.do_pick_fi,
            u.drop_fi,
            u.right_click_fi,
            u.tip_fi,
        ];
        for i in 0..n {
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                sites.contains(&i),
                "function #{i}"
            );
        }

        // N: ten ops rewritten in place, nothing else.
        let (a, b) = (&orig.functions[p.n.fi], &back.functions[p.n.fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        for i in (0..41).chain(51..56) {
            assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[i]));
        }
        check_types(&back, b, 41..51);
        check_flow(b);

        // S: four ops after the MoveTo test; the new jump lands on the raw send.
        let (a, b) = (&orig.functions[p.s.fi], &back.functions[p.s.fi]);
        shifted(a, b, p.s.at, 4);
        assert_eq!(jump_targets(b, p.s.at + 3), [p.s.raw + 4]);
        check_types(&back, b, p.s.at..p.s.at + 4);
        check_flow(b);

        // U sites: gates, give, hover, tick at op 0; button before the bar's Ret.
        for (fi, k) in [
            (u.allow_pick_fi, GATE_BOOL_OPS),
            (u.allow_drop_fi, GATE_BOOL_OPS),
            (u.do_pick_fi, GATE_VOID_OPS),
            (u.drop_fi, GATE_BOOL_OPS),
            (u.right_click_fi, 3),
            (u.tip_fi, 1),
            (u.ui_update_fi, 1),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, 0, k);
            check_types(&back, b, 0..k);
            check_flow(b);
        }
        let (a, b) = (
            &orig.functions[u.bar_ctor_fi],
            &back.functions[u.bar_ctor_fi],
        );
        shifted(a, b, u.bar_ret, 1);
        check_types(&back, b, u.bar_ret..u.bar_ret + 1);
        check_flow(b);
        // appended after window_drag's API_FNS, in build() order: ... button (14), tick (15)
        let mine = |k: usize| back.functions[n + window_drag::API_FNS + k].findex;
        assert!(
            matches!(b.ops[u.bar_ret], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == mine(14))
        );
        let up = &back.functions[u.ui_update_fi];
        assert!(matches!(up.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == mine(15)));
        let rc = &back.functions[u.right_click_fi];
        assert!(matches!(rc.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == mine(10)));
        let tip = &back.functions[u.tip_fi];
        assert!(matches!(tip.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == mine(11)));
        for fi in [u.allow_pick_fi, u.allow_drop_fi, u.do_pick_fi, u.drop_fi] {
            let g = &back.functions[fi];
            assert!(matches!(g.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == mine(3)));
        }

        let mut traps = 0;
        for f in &back.functions[n..] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
            traps += traps_ok(f);
        }
        assert_eq!(traps, 13 + 8); // window_drag's 13 + ours

        // Idempotent: every part refuses, the image stays as is.
        let mut again = read(&patched);
        assert!(net_plan(&again).is_err());
        assert!(api_plan(&again).is_err());
        assert!(ui_plan(&again).is_err());
        patch_all_inv(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass runs in the full pipeline (after take_all / window_drag).
    #[test]
    fn applies_in_pipeline() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let out = crate::patch_image(&image).expect("patch");
        let code = read(&out);
        assert!(ui_applied(&code));
        for (what, r) in [
            ("permission", net_plan(&code).err()),
            ("slot api", api_plan(&code).err()),
            ("ui", ui_plan(&code).err()),
        ] {
            let e = r.unwrap_or_else(|| panic!("{what}: not applied"));
            assert_eq!(format!("{e:#}"), "already applied", "{what}");
        }
    }

    /// A mismatch in any part skips the whole pass.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plans(&orig);
        let (nfi, sfi, sat, rfi, bfi, bret) = (
            p.n.fi,
            p.s.fi,
            p.s.at,
            p.u.right_click_fi,
            p.u.bar_ctor_fi,
            p.u.bar_ret,
        );
        let breakers: [(&str, Box<dyn Fn(&mut Bytecode)>); 4] = [
            (
                "networkAllow",
                Box::new(move |c: &mut Bytecode| c.functions[nfi].ops[50] = Opcode::Label),
            ),
            (
                "slot api closure",
                Box::new(move |c: &mut Bytecode| c.functions[sfi].ops[sat] = Opcode::Label),
            ),
            (
                "onRightClick",
                Box::new(move |c: &mut Bytecode| c.functions[rfi].ops[1] = Opcode::Label),
            ),
            (
                "bar constructor",
                Box::new(move |c: &mut Bytecode| {
                    insert_ops(
                        &mut c.functions[bfi],
                        bret,
                        vec![Opcode::JAlways { offset: 0 }],
                    )
                }),
            ),
        ];
        for (what, brk) in breakers {
            let mut code = read(&image);
            brk(&mut code);
            let before = write(&code);
            patch_all_inv(&mut code);
            assert!(write(&code) == before, "{what}: image changed");
        }
    }

    // ---------- behaviour: a tiny interpreter ----------

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        B(bool),
        I(i32),
        F(f64),
        O(usize),
        En(usize, Vec<V>),
        Clo(RefFun, Box<V>),
        S(String),
    }

    /// Hidden heap keys: class tag (isOfType) and stub results.
    const TAG: usize = usize::MAX;
    const UINV: usize = usize::MAX - 1;
    const ITEM: usize = usize::MAX - 2;
    const COUNT: usize = usize::MAX - 3;
    const LOCKED: usize = usize::MAX - 4;
    /// createNew's attrs object, on the created object.
    const ATTRS: usize = usize::MAX - 5;
    /// DynSet fields: DYN + the field name's string index.
    const DYN: usize = 1 << 40;

    struct Sim<'a> {
        code: &'a Bytecode,
        p: &'a UiPlan,
        orig_n: usize,
        globals: HashMap<usize, V>,
        heap: Vec<HashMap<usize, V>>,
        ctrl: bool,
        shift: bool,
        multi: bool,
        /// window_drag's mpDragPanel, stubbed.
        drag: Option<RefFun>,
        log: Vec<(&'static str, Vec<V>)>,
    }

    impl Sim<'_> {
        fn obj(&mut self, fields: &[(usize, V)]) -> V {
            self.heap.push(fields.iter().cloned().collect());
            V::O(self.heap.len() - 1)
        }
        fn get(&self, o: &V, f: usize) -> V {
            let V::O(i) = o else {
                panic!("null access field {f}")
            };
            self.heap[*i].get(&f).cloned().unwrap_or(V::Null)
        }
        fn set(&mut self, o: &V, f: usize, v: V) {
            let V::O(i) = o else { panic!("set on {o:?}") };
            self.heap[*i].insert(f, v);
        }
        /// An ArrayObj holding `items`.
        fn arr(&mut self, items: Vec<V>) -> V {
            let n = items.len() as i32;
            let raw = self.obj(&[]);
            for (k, v) in items.into_iter().enumerate() {
                self.set(&raw, k, v);
            }
            let (len, a) = (self.p.arr_len.0, self.p.arr_raw.0);
            self.obj(&[(len, V::I(n)), (a, raw)])
        }
        fn children(&self, o: &V) -> Vec<V> {
            let ch = self.get(o, self.p.children.0);
            if ch == V::Null {
                return vec![];
            }
            let n = match self.get(&ch, self.p.arr_len.0) {
                V::I(n) => n as usize,
                _ => 0,
            };
            let raw = self.get(&ch, self.p.arr_raw.0);
            (0..n).map(|k| self.get(&raw, k)).collect()
        }
        fn set_children(&mut self, o: &V, items: Vec<V>) {
            for c in &items {
                self.set(c, self.p.parent.0, o.clone());
            }
            let a = self.arr(items);
            self.set(o, self.p.children.0, a);
        }
        fn global(&self, g: RefGlobal) -> V {
            if let Some(v) = self.globals.get(&g.0) {
                return v.clone();
            }
            match crate::job_xp::const_str(self.code, g) {
                Some(s) => V::S(s.to_string()),
                None => V::Null,
            }
        }

        fn stub(&mut self, f: RefFun, a: &[V]) -> Option<V> {
            let p = self.p;
            let v = if f == p.is_of_type {
                V::B(matches!(a[0], V::O(_)) && self.get(&a[0], TAG) == a[1])
            } else if f == p.get_inventory {
                self.get(&a[0], UINV)
            } else if f == p.ctrl_down {
                V::B(self.ctrl)
            } else if f == p.is_down {
                assert_eq!(a[0], V::I(p.shift_key));
                V::B(self.shift)
            } else if f == p.get_item {
                self.get(&a[0], ITEM)
            } else if f == p.get_count {
                self.get(&a[0], COUNT)
            } else if f == p.get_locked {
                V::B(self.get(&a[0], LOCKED) == V::B(true))
            } else if f == p.is_multi {
                V::B(self.multi)
            } else if f == p.inv_allowed {
                V::B(true)
            } else if f == p.blk.create {
                // createNew(comp, parentProps, args, attrs): a child object of the parent's
                let o = self.obj(&[(TAG, a[0].clone()), (ATTRS, a[3].clone())]);
                let props = self.obj(&[(p.blk.props_obj.0, o.clone())]);
                if a[1] != V::Null {
                    let po = self.get(&a[1], p.blk.props_obj.0);
                    let mut ch = self.children(&po);
                    ch.push(o.clone());
                    self.set_children(&po, ch);
                    self.set(&o, p.parent.0, po);
                }
                self.log.push(("create", vec![a[0].clone()]));
                props
            } else if f == p.blk.alloc || f == p.blk.wrap || f == p.blk.dyn_alloc {
                V::Null
            } else if f == p.std_string {
                match &a[0] {
                    V::I(n) => V::S(n.to_string()),
                    o => panic!("Std.string {o:?}"),
                }
            } else if f == p.str_add {
                match (&a[0], &a[1]) {
                    (V::S(x), V::S(y)) => V::S(format!("{x}{y}")),
                    o => panic!("String add {o:?}"),
                }
            } else if f == p.uinv_ctor {
                self.log.push(("inventory", a.to_vec()));
                V::Null
            } else if f == p.user_name {
                V::S("nick".into())
            } else if f == p.set_text
                || f == p.set_tip
                || f == p.set_enable
                || f == p.flow_set_visible
            {
                self.log.push(("set", a.to_vec()));
                V::Null
            } else if Some(f) == self.drag {
                self.log.push(("drag", a.to_vec()));
                V::Null
            } else if f == p.child_index {
                let ch = self.children(&a[0]);
                V::I(ch.iter().position(|c| *c == a[1]).map_or(-1, |i| i as i32))
            } else if f == p.bar_add_at {
                let bo = a[0].clone();
                let mut ch: Vec<V> = self
                    .children(&bo)
                    .into_iter()
                    .filter(|c| *c != a[1])
                    .collect();
                let V::I(i) = a[2] else {
                    panic!("addChildAt index")
                };
                ch.insert(i as usize, a[1].clone());
                self.set_children(&bo, ch);
                self.log.push(("addChildAt", a.to_vec()));
                V::Null
            } else if f == p.select_amount {
                self.log.push(("select", a.to_vec()));
                V::Null
            } else if f == p.remove {
                // detach from the parent's children
                let pa = self.get(&a[0], p.parent.0);
                if pa != V::Null {
                    let rest: Vec<V> = self
                        .children(&pa)
                        .into_iter()
                        .filter(|c| *c != a[0])
                        .collect();
                    let ar = self.arr(rest);
                    self.set(&pa, p.children.0, ar);
                    self.set(&a[0], p.parent.0, V::Null);
                }
                self.log.push(("remove", a.to_vec()));
                V::Null
            } else {
                return None;
            };
            Some(v)
        }

        fn call(&mut self, f: RefFun, args: Vec<V>) -> V {
            if let Some(v) = self.stub(f, &args) {
                return v;
            }
            let fi = index_of(self.code, f).unwrap();
            assert!(fi >= self.orig_n, "unexpected vanilla call fn@{}", f.0);
            self.run(f, args)
        }

        fn run(&mut self, f: RefFun, args: Vec<V>) -> V {
            let code = self.code;
            let fun = &code.functions[index_of(code, f).unwrap()];
            let mut r = vec![V::Null; fun.regs.len()];
            for (i, a) in args.into_iter().enumerate() {
                r[i] = a;
            }
            let mut pc = 0usize;
            let num = |v: &V| match v {
                V::I(x) => *x,
                o => panic!("not an int: {o:?}"),
            };
            loop {
                let op = &fun.ops[pc];
                let mut next = pc + 1;
                let jump = |off: i32| (pc as i64 + 1 + off as i64) as usize;
                let rr = |x: &Reg| x.0 as usize;
                match op {
                    Opcode::Label
                    | Opcode::Trap { .. }
                    | Opcode::EndTrap { .. }
                    | Opcode::SetArray { .. } => {}
                    // kept under DYN + the field name's string index
                    Opcode::DynSet { obj, field, src } => {
                        let o = r[rr(obj)].clone();
                        self.set(&o, DYN + field.0, r[rr(src)].clone());
                    }
                    Opcode::Float { dst, ptr } => r[rr(dst)] = V::F(code.floats[ptr.0]),
                    Opcode::ToInt { dst, src } => {
                        let V::F(x) = r[rr(src)] else { panic!("ToInt") };
                        r[rr(dst)] = V::I(x as i32)
                    }
                    Opcode::Type { dst, .. } => r[rr(dst)] = V::Null,
                    Opcode::NullCheck { reg } => assert_ne!(r[rr(reg)], V::Null, "null check"),
                    Opcode::Bool { dst, value } => r[rr(dst)] = V::B(value.0),
                    Opcode::Int { dst, ptr } => r[rr(dst)] = V::I(code.ints[ptr.0]),
                    Opcode::Null { dst } => r[rr(dst)] = V::Null,
                    Opcode::Mov { dst, src }
                    | Opcode::SafeCast { dst, src }
                    | Opcode::UnsafeCast { dst, src }
                    | Opcode::ToVirtual { dst, src }
                    | Opcode::ToDyn { dst, src } => r[rr(dst)] = r[rr(src)].clone(),
                    Opcode::Ref { dst, .. } => r[rr(dst)] = V::Null,
                    Opcode::GetGlobal { dst, global } => r[rr(dst)] = self.global(*global),
                    Opcode::SetGlobal { global, src } => {
                        self.globals.insert(global.0, r[rr(src)].clone());
                    }
                    Opcode::Field { dst, obj, field } => {
                        r[rr(dst)] = self.get(&r[rr(obj)], field.0)
                    }
                    Opcode::GetThis { dst, field } => r[rr(dst)] = self.get(&r[0], field.0),
                    Opcode::SetField { obj, field, src } => {
                        let (o, v) = (r[rr(obj)].clone(), r[rr(src)].clone());
                        self.set(&o, field.0, v);
                    }
                    Opcode::New { dst } => r[rr(dst)] = self.obj(&[]),
                    Opcode::GetArray { dst, array, index } => {
                        let i = num(&r[rr(index)]) as usize;
                        r[rr(dst)] = self.get(&r[rr(array)], i);
                    }
                    Opcode::Incr { dst } => r[rr(dst)] = V::I(num(&r[rr(dst)]) + 1),
                    Opcode::Sub { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) - num(&r[rr(b)])),
                    Opcode::Add { dst, a, b } => {
                        r[rr(dst)] = match (&r[rr(a)], &r[rr(b)]) {
                            (V::F(x), V::F(y)) => V::F(x + y),
                            (x, y) => V::I(num(x) + num(y)),
                        }
                    }
                    Opcode::EnumIndex { dst, value } => {
                        let V::En(k, _) = &r[rr(value)] else {
                            panic!("not an enum")
                        };
                        r[rr(dst)] = V::I(*k as i32);
                    }
                    Opcode::MakeEnum {
                        dst,
                        construct,
                        args,
                    } => {
                        r[rr(dst)] =
                            V::En(construct.0, args.iter().map(|x| r[rr(x)].clone()).collect());
                    }
                    Opcode::StaticClosure { dst, fun } => {
                        r[rr(dst)] = V::Clo(*fun, Box::new(V::Null))
                    }
                    Opcode::InstanceClosure { dst, fun, obj } => {
                        r[rr(dst)] = V::Clo(*fun, Box::new(r[rr(obj)].clone()))
                    }
                    Opcode::CallMethod { dst, field, args } => {
                        assert_eq!(*field, self.p.api_net_op);
                        let a: Vec<V> = args.iter().map(|x| r[rr(x)].clone()).collect();
                        self.log.push(("netop", a[1..].to_vec()));
                        r[rr(dst)] = V::Null;
                    }
                    Opcode::JAlways { offset } => next = jump(*offset),
                    Opcode::JTrue { cond, offset } => {
                        if r[rr(cond)] == V::B(true) {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JFalse { cond, offset } => {
                        if r[rr(cond)] != V::B(true) {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JNull { reg, offset } => {
                        if r[rr(reg)] == V::Null {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JNotNull { reg, offset } => {
                        if r[rr(reg)] != V::Null {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JEq { a, b, offset } => {
                        if r[rr(a)] == r[rr(b)] {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JNotEq { a, b, offset } => {
                        if r[rr(a)] != r[rr(b)] {
                            next = jump(*offset)
                        }
                    }
                    Opcode::JSGte { a, b, offset }
                    | Opcode::JSGt { a, b, offset }
                    | Opcode::JSLte { a, b, offset } => {
                        let fl = |v: &V| match v {
                            V::F(x) => *x,
                            o => num(o) as f64,
                        };
                        let (x, y) = (fl(&r[rr(a)]), fl(&r[rr(b)]));
                        let t = match op {
                            Opcode::JSGte { .. } => x >= y,
                            Opcode::JSGt { .. } => x > y,
                            _ => x <= y,
                        };
                        if t {
                            next = jump(*offset)
                        }
                    }
                    Opcode::Call0 { dst, fun } => r[rr(dst)] = self.call(*fun, vec![]),
                    Opcode::Call1 { dst, fun, arg0 } => {
                        let a = vec![r[rr(arg0)].clone()];
                        r[rr(dst)] = self.call(*fun, a)
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } => {
                        let a = vec![r[rr(arg0)].clone(), r[rr(arg1)].clone()];
                        r[rr(dst)] = self.call(*fun, a)
                    }
                    Opcode::Call3 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                        arg2,
                    } => {
                        let a = vec![
                            r[rr(arg0)].clone(),
                            r[rr(arg1)].clone(),
                            r[rr(arg2)].clone(),
                        ];
                        r[rr(dst)] = self.call(*fun, a)
                    }
                    Opcode::Call4 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                        arg2,
                        arg3,
                    } => {
                        let a = [arg0, arg1, arg2, arg3]
                            .iter()
                            .map(|x| r[rr(x)].clone())
                            .collect();
                        r[rr(dst)] = self.call(*fun, a)
                    }
                    Opcode::CallN { dst, fun, args } => {
                        let a = args.iter().map(|x| r[rr(x)].clone()).collect();
                        r[rr(dst)] = self.call(*fun, a)
                    }
                    Opcode::Ret { ret } => return r[rr(ret)].clone(),
                    o => panic!("interpreter: unsupported {o:?}"),
                }
                pc = next;
            }
        }
    }

    /// networkAllow decisions, vanilla vs patched, for an owned inventory and a
    /// container: only modes 0 / 6 of RPCs 7 / 0 open up, for other players.
    #[test]
    fn network_allow_behaviour() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut orig = read(&image);
        let p = plans(&orig);
        let mut code = read(&image);
        patch_all_inv(&mut code);
        let f = orig.functions[p.n.fi].clone();
        let allow = f.findex;
        let fld = |i: usize| match f.ops[i] {
            Opcode::Field { field, .. } | Opcode::GetThis { field, .. } => field.0,
            _ => panic!("op {i}"),
        };
        let (owner_f, inst_f, state_f, player_f) = (fld(1), fld(12), fld(14), fld(20));
        let Opcode::GetGlobal { global: game_g, .. } = f.ops[11] else {
            panic!()
        };
        // vanilla: networkGetName (its result is unused) stubbed to null
        let Opcode::Call3 { dst, .. } = f.ops[50] else {
            panic!()
        };
        orig.functions[p.n.fi].ops[50] = Opcode::Null { dst };
        let decide = |c: &Bytecode, owned: bool, mode: i32, id: i32, client: u8| {
            let mut sim = Sim {
                code: c,
                p: &p.u,
                orig_n: 0,
                globals: HashMap::new(),
                heap: vec![],
                ctrl: false,
                shift: false,
                multi: true,
                drag: None,
                log: vec![],
            };
            let (a, b) = (sim.obj(&[]), sim.obj(&[]));
            let st = sim.obj(&[(player_f, a.clone())]);
            let game = sim.obj(&[(state_f, st)]);
            let cls = sim.obj(&[(inst_f, game)]);
            sim.globals.insert(game_g.0, cls);
            let inv = sim.obj(&[(owner_f, if owned { a.clone() } else { V::Null })]);
            let cl = match client {
                0 => a,
                1 => b,
                _ => V::Null,
            };
            sim.run(allow, vec![inv, V::I(mode), V::I(id), cl])
        };
        for owned in [true, false] {
            for mode in 0..=6 {
                for id in 0..=10 {
                    for client in 0..3u8 {
                        let before = decide(&orig, owned, mode, id, client);
                        let after = decide(&code, owned, mode, id, client);
                        let opened = owned
                            && (mode == 0 || mode == 6)
                            && (id == 7 || id == 0)
                            && client == 1;
                        let what = format!("owned {owned} mode {mode} id {id} client {client}");
                        if opened {
                            assert_eq!((&before, &after), (&V::B(false), &V::B(true)), "{what}");
                        } else {
                            assert_eq!(before, after, "{what}");
                        }
                        if owned {
                            // the owner keeps every right, a non-player client gets none
                            let want = client == 0 || opened;
                            assert_eq!(after, V::B(want), "{what}");
                        }
                    }
                }
            }
        }
    }

    /// S: an owned source inventory takes the raw send, a container keeps the
    /// Remove conversion.
    #[test]
    fn slot_api_routing() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plans(&orig);
        let mut code = read(&image);
        patch_all_inv(&mut code);
        let f = &code.functions[p.s.fi];
        let at = p.s.at;
        let Opcode::EnumField {
            construct, field, ..
        } = f.ops[at]
        else {
            panic!()
        };
        assert_eq!((construct, field), p.s.ctx);
        assert!(matches!(f.ops[at + 2], Opcode::Field { field, .. } if field == p.s.owner));
        // owner set: the JNotNull lands on the raw block, which sends reg1 (the MoveTo)
        let raw = jump_targets(f, at + 3)[0];
        assert_eq!(raw, p.s.raw + 4);
        assert!(f.ops[raw..]
            .iter()
            .take(16)
            .any(|o| matches!(o, Opcode::CallN { args, .. } if args.get(2) == Some(&Reg(1)))));
        // owner null: falls through to vanilla's MoveTo -> Remove conversion
        assert!(matches!(
            f.ops[at + 4],
            Opcode::EnumField { value: Reg(1), .. }
        ));
        assert!(f.ops[at + 4..raw]
            .iter()
            .any(|o| matches!(o, Opcode::MakeEnum { .. })));
    }

    /// Players A (me), B, C; panels for B and C. Foreign detection, give target,
    /// ctrl / shift give, locked items, disconnect and battle in the tick.
    #[test]
    fn give_take_behaviour() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let pl = plans(&orig);
        let mut code = read(&image);
        patch_all_inv(&mut code);
        let u = &pl.u;
        let fun = |n: &str| find_named(&code, n).expect(n);
        let (foreign, give, tick) = (fun(N_FOREIGN), fun(N_GIVE), fun(N_TICK));
        let (target, give_n) = (fun("mpAllInvTarget"), fun("mpAllInvGiveN"));
        // build() appends, after window_drag's globals: box, gi, target, btn, icon, logs
        let gi_g = (orig.globals.len()..code.globals.len())
            .find(|&i| code.globals[i] == u.gi_t)
            .expect("gi global");
        let (gbox, ggi, gtarget) = (RefGlobal(gi_g - 1), RefGlobal(gi_g), RefGlobal(gi_g + 1));
        assert_eq!(code.globals[gbox.0], u.obj_t);
        assert_eq!(code.globals[gtarget.0], u.inv_t);

        let mut sim = Sim {
            code: &code,
            p: u,
            orig_n: orig.functions.len(),
            globals: HashMap::new(),
            heap: vec![],
            ctrl: false,
            shift: false,
            multi: true,
            drag: None,
            log: vec![],
        };
        let (uinv_tag, ic_tag) = (V::I(1), V::I(2));
        sim.globals.insert(u.uinv_cls.0, uinv_tag.clone());
        sim.globals.insert(u.ic_cls.0, ic_tag.clone());
        let player = |sim: &mut Sim| {
            let pl = sim.obj(&[(u.bp_connected.0, V::B(true))]);
            let inv = sim.obj(&[(u.inv_owner.0, pl.clone())]);
            sim.set(&pl, u.bp_inv.0, inv.clone());
            (pl, inv)
        };
        let (a, inv_a) = player(&mut sim);
        let (b, inv_b) = player(&mut sim);
        let (c, inv_c) = player(&mut sim);
        let game = sim.obj(&[(u.game_me.0, a.clone()), (u.game_loading.0, V::B(false))]);
        let cls = sim.obj(&[(u.game_inst.0, game.clone())]);
        sim.globals.insert(u.game_cls.0, cls);
        // a panel: [title, inventory-content -> ui.comp.Inventory(inv)]
        let panel = |sim: &mut Sim, inv: &V| {
            let ui = sim.obj(&[(TAG, uinv_tag.clone()), (u.uinv_inv.0, inv.clone())]);
            let content = sim.obj(&[(TAG, ic_tag.clone()), (UINV, ui.clone())]);
            let title = sim.obj(&[]);
            let p = sim.obj(&[]);
            sim.set_children(&p, vec![title, content]);
            (p, ui)
        };
        let (pb, ui_b) = panel(&mut sim, &inv_b);
        let (pc, ui_c) = panel(&mut sim, &inv_c);
        let gi = sim.obj(&[]);
        let root = sim.obj(&[]);
        let bx = sim.obj(&[]);
        sim.set_children(&root, vec![bx.clone()]);
        sim.set_children(&bx, vec![pb.clone(), pc.clone()]);
        let item = |sim: &mut Sim, n: i32, locked: bool| {
            let k = sim.obj(&[(LOCKED, V::B(locked))]);
            sim.obj(&[(u.item_k.0, k), (COUNT, V::I(n))])
        };
        let slot = |sim: &mut Sim, mode: usize, parent: &V, it: V| {
            let api = sim.obj(&[]);
            sim.obj(&[
                (u.slot_mode.0, V::En(mode, vec![])),
                (u.parent.0, parent.clone()),
                (u.slot_api.0, api),
                (ITEM, it),
            ])
        };
        let found = u.found as usize;
        // slots: one in B's grid (FoundItems, below a sub flow), one in my own grid
        let it = item(&mut sim, 5, false);
        let grid_b = sim.obj(&[(u.parent.0, ui_b.clone())]);
        let slot_b = slot(&mut sim, found, &grid_b, it);
        let ui_a = sim.obj(&[(TAG, uinv_tag.clone()), (u.uinv_inv.0, inv_a.clone())]);
        let it = item(&mut sim, 7, false);
        let slot_a = slot(&mut sim, 0, &ui_a, it);
        let it = item(&mut sim, 2, false);
        let slot_c = slot(&mut sim, found, &ui_c, it);
        let netop = |log: &[(&str, Vec<V>)]| -> Vec<(V, V)> {
            log.iter()
                .filter(|(n, _)| *n == "netop")
                .map(|(_, a)| match &a[0] {
                    V::En(k, args) if *k == u.move_to.0 => (args[0].clone(), args[1].clone()),
                    o => panic!("not a MoveTo: {o:?}"),
                })
                .collect()
        };

        // closed: nothing is foreign, give does nothing
        assert_eq!(sim.run(foreign, vec![slot_b.clone()]), V::Null);
        sim.ctrl = true;
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(false));
        assert!(sim.log.is_empty());

        sim.globals.insert(gbox.0, bx.clone());
        sim.globals.insert(ggi.0, gi.clone());
        assert_eq!(sim.run(foreign, vec![slot_b.clone()]), inv_b);
        assert_eq!(sim.run(foreign, vec![slot_a.clone()]), V::Null, "own grid");
        // a FoundItems grid of my own inventory, or of a container, is not foreign
        sim.set(&ui_b, u.uinv_inv.0, inv_a.clone());
        assert_eq!(sim.run(foreign, vec![slot_b.clone()]), V::Null);
        let chest = sim.obj(&[]);
        sim.set(&ui_b, u.uinv_inv.0, chest);
        assert_eq!(sim.run(foreign, vec![slot_b.clone()]), V::Null);
        sim.set(&ui_b, u.uinv_inv.0, inv_b.clone());

        // default target: the first open panel (B)
        assert_eq!(sim.run(target, vec![]), inv_b);
        // ctrl + right click on my own stack: the whole stack to B, through the slot api
        sim.log.clear();
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(true));
        assert_eq!(netop(&sim.log), [(inv_b.clone(), V::I(7))]);
        assert!(sim
            .log
            .iter()
            .all(|(n, a)| *n != "netop" || (a[1] == slot_a && matches!(a[2], V::Clo(..)))));
        // without ctrl: vanilla handles it
        sim.ctrl = false;
        sim.log.clear();
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(false));
        assert!(sim.log.is_empty());
        sim.ctrl = true;
        // a right click (take) on C's grid makes C the target; vanilla take goes on
        assert_eq!(sim.run(give, vec![slot_c.clone()]), V::B(false));
        assert_eq!(sim.global(gtarget), inv_c);
        sim.log.clear();
        sim.run(give, vec![slot_a.clone()]);
        assert_eq!(netop(&sim.log), [(inv_c.clone(), V::I(7))]);
        // C disconnects: the target falls back to B
        sim.set(&c, u.bp_connected.0, V::B(false));
        assert_eq!(sim.run(target, vec![]), inv_b);
        // ctrl + shift: the amount box; its callback gives that many
        sim.shift = true;
        sim.log.clear();
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(true));
        let [("select", args)] = &sim.log[..] else {
            panic!("{:?}", sim.log)
        };
        let V::Clo(cb, bound) = &args[3] else {
            panic!()
        };
        assert_eq!((*cb, (**bound).clone()), (give_n, slot_a.clone()));
        sim.log.clear();
        sim.run(give_n, vec![slot_a.clone(), V::I(3)]);
        assert_eq!(netop(&sim.log), [(inv_b.clone(), V::I(3))]);
        sim.log.clear();
        sim.run(give_n, vec![slot_a.clone(), V::I(0)]);
        assert!(sim.log.is_empty(), "nothing for 0");
        sim.shift = false;
        // a locked item is not given
        let locked = item(&mut sim, 1, true);
        sim.set(&slot_a, ITEM, locked);
        sim.log.clear();
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(false));
        assert!(sim.log.is_empty());
        // nobody connected to give to
        sim.set(&b, u.bp_connected.0, V::B(false));
        let it = item(&mut sim, 4, false);
        sim.set(&slot_a, ITEM, it);
        assert_eq!(sim.run(give, vec![slot_a.clone()]), V::B(false));
        assert!(sim.log.is_empty());

        // tick: a disconnected player's panel goes
        let ui = sim.obj(&[(u.ui_game.0, game.clone()), (u.ui_gi.0, gi.clone())]);
        sim.set(&b, u.bp_connected.0, V::B(true));
        sim.run(tick, vec![ui.clone()]);
        assert_eq!(sim.children(&bx), [pb.clone()], "C's panel removed");
        assert_eq!(sim.global(gbox), bx);
        // battle: everything closes
        let battle = sim.obj(&[]);
        sim.set(&game, u.game_battle.0, battle);
        sim.run(tick, vec![ui.clone()]);
        assert_eq!(sim.global(gbox), V::Null);
        assert_eq!(sim.global(gtarget), V::Null);
        assert!(sim.log.iter().any(|(n, a)| *n == "remove" && a[0] == bx));
        let _ = (pc, a);
    }

    /// Button and toggle: solo does nothing; in co-op one vanilla-shaped panel
    /// per other connected player (FoundItems grid of their inventory, drag key
    /// AllInv#<index>); a second click closes, a third opens again; no panel
    /// during a battle.
    #[test]
    fn toggle_behaviour() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let pl = plans(&orig);
        let mut code = read(&image);
        patch_all_inv(&mut code);
        let u = &pl.u;
        let fun = |n: &str| find_named(&code, n).expect(n);
        let (toggle, button) = (fun(N_TOGGLE), fun(N_BUTTON));
        let gi_g = (orig.globals.len()..code.globals.len())
            .find(|&i| code.globals[i] == u.gi_t)
            .expect("gi global");
        let gbox = RefGlobal(gi_g - 1);
        let mut sim = Sim {
            code: &code,
            p: u,
            orig_n: orig.functions.len(),
            globals: HashMap::new(),
            heap: vec![],
            ctrl: false,
            shift: false,
            multi: false,
            drag: Some(fun("mpDragPanel")),
            log: vec![],
        };
        let player = |sim: &mut Sim, connected: bool| {
            let pl = sim.obj(&[(u.bp_connected.0, V::B(connected))]);
            let inv = sim.obj(&[(u.inv_owner.0, pl.clone())]);
            sim.set(&pl, u.bp_inv.0, inv.clone());
            (pl, inv)
        };
        let (a, _) = player(&mut sim, true);
        let (b, inv_b) = player(&mut sim, true);
        let (c, _) = player(&mut sim, false);
        let (d, inv_d) = player(&mut sim, true);
        let players = sim.arr(vec![a.clone(), b, c, d]);
        let plist = sim.obj(&[(u.players_arr.0, players)]);
        let st = sim.obj(&[(u.state_players.0, plist)]);
        let gi_o = sim.obj(&[]);
        let gi_dom = sim.obj(&[(u.blk.props_obj.0, gi_o.clone())]);
        let gi = sim.obj(&[(u.dom.0, gi_dom)]);
        let ui_o = sim.obj(&[]);
        let ui_dom = sim.obj(&[(u.blk.props_obj.0, ui_o.clone())]);
        let ui = sim.obj(&[(u.ui_gi.0, gi.clone()), (u.dom.0, ui_dom)]);
        let game = sim.obj(&[(u.game_me.0, a), (u.game_state.0, st), (u.game_ui.0, ui)]);
        // The bar's row: ..., the inventory button flow, the pause / settings icons.
        let bar_o = sim.obj(&[]);
        let bt_flow = sim.obj(&[(u.parent.0, bar_o.clone())]);
        let bt = sim.obj(&[(u.parent.0, bt_flow.clone())]);
        let options = sim.obj(&[(u.parent.0, bar_o.clone())]);
        let first = sim.obj(&[(u.parent.0, bar_o.clone())]);
        sim.set_children(
            &bar_o,
            vec![first.clone(), bt_flow.clone(), options.clone()],
        );
        let bar_dom = sim.obj(&[(u.blk.props_obj.0, bar_o.clone())]);
        let bar = sim.obj(&[
            (u.bar_game.0, game.clone()),
            (u.dom.0, bar_dom),
            (u.bar_bt.0, bt),
        ]);
        let s = |x: &str| V::S(x.to_string());

        // the button: a "button" flow with the icon, hidden in solo, placed
        // right after the inventory button (before pause / settings)
        sim.run(button, vec![bar.clone()]);
        let creates: Vec<V> = sim
            .log
            .iter()
            .filter(|(n, _)| *n == "create")
            .map(|(_, a)| a[0].clone())
            .collect();
        assert_eq!(creates, [s("flow"), s("icon"), s("icon")]);
        let row = sim.children(&bar_o);
        assert_eq!(row.len(), 4);
        assert_eq!(row[0], first);
        assert_eq!(row[1], bt_flow);
        assert_eq!(row[3], options);
        let btn = row[2].clone();
        let icon = sim.children(&btn)[0].clone();
        assert!(matches!(sim.get(&icon, u.blk.onclick.0), V::Clo(f, _) if f == toggle));
        // the two-people badge sits on the icon and clicks like it
        let badge = sim.children(&icon)[0].clone();
        assert!(matches!(sim.get(&badge, u.blk.onclick.0), V::Clo(f, _) if f == toggle));
        assert!(
            sim.log.contains(&("set", vec![btn.clone(), V::B(false)])),
            "hidden in solo"
        );

        // solo: a click does nothing
        sim.log.clear();
        sim.run(toggle, vec![bar.clone()]);
        assert!(sim.log.is_empty());
        assert_eq!(sim.global(gbox), V::Null);

        // co-op: panels for B and D (C is offline, A is me)
        sim.multi = true;
        // HUD at the left edge: #gameInfo (x 0, 270 wide) and a hidden one;
        // a full-width layer and a flow further right do not count.
        let hud = |sim: &mut Sim, x: i32, w: f64, vis: bool| {
            let o = sim.obj(&[
                (u.obj_x.0, V::F(x as f64)),
                (u.flow_calc_w.0, V::F(w)),
                (u.visible.0, V::B(vis)),
            ]);
            o
        };
        let left = vec![
            hud(&mut sim, 0, 270.0, true),
            hud(&mut sim, 0, 400.0, false),
            hud(&mut sim, 0, 1920.0, true),
            hud(&mut sim, 800, 200.0, true),
        ];
        sim.set_children(&ui_o, left.clone());
        sim.run(toggle, vec![bar.clone()]);
        let bx = sim.global(gbox);
        let k_off = (0..code.strings.len())
            .find(|&i| super::s(&code, RefString(i)) == "offset")
            .expect("offset string");
        let attrs = sim.get(&bx, ATTRS);
        assert_eq!(sim.get(&attrs, DYN + k_off), V::S("280 -70".into()));
        let mut kids = sim.children(&ui_o);
        assert_eq!(kids.split_off(4), [bx.clone()]);
        sim.set_children(&ui_o, vec![bx.clone()]);
        // on the HUD root (left edge), not inside GameInventory
        assert_eq!(sim.children(&ui_o), [bx.clone()]);
        assert!(sim.children(&gi_o).is_empty());
        let panels = sim.children(&bx);
        assert_eq!(panels.len(), 2);
        let invs: Vec<(V, V)> = sim
            .log
            .iter()
            .filter(|(n, _)| *n == "inventory")
            .map(|(_, a)| (a[1].clone(), a[2].clone()))
            .collect();
        let found_v = sim.global(u.found_g);
        assert_eq!(
            invs,
            [(found_v.clone(), inv_b.clone()), (found_v, inv_d.clone())]
        );
        let keys: Vec<V> = sim
            .log
            .iter()
            .filter(|(n, _)| *n == "drag")
            .map(|(_, a)| a[1].clone())
            .collect();
        assert_eq!(keys, [s("AllInv#1"), s("AllInv#3")]);
        for (k, pnl) in panels.iter().enumerate() {
            assert_eq!(sim.get(pnl, TAG), s("element"));
            let ch = sim.children(pnl);
            assert_eq!(ch.len(), 2, "title + inventory-content");
            assert_eq!(sim.get(&ch[1], TAG), s("inventory-content"));
            let title = sim.children(&ch[0]);
            assert_eq!(
                title.iter().map(|o| sim.get(o, TAG)).collect::<Vec<_>>(),
                [s("text-fixed"), s("icon")]
            );
            // header: the owner's name (getName: nickname in the player colour)
            assert!(sim
                .log
                .contains(&("set", vec![title[0].clone(), s("nick")])));
            assert!(
                matches!(sim.get(&title[1], u.blk.onclick.0), V::Clo(..)),
                "close button"
            );
            let drags: Vec<&V> = sim
                .log
                .iter()
                .filter(|(n, _)| *n == "drag")
                .map(|(_, a)| &a[0])
                .collect();
            assert_eq!(drags[k], pnl);
        }
        // second click: closed
        sim.run(toggle, vec![bar.clone()]);
        assert_eq!(sim.global(gbox), V::Null);
        assert!(sim.children(&ui_o).is_empty());
        // third click: open again
        sim.run(toggle, vec![bar.clone()]);
        assert_ne!(sim.global(gbox), V::Null);
        sim.run(toggle, vec![bar.clone()]);
        // battle: nothing opens
        let battle = sim.obj(&[]);
        sim.set(&game, u.game_battle.0, battle);
        sim.run(toggle, vec![bar.clone()]);
        assert_eq!(sim.global(gbox), V::Null);
        assert!(sim.children(&ui_o).is_empty());
    }
}
