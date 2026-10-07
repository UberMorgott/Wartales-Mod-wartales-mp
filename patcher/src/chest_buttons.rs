// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Buttons on the co-op shared chest panel of the inventory side panel.
//
// In co-op the inventory panel (ui.comp.gameUIComp.GameInventory) shows the
// shared camp chest next to the player's own inventory (#chestInventory, opened
// with the chest icon). The player's panel has a sort button; the chest panel
// has none. This pass adds a small row of icons at the top left of the chest
// panel:
//
//   sort  - the player panel's sort menu (same icon "SortButton", same entries
//           from Texts.tips.inventory_sort), applied to the chest through the
//           host-authoritative st.Inventory.netSortBy RPC, the call the camp
//           chest window (ui.win.CampChest) uses for the same chest.
//   stack - quick stack: every stack of the player's inventory whose item kind
//           the chest already holds moves to the chest (icon "CampChestButton",
//           tooltip Texts campChest.moveTo_chest).
//   take  - take similar: every chest stack whose kind the player's inventory
//           already holds moves to it (icon "LootAll", tooltip moveTo_inventory).
//
// Moves are the vanilla slot operation MoveTo(target, count), run the way the
// slot UI runs it (ui.comp.Slot handler): locally on an inventory this machine
// has authority over (its own inventory; everything on the host), else as the
// st.Inventory.networkOperation RPC, so the host consumes the chest stack and
// adds it to the player's inventory in one step. Item conservation is the
// vanilla one; a stack index that went stale (another player moved the chest
// in the meantime) moves whatever stack the host has at that index, as a
// vanilla drag would. Equipped items are not inventory content and never move.
// GameInventory's constructor gets, before its Ret:
//
//   var cp = this.chestInventory?.dom; if (cp == null) skip;
//   var row = createNew("flow", cp, [], {position:"absolute", align:"top left", offset, hspacing});
//   var sort = createNew("icon", row, ["SortButton"], {scale}).obj; sort.onClick = chestSortMenu.bind(sort);
//
// `chestSortMenu(icon)` is GameInventory.onSortMenu cloned: the chest (camp tool
// "Chest") replaces the panel's own inventory, the clicked icon anchors the popup,
// and each entry is `chestSortPick.bind(SortKind)`:
//
//   chest = Game.inst.state.camp.getTool("Chest").inventory; if (chest == null) return;
//   chest.netSortBy(kind, null); Game.inst.ui.sfx("InventorySortConfirm", null);
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::*;
use super::*;
use hlbc::types::RefGlobal;

/// Icon size 48 px in the cdb sheet; 0.625 gives the 30 px of the player panel row.
const ICON_SCALE: &str = "0.625";
const ROW_OFFSET: &str = "14 12";
const ROW_SPACING: &str = "2";

struct Types {
    void: RefType,
    bool_: RefType,
    i32_: RefType,
    dyn_: RefType,
    dynobj: RefType,
    str_: RefType,
    arr_dyn: RefType,
    ref_bool: RefType,
}

/// What the constructor's own sortButton block uses to build an icon
/// (also read by all_inv from WorldButtonsBar's btInventory block).
pub(crate) struct IconBlock {
    pub(crate) icon_g: RefGlobal,
    pub(crate) alloc: RefFun,
    pub(crate) arr_elem_t_op: Opcode,
    pub(crate) wrap: RefFun,
    pub(crate) arr_t: RefType,
    pub(crate) arr_obj_t: RefType,
    pub(crate) dyn_alloc: RefFun,
    pub(crate) create: RefFun,
    pub(crate) props_t: RefType,
    pub(crate) props_obj: RefField,
    pub(crate) obj_t: RefType,
    pub(crate) onclick: RefField,
    pub(crate) closure_t: RefType,
}

struct Plan {
    t: Types,
    dbg_file: usize,
    ctor_fi: usize,
    chest_inv: (RefField, RefType),
    elem_dom: RefField,
    icon_t: RefType,
    blk: IconBlock,
    /// onSortMenu (function index) and the op ranges replaced in the clone.
    menu_fi: usize,
    icon_game: RefField,
    game_t: RefType,
    game_state: (RefField, RefType),
    game_ui: (RefField, RefType),
    game_cls_g: RefGlobal,
    game_cls_t: RefType,
    game_inst: RefField,
    get_camp: (RefFun, RefType),
    get_tool: (RefFun, RefType),
    tool_inv: (RefField, RefType),
    chest_s: RefGlobal,
    sort_kind_cls: (RefGlobal, RefType),
    sort_kind_t: RefType,
    create_enum: RefFun,
    net_sort: RefFun,
    bool_cb_t: RefType,
    sfx: RefFun,
    sfx_confirm: RefGlobal,
    flow_g: RefGlobal,
    sort_icon_g: RefGlobal,
    mv: Move,
}

/// Quick stack / quick take: vanilla slot moves (SlotOperation.MoveTo).
struct Move {
    game_me: (RefField, RefType),
    player_inv: RefField,
    inv_t: RefType,
    content: (RefField, RefType),
    proxy_array: RefField,
    arr_obj_t: RefType,
    arr_len: RefField,
    arr_raw: (RefField, RefType),
    slot_t: RefType,
    slot_k: (RefField, RefType),
    get_count: RefFun,
    item_kind: RefField,
    count: RefFun,
    null_i32: RefType,
    has_authority: RefFun,
    local_op: RefFun,
    net_op: RefFun,
    op_t: RefType,
    move_to: hlbc::types::RefEnumConstruct,
    /// Texts access for the tooltips, as CampChest.init reads them.
    texts_g: RefGlobal,
    texts_data: (RefField, RefType),
    texts_group: hlbc::types::RefString,
    texts_vt: RefType,
    camp_chest: (RefField, RefType),
    to_chest: RefField,
    to_inv: RefField,
    set_tip: RefFun,
    title_tip: RefField,
    stack_icon_g: RefGlobal,
}

