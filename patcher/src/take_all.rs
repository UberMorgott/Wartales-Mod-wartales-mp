// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// "Take all" icon on the small loot windows: a searched barrel / crate / chest
// (ui.win.SearchElement) and the item grid a dialog shows for an element with
// items (Dialog.inv). Both are a `ui.win.ElementInventory`, whose `rebuild`
// builds the grid as a `ui.comp.Inventory` in ItemSlotMode.FoundItems (take
// only; trade, steal, choose-a-reward (Select) and locked (ViewOnly) use other
// modes and get no icon). Vanilla has no take-all there, in solo or co-op.
//
// ElementInventory.rebuild gets, before its Ret:
//
//   var inv = Std.downcast(invProps.obj, ui.comp.Inventory);   // the grid it just built
//   if (inv != null && inv.mode.index == FoundItems && dom != null) {
//       var i = createNew("icon", dom, ["LootAll"], {position: "absolute", align: "top right",
//           offset, scale, cursor: "button", networkable: "false"}).obj;
//       i.onClick = takeAll.bind(inv);
//   }
//
// The icon is the post-battle loot button's icon "LootAll"; Icon.set_icon gives it
// the same localized cdb title the Debrief button shows, as its tooltip. It is not
// networkable (like vanilla's per-player steal button there), so a click runs on
// the clicking machine only, and is not a wait-all-players button (coop_gates G1).
//
// `takeAll(inv)` runs, for every grid slot from the last to the first, exactly
// what a right click on a FoundItems slot runs (ItemSlot.onRightClick, no
// modifier key: the whole stack):
//
//   var s = inv.slots[i]; if (s == null || s.mode.index != FoundItems) continue;
//   var it = s.get_item(); if (it == null || it.k == null || it.k.locked) continue;
//   var n = it.count; if (n <= 0) continue;
//   <FoundItems right-click body>({item: it, slot: s}, n);
//
// That body (an anonymous closure function of onRightClick) is vanilla's take:
// onPick (OnPickFromInventory signal), target = getActionPlayer().inventory (or
// the global inventory for global items), then
// `slot.api.networkOperation(MoveTo(target, n), slot, cb)`: on the host / in solo
// the stack moves locally; on a client the container is never touched locally:
// the st.Inventory networkOperation RPC has the host remove the stack, and only
// when the host confirms does the client add it to its own player inventory
// (the one inventory a client owns: st.Inventory.hasAuthority is owner == me),
// exactly as a manual right click does. No new network path. Last slot first,
// so a container that closes the gap after a removal keeps every lower index
// valid; each slot reads its stack live, at its own index. Player inventories
// have no size limit; carry weight is vanilla's soft overload, as with a manual
// take.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::*;
use super::*;
use hlbc::types::{RefEnumConstruct, RefGlobal};

const ICON_SCALE: &str = "0.625";
/// Above the grid's top right corner, left of the window's close icon.
const ICON_OFFSET: &str = "-4 -37";

/// What `createNew(comp, parent, [args], attrs)` in rebuild is made of.
struct Create {
    alloc: RefFun,
    elem_ty: RefType,
    wrap: RefFun,
    dyn_alloc: RefFun,
    create: RefFun,
    raw_t: RefType,
    arr_t: RefType,
    wrapped_t: RefType,
    args_t: RefType,
    ref_bool: RefType,
}

/// The FoundItems branch of ItemSlot.onRightClick.
struct Take {
    slot_t: RefType,
    slot_mode: RefField,
    get_item: RefFun,
    get_item_t: RefType,
    item_t: RefType,
    item_k: RefField,
    k_t: RefType,
    get_locked: RefFun,
    ctx_t: RefType,
    body: RefFun,
    get_count: RefFun,
    count_arg_t: RefType,
}

