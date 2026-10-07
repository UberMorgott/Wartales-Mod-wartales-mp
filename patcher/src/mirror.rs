// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Shared parts of the in-scene mini-game mirrors (forge_mirror.rs,
// work_mirror.rs): the player doing a local-only mini-game sends tiny events
// over the existing ping RPC (`Controller.ping(x, y, z, player)`: any player
// -> host -> every machine, see ping_cell.rs) with a sentinel x that no world
// point has, one per mirror; `Controller.ping__impl` starts with one
// `if (<mirror>Recv(this, x, y, z, player)) return;` per mirror, which plays
// the event on the worker the other players already see and swallows the
// ping (no marker, no sound). No new RPC, no synced field, no data change.
//
// This module validates and holds what every mirror needs (types, fields,
// the ping RPC and its handler, Entity.play, WaitEvent.wait, logging) and
// appends the shared pieces:
//   - the worker lookup `mirrorWorker(game, uid, camp) -> ent.Entity`: in a
//     PlaceView, uid is the activity element's hxbit uid and the worker is
//     `activityUnits.get(element).unitView` (the unit Activity.setUnit__impl
//     parks there through `addUnitToElement` for the others); in the camp
//     (CampMode), uid is the unit's and the worker is the camp entry entity
//     of that unit (`entryEntities[i].entry.content == Unit(u)`, the loop of
//     setUnit__impl); null otherwise;
//   - `<mirror>IdleOf(e, how) -> String`: each worker's own idle anim, in a
//     per-mirror ObjectMap (worker -> anim): a start (how 0) takes the
//     worker's current anim; a hit (1) reads it, or takes the current anim
//     when the start was missed (joined late); the end (2) reads and forgets
//     it, so a later activity of that worker starts afresh. Two players
//     working at once each keep their own; Game.dispose drops the map, so a
//     worker whose end was missed is not kept past its game;
//   - `<mirror>Idle({e, anim})`: that anim, looped, on a worker still in the
//     scene (the onEnd of a one-shot anim, bound to the worker and the idle
//     it had when the anim started).

use super::asm::{push_fn, string_ref, Asm, Regs};
use super::coop_spectate::dst_of;
use super::diag::static_fn;
use super::ping_cell::vproto;
use super::*;
use hlbc::types::{RefEnumConstruct, RefGlobal, ValBool};

/// What every mirror reads, validated once.
pub(crate) struct Base {
    pub(crate) ent_t: RefType,
    pub(crate) obj_t: RefType,
    pub(crate) ctrl_t: RefType,
    pub(crate) player_t: RefType,
    pub(crate) game_t: RefType,
    pub(crate) mode_t: RefType,
    pub(crate) pv_t: RefType,
    pub(crate) host_t: RefType,
    pub(crate) ctx_t: RefType,
    pub(crate) refs_t: RefType,
    pub(crate) omap_t: RefType,
    pub(crate) ui_t: RefType,
    pub(crate) ev_t: RefType,
    pub(crate) str_t: RefType,
    pub(crate) dyn_t: RefType,
    pub(crate) dynobj_t: RefType,
    pub(crate) f64_: RefType,
    pub(crate) i32_: RefType,
    pub(crate) bool_: RefType,
    pub(crate) void_: RefType,
    pub(crate) nbool_t: RefType,
    pub(crate) arr_t: RefType,
    pub(crate) raw_arr_t: RefType,
    pub(crate) act_t: RefType,
    pub(crate) unit_t: RefType,
    pub(crate) a_target: RefField,
    pub(crate) a_unit: RefField,
    pub(crate) a_host: RefField,
    pub(crate) e_uid: RefField,
    pub(crate) u_uid: RefField,
    pub(crate) g_ctrl: RefField,
    pub(crate) g_me: RefField,
    pub(crate) g_mode: RefField,
    pub(crate) g_event: RefField,
    pub(crate) g_ui: RefField,
    pub(crate) c_game: RefField,
    pub(crate) c_host: RefField,
    pub(crate) h_ctx: RefField,
    pub(crate) x_refs: RefField,
    pub(crate) pv_units: RefField,
    pub(crate) e_obj: RefField,
    pub(crate) e_anim: RefField,
    pub(crate) o_parent: RefField,
    pub(crate) o_children: RefField,
    pub(crate) a_len: RefField,
    pub(crate) a_arr: RefField,
    pub(crate) ping: RefFun,
    pub(crate) check: RefFun,
    pub(crate) pv_cls: RefGlobal,
    pub(crate) pv_cls_t: RefType,
    pub(crate) refs_get: RefFun,
    pub(crate) omap_get: RefFun,
    pub(crate) omap_set: RefFun,
    pub(crate) omap_remove: RefFun,
    pub(crate) omap_new: RefFun,
    pub(crate) play_pi: RefField,
    pub(crate) opts_t: RefType,
    pub(crate) pos_t: RefType,
    pub(crate) end_cb_t: RefType,
    pub(crate) wait_cb_t: RefType,
    pub(crate) wait: RefFun,
    pub(crate) println: RefFun,
    pub(crate) std_string: RefFun,
    pub(crate) str_add: RefFun,
    /// Controller.ping__impl and the register its (last) Ret returns.
    pub(crate) impl_fi: usize,
    pub(crate) impl_ret: Reg,
    /// Game.dispose: the per-worker idle maps go with their game.
    pub(crate) dispose_fi: usize,
}