fn plan_move(code: &Bytecode, t: &Types, game_t: RefType, icon_t: RefType) -> Result<Move> {
    let game_me = field(code, game_t, "me")?;
    let (player_inv, inv_t) = field(code, game_me.1, "inventory")?;
    if s(code, obj(code, inv_t)?.name) != "st.Inventory" {
        bail!("BasePlayer.inventory is not st.Inventory");
    }
    let content = field(code, inv_t, "content")?;
    let (proxy_array, pa_t) = field(code, content.1, "array")?;
    if pa_t != t.arr_dyn {
        bail!("content.array is not ArrayDyn");
    }
    let arr_obj_t = obj_type(code, "hl.types.ArrayObj")?;
    let (arr_len, len_t) = field(code, arr_obj_t, "length")?;
    if len_t != t.i32_ {
        bail!("ArrayObj.length is not I32");
    }
    let arr_raw = field(code, arr_obj_t, "array")?;
    // slot = virtual {chk, count, k}; get_count(slot) -> Int
    let gc = code
        .functions
        .iter()
        .find(|f| {
            s(code, f.name) == "get_count"
                && f.t.as_fun(code).is_some_and(|ft| {
                    ft.args.len() == 1
                        && ft.ret == t.i32_
                        && matches!(&code.types[ft.args[0].0], Type::Virtual { fields }
                            if fields.iter().map(|x| s(code, x.name)).collect::<Vec<_>>() == ["chk", "count", "k"])
                })
        })
        .context("SlotItem.get_count not found")?;
    let slot_t = gc.t.as_fun(code).unwrap().args[0];
    let Type::Virtual { fields } = &code.types[slot_t.0] else {
        unreachable!()
    };
    if s(code, fields[2].name) != "k" {
        bail!("slot virtual has no k");
    }
    // `k` is Dyn in the virtual; vanilla reads it straight into an st.Item register.
    let slot_k = (RefField(2), obj_type(code, "st.Item")?);
    let item_kind = field(code, slot_k.1, "kind")?;
    if item_kind.1 != t.str_ {
        bail!("Item.kind is not String");
    }
    let count_f = proto_fn(code, inv_t, "count")?;
    let ct = count_f.t.as_fun(code).unwrap();
    if ct.args.len() != 4 || ct.args[1] != t.str_ || ct.args[3] != t.ref_bool || ct.ret != t.i32_ {
        bail!("Inventory.count is not (String, Null<Int>, Ref<Bool>) -> Int");
    }
    let null_i32 = ct.args[2];
    let has_authority = proto_fn(code, inv_t, "hasAuthority")?;
    let local_op = proto_fn(code, inv_t, "_networkOperation")?;
    let net_op = proto_fn(code, inv_t, "networkOperation")?;
    let lt = local_op.t.as_fun(code).unwrap();
    let nt = net_op.t.as_fun(code).unwrap();
    if lt.args.len() != 4
        || nt.args.len() != 5
        || lt.args[..4] != nt.args[..4]
        || lt.args[1] != t.i32_
    {
        bail!("Inventory network operation signatures changed");
    }
    let op_t = lt.args[2];
    let Type::Enum { constructs, .. } = &code.types[op_t.0] else {
        bail!("slot operation is not an enum");
    };
    let mi = constructs
        .iter()
        .position(|c| s(code, c.name) == "MoveTo")
        .context("SlotOperation.MoveTo not found")?;
    if constructs[mi].params != [inv_t, t.i32_] {
        bail!("SlotOperation.MoveTo is not (Inventory, Int)");
    }
    // Tooltip texts: CampChest.init reads Texts.DATA.<group>.campChest.moveTo_chest.
    let cc_t = obj_type(code, "ui.win.CampChest")?;
    let init = method(code, cc_t, "init")?;
    let at = init
        .ops
        .iter()
        .enumerate()
        .find_map(|(i, o)| match o {
            Opcode::Field { obj, field: fl, .. }
                if field_name(code, init.regs[obj.0 as usize], *fl) == Some("moveTo_chest") =>
            {
                Some(i)
            }
            _ => None,
        })
        .context("CampChest.init: moveTo_chest not read")?;
    let window: Vec<&Opcode> = init.ops[at.saturating_sub(10)..at]
        .iter()
        .filter(|o| !matches!(o, Opcode::NullCheck { .. }))
        .collect();
    let [.., Opcode::GetGlobal {
        global: texts_g, ..
    }, Opcode::Field {
        dst: d1,
        field: data_f,
        ..
    }, Opcode::DynGet { field: group, .. }, Opcode::ToVirtual { dst: vt_r, .. }, Opcode::Field {
        dst: cc_r,
        field: cc_f,
        ..
    }] = window[..]
    else {
        bail!("CampChest.init: unexpected Texts access shape");
    };
    let texts_data = (*data_f, init.regs[d1.0 as usize]);
    let texts_vt = init.regs[vt_r.0 as usize];
    let camp_chest = (*cc_f, init.regs[cc_r.0 as usize]);
    let to_chest = field_of_virtual(code, camp_chest.1, "moveTo_chest")?.0;
    let to_inv = field_of_virtual(code, camp_chest.1, "moveTo_inventory")?.0;
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let set_tip = method(code, elem_t, "set_tipText")?.findex;
    let (title_tip, tt_t) = field(code, icon_t, "titleTip")?;
    if tt_t != t.bool_ {
        bail!("Icon.titleTip is not Bool");
    }
    Ok(Move {
        game_me,
        player_inv,
        inv_t,
        content,
        proxy_array,
        arr_obj_t,
        arr_len,
        arr_raw,
        slot_t,
        slot_k,
        get_count: gc.findex,
        item_kind: item_kind.0,
        count: count_f.findex,
        null_i32,
        has_authority: has_authority.findex,
        local_op: local_op.findex,
        net_op: net_op.findex,
        op_t,
        move_to: hlbc::types::RefEnumConstruct(mi),
        texts_g: *texts_g,
        texts_data,
        texts_group: *group,
        texts_vt,
        camp_chest,
        to_chest,
        to_inv,
        set_tip,
        title_tip,
        stack_icon_g: RefGlobal(0),
    })
}

/// The function bound to method `name` in class `t`'s own prototype.
fn proto_fn<'a>(code: &'a Bytecode, t: RefType, name: &str) -> Result<&'a Function> {
    let f = proto(code, t, name)?;
    Ok(&code.functions[fun_index(code, f)?])
}

fn fun_sig(code: &Bytecode, f: RefFun) -> Result<TypeFun> {
    let t = code
        .functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| g.t)
        .or_else(|| code.natives.iter().find(|n| n.findex == f).map(|n| n.t))
        .with_context(|| format!("function @{} not found", f.0))?;
    t.as_fun(code)
        .cloned()
        .with_context(|| format!("function @{} has no function type", f.0))
}