struct Plan {
    fi: usize,
    dbg_file: usize,
    /// Properties register of the rebuild's "inventory" createNew.
    inv_props: Reg,
    props_t: RefType,
    props_obj: RefField,
    h2d_obj: RefType,
    dom: RefField,
    inv_t: RefType,
    inv_mode: (RefField, RefType),
    slots: (RefField, RefType),
    arr_len: RefField,
    arr_raw: (RefField, RefType),
    found: usize,
    icon_t: RefType,
    onclick: (RefField, RefType),
    mk: Create,
    take: Take,
    str_t: RefType,
    dynobj: RefType,
    i32_: RefType,
    bool_: RefType,
    void: RefType,
    dyn_: RefType,
    type_t: RefType,
}

/// Last op before `at` that writes register `r` (by its `dst`).
fn writer(f: &Function, at: usize, r: Reg) -> Option<usize> {
    f.ops[..at].iter().rposition(|o| {
        matches!(o,
            Opcode::Call1 { dst, .. } | Opcode::Call2 { dst, .. } | Opcode::Call4 { dst, .. }
            | Opcode::GetGlobal { dst, .. } | Opcode::Type { dst, .. } | Opcode::UnsafeCast { dst, .. }
            if *dst == r)
    })
}

fn plan_create(f: &Function, ci: usize) -> Result<Create> {
    let Opcode::Call4 {
        fun: create,
        arg2: args,
        ..
    } = f.ops[ci]
    else {
        bail!("not a createNew call");
    };
    let di = writer(f, ci, args).context("no ArrayDyn.alloc")?;
    let Opcode::Call2 {
        fun: dyn_alloc,
        arg0: wrapped,
        arg1: rb,
        ..
    } = f.ops[di]
    else {
        bail!("createNew args are not ArrayDyn.alloc(..)");
    };
    let wi = writer(f, di, wrapped).context("no array wrap")?;
    let Opcode::Call1 {
        fun: wrap,
        arg0: arr,
        ..
    } = f.ops[wi]
    else {
        bail!("createNew args are not wrapped");
    };
    let ci2 = writer(f, wi, arr).context("no array cast")?;
    let Opcode::UnsafeCast { src: raw, .. } = f.ops[ci2] else {
        bail!("createNew array is not cast");
    };
    let ai = writer(f, ci2, raw).context("no alloc_array")?;
    let Opcode::Call2 {
        fun: alloc,
        arg0: ty,
        ..
    } = f.ops[ai]
    else {
        bail!("createNew array is not alloc_array");
    };
    let ti = writer(f, ai, ty).context("no element type")?;
    let Opcode::Type { ty: elem_ty, .. } = f.ops[ti] else {
        bail!("alloc_array element type is not a Type op");
    };
    let rt = |r: Reg| f.regs[r.0 as usize];
    Ok(Create {
        alloc,
        elem_ty,
        wrap,
        dyn_alloc,
        create,
        raw_t: rt(raw),
        arr_t: rt(arr),
        wrapped_t: rt(wrapped),
        args_t: rt(args),
        ref_bool: rt(rb),
    })
}