/// The camp side of the worker lookup, read from Activity.setUnit__impl's loop.
pub(crate) struct Camp {
    pub(crate) cls: RefGlobal,
    pub(crate) cls_t: RefType,
    /// The mode type that holds `entryEntities` (setUnit__impl's cast).
    pub(crate) cm_t: RefType,
    pub(crate) cm_ents: RefField,
    /// The entry entity type and its `entry`, the entry type and its `content`.
    pub(crate) ce_t: RefType,
    pub(crate) ce_entry: RefField,
    pub(crate) en_t: RefType,
    pub(crate) en_content: RefField,
    pub(crate) ct_t: RefType,
    /// The content constructor holding the unit, and its field.
    pub(crate) unit_c: RefEnumConstruct,
    pub(crate) unit_f: RefField,
    pub(crate) unit_idx: i32,
}

pub(crate) fn want(
    code: &Bytecode,
    f: RefFun,
    what: &str,
    args: &[RefType],
    ret: RefType,
) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("unexpected {what} signature");
    }
    Ok(())
}

/// The op before `before` that last writes register `r`.
pub(crate) fn writer(f: &Function, r: Reg, before: usize) -> Option<&Opcode> {
    f.ops[..before].iter().rev().find(|o| dst_of(o) == Some(r))
}

pub(crate) fn enum_constructs(code: &Bytecode, t: RefType) -> Result<Vec<String>> {
    match &code.types[t.0] {
        Type::Enum { constructs, .. } => Ok(constructs
            .iter()
            .map(|c| s(code, c.name).to_string())
            .collect()),
        _ => bail!("type {} is not an enum", t.0),
    }
}

fn last_ret(f: &Function) -> Option<Reg> {
    match f.ops.last() {
        Some(Opcode::Ret { ret }) => Some(*ret),
        _ => None,
    }
}