/// Class global of enum `name` (its `$Name` BaseType object), found as the global
/// a function loads right before `$Type.createEnumIndex` with that enum's result.
fn enum_class_global(
    code: &Bytecode,
    create_enum: RefFun,
    enum_t: RefType,
) -> Result<(RefGlobal, RefType)> {
    for f in &code.functions {
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Call3 { fun, arg0, dst, .. } = op else {
                continue;
            };
            if *fun != create_enum {
                continue;
            }
            let Some(Opcode::SafeCast { dst: k, src }) = f.ops.get(i + 1) else {
                continue;
            };
            if src != dst || f.regs[k.0 as usize] != enum_t {
                continue;
            }
            let g = f.ops[..i].iter().rev().find_map(|o| match o {
                Opcode::GetGlobal { dst, global } if dst == arg0 => Some(*global),
                _ => None,
            });
            if let Some(g) = g {
                return Ok((g, code.globals[g.0]));
            }
        }
    }
    bail!("no createEnumIndex for enum type {}", enum_t.0)
}

pub(crate) fn plan_icon_block(
    code: &Bytecode,
    ctor: &Function,
    sort_field: RefField,
    gi_t: RefType,
) -> Result<IconBlock> {
    let set = ctor
        .ops
        .iter()
        .rposition(|o| matches!(o, Opcode::SetThis { field, .. } if *field == sort_field))
        .context("ctor: sortButton never set")?;
    // ... createNew(comp, parent, ArrayDyn.alloc(wrap(arr), true), attrs); NullCheck; Field obj; SafeCast; SetThis
    let Some(Opcode::SafeCast { src: o, dst: cast }) = ctor.ops.get(set - 1) else {
        bail!("ctor: sortButton not a SafeCast");
    };
    let Some(Opcode::Field {
        dst: o2,
        obj: p,
        field: props_obj,
    }) = ctor.ops.get(set - 2)
    else {
        bail!("ctor: sortButton not read from Properties.obj");
    };
    if o2 != o {
        bail!("ctor: sortButton block shape");
    }
    let props_t = ctor.regs[p.0 as usize];
    let obj_t = ctor.regs[o.0 as usize];
    let cast_t = ctor.regs[cast.0 as usize];
    let ci = ctor.ops[..set]
        .iter()
        .rposition(|o| matches!(o, Opcode::Call4 { dst, .. } if dst == p))
        .context("ctor: no createNew before sortButton")?;
    let Opcode::Call4 {
        fun: create,
        arg0: comp,
        arg2: args,
        ..
    } = ctor.ops[ci]
    else {
        unreachable!()
    };
    let di = ctor.ops[..ci]
        .iter()
        .rposition(|o| matches!(o, Opcode::Call2 { dst, .. } if *dst == args))
        .context("ctor: no ArrayDyn.alloc")?;
    let Opcode::Call2 {
        fun: dyn_alloc,
        arg0: wrapped,
        ..
    } = ctor.ops[di]
    else {
        unreachable!()
    };
    let wi = ctor.ops[..di]
        .iter()
        .rposition(|o| matches!(o, Opcode::Call1 { dst, .. } if *dst == wrapped))
        .context("ctor: no array wrap")?;
    let Opcode::Call1 {
        fun: wrap,
        arg0: arr,
        ..
    } = ctor.ops[wi]
    else {
        unreachable!()
    };
    let ai = ctor.ops[..wi]
        .iter()
        .rposition(|o| matches!(o, Opcode::Call2 { dst, .. } if *dst == arr))
        .context("ctor: no alloc_array")?;
    let Opcode::Call2 {
        fun: alloc,
        arg0: elem_t_reg,
        ..
    } = ctor.ops[ai]
    else {
        unreachable!()
    };
    let arr_elem_t_op = ctor.ops[..ai]
        .iter()
        .rev()
        .find(|o| matches!(o, Opcode::Type { dst, .. } if *dst == elem_t_reg))
        .cloned()
        .context("ctor: no element Type op")?;
    let icon_g = ctor.ops[..ai]
        .iter()
        .rev()
        .find_map(|o| match o {
            Opcode::GetGlobal { dst, global } if *dst == comp => Some(*global),
            _ => None,
        })
        .context("ctor: no component name")?;
    // onClick: SetField obj.onClick = InstanceClosure(...) right after.
    let (onclick, closure_t) = ctor.ops[set..]
        .iter()
        .take(8)
        .find_map(|op| match op {
            Opcode::SetField {
                field,
                src,
                obj: fobj,
            } if ctor.regs[fobj.0 as usize] == cast_t => Some((*field, ctor.regs[src.0 as usize])),
            _ => None,
        })
        .context("ctor: sortButton.onClick not set")?;
    if field_name(code, cast_t, onclick) != Some("onClick") {
        bail!("ctor: sortButton handler field is not onClick");
    }
    if !matches!(&code.types[closure_t.0], Type::Fun(f) if f.args.is_empty()) {
        bail!("ctor: onClick is not a () -> Void closure");
    }
    let _ = gi_t;
    Ok(IconBlock {
        icon_g,
        alloc,
        arr_elem_t_op,
        wrap,
        arr_t: ctor.regs[arr.0 as usize],
        arr_obj_t: ctor.regs[wrapped.0 as usize],
        dyn_alloc,
        create,
        props_t,
        props_obj: *props_obj,
        obj_t,
        onclick,
        closure_t,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let str_t = obj_type(code, "String")?;
    let t = Types {
        void: prim("void", |t| matches!(t, Type::Void))?,
        bool_: prim("Bool", |t| matches!(t, Type::Bool))?,
        i32_: prim("I32", |t| matches!(t, Type::I32))?,
        dyn_: prim("Dyn", |t| matches!(t, Type::Dyn))?,
        dynobj: prim("DynObj", |t| matches!(t, Type::DynObj))?,
        str_: str_t,
        arr_dyn: obj_type(code, "hl.types.ArrayDyn")?,
        ref_bool: RefType(0),
    };
    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let ctor = method(code, gi_t, "__constructor__")?;
    let ctor_fi = fun_index(code, ctor.findex)?;
    if !matches!(ctor.ops.last(), Some(Opcode::Ret { .. })) {
        bail!("GameInventory ctor does not end with Ret");
    }
    // Applied: the vanilla constructor sets only `class` / `id` attributes.
    let marker = code.strings.iter().position(|v| v.as_str() == "position");
    if ctor
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::DynSet { field, .. } if Some(field.0) == marker))
    {
        bail!("already applied");
    }
    let chest_inv = field(code, gi_t, "chestInventory")?;
    let (sort_field, icon_t) = field(code, gi_t, "sortButton")?;
    if s(code, obj(code, icon_t)?.name) != "ui.comp.Icon" {
        bail!("sortButton is not a ui.comp.Icon");
    }
    let blk = plan_icon_block(code, ctor, sort_field, gi_t)?;
    if !is_sub(code, icon_t, blk.obj_t) {
        bail!("sortButton block casts to an unrelated type");
    }
    let (elem_dom, dom_t) = field(code, chest_inv.1, "dom")?;
    if dom_t != blk.props_t {
        bail!("chestInventory.dom is not domkit.Properties");
    }
    let dbg_file = ctor
        .debug_info
        .as_ref()
        .and_then(|d| d.first())
        .map(|x| x.0)
        .unwrap_or(0);

    // onSortMenu, cloned for the chest.
    let menu = method(code, gi_t, "onSortMenu")?;
    let menu_fi = fun_index(code, menu.findex)?;
    check_menu_shape(code, menu, gi_t, chest_inv.0, sort_field)?;

    let (icon_game, game_t) = field(code, icon_t, "game")?;
    if s(code, obj(code, game_t)?.name) != "Game" {
        bail!("Icon.game is not Game");
    }
    let game_state = field(code, game_t, "state")?;
    let game_ui = field(code, game_t, "ui")?;
    let (game_cls_g, game_cls_t) = class_global(code, "Game")?;
    let (game_inst, inst_t) = field(code, game_cls_t, "inst")?;
    if inst_t != game_t {
        bail!("$Game.inst is not Game");
    }
    let get_camp_f = method(code, game_state.1, "get_camp")?;
    let camp_t = get_camp_f.t.as_fun(code).context("get_camp type")?.ret;
    let get_tool_f = code
        .functions
        .iter()
        .find(|f| {
            s(code, f.name) == "getTool"
                && f.t.as_fun(code).is_some_and(|ft| {
                    ft.args.len() == 3 && is_sub(code, camp_t, ft.args[0]) && ft.args[1] == str_t
                })
        })
        .context("Camp.getTool(String, ?) not found")?;
    let gt = get_tool_f.t.as_fun(code).unwrap().clone();
    let ref_bool = gt.args[2];
    if !matches!(code.types[ref_bool.0], Type::Ref(b) if b == t.bool_) {
        bail!("getTool third argument is not Ref<Bool>");
    }
    let tool_t = gt.ret;
    let tool_inv = field(code, tool_t, "inventory")?;
    let inv_t = obj_type(code, "st.Inventory")?;
    if tool_inv.1 != inv_t {
        bail!("Tool.inventory is not st.Inventory");
    }
    let chest_s = existing_str(code, str_t, "Chest")?;
    let net_sort_f = proto_fn(code, inv_t, "netSortBy")?;
    let ns = net_sort_f.t.as_fun(code).unwrap().clone();
    if ns.args.len() != 3 || !matches!(code.types[ns.args[1].0], Type::Enum { .. }) {
        bail!("netSortBy is not (Inventory, SortKind, cb)");
    }
    let sort_kind_t = ns.args[1];
    let bool_cb_t = ns.args[2];
    let create_enum = code
        .functions
        .iter()
        .find(|f| {
            s(code, f.name) == "createEnumIndex"
                && f.t
                    .as_fun(code)
                    .is_some_and(|ft| ft.args.len() == 3 && ft.ret == t.dyn_)
        })
        .context("$Type.createEnumIndex not found")?
        .findex;
    let sort_kind_cls = enum_class_global(code, create_enum, sort_kind_t)?;
    let ce = fun_sig(code, create_enum)?;
    if ce.args[0] != sort_kind_cls.1 && !matches!(code.types[ce.args[0].0], Type::Obj(_)) {
        bail!("createEnumIndex first argument mismatch");
    }
    let ui_t = game_ui.1;
    let sfx_f = method(code, ui_t, "sfx")?;
    let sfx_sig = sfx_f.t.as_fun(code).unwrap();
    if sfx_sig.args.len() != 3 || sfx_sig.args[1] != str_t || sfx_sig.args[2] != t.dyn_ {
        bail!("GameUI.sfx is not (String, Dyn)");
    }
    let sfx_confirm = existing_str(code, str_t, "InventorySortConfirm")?;
    let flow_g = existing_str(code, str_t, "flow")?;
    if job_xp::const_str(code, blk.icon_g) != Some("icon") {
        bail!("ctor: sortButton component is not \"icon\"");
    }
    let sort_icon_g = existing_str(code, str_t, "SortButton")?;
    let t = Types { ref_bool, ..t };
    let mut mv = plan_move(code, &t, game_t, icon_t)?;
    mv.stack_icon_g = existing_str(code, str_t, "CampChestButton")?;
    Ok(Plan {
        t,
        mv,
        dbg_file,
        ctor_fi,
        chest_inv,
        elem_dom,
        icon_t,
        blk,
        menu_fi,
        icon_game,
        game_t,
        game_state,
        game_ui,
        game_cls_g,
        game_cls_t,
        game_inst,
        get_camp: (get_camp_f.findex, camp_t),
        get_tool: (get_tool_f.findex, tool_t),
        tool_inv,
        chest_s,
        sort_kind_cls,
        sort_kind_t,
        create_enum,
        net_sort: net_sort_f.findex,
        bool_cb_t,
        sfx: sfx_f.findex,
        sfx_confirm,
        flow_g,
        sort_icon_g,
    })
}