fn plan_take(code: &Bytecode, mode_t: RefType, found: usize) -> Result<Take> {
    let slot_t = obj_type(code, "ui.comp.ItemSlot")?;
    let (slot_mode, sm_t) = field(code, slot_t, "mode")?;
    if sm_t != mode_t {
        bail!("ItemSlot.mode is not ItemSlotMode");
    }
    let rc = method(code, slot_t, "onRightClick")?;
    let o = &rc.ops;
    let rt = |r: Reg| rc.regs[r.0 as usize];
    // var it = get_item(); (virtual) ... it.k.locked
    let (
        Some(Opcode::Call1 {
            dst: g,
            fun: get_item,
            arg0: Reg(0),
        }),
        Some(Opcode::ToVirtual { dst: item_r, src }),
    ) = (o.first(), o.get(1))
    else {
        bail!("onRightClick does not start with get_item");
    };
    if src != g || fname(code, *get_item) != "get_item" {
        bail!("onRightClick does not start with get_item");
    }
    let (item_k, k_r, get_locked) = (2..o.len().min(12))
        .find_map(|i| match (&o[i - 1], &o[i]) {
            (
                Opcode::Field {
                    dst: k,
                    obj,
                    field: kf,
                },
                Opcode::NullCheck { .. },
            ) if obj == item_r => o[i + 1..i + 3].iter().find_map(|x| match x {
                Opcode::Call1 { fun, arg0, .. }
                    if arg0 == k && fname(code, *fun) == "get_locked" =>
                {
                    Some((*kf, *k, *fun))
                }
                _ => None,
            }),
            _ => None,
        })
        .context("onRightClick: no item.k.locked test")?;
    // switch (this.mode) { case FoundItems: ... }
    let sw = o
        .iter()
        .position(|x| matches!(x, Opcode::Switch { .. }))
        .context("onRightClick: no mode switch")?;
    let Opcode::Switch {
        reg: sw_r, offsets, ..
    } = &o[sw]
    else {
        unreachable!()
    };
    let mode_read = matches!(o[..sw].iter().rev().take(3).collect::<Vec<_>>()[..],
        [Opcode::EnumIndex { dst, value }, Opcode::NullCheck { .. }, Opcode::GetThis { dst: m, field }]
        if dst == sw_r && value == m && *field == slot_mode);
    if !mode_read {
        bail!("onRightClick: switch is not on this.mode");
    }
    let target = sw
        + 1
        + *offsets
            .get(found)
            .context("switch has no FoundItems case")? as usize;
    let (
        Some(Opcode::EnumAlloc {
            dst: c,
            construct: RefEnumConstruct(0),
        }),
        Some(Opcode::SetEnumField {
            value: c1,
            field: RefField(0),
            src: s0,
        }),
        Some(Opcode::SetEnumField {
            value: c2,
            field: RefField(1),
            src: Reg(0),
        }),
        Some(Opcode::InstanceClosure {
            dst: h,
            fun: body,
            obj: c3,
        }),
    ) = (
        o.get(target),
        o.get(target + 1),
        o.get(target + 2),
        o.get(target + 3),
    )
    else {
        bail!("onRightClick: FoundItems case is not the take closure");
    };
    if [c1, c2, c3] != [c, c, c] || s0 != item_r {
        bail!("onRightClick: FoundItems closure context is not (item, slot)");
    }
    // the plain click: body(get_count(it)) -- ToVirtual x = it; n = get_count(x); h(n)
    let (get_count, count_arg_t) = (target + 4..o.len().min(target + 20))
        .find_map(|i| match (&o[i - 2], &o[i - 1], &o[i]) {
            (
                Opcode::ToVirtual { dst: x, src },
                Opcode::Call1 { dst: n, fun, arg0 },
                Opcode::CallClosure { fun: hc, args, .. },
            ) if src == item_r && arg0 == x && hc == h && args[..] == [*n] => Some((*fun, rt(*x))),
            _ => None,
        })
        .context("onRightClick: FoundItems case does not take the whole stack")?;
    if fname(code, get_count) != "get_count" {
        bail!("onRightClick: stack count is not get_count");
    }
    // The body must be vanilla's host-authoritative take: MoveTo through the slot
    // api's networkOperation, into the action player's inventory.
    let bf = &code.functions[fun_index(code, *body)?];
    let bt = bf.t.as_fun(code).context("take body type")?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    if bt.args != [rt(*c), i32_t] || !matches!(code.types[bt.ret.0], Type::Void) {
        bail!("take body is not (ctx, Int) -> Void");
    }
    let game_t = obj_type(code, "Game")?;
    let action_player = method(code, game_t, "getActionPlayer")?.findex;
    // `api.networkOperation(op, slot, cb)` where op = MoveTo(..) built in the body.
    let move_to: Vec<Reg> = bf
        .ops
        .iter()
        .filter_map(|x| match x {
            Opcode::MakeEnum { dst, construct, .. } => match &code.types[bf.regs[dst.0 as usize].0]
            {
                Type::Enum { constructs, .. } => constructs
                    .get(construct.0)
                    .is_some_and(|k| s(code, k.name) == "MoveTo")
                    .then_some(*dst),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let net = bf.ops.iter().any(|x| {
        let Opcode::CallMethod { field, args, .. } = x else {
            return false;
        };
        let [api, op, ..] = args[..] else {
            return false;
        };
        let called = match &code.types[bf.regs[api.0 as usize].0] {
            Type::Virtual { fields } => fields.get(field.0).map(|f| s(code, f.name)),
            _ => None,
        };
        called == Some("networkOperation") && move_to.contains(&op)
    });
    if !net || !calls(bf, action_player) {
        bail!("take body is not MoveTo(actionPlayer inventory) via networkOperation");
    }
    Ok(Take {
        slot_t,
        slot_mode,
        get_item: *get_item,
        get_item_t: rt(*g),
        item_t: rt(*item_r),
        item_k,
        k_t: rt(k_r),
        get_locked,
        ctx_t: rt(*c),
        body: *body,
        get_count,
        count_arg_t,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let prim = |what, pred: fn(&Type) -> bool| prim_type(code, what, pred);
    let str_t = obj_type(code, "String")?;
    let ei_t = obj_type(code, "ui.win.ElementInventory")?;
    let rebuild = method(code, ei_t, "rebuild")?;
    let fi = fun_index(code, rebuild.findex)?;
    let f = rebuild;
    let ret_at = f.ops.len() - 1;
    if !matches!(f.ops[ret_at], Opcode::Ret { .. }) {
        bail!("ElementInventory.rebuild does not end with Ret");
    }
    // Applied: only this pass binds a closure to the grid (a ui.comp.Inventory).
    let inv_t = obj_type(code, "ui.comp.Inventory")?;
    if f.ops.iter().any(
        |o| matches!(o, Opcode::InstanceClosure { obj, .. } if f.regs[obj.0 as usize] == inv_t),
    ) {
        bail!("already applied");
    }
    for i in 0..f.ops.len() {
        if jump_targets(f, i).contains(&ret_at) {
            bail!("rebuild op {i} jumps to its Ret");
        }
    }
    // The grid: P = createNew("inventory", ..), written nowhere else.
    let ci = (0..f.ops.len())
        .filter(|&i| {
            let Opcode::Call4 { arg0, .. } = f.ops[i] else {
                return false;
            };
            writer(f, i, arg0).is_some_and(|w| {
                matches!(f.ops[w], Opcode::GetGlobal { global, .. }
                if job_xp::const_str(code, global) == Some("inventory"))
            })
        })
        .collect::<Vec<_>>();
    let [ci] = ci[..] else {
        bail!(
            "rebuild: expected one \"inventory\" createNew, found {}",
            ci.len()
        );
    };
    let Opcode::Call4 { dst: inv_props, .. } = f.ops[ci] else {
        unreachable!()
    };
    let (w1, w2) = (
        format!("dst: Reg({}),", inv_props.0),
        format!("dst: Reg({}) ", inv_props.0),
    );
    let writes = f
        .ops
        .iter()
        .filter(|o| {
            let d = format!("{o:?}");
            d.contains(&w1) || d.contains(&w2)
        })
        .count();
    if writes != 1 {
        bail!("rebuild: grid Properties register is reused");
    }
    let props_t = f.regs[inv_props.0 as usize];
    let mk = plan_create(f, ci)?;
    let (props_obj, h2d_obj) = field(code, props_t, "obj")?;
    let (dom, dom_t) = field(code, ei_t, "dom")?;
    if dom_t != props_t {
        bail!("ElementInventory.dom is not domkit.Properties");
    }
    let dbg_file = f
        .debug_info
        .as_ref()
        .and_then(|d| d.first())
        .map(|x| x.0)
        .unwrap_or(0);

    let inv_mode = field(code, inv_t, "mode")?;
    let Type::Enum { constructs, .. } = &code.types[inv_mode.1 .0] else {
        bail!("Inventory.mode is not an enum");
    };
    let found = constructs
        .iter()
        .position(|c| s(code, c.name) == "FoundItems")
        .context("ItemSlotMode.FoundItems not found")?;
    if !constructs[found].params.is_empty() {
        bail!("ItemSlotMode.FoundItems has parameters");
    }
    let slots = field(code, inv_t, "slots")?;
    let arr_obj_t = obj_type(code, "hl.types.ArrayObj")?;
    if slots.1 != arr_obj_t {
        bail!("Inventory.slots is not an ArrayObj");
    }
    let (arr_len, len_t) = field(code, arr_obj_t, "length")?;
    let i32_ = prim("I32", |t| matches!(t, Type::I32))?;
    if len_t != i32_ {
        bail!("ArrayObj.length is not I32");
    }
    let arr_raw = field(code, arr_obj_t, "array")?;
    let icon_t = obj_type(code, "ui.comp.Icon")?;
    let onclick = field(code, icon_t, "onClick")?;
    let void = prim("void", |t| matches!(t, Type::Void))?;
    if !matches!(&code.types[onclick.1 .0], Type::Fun(t) if t.args.is_empty() && t.ret == void) {
        bail!("Icon.onClick is not () -> Void");
    }
    let take = plan_take(code, inv_mode.1, found)?;
    Ok(Plan {
        fi,
        dbg_file,
        inv_props,
        props_t,
        props_obj,
        h2d_obj,
        dom,
        inv_t,
        inv_mode,
        slots,
        arr_len,
        arr_raw,
        found,
        icon_t,
        onclick,
        mk,
        take,
        str_t,
        dynobj: prim("DynObj", |t| matches!(t, Type::DynObj))?,
        i32_,
        bool_: prim("Bool", |t| matches!(t, Type::Bool))?,
        void,
        dyn_: prim("Dyn", |t| matches!(t, Type::Dyn))?,
        type_t: prim("Type", |t| matches!(t, Type::Type))?,
    })
}

/// `takeAll(inv)`: the FoundItems right-click take on every slot, last first.
fn add_take_all(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let t = &p.take;
    let mut r = Regs(vec![p.inv_t]);
    let inv = Reg(0);
    let slots = r.r(p.slots.1);
    let raw = r.r(p.arr_raw.1);
    let i = r.r(p.i32_);
    let k = r.r(p.i32_);
    let zero = r.r(p.i32_);
    let found = r.r(p.i32_);
    let d = r.r(p.dyn_);
    let s = r.r(t.slot_t);
    let m = r.r(p.inv_mode.1);
    let e = r.r(p.i32_);
    let g = r.r(t.get_item_t);
    let item = r.r(t.item_t);
    let it = r.r(t.k_t);
    let locked = r.r(p.bool_);
    let ctx = r.r(t.ctx_t);
    let x = r.r(t.count_arg_t);
    let n = r.r(p.i32_);
    let v = r.r(p.void);
    let zc = int_const(code, 0);
    let fc = int_const(code, p.found as i32);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: slots,
        obj: inv,
        field: p.slots.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: slots,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: i,
        obj: slots,
        field: p.arr_len,
    });
    a.op(Opcode::Int { dst: zero, ptr: zc });
    a.op(Opcode::Int {
        dst: found,
        ptr: fc,
    });
    a.loop_head("loop");
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: i,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Decr { dst: i });
    a.op(Opcode::Field {
        dst: k,
        obj: slots,
        field: p.arr_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: k,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: slots,
        field: p.arr_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::UnsafeCast { dst: s, src: d });
    a.jmp(Opcode::JNull { reg: s, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: m,
        obj: s,
        field: t.slot_mode,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "loop");
    a.op(Opcode::EnumIndex { dst: e, value: m });
    a.jmp(
        Opcode::JNotEq {
            a: e,
            b: found,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Call1 {
        dst: g,
        fun: t.get_item,
        arg0: s,
    });
    a.op(Opcode::ToVirtual { dst: item, src: g });
    a.jmp(
        Opcode::JNull {
            reg: item,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: it,
        obj: item,
        field: t.item_k,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "loop");
    a.op(Opcode::Call1 {
        dst: locked,
        fun: t.get_locked,
        arg0: it,
    });
    a.jmp(
        Opcode::JTrue {
            cond: locked,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::ToVirtual { dst: x, src: item });
    a.op(Opcode::Call1 {
        dst: n,
        fun: t.get_count,
        arg0: x,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: n,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::EnumAlloc {
        dst: ctx,
        construct: RefEnumConstruct(0),
    });
    a.op(Opcode::SetEnumField {
        value: ctx,
        field: RefField(0),
        src: item,
    });
    a.op(Opcode::SetEnumField {
        value: ctx,
        field: RefField(1),
        src: s,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: t.body,
        arg0: ctx,
        arg1: n,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.inv_t], p.void, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let take_all = add_take_all(code, &p)?;
    let icon_g = job_xp::str_global(code, p.str_t, "icon");
    let loot_g = job_xp::str_global(code, p.str_t, "LootAll");
    let attrs: Vec<(hlbc::types::RefString, RefGlobal)> = [
        ("position", "absolute"),
        ("align", "top right"),
        ("offset", ICON_OFFSET),
        ("scale", ICON_SCALE),
        ("cursor", "button"),
        ("networkable", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (string_ref(code, k), job_xp::str_global(code, p.str_t, v)))
    .collect();
    let zc = int_const(code, 0);
    let oc = int_const(code, 1);
    let fc = int_const(code, p.found as i32);

    let mk = &p.mk;
    let mut regs = Regs(std::mem::take(&mut code.functions[p.fi].regs));
    let o = regs.r(p.h2d_obj);
    let inv = regs.r(p.inv_t);
    let m = regs.r(p.inv_mode.1);
    let e = regs.r(p.i32_);
    let fr = regs.r(p.i32_);
    let dom = regs.r(p.props_t);
    let comp = regs.r(p.str_t);
    let nr = regs.r(p.i32_);
    let ty = regs.r(p.type_t);
    let raw = regs.r(mk.raw_t);
    let arr = regs.r(mk.arr_t);
    let sv = regs.r(p.str_t);
    let wrapped = regs.r(mk.wrapped_t);
    let b = regs.r(p.bool_);
    let rb = regs.r(mk.ref_bool);
    let args = regs.r(mk.args_t);
    let at = regs.r(p.dynobj);
    let ip = regs.r(p.props_t);
    let icon = regs.r(p.icon_t);
    let cl = regs.r(p.onclick.1);
    code.functions[p.fi].regs = regs.0;

    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: p.inv_props,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: o,
        obj: p.inv_props,
        field: p.props_obj,
    });
    a.op(Opcode::SafeCast { dst: inv, src: o });
    a.jmp(
        Opcode::JNull {
            reg: inv,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: m,
        obj: inv,
        field: p.inv_mode.0,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "end");
    a.op(Opcode::EnumIndex { dst: e, value: m });
    a.op(Opcode::Int { dst: fr, ptr: fc });
    a.jmp(
        Opcode::JNotEq {
            a: e,
            b: fr,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetThis {
        dst: dom,
        field: p.dom,
    });
    a.jmp(
        Opcode::JNull {
            reg: dom,
            offset: 0,
        },
        "end",
    );
    // createNew("icon", dom, ["LootAll"], {attrs})
    a.op(Opcode::GetGlobal {
        dst: comp,
        global: icon_g,
    });
    a.op(Opcode::Int { dst: nr, ptr: oc });
    a.op(Opcode::Type {
        dst: ty,
        ty: mk.elem_ty,
    });
    a.op(Opcode::Call2 {
        dst: raw,
        fun: mk.alloc,
        arg0: ty,
        arg1: nr,
    });
    a.op(Opcode::UnsafeCast { dst: arr, src: raw });
    a.op(Opcode::GetGlobal {
        dst: sv,
        global: loot_g,
    });
    a.op(Opcode::Int { dst: nr, ptr: zc });
    a.op(Opcode::SetArray {
        array: arr,
        index: nr,
        src: sv,
    });
    a.op(Opcode::Call1 {
        dst: wrapped,
        fun: mk.wrap,
        arg0: arr,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: hlbc::types::ValBool(true),
    });
    a.op(Opcode::Ref { dst: rb, src: b });
    a.op(Opcode::Call2 {
        dst: args,
        fun: mk.dyn_alloc,
        arg0: wrapped,
        arg1: rb,
    });
    a.op(Opcode::New { dst: at });
    for (ks, vg) in &attrs {
        a.op(Opcode::GetGlobal {
            dst: sv,
            global: *vg,
        });
        a.op(Opcode::DynSet {
            obj: at,
            field: *ks,
            src: sv,
        });
    }
    a.op(Opcode::Call4 {
        dst: ip,
        fun: mk.create,
        arg0: comp,
        arg1: dom,
        arg2: args,
        arg3: at,
    });
    a.jmp(Opcode::JNull { reg: ip, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: o,
        obj: ip,
        field: p.props_obj,
    });
    a.op(Opcode::SafeCast { dst: icon, src: o });
    a.jmp(
        Opcode::JNull {
            reg: icon,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::InstanceClosure {
        dst: cl,
        fun: take_all,
        obj: inv,
    });
    a.op(Opcode::SetField {
        obj: icon,
        field: p.onclick.0,
        src: cl,
    });
    a.label("end");
    // "end" is rebuild's own Ret, right after the inserted block.
    a.op(Opcode::Label);
    let mut ops = a.finish();
    ops.pop();
    let f = &mut code.functions[p.fi];
    let at_ret = f.ops.len() - 1;
    insert_ops(f, at_ret, ops);
    eprintln!(
        "patched take all fn@{}: Take all icon on found-items windows (take fn@{})",
        f.findex.0, take_all.0
    );
    Ok(())
}

/// Adds the Take all icon to found-items loot windows, or leaves `code` untouched and logs why.
pub(crate) fn patch_take_all(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("take all skipped: {e:#}"));
            return;
        }
    };
    let snap = Snap::take(code);
    let before = code.functions[p.fi].clone();
    let fi = p.fi;
    if let Err(e) = apply(code, p) {
        crate::skipped(format!("take all skipped: {e:#}"));
        code.functions[fi] = before;
        snap.restore(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Patches a copy of the installed game (skipped when absent): rebuild only gains
    /// a block before its Ret, the new function is well typed and calls the vanilla
    /// take body, the image round-trips, and a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, body, found) = (p.fi, p.take.body, p.found);
        let mut code = read(&image);
        patch_take_all(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings[..orig.strings.len()], orig.strings[..]);
        assert_eq!(back.globals[..orig.globals.len()], orig.globals[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == fi {
                let at = a.ops.len() - 1;
                let n = b.ops.len() - a.ops.len();
                assert!(n > 20);
                shifted(a, b, at, n);
                check_types(&back, b, at..b.ops.len());
                check_flow(b);
            } else {
                assert_eq!(format!("{:?}", a.ops), format!("{:?}", b.ops), "fn#{i}");
                assert_eq!(a.regs, b.regs, "fn#{i}");
            }
        }
        let take = &back.functions[nf];
        check_types(&back, take, 0..take.ops.len());
        check_flow(take);
        assert!(take
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == body)));
        assert_eq!(found, 4, "ItemSlotMode.FoundItems index in 1.0.48274");

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_take_all(&mut again);
        assert!(write(&again) == patched);
    }

    /// A FoundItems case that is not the vanilla take closure is refused, and the
    /// pass leaves the image as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let body = plan(&read(&image)).expect("plan").take.body;
        let mut code = read(&image);
        let slot_t = obj_type(&code, "ui.comp.ItemSlot").unwrap();
        let rc = method(&code, slot_t, "onRightClick").unwrap().findex;
        let ri = fun_index(&code, rc).unwrap();
        let at = code.functions[ri]
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::InstanceClosure { fun, .. } if *fun == body))
            .expect("take closure");
        // the slot no longer goes into the closure context
        code.functions[ri].ops[at - 1] = Opcode::Label;
        assert!(plan(&code).is_err());
        let before = write(&code);
        patch_take_all(&mut code);
        assert!(write(&code) == before);
    }
}