pub(crate) fn base(code: &Bytecode) -> Result<Base> {
    let f64_ = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let dynobj_t = prim_type(code, "dynobj", |t| matches!(t, Type::DynObj))?;
    let nbool_t = prim_type(
        code,
        "null<bool>",
        |t| matches!(t, Type::Null(x) if *x == bool_),
    )?;
    let str_t = obj_type(code, "String")?;
    let act_t = obj_type(code, "ui.win.Activity")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let ent_t = obj_type(code, "ent.Entity")?;
    let obj_t = obj_type(code, "h3d.scene.Object")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let game_t = obj_type(code, "Game")?;
    let mode_t = obj_type(code, "GameMode")?;
    let pv_t = obj_type(code, "world.PlaceView")?;
    let host_t = obj_type(code, "hxbit.NetworkHost")?;
    let omap_t = obj_type(code, "haxe.ds.ObjectMap")?;
    let ev_t = obj_type(code, "hxd.WaitEvent")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;

    let a_target = typed(code, act_t, "target", ent_t)?;
    let a_unit = typed(code, act_t, "unit", unit_t)?;
    let a_host = typed(code, act_t, "__host", host_t)?;
    let e_uid = typed(code, ent_t, "__uid", i32_)?;
    let u_uid = typed(code, unit_t, "__uid", i32_)?;
    let g_ctrl = typed(code, game_t, "ctrl", ctrl_t)?;
    let g_me = field(code, game_t, "me")?;
    if !is_sub(code, g_me.1, player_t) {
        bail!("Game.me is not a BasePlayer");
    }
    let g_me = g_me.0;
    let g_mode = typed(code, game_t, "mode", mode_t)?;
    let g_event = typed(code, game_t, "globalEvent", ev_t)?;
    let (g_ui, ui_t) = field(code, game_t, "ui")?;
    let c_game = typed(code, ctrl_t, "game", game_t)?;
    let c_host = typed(code, ctrl_t, "__host", host_t)?;
    let (h_ctx, ctx_t) = field(code, host_t, "ctx")?;
    let (x_refs, refs_t) = field(code, ctx_t, "refs")?;
    if s(code, obj(code, refs_t)?.name) != "haxe.ds.IntMap" {
        bail!("hxbit ctx.refs is not an IntMap");
    }
    let pv_units = typed(code, pv_t, "activityUnits", omap_t)?;
    let e_obj = typed(code, ent_t, "obj", obj_t)?;
    let e_anim = typed(code, ent_t, "currentAnim", str_t)?;
    let o_parent = typed(code, obj_t, "parent", obj_t)?;
    let o_children = typed(code, obj_t, "children", arr_t)?;
    let a_len = typed(code, arr_t, "length", i32_)?;
    let (a_arr, raw_arr_t) = field(code, arr_t, "array")?;
    if !matches!(code.types[raw_arr_t.0], Type::Array) {
        bail!("ArrayObj.array is not a raw array");
    }

    // The ping RPC (stub) and its handler.
    let ping = method(code, ctrl_t, "ping")?.findex;
    want(
        code,
        ping,
        "Controller.ping",
        &[ctrl_t, f64_, f64_, f64_, player_t],
        void_,
    )?;
    let imp = method(code, ctrl_t, "ping__impl")?;
    if fun_args(code, imp) != [ctrl_t, f64_, f64_, f64_, player_t] {
        bail!("unexpected Controller.ping__impl signature");
    }
    let impl_fi = fun_index(code, imp.findex)?;
    let impl_ret = last_ret(imp).context("ping__impl does not end in Ret")?;
    if imp.regs[impl_ret.0 as usize] != void_ {
        bail!("ping__impl: Ret register is not void");
    }

    // PlaceView check, as Activity.setUnit__impl does it: check(world.$PlaceView, mode)
    let (pv_cls, pv_cls_t) = class_global(code, "world.PlaceView")?;
    let set_unit = method(code, act_t, "setUnit__impl")?;
    let checks: Vec<RefFun> = set_unit
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call2 {
                fun, arg0, arg1, ..
            } if set_unit.regs[arg0.0 as usize] == pv_cls_t
                && set_unit.regs[arg1.0 as usize] == mode_t =>
            {
                Some(*fun)
            }
            _ => None,
        })
        .collect();
    let [check] = checks[..] else {
        bail!(
            "Activity.setUnit__impl: {} PlaceView checks, want 1",
            checks.len()
        );
    };
    if fname(code, check) != "check" || sig(code, check)?.1 != bool_ {
        bail!("the PlaceView check is not a Bool check");
    }

    let refs_get = proto(code, refs_t, "get")?;
    want(code, refs_get, "IntMap.get", &[refs_t, i32_], dyn_t)?;
    let omap_get = proto(code, omap_t, "get")?;
    want(code, omap_get, "ObjectMap.get", &[omap_t, dyn_t], dyn_t)?;
    let omap_set = proto(code, omap_t, "set")?;
    want(
        code,
        omap_set,
        "ObjectMap.set",
        &[omap_t, dyn_t, dyn_t],
        void_,
    )?;
    let omap_remove = proto(code, omap_t, "remove")?;
    want(
        code,
        omap_remove,
        "ObjectMap.remove",
        &[omap_t, dyn_t],
        bool_,
    )?;
    let omap_new = method(code, omap_t, "__constructor__")?.findex;
    want(code, omap_new, "ObjectMap constructor", &[omap_t], void_)?;

    // Entity.play(anim, opts, pos), virtual
    let (play, play_pi) = vproto(code, ent_t, "play")?;
    let (ppa, ppr) = sig(code, play)?;
    let (opts_t, pos_t) = match ppa[..] {
        [e, a, o, p] if e == ent_t && a == str_t && ppr == void_ => (o, p),
        _ => bail!("unexpected Entity.play signature"),
    };
    if play_pi < 0 {
        bail!("Entity.play is not virtual");
    }
    let (_, loop_t) = field_of_virtual(code, opts_t, "loop")?;
    let (_, end_cb_t) = field_of_virtual(code, opts_t, "onEnd")?;
    if loop_t != nbool_t || !matches!(code.types[pos_t.0], Type::Virtual { .. }) {
        bail!("Entity.play options have unexpected types");
    }
    let wait = proto(code, ev_t, "wait")?;
    let (wa, wr) = sig(code, wait)?;
    if wa.len() != 3 || wa[0] != ev_t || wa[1] != f64_ || wr != void_ {
        bail!("unexpected WaitEvent.wait signature");
    }
    let wait_cb_t = wa[2];
    for t in [wait_cb_t, end_cb_t] {
        match &code.types[t.0] {
            Type::Fun(f) if f.args.is_empty() && f.ret == void_ => {}
            _ => bail!("callback type {} is not () -> Void", t.0),
        }
    }

    let println_f = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println_f) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println_f.findex;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;

    Ok(Base {
        ent_t,
        obj_t,
        ctrl_t,
        player_t,
        game_t,
        mode_t,
        pv_t,
        host_t,
        ctx_t,
        refs_t,
        omap_t,
        ui_t,
        ev_t,
        str_t,
        dyn_t,
        dynobj_t,
        f64_,
        i32_,
        bool_,
        void_,
        nbool_t,
        arr_t,
        raw_arr_t,
        act_t,
        unit_t,
        a_target,
        a_unit,
        a_host,
        e_uid,
        u_uid,
        g_ctrl,
        g_me,
        g_mode,
        g_event,
        g_ui,
        c_game,
        c_host,
        h_ctx,
        x_refs,
        pv_units,
        e_obj,
        e_anim,
        o_parent,
        o_children,
        a_len,
        a_arr,
        ping,
        check,
        pv_cls,
        pv_cls_t,
        refs_get,
        omap_get,
        omap_set,
        omap_remove,
        omap_new,
        play_pi: RefField(play_pi as usize),
        opts_t,
        pos_t,
        end_cb_t,
        wait_cb_t,
        wait,
        println,
        std_string,
        str_add,
        impl_fi,
        impl_ret,
        dispose_fi: game_dispose_fi(code)?,
    })
}