// Op positions in GameInventory.onSortMenu (1.0.48274) that the clone rewrites.
const M_INV: std::ops::RangeInclusive<usize> = 1..=4; // GetThis inventoryContent; NullCheck; getInventory; JNull end
const M_ANCHOR: usize = 5; // GetThis sortButton
const M_GAME: usize = 10; // GetThis game
const M_CTX: std::ops::RangeInclusive<usize> = 39..=43; // EnumAlloc ctx .. InstanceClosure(entry)

fn check_menu_shape(
    code: &Bytecode,
    m: &Function,
    gi_t: RefType,
    _chest: RefField,
    sort_field: RefField,
) -> Result<()> {
    let o = &m.ops;
    let name = |f: RefField| field_name(code, gi_t, f);
    let ok = o.len() == 64
        && matches!(
            o[0],
            Opcode::Mov {
                dst: Reg(1),
                src: Reg(0)
            }
        )
        && matches!(o[1], Opcode::GetThis { dst: Reg(4), field } if name(field) == Some("inventoryContent"))
        && matches!(o[2], Opcode::NullCheck { reg: Reg(4) })
        && matches!(
            o[3],
            Opcode::Call1 {
                dst: Reg(3),
                arg0: Reg(4),
                ..
            }
        )
        && matches!(o[4], Opcode::JNull { reg: Reg(3), offset } if 4 + 1 + offset as usize == 63)
        && matches!(o[M_ANCHOR], Opcode::GetThis { dst: Reg(6), field } if field == sort_field)
        && matches!(
            o[6],
            Opcode::Call1 {
                arg0: Reg(6),
                dst: Reg(5),
                ..
            }
        )
        && matches!(o[M_GAME], Opcode::GetThis { dst: Reg(9), field } if name(field) == Some("game"))
        && matches!(o[39], Opcode::EnumAlloc { dst: Reg(22), .. })
        && matches!(
            o[40],
            Opcode::SetEnumField {
                value: Reg(22),
                field: RefField(0),
                src: Reg(3)
            }
        )
        && matches!(
            o[41],
            Opcode::SetEnumField {
                value: Reg(22),
                field: RefField(1),
                src: Reg(19)
            }
        )
        && matches!(
            o[42],
            Opcode::SetEnumField {
                value: Reg(22),
                field: RefField(2),
                src: Reg(1)
            }
        )
        && matches!(
            o[43],
            Opcode::InstanceClosure {
                dst: Reg(21),
                obj: Reg(22),
                ..
            }
        )
        && matches!(o[63], Opcode::Ret { .. });
    if !ok {
        bail!("GameInventory.onSortMenu has an unexpected shape");
    }
    // No other op touches r1/r3/r4/r22 (their writers are dropped from the clone).
    for (i, op) in o.iter().enumerate() {
        if i == 0 || M_INV.contains(&i) || M_CTX.contains(&i) {
            continue;
        }
        for r in [1, 3, 4, 22] {
            if uses_reg(op, r) {
                bail!("onSortMenu op {i} uses r{r}");
            }
        }
    }
    Ok(())
}