/// The camp lookup, taken from Activity.setUnit__impl:
/// `check(CampMode, mode)`, `for (e in view.entryEntities) switch (e.entry.content) { case Unit(u): ... }`.
pub(crate) fn camp(code: &Bytecode, b: &Base) -> Result<Camp> {
    let (cls, cls_t) = class_global(code, "world.camp.CampMode")?;
    let su = method(code, b.act_t, "setUnit__impl")?;
    let rt = |r: Reg| su.regs[r.0 as usize];
    let checks: Vec<RefFun> = su
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call2 { fun, arg0, .. } if rt(*arg0) == cls_t => Some(*fun),
            _ => None,
        })
        .collect();
    if checks != [b.check] {
        bail!("Activity.setUnit__impl: no single CampMode check");
    }
    // case Unit(u): the EnumField read into a Unit register, compared with this.unit
    let reads: Vec<(usize, Reg, RefEnumConstruct, RefField)> = su
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::EnumField {
                dst,
                value,
                construct,
                field,
            } if rt(*dst) == b.unit_t => Some((i, *value, *construct, *field)),
            _ => None,
        })
        .collect();
    let [(ei, ct_r, unit_c, unit_f)] = reads[..] else {
        bail!(
            "Activity.setUnit__impl: {} Unit enum reads, want 1",
            reads.len()
        );
    };
    let unit_idx = {
        let idx: Vec<i32> = su.ops[..ei]
            .iter()
            .rev()
            .take(6)
            .filter_map(|o| match o {
                Opcode::Int { ptr, .. } => Some(code.ints[ptr.0]),
                _ => None,
            })
            .collect();
        match (
            idx.first(),
            su.ops[..ei]
                .iter()
                .rev()
                .take(6)
                .any(|o| matches!(o, Opcode::EnumIndex { value, .. } if *value == ct_r)),
        ) {
            (Some(&k), true) if k == unit_c.0 as i32 => k,
            _ => bail!("Activity.setUnit__impl: the Unit case test is not EnumIndex == construct"),
        }
    };
    let (en_r, en_content) = match writer(su, ct_r, ei) {
        Some(Opcode::Field { obj, field, .. }) => (*obj, *field),
        _ => bail!("Activity.setUnit__impl: entry.content is not a field read"),
    };
    let (ce_r, ce_entry) = match writer(su, en_r, ei) {
        Some(Opcode::Field { obj, field, .. }) => (*obj, *field),
        _ => bail!("Activity.setUnit__impl: e.entry is not a field read"),
    };
    let (ce_t, en_t, ct_t) = (rt(ce_r), rt(en_r), rt(ct_r));
    if !is_sub(code, ce_t, b.ent_t) {
        bail!("camp entry entities are not Entities");
    }
    if !matches!(&code.types[ct_t.0], Type::Enum { constructs, .. }
        if constructs.get(unit_c.0).is_some_and(|c| c.params.get(unit_f.0) == Some(&b.unit_t)))
    {
        bail!("camp entry content: unexpected Unit constructor");
    }
    // the array iterated: view.entryEntities, view = the CampMode cast
    let ents: Vec<(Reg, RefField)> = su.ops[..ei]
        .iter()
        .filter_map(|o| match o {
            Opcode::Field { obj, field, dst } if rt(*dst) == b.arr_t => Some((*obj, *field)),
            _ => None,
        })
        .collect();
    let [(cm_r, cm_ents)] = ents[..] else {
        bail!(
            "Activity.setUnit__impl: {} array reads before the case, want 1",
            ents.len()
        );
    };
    let cm_t = rt(cm_r);
    if !is_sub(code, cm_t, b.mode_t)
        || s(code, obj(code, cm_t)?.fields[cm_ents.0].name) != "entryEntities"
    {
        bail!("Activity.setUnit__impl: the camp array is not mode.entryEntities");
    }
    Ok(Camp {
        cls,
        cls_t,
        cm_t,
        cm_ents,
        ce_t,
        ce_entry,
        en_t,
        en_content,
        ct_t,
        unit_c,
        unit_f,
        unit_idx,
    })
}