/// `op` names register `n` (any operand position), from its Debug form `Reg(n)`.
pub(crate) fn uses_reg(op: &Opcode, n: u32) -> bool {
    let d = format!("{op:?}");
    let pat = format!("Reg({n})");
    d.match_indices(&pat).next().is_some()
}

/// `chestSortPick(kind)`: chest.netSortBy(kind, null) + confirm sound.
fn add_sort_pick(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.sort_kind_t]);
    let gc = r.r(p.game_cls_t);
    let game = r.r(p.game_t);
    let st = r.r(p.game_state.1);
    let camp = r.r(p.get_camp.1);
    let name = r.r(p.t.str_);
    let rb = r.r(p.t.ref_bool);
    let tool = r.r(p.get_tool.1);
    let inv = r.r(p.tool_inv.1);
    let cb = r.r(p.bool_cb_t);
    let ui = r.r(p.game_ui.1);
    let d = r.r(p.t.dyn_);
    let v = r.r(p.t.void);
    let mut a = Asm::new();
    chest_lookup(&mut a, p, gc, game, st, camp, name, rb, tool, inv);
    a.op(Opcode::Null { dst: cb });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.net_sort,
        arg0: inv,
        arg1: Reg(0),
        arg2: cb,
    });
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.game_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: name,
        global: p.sfx_confirm,
    });
    a.op(Opcode::Null { dst: d });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.sfx,
        arg0: ui,
        arg1: name,
        arg2: d,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.sort_kind_t],
        p.t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `inv = Game.inst.state.camp.getTool("Chest").inventory`, jumping to "end" on any null.
#[allow(clippy::too_many_arguments)]
fn chest_lookup(
    a: &mut Asm,
    p: &Plan,
    gc: Reg,
    game: Reg,
    st: Reg,
    camp: Reg,
    name: Reg,
    rb: Reg,
    tool: Reg,
    inv: Reg,
) {
    a.op(Opcode::GetGlobal {
        dst: gc,
        global: p.game_cls_g,
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
        "end",
    );
    a.op(Opcode::Field {
        dst: st,
        obj: game,
        field: p.game_state.0,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "end");
    a.op(Opcode::Call1 {
        dst: camp,
        fun: p.get_camp.0,
        arg0: st,
    });
    a.jmp(
        Opcode::JNull {
            reg: camp,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: name,
        global: p.chest_s,
    });
    a.op(Opcode::Null { dst: rb });
    a.op(Opcode::Call3 {
        dst: tool,
        fun: p.get_tool.0,
        arg0: camp,
        arg1: name,
        arg2: rb,
    });
    a.jmp(
        Opcode::JNull {
            reg: tool,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: inv,
        obj: tool,
        field: p.tool_inv.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: inv,
            offset: 0,
        },
        "end",
    );
}

/// `chestSortMenu(icon)`: GameInventory.onSortMenu for the shared chest.
fn add_sort_menu(code: &mut Bytecode, p: &Plan, pick: RefFun) -> Result<RefFun> {
    let m = &code.functions[p.menu_fi];
    let mut regs = m.regs.clone();
    regs[0] = p.icon_t;
    let mut r = Regs(regs);
    let gc = r.r(p.game_cls_t);
    let game = r.r(p.game_t);
    let st = r.r(p.game_state.1);
    let camp = r.r(p.get_camp.1);
    let name = r.r(p.t.str_);
    let rb = r.r(p.t.ref_bool);
    let tool = r.r(p.get_tool.1);
    let inv = r.r(p.tool_inv.1);
    let skc = r.r(p.sort_kind_cls.1);
    let nad = r.r(p.t.arr_dyn);
    let kd = r.r(p.t.dyn_);
    let kind = r.r(p.sort_kind_t);
    let src = m.ops.clone();
    let mut a = Asm::new();
    // chest lookup instead of `inventoryContent.getInventory()`; "end" = the final Ret.
    chest_lookup(&mut a, p, gc, game, st, camp, name, rb, tool, inv);
    for (i, op) in src.iter().enumerate() {
        if i == 0 || M_INV.contains(&i) {
            continue;
        }
        if i == 63 {
            a.label("end");
            a.op(op.clone());
            continue;
        }
        if i == M_ANCHOR {
            a.op(Opcode::Mov {
                dst: Reg(6),
                src: Reg(0),
            });
            continue;
        }
        if i == M_GAME {
            a.op(Opcode::Field {
                dst: Reg(9),
                obj: Reg(0),
                field: p.icon_game,
            });
            continue;
        }
        if M_CTX.contains(&i) {
            if i == *M_CTX.start() {
                a.op(Opcode::GetGlobal {
                    dst: skc,
                    global: p.sort_kind_cls.0,
                });
                a.op(Opcode::Null { dst: nad });
                a.op(Opcode::Call3 {
                    dst: kd,
                    fun: p.create_enum,
                    arg0: skc,
                    arg1: Reg(19),
                    arg2: nad,
                });
                a.op(Opcode::SafeCast { dst: kind, src: kd });
                a.op(Opcode::InstanceClosure {
                    dst: Reg(21),
                    fun: pick,
                    obj: kind,
                });
            }
            continue;
        }
        match op {
            // jumps inside the loop keep their relative distance only if the block
            // sizes around them do not change; recompute from labels instead.
            Opcode::JSGte { a: x, b: y, .. } if i == 34 => {
                a.jmp(
                    Opcode::JSGte {
                        a: *x,
                        b: *y,
                        offset: 0,
                    },
                    "end",
                );
            }
            Opcode::JAlways { .. } if i == 62 => {
                a.jmp(Opcode::JAlways { offset: 0 }, "loop");
            }
            Opcode::Label if i == 33 => a.loop_head("loop"),
            Opcode::JULt { a: x, b: y, .. } if i == 53 => {
                // bounds check `i < length ? array[i] : null`
                a.jmp(
                    Opcode::JULt {
                        a: *x,
                        b: *y,
                        offset: 0,
                    },
                    "get",
                );
            }
            Opcode::JAlways { .. } if i == 55 => a.jmp(Opcode::JAlways { offset: 0 }, "got"),
            _ => {
                if i == 56 {
                    a.label("get");
                }
                if i == 59 {
                    a.label("got");
                }
                if !jump_targets_of(op).is_empty() {
                    bail!("onSortMenu: unhandled jump at {i}");
                }
                a.op(op.clone());
            }
        }
    }
    let ops = a.finish();
    push_fn(code, vec![p.icon_t], p.t.void, r.0, ops, p.dbg_file)
}

/// `quickMove(src, dst, filter)`: every stack of `src` whose kind `filter` already
/// holds moves to `dst` with the vanilla slot operation `MoveTo(dst, count)`,
/// run like the slot UI runs it: locally where this machine has authority over
/// `src` (own inventory; everything on the host), else as the
/// `networkOperation` RPC (host consumes `src` and adds to `dst` in one step).
///
/// ```text
/// var a = src.content.array; var n = a.length;
/// for (i in 0...n) {
///   var s = a[i]; if (s == null || s.k == null || s.k.kind == null) continue;
///   if (filter.count(s.k.kind, null, null) <= 0) continue;
///   var c = get_count(s); if (c <= 0) continue;
///   var op = MoveTo(dst, c);
///   if (src.hasAuthority()) src._networkOperation(i, op, false) else src.networkOperation(i, op, false, null);
/// }
/// ```
fn add_quick_move(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let m = &p.mv;
    let mut r = Regs(vec![m.inv_t, m.inv_t, m.inv_t]);
    let (src, dst, filter) = (Reg(0), Reg(1), Reg(2));
    let content = r.r(m.content.1);
    let ad = r.r(p.t.arr_dyn);
    let arr = r.r(m.arr_obj_t);
    let n = r.r(p.t.i32_);
    let i = r.r(p.t.i32_);
    let raw = r.r(m.arr_raw.1);
    let d = r.r(p.t.dyn_);
    let slot = r.r(m.slot_t);
    let item = r.r(m.slot_k.1);
    let kind = r.r(p.t.str_);
    let q = r.r(m.null_i32);
    let rb = r.r(p.t.ref_bool);
    let k = r.r(p.t.i32_);
    let zero = r.r(p.t.i32_);
    let cnt = r.r(p.t.i32_);
    let op = r.r(m.op_t);
    let auth = r.r(p.t.bool_);
    let f = r.r(p.t.bool_);
    let cb = r.r(p.bool_cb_t);
    let v = r.r(p.t.void);
    let zc = int_const(code, 0);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: content,
        obj: src,
        field: m.content.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: content,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: ad,
        obj: content,
        field: m.proxy_array,
    });
    a.op(Opcode::SafeCast { dst: arr, src: ad });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: m.arr_len,
    });
    a.op(Opcode::Int { dst: zero, ptr: zc });
    a.op(Opcode::Int { dst: i, ptr: zc });
    a.loop_head("loop");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "end",
    );
    // the array may have been shortened by an earlier move: re-check the bound
    a.op(Opcode::Field {
        dst: k,
        obj: arr,
        field: m.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: k,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: m.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "next");
    a.op(Opcode::ToVirtual { dst: slot, src: d });
    a.op(Opcode::Field {
        dst: item,
        obj: slot,
        field: m.slot_k.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: item,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::Field {
        dst: kind,
        obj: item,
        field: m.item_kind,
    });
    a.jmp(
        Opcode::JNull {
            reg: kind,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::Null { dst: q });
    a.op(Opcode::Null { dst: rb });
    a.op(Opcode::Call4 {
        dst: k,
        fun: m.count,
        arg0: filter,
        arg1: kind,
        arg2: q,
        arg3: rb,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: k,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::Call1 {
        dst: cnt,
        fun: m.get_count,
        arg0: slot,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: cnt,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::MakeEnum {
        dst: op,
        construct: m.move_to,
        args: vec![dst, cnt],
    });
    a.op(Opcode::Bool {
        dst: f,
        value: hlbc::types::ValBool(false),
    });
    a.op(Opcode::Call1 {
        dst: auth,
        fun: m.has_authority,
        arg0: src,
    });
    a.jmp(
        Opcode::JFalse {
            cond: auth,
            offset: 0,
        },
        "rpc",
    );
    a.op(Opcode::Call4 {
        dst: auth,
        fun: m.local_op,
        arg0: src,
        arg1: i,
        arg2: op,
        arg3: f,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "next");
    a.label("rpc");
    a.op(Opcode::Null { dst: cb });
    a.op(Opcode::CallN {
        dst: v,
        fun: m.net_op,
        args: vec![src, i, op, f, cb],
    });
    a.label("next");
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![m.inv_t, m.inv_t, m.inv_t],
        p.t.void,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `quickStack(icon)` / `quickTake(icon)`: quickMove between the local player's
/// inventory and the shared chest, then the sort-confirm sound.
fn add_quick_side(code: &mut Bytecode, p: &Plan, quick_move: RefFun, take: bool) -> Result<RefFun> {
    let mut r = Regs(vec![p.icon_t]);
    let gc = r.r(p.game_cls_t);
    let game = r.r(p.game_t);
    let st = r.r(p.game_state.1);
    let camp = r.r(p.get_camp.1);
    let name = r.r(p.t.str_);
    let rb = r.r(p.t.ref_bool);
    let tool = r.r(p.get_tool.1);
    let chest = r.r(p.tool_inv.1);
    let me = r.r(p.mv.game_me.1);
    let mine = r.r(p.mv.inv_t);
    let ui = r.r(p.game_ui.1);
    let d = r.r(p.t.dyn_);
    let v = r.r(p.t.void);
    let mut a = Asm::new();
    chest_lookup(&mut a, p, gc, game, st, camp, name, rb, tool, chest);
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.mv.game_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: mine,
        obj: me,
        field: p.mv.player_inv,
    });
    a.jmp(
        Opcode::JNull {
            reg: mine,
            offset: 0,
        },
        "end",
    );
    let (src, dst) = if take { (chest, mine) } else { (mine, chest) };
    a.op(Opcode::Call3 {
        dst: v,
        fun: quick_move,
        arg0: src,
        arg1: dst,
        arg2: dst,
    });
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.game_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: name,
        global: p.sfx_confirm,
    });
    a.op(Opcode::Null { dst: d });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.sfx,
        arg0: ui,
        arg1: name,
        arg2: d,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.icon_t], p.t.void, r.0, a.finish(), p.dbg_file)
}
/// Registers the constructor block needs, appended to the constructor.
struct CtorRegs {
    ci: Reg,
    cp: Reg,
    row: Reg,
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
    ip: Reg,
    o: Reg,
    icon: Reg,
    cl: Reg,
    texts: Reg,
    data: Reg,
    vt: Reg,
    cc: Reg,
}