/// `println(tag + " " + Std.string(i) ...)` for the given Int registers.
pub(crate) fn emit_log(
    a: &mut Asm,
    r: &mut Regs,
    b: &Base,
    tag: RefGlobal,
    sp: RefGlobal,
    ints: &[Reg],
) {
    let (acc, t, d, v) = (r.r(b.str_t), r.r(b.str_t), r.r(b.dyn_t), r.r(b.void_));
    a.op(Opcode::GetGlobal {
        dst: acc,
        global: tag,
    });
    for &i in ints {
        a.op(Opcode::GetGlobal { dst: t, global: sp });
        a.op(Opcode::Call2 {
            dst: acc,
            fun: b.str_add,
            arg0: acc,
            arg1: t,
        });
        a.op(Opcode::ToDyn { dst: d, src: i });
        a.op(Opcode::Call1 {
            dst: t,
            fun: b.std_string,
            arg0: d,
        });
        a.op(Opcode::Call2 {
            dst: acc,
            fun: b.str_add,
            arg0: acc,
            arg1: t,
        });
    }
    a.op(Opcode::Call1 {
        dst: v,
        fun: b.println,
        arg0: acc,
    });
}

pub(crate) fn int(a: &mut Asm, code: &mut Bytecode, dst: Reg, v: i32) {
    let ptr = int_const(code, v);
    a.op(Opcode::Int { dst, ptr });
}

/// `{e, anim}`, the object `idle` is bound to (registers given).
fn emit_idle_obj(a: &mut Asm, code: &mut Bytecode, dob: Reg, e: Reg, idl: Reg) {
    let (ke, ka) = (string_ref(code, "e"), string_ref(code, "anim"));
    a.op(Opcode::New { dst: dob });
    for (field, src) in [(ke, e), (ka, idl)] {
        a.op(Opcode::DynSet {
            obj: dob,
            field,
            src,
        });
    }
}