/// `createNew(comp, parent, [arg?], {attrs})` into `dst` (domkit.Properties).
#[allow(clippy::too_many_arguments)]
fn emit_create(
    a: &mut Asm,
    code: &mut Bytecode,
    p: &Plan,
    r: &CtorRegs,
    dst: Reg,
    parent: Reg,
    comp_g: RefGlobal,
    arg_g: Option<RefGlobal>,
    attrs: &[(&str, &str)],
) {
    let b = &p.blk;
    a.op(Opcode::GetGlobal {
        dst: r.comp,
        global: comp_g,
    });
    a.op(Opcode::Int {
        dst: r.n,
        ptr: int_const(code, arg_g.is_some() as i32),
    });
    a.op(b.arr_elem_t_op_for(r.ty));
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
    if let Some(g) = arg_g {
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
        value: hlbc::types::ValBool(true),
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
    a.op(Opcode::New { dst: r.attrs });
    for (k, v) in attrs {
        let ks = string_ref(code, k);
        let vg = job_xp::str_global(code, p.t.str_, leak(v));
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
    a.op(Opcode::Call4 {
        dst,
        fun: b.create,
        arg0: r.comp,
        arg1: parent,
        arg2: r.args,
        arg3: r.attrs,
    });
}

/// str_global wants `'static`; the attribute texts are compile-time constants.
fn leak(v: &str) -> &'static str {
    Box::leak(v.to_owned().into_boxed_str())
}

impl IconBlock {
    fn arr_elem_t_op_for(&self, dst: Reg) -> Opcode {
        match self.arr_elem_t_op {
            Opcode::Type { ty, .. } => Opcode::Type { dst, ty },
            _ => unreachable!(),
        }
    }
}

/// An icon `id` in `row` whose click runs `handler(icon)`; `tip` replaces the
/// icon's own cdb title with `Texts...campChest.<tip>`.
fn emit_icon(
    a: &mut Asm,
    code: &mut Bytecode,
    p: &Plan,
    r: &CtorRegs,
    id: RefGlobal,
    handler: RefFun,
    tip: Option<(RefField, &'static str)>,
) {
    emit_create(
        a,
        code,
        p,
        r,
        r.ip,
        r.row,
        p.blk.icon_g,
        Some(id),
        &[("scale", ICON_SCALE), ("cursor", "button")],
    );
    a.op(Opcode::Field {
        dst: r.o,
        obj: r.ip,
        field: p.blk.props_obj,
    });
    a.op(Opcode::SafeCast {
        dst: r.icon,
        src: r.o,
    });
    a.op(Opcode::InstanceClosure {
        dst: r.cl,
        fun: handler,
        obj: r.icon,
    });
    a.op(Opcode::SetField {
        obj: r.icon,
        field: p.blk.onclick,
        src: r.cl,
    });
    let Some((tip, skip)) = tip else {
        return;
    };
    let m = &p.mv;
    a.op(Opcode::GetGlobal {
        dst: r.texts,
        global: m.texts_g,
    });
    a.op(Opcode::Field {
        dst: r.data,
        obj: r.texts,
        field: m.texts_data.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.data,
            offset: 0,
        },
        skip,
    );
    a.op(Opcode::DynGet {
        dst: r.data,
        obj: r.data,
        field: m.texts_group,
    });
    a.op(Opcode::ToVirtual {
        dst: r.vt,
        src: r.data,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.vt,
            offset: 0,
        },
        skip,
    );
    a.op(Opcode::Field {
        dst: r.cc,
        obj: r.vt,
        field: m.camp_chest.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.cc,
            offset: 0,
        },
        skip,
    );
    a.op(Opcode::Field {
        dst: r.sv,
        obj: r.cc,
        field: tip,
    });
    a.op(Opcode::Call2 {
        dst: r.sv,
        fun: m.set_tip,
        arg0: r.icon,
        arg1: r.sv,
    });
    a.op(Opcode::Bool {
        dst: r.b,
        value: hlbc::types::ValBool(false),
    });
    a.op(Opcode::SetField {
        obj: r.icon,
        field: m.title_tip,
        src: r.b,
    });
    a.label(skip);
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let pick = add_sort_pick(code, &p)?;
    let menu = add_sort_menu(code, &p, pick)?;
    let quick_move = add_quick_move(code, &p)?;
    let stack = add_quick_side(code, &p, quick_move, false)?;
    let take = add_quick_side(code, &p, quick_move, true)?;
    let take_icon_g = job_xp::str_global(code, p.t.str_, "LootAll");

    let type_t = prim_type(code, "Type", |t| matches!(t, Type::Type))?;
    let h2d_obj = obj_type(code, "h2d.Object")?;
    let mut regs = Regs(std::mem::take(&mut code.functions[p.ctor_fi].regs));
    let r = CtorRegs {
        ci: regs.r(p.chest_inv.1),
        cp: regs.r(p.blk.props_t),
        row: regs.r(p.blk.props_t),
        comp: regs.r(p.t.str_),
        n: regs.r(p.t.i32_),
        ty: regs.r(type_t),
        raw: regs.r(p.blk.arr_t),
        arr: regs.r(p.blk.arr_t),
        wrapped: regs.r(p.blk.arr_obj_t),
        b: regs.r(p.t.bool_),
        rb: regs.r(p.t.ref_bool),
        args: regs.r(p.t.arr_dyn),
        attrs: regs.r(p.t.dynobj),
        sv: regs.r(p.t.str_),
        ip: regs.r(p.blk.props_t),
        o: regs.r(h2d_obj),
        icon: regs.r(p.icon_t),
        cl: regs.r(p.blk.closure_t),
        texts: regs.r(code.globals[p.mv.texts_g.0]),
        data: regs.r(p.mv.texts_data.1),
        vt: regs.r(p.mv.texts_vt),
        cc: regs.r(p.mv.camp_chest.1),
    };
    code.functions[p.ctor_fi].regs = regs.0;
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: r.ci,
        field: p.chest_inv.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.ci,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: r.cp,
        obj: r.ci,
        field: p.elem_dom,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.cp,
            offset: 0,
        },
        "end",
    );
    emit_create(
        &mut a,
        code,
        &p,
        &r,
        r.row,
        r.cp,
        p.flow_g,
        None,
        &[
            ("position", "absolute"),
            ("align", "top left"),
            ("offset", ROW_OFFSET),
            ("hspacing", ROW_SPACING),
            ("content-valign", "middle"),
        ],
    );
    emit_icon(&mut a, code, &p, &r, p.sort_icon_g, menu, None);
    emit_icon(
        &mut a,
        code,
        &p,
        &r,
        p.mv.stack_icon_g,
        stack,
        Some((p.mv.to_chest, "tip_stack")),
    );
    emit_icon(
        &mut a,
        code,
        &p,
        &r,
        take_icon_g,
        take,
        Some((p.mv.to_inv, "tip_take")),
    );
    a.label("end");
    // `end` is the constructor's own Ret, which follows the inserted block.
    a.op(Opcode::Label);
    let mut ops = a.finish();
    ops.pop(); // drop the placeholder: "end" now resolves to the op after the block
    let f = &mut code.functions[p.ctor_fi];
    let at = f.ops.len() - 1;
    insert_ops(f, at, ops);
    eprintln!(
        "patched chest buttons fn@{}: chest sort fn@{} (pick fn@{}), quick stack fn@{}, quick take fn@{} (move fn@{})",
        f.findex.0, menu.0, pick.0, stack.0, take.0, quick_move.0
    );
    Ok(())
}