/// Plays `anim` once on entity `e` (registers given), `onEnd` = idle({e, idl}).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_play_once(
    a: &mut Asm,
    r: &mut Regs,
    code: &mut Bytecode,
    b: &Base,
    e: Reg,
    anim: Reg,
    idl: Reg,
    idle: RefFun,
) {
    let (dob, io, f, nb, cb, opts, pos, v) = (
        r.r(b.dynobj_t),
        r.r(b.dynobj_t),
        r.r(b.bool_),
        r.r(b.nbool_t),
        r.r(b.end_cb_t),
        r.r(b.opts_t),
        r.r(b.pos_t),
        r.r(b.void_),
    );
    let (kl, ke) = (string_ref(code, "loop"), string_ref(code, "onEnd"));
    a.op(Opcode::New { dst: dob });
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    a.op(Opcode::ToDyn { dst: nb, src: f });
    a.op(Opcode::DynSet {
        obj: dob,
        field: kl,
        src: nb,
    });
    emit_idle_obj(a, code, io, e, idl);
    a.op(Opcode::InstanceClosure {
        dst: cb,
        fun: idle,
        obj: io,
    });
    a.op(Opcode::DynSet {
        obj: dob,
        field: ke,
        src: cb,
    });
    a.op(Opcode::ToVirtual {
        dst: opts,
        src: dob,
    });
    a.op(Opcode::Null { dst: pos });
    a.op(Opcode::CallMethod {
        dst: v,
        field: b.play_pi,
        args: vec![e, anim, opts, pos],
    });
}

/// `idle({e, idl})` now (registers given).
pub(crate) fn emit_idle_now(
    a: &mut Asm,
    r: &mut Regs,
    code: &mut Bytecode,
    b: &Base,
    e: Reg,
    idl: Reg,
    idle: RefFun,
) {
    let (io, v) = (r.r(b.dynobj_t), r.r(b.void_));
    emit_idle_obj(a, code, io, e, idl);
    a.op(Opcode::Call1 {
        dst: v,
        fun: idle,
        arg0: io,
    });
}

/// What `<mirror>IdleOf(e, how)` does with the worker's entry.
pub(crate) const IDLE_START: i32 = 0;
pub(crate) const IDLE_READ: i32 = 1;
pub(crate) const IDLE_END: i32 = 2;

/// `idleOf(e, how) -> String` over the ObjectMap in global `map` (see the header).
pub(crate) fn add_idle_of(
    code: &mut Bytecode,
    b: &Base,
    map: RefGlobal,
    dbg: usize,
) -> Result<RefFun> {
    let mut r = Regs(vec![b.ent_t, b.i32_]);
    let (e, how) = (Reg(0), Reg(1));
    let (m, k, d, cur, v, ok) = (
        r.r(b.omap_t),
        r.r(b.i32_),
        r.r(b.dyn_t),
        r.r(b.str_t),
        r.r(b.void_),
        r.r(b.bool_),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: m,
        global: map,
    });
    a.jmp(Opcode::JNotNull { reg: m, offset: 0 }, "have");
    a.op(Opcode::New { dst: m });
    a.op(Opcode::Call1 {
        dst: v,
        fun: b.omap_new,
        arg0: m,
    });
    a.op(Opcode::SetGlobal {
        global: map,
        src: m,
    });
    a.label("have");
    a.op(Opcode::Mov { dst: d, src: e });
    int(&mut a, code, k, IDLE_START);
    a.jmp(
        Opcode::JEq {
            a: how,
            b: k,
            offset: 0,
        },
        "take",
    );
    a.op(Opcode::Call2 {
        dst: d,
        fun: b.omap_get,
        arg0: m,
        arg1: d,
    });
    a.op(Opcode::SafeCast { dst: cur, src: d });
    a.op(Opcode::Mov { dst: d, src: e });
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "take",
    );
    int(&mut a, code, k, IDLE_END);
    a.jmp(
        Opcode::JNotEq {
            a: how,
            b: k,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call2 {
        dst: ok,
        fun: b.omap_remove,
        arg0: m,
        arg1: d,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "ret");
    // a start, or none remembered (the start was missed): the current anim
    a.label("take");
    a.op(Opcode::Field {
        dst: cur,
        obj: e,
        field: b.e_anim,
    });
    int(&mut a, code, k, IDLE_END);
    a.jmp(
        Opcode::JEq {
            a: how,
            b: k,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call3 {
        dst: v,
        fun: b.omap_set,
        arg0: m,
        arg1: d,
        arg2: cur,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: cur });
    push_fn(code, vec![b.ent_t, b.i32_], b.str_t, r.0, a.finish(), dbg)
}

/// `idle(o)`, `o = {e, anim}`: the anim, looped, on a worker still in the scene.
pub(crate) fn add_idle(code: &mut Bytecode, b: &Base, dbg: usize) -> Result<RefFun> {
    let mut r = Regs(vec![b.dynobj_t]);
    let e = r.r(b.ent_t);
    let (exc, anim, o, par, dob, t, nb, opts, pos, v) = (
        r.r(b.dyn_t),
        r.r(b.str_t),
        r.r(b.obj_t),
        r.r(b.obj_t),
        r.r(b.dynobj_t),
        r.r(b.bool_),
        r.r(b.nbool_t),
        r.r(b.opts_t),
        r.r(b.pos_t),
        r.r(b.void_),
    );
    let (ke, ka) = (string_ref(code, "e"), string_ref(code, "anim"));
    let kl = string_ref(code, "loop");
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    for (dst, field) in [(e, ke), (anim, ka)] {
        a.op(Opcode::DynGet {
            dst,
            obj: Reg(0),
            field,
        });
    }
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "untrap");
    a.jmp(
        Opcode::JNull {
            reg: anim,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: o,
        obj: e,
        field: b.e_obj,
    });
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: par,
        obj: o,
        field: b.o_parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: par,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::New { dst: dob });
    a.op(Opcode::Bool {
        dst: t,
        value: ValBool(true),
    });
    a.op(Opcode::ToDyn { dst: nb, src: t });
    a.op(Opcode::DynSet {
        obj: dob,
        field: kl,
        src: nb,
    });
    a.op(Opcode::ToVirtual {
        dst: opts,
        src: dob,
    });
    a.op(Opcode::Null { dst: pos });
    a.op(Opcode::CallMethod {
        dst: v,
        field: b.play_pi,
        args: vec![e, anim, opts, pos],
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![b.dynobj_t], b.void_, r.0, a.finish(), dbg)
}

/// `mirrorWorker(game, uid, camp) -> ent.Entity` (see the header); null when
/// the mode, the uid or the worker is not there.
pub(crate) fn add_worker(code: &mut Bytecode, b: &Base, c: &Camp, dbg: usize) -> Result<RefFun> {
    let mut r = Regs(vec![b.game_t, b.i32_, b.bool_]);
    let (game, uid, is_camp) = (Reg(0), Reg(1), Reg(2));
    let (res, exc, ctrl, host, ctx, refs, d, mode, ok) = (
        r.r(b.ent_t),
        r.r(b.dyn_t),
        r.r(b.ctrl_t),
        r.r(b.host_t),
        r.r(b.ctx_t),
        r.r(b.refs_t),
        r.r(b.dyn_t),
        r.r(b.mode_t),
        r.r(b.bool_),
    );
    let (ccls, cm, ents, len, k, raw, dv, ce, en, ct, idx, ki, u, du) = (
        r.r(c.cls_t),
        r.r(c.cm_t),
        r.r(b.arr_t),
        r.r(b.i32_),
        r.r(b.i32_),
        r.r(b.raw_arr_t),
        r.r(b.dyn_t),
        r.r(c.ce_t),
        r.r(c.en_t),
        r.r(c.ct_t),
        r.r(b.i32_),
        r.r(b.i32_),
        r.r(b.unit_t),
        r.r(b.unit_t),
    );
    let (pcls, pv, au, ed, e) = (
        r.r(b.pv_cls_t),
        r.r(b.pv_t),
        r.r(b.omap_t),
        r.r(b.dyn_t),
        r.r(b.ent_t),
    );
    let ku = string_ref(code, "unitView");
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: res });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    let get = |a: &mut Asm, dst: Reg, obj: Reg, field: RefField| {
        a.op(Opcode::Field { dst, obj, field });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "untrap",
        );
    };
    get(&mut a, ctrl, game, b.g_ctrl);
    get(&mut a, host, ctrl, b.c_host);
    get(&mut a, ctx, host, b.h_ctx);
    get(&mut a, refs, ctx, b.x_refs);
    a.op(Opcode::Call2 {
        dst: d,
        fun: b.refs_get,
        arg0: refs,
        arg1: uid,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "untrap");
    get(&mut a, mode, game, b.g_mode);
    a.jmp(
        Opcode::JFalse {
            cond: is_camp,
            offset: 0,
        },
        "place",
    );
    // camp: the entry entity whose content is Unit(the unit)
    a.op(Opcode::GetGlobal {
        dst: ccls,
        global: c.cls,
    });
    a.op(Opcode::Call2 {
        dst: ok,
        fun: b.check,
        arg0: ccls,
        arg1: mode,
    });
    a.jmp(
        Opcode::JFalse {
            cond: ok,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::UnsafeCast { dst: cm, src: mode });
    a.op(Opcode::UnsafeCast { dst: du, src: d });
    get(&mut a, ents, cm, c.cm_ents);
    a.op(Opcode::Field {
        dst: len,
        obj: ents,
        field: b.a_len,
    });
    int(&mut a, code, k, 0);
    int(&mut a, code, ki, c.unit_idx);
    a.loop_head("scan");
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: len,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: ents,
        field: b.a_arr,
    });
    a.op(Opcode::GetArray {
        dst: dv,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: ce, src: dv });
    a.jmp(Opcode::JNull { reg: ce, offset: 0 }, "scan");
    a.op(Opcode::Field {
        dst: en,
        obj: ce,
        field: c.ce_entry,
    });
    a.jmp(Opcode::JNull { reg: en, offset: 0 }, "scan");
    a.op(Opcode::Field {
        dst: ct,
        obj: en,
        field: c.en_content,
    });
    a.jmp(Opcode::JNull { reg: ct, offset: 0 }, "scan");
    a.op(Opcode::EnumIndex {
        dst: idx,
        value: ct,
    });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: ki,
            offset: 0,
        },
        "scan",
    );
    a.op(Opcode::EnumField {
        dst: u,
        value: ct,
        construct: c.unit_c,
        field: c.unit_f,
    });
    a.jmp(
        Opcode::JNotEq {
            a: u,
            b: du,
            offset: 0,
        },
        "scan",
    );
    a.op(Opcode::Mov { dst: res, src: ce });
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    // a place: the unit parked at the element
    a.label("place");
    a.op(Opcode::GetGlobal {
        dst: pcls,
        global: b.pv_cls,
    });
    a.op(Opcode::Call2 {
        dst: ok,
        fun: b.check,
        arg0: pcls,
        arg1: mode,
    });
    a.jmp(
        Opcode::JFalse {
            cond: ok,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::UnsafeCast { dst: pv, src: mode });
    get(&mut a, au, pv, b.pv_units);
    a.op(Opcode::Call2 {
        dst: ed,
        fun: b.omap_get,
        arg0: au,
        arg1: d,
    });
    a.jmp(Opcode::JNull { reg: ed, offset: 0 }, "untrap");
    a.op(Opcode::DynGet {
        dst: e,
        obj: ed,
        field: ku,
    });
    a.op(Opcode::Mov { dst: res, src: e });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: res });
    push_fn(
        code,
        vec![b.game_t, b.i32_, b.bool_],
        b.ent_t,
        r.0,
        a.finish(),
        dbg,
    )
}

/// `if (recv(this, x, y, z, player)) return;` at the entry of ping__impl.
pub(crate) fn hook_ping(code: &mut Bytecode, b: &Base, recv: RefFun) {
    let f = &mut code.functions[b.impl_fi];
    f.regs.push(b.bool_);
    let hb = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::CallN {
                dst: hb,
                fun: recv,
                args: vec![Reg(0), Reg(1), Reg(2), Reg(3), Reg(4)],
            },
            Opcode::JFalse {
                cond: hb,
                offset: 1,
            },
            Opcode::Ret { ret: b.impl_ret },
        ],
    );
}

/// The receivers already at the entry of ping__impl.
#[cfg(test)]
pub(crate) fn ping_hooks(code: &Bytecode, b: &Base) -> Vec<RefFun> {
    code.functions[b.impl_fi]
        .ops
        .chunks(3)
        .map_while(|c| match c {
            [Opcode::CallN { fun, args, dst }, Opcode::JFalse { cond, offset: 1 }, Opcode::Ret { .. }]
                if args[..] == [Reg(0), Reg(1), Reg(2), Reg(3), Reg(4)] && cond == dst =>
            {
                Some(*fun)
            }
            _ => None,
        })
        .collect()
}