/// Adds the shared-chest buttons, or leaves `code` untouched and logs why.
pub(crate) fn patch_chest_buttons(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("chest buttons skipped: {e:#}"));
            return;
        }
    };
    let snap = Snap::take(code);
    let ctor_before = code.functions[p.ctor_fi].clone();
    if let Err(e) = apply(code, p) {
        crate::skipped(format!("chest buttons skipped: {e:#}"));
        let fi = code
            .functions
            .iter()
            .position(|f| f.findex == ctor_before.findex)
            .expect("ctor");
        code.functions[fi] = ctor_before;
        snap.restore(code);
    }
}

fn jump_targets_of(op: &Opcode) -> Vec<i32> {
    match op {
        Opcode::JTrue { offset, .. }
        | Opcode::JFalse { offset, .. }
        | Opcode::JNull { offset, .. }
        | Opcode::JNotNull { offset, .. }
        | Opcode::JSLt { offset, .. }
        | Opcode::JSGte { offset, .. }
        | Opcode::JSGt { offset, .. }
        | Opcode::JSLte { offset, .. }
        | Opcode::JULt { offset, .. }
        | Opcode::JUGte { offset, .. }
        | Opcode::JNotLt { offset, .. }
        | Opcode::JNotGte { offset, .. }
        | Opcode::JEq { offset, .. }
        | Opcode::JNotEq { offset, .. }
        | Opcode::JAlways { offset }
        | Opcode::Trap { offset, .. } => vec![*offset],
        Opcode::Switch { .. } => vec![0],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Patches a copy of the installed game (skipped when absent): the constructor
    /// only gains a block before its Ret, the new functions are well typed, the image
    /// round-trips, and a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let ctor_fi = p.ctor_fi;
        let mut code = read(&image);
        patch_chest_buttons(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        let added = back.functions.len() - nf;
        assert!(added >= 2, "new functions appended");
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == ctor_fi {
                let at = a.ops.len() - 1;
                let n = b.ops.len() - a.ops.len();
                assert!(n > 20);
                shifted(a, b, at, n);
                // no original jump lands on the Ret (it would skip the new block)
                for k in 0..a.ops.len() {
                    assert!(
                        !jump_targets(a, k).contains(&at),
                        "ctor op {k} jumps to Ret"
                    );
                }
                check_types(&back, b, at..b.ops.len());
                check_flow(b);
            } else {
                assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops), "fn#{i}");
                assert_eq!(a.regs, b.regs, "fn#{i}");
            }
        }
        for f in &back.functions[nf..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        let (pick, menu) = (&back.functions[nf], &back.functions[nf + 1]);
        assert!(menu
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { arg0: Reg(6), .. })));
        let net_sort = method(&back, obj_type(&back, "st.Inventory").unwrap(), "netSortBy")
            .unwrap()
            .findex;
        assert!(pick
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call3 { fun, arg1: Reg(0), .. } if *fun == net_sort)));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_chest_buttons(&mut again);
        assert!(write(&again) == patched);
    }
}
