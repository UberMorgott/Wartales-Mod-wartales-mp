// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op forge mirror: while a player forges, the other players see that
// player's worker at the anvil hammer, with the hit particles and sounds of
// each hit's grade and the success / fail gesture at the end. Their camera,
// UI and input are untouched; nothing is created on their side but a few
// particle prefabs and animation calls on the worker they already see.
//
// Vanilla: `ui.win.Activity._start` shares a mini-game window only when the
// data.cdb activity has `props.coop` (`win.netSharing(true)`), and the window
// class then carries hxbit-synced fields / RPCs (GatherAction: `activity`,
// `totalHits`, `lootKind`, `netFeedbackMine/Wood`, `addWoodLog`). Forge has no
// `coop` flag and `ui.win.ForgeAction` no synced field or RPC: its steps are
// rolled locally, input is a local raycast, the result item is made locally.
// The others only get the worker parked at the anvil with its idle anim:
// `Activity.setUnit__impl` -> `Controller.addUnitToElement` (RPC to the host)
// -> `PlaceView.addUnitToElement` on every machine, which keeps that entity in
// `PlaceView.activityUnits.get(element).unitView`.
//
// Why not the GatherAction way (coop flag + replicated window): it needs a
// data.cdb change (pak) and, on ForgeAction, a hand-written hxbit field
// (`activity`: serialize / unserialize / getSerializeSchema / networkSetBitCond
// overrides on a class that has none), new RPCs (networkRPC / getRPCSchema /
// networkGetName / networkAllow overrides plus stubs) for the locally rolled
// hits, and owner-only gates in init / update / setActionDone (a replicated
// copy would roll its own steps, read the peer's mouse and build items). And
// a replicated window is a window on every peer: it takes over their screen,
// which is what the spectators must not get.
//
// The patch rides the existing ping RPC (`Controller.ping(x, y, z, player)`,
// any player -> host -> every machine, as `ping_cell.rs` documents) with a
// sentinel x that no world point has; no new RPC, no synced field, no data
// change:
//   sender (the forging player's machine), `forgeSend(win, code, a, shard)`:
//     ForgeAction.init entry          -> code 0 (start)
//     ForgeAction.setActionDone entry -> code 1 (hit), a = EScoreTier index,
//                                        shard = the hit metal shard
//     ForgeAction.endActivity entry   -> code 2 (end), a = ActivityResult index
//     co-op only (`ctrl.__host != null`), under a trap:
//     ctrl.ping(SENTINEL, activity.target.__uid,
//               code + (a << 2) + (b << 6), game.me)
//     b = 1 + the shard's index among its parent's children (0: none).
//   receiver, `Controller.ping__impl` entry:
//     if (forgeRecv(this, x, y, z, player)) return;
//     forgeRecv: false unless x == SENTINEL (the vanilla ping goes on); true
//     otherwise (never a marker or ping sound for these). Under a trap, skipped
//     for the sender itself (`player == game.me`) or outside a PlaceView:
//     element = __host.ctx.refs.get(y) (hxbit uid), entity =
//     activityUnits.get(element).unitView; start: remember its current anim
//     as the idle one; hit: play ForgeAction's `animHit` ("Forge") once, back
//     to the idle loop on its end (`forgeIdle`), and 0.4 s later (the vanilla
//     hit timing) `forgeFx`: the grade's sound through `game.ui.sfx` and the
//     vanilla particle prefabs (base + good + perfect / base + good / base +
//     bad) loaded into the scene at the hit shard (scene `allShards` child b-1),
//     else the `anvil`, else the worker, removed after 1.5 s; end: `animSuccess`
//     ("ForgeYes") when the result is Success, else `animFail` ("ForgeMeh"),
//     once, then idle. Anim names come from the ForgeAction constructor.
// shim.log: `mp: forge send <code> <a> <b> <uid>` on the forging machine and
// `mp: forge recv <code> <a> <b> <stage>` on the others (stage 5 start, 6 end,
// 7 hit played; lower: where it stopped).
//
// The validated types / fields / functions every mirror needs, the idle and
// one-shot anim code and the ping__impl hook live in mirror.rs (shared with
// work_mirror.rs, the archery / UnitAction mirror on its own sentinel).
//
// All players need this build: an older one shows a far-away ping marker and
// plays the ping sound for each event. Validated before editing; a mismatch
// skips the pass (logged).

use super::asm::{push_fn, string_ref, Asm, Regs};
use super::diag::index_of;
use super::job_xp::{const_str, str_global};
use super::mirror::{self, emit_log, emit_play_once, enum_constructs, int, want, want_field, Base};
use super::ping_cell::fname;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

/// The x of a mirror event: far outside any map, never a cursor point.
pub(crate) const SENTINEL: f64 = -987_654_321.0;
/// ForgeAction.feedbackOnAction plays the hit sound / particles 0.4 s into the anim.
const FX_DELAY: f64 = 0.4;
const FX_LIFE: f64 = 1.5;
const SND_PERFECT: &str = "ForgePerfectHit";
const SND_GOOD: &str = "ForgeGoodHit";
const SND_BAD: &str = "ForgeBadHit";
const FX_BASE: &str = "prefabs/fx/activity/forge_particles_base_hit.fx";
const FX_GOOD: &str = "prefabs/fx/activity/forge_particles_good_hit.fx";
const FX_BAD: &str = "prefabs/fx/activity/forge_particles_bad_hit.fx";
const FX_PERFECT: &str = "prefabs/fx/activity/forge_particles_perfect_hit.fx";

pub(crate) struct Plan {
    /// The types, fields and functions every mirror shares (mirror.rs).
    pub(crate) b: Base,
    pub(crate) fa_t: RefType,
    scene_t: RefType,
    mat_t: RefType,
    pub(crate) w_game: RefField,
    pub(crate) w_act: RefField,
    pub(crate) m_s3d: RefField,
    m41: RefField,
    m42: RefField,
    m43: RefField,
    pub(crate) sfx: RefFun,
    pub(crate) by_name: RefFun,
    pub(crate) abs_pos: RefFun,
    pub(crate) set_pos: RefFun,
    pub(crate) remove: RefFun,
    pub(crate) main_g: RefGlobal,
    main_t: RefType,
    pub(crate) m_cache: RefField,
    cache_t: RefType,
    pub(crate) get_loader: RefFun,
    loader_t: RefType,
    pub(crate) load_cache: RefFun,
    hres_t: RefType,
    pub(crate) rescls_g: RefGlobal,
    rescls_t: RefType,
    res_t: RefType,
    prefab_t: RefType,
    pub(crate) load_prefab: RefFun,
    pub(crate) hit_g: RefGlobal,
    pub(crate) yes_g: RefGlobal,
    pub(crate) meh_g: RefGlobal,
    pub(crate) success: i32,
    pub(crate) init_fi: usize,
    pub(crate) done_fi: usize,
    pub(crate) end_fi: usize,
    dbg_file: usize,
}

impl std::ops::Deref for Plan {
    type Target = Base;
    fn deref(&self) -> &Base {
        &self.b
    }
}

/// The op before `before` that last writes register `r` (loads only).
fn writer(f: &Function, r: Reg, before: usize) -> Option<&Opcode> {
    f.ops[..before].iter().rev().find(|o| {
        matches!(o,
            Opcode::GetGlobal { dst, .. }
            | Opcode::Field { dst, .. }
            | Opcode::Call0 { dst, .. }
            | Opcode::Call3 { dst, .. }
            | Opcode::SafeCast { dst, .. }
            | Opcode::Null { dst } if *dst == r)
    })
}

/// The constructor's `this.<name> = "<literal>"`: the literal's String global.
fn ctor_literal(code: &Bytecode, ctor: &Function, f: RefField) -> Result<RefGlobal> {
    for (i, op) in ctor.ops.iter().enumerate() {
        if let Opcode::SetThis { field, src } = op {
            if *field == f {
                if let Some(Opcode::GetGlobal { global, .. }) = writer(ctor, *src, i) {
                    if const_str(code, *global).is_some() {
                        return Ok(*global);
                    }
                }
            }
        }
    }
    bail!("ForgeAction constructor: no literal for field {}", f.0)
}

/// The marker of an applied pass: its receiver's log tag.
const RECV_TAG: &str = "mp: forge recv";

pub(crate) fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|s| s.as_str() == RECV_TAG) {
        bail!("already applied");
    }
    let b = mirror::base(code)?;
    let fa_t = obj_type(code, "ui.win.ForgeAction")?;
    let scene_t = obj_type(code, "h3d.scene.Scene")?;
    let mat_t = obj_type(code, "h3d.MatrixImpl")?;
    let (obj_t, str_t, f64_, void_, dyn_t) = (b.obj_t, b.str_t, b.f64_, b.void_, b.dyn_t);

    // ForgeAction: Window.game, activity
    let w_game = want_field(code, fa_t, "game", b.game_t)?;
    let w_act = want_field(code, fa_t, "activity", b.act_t)?;
    let m_s3d = want_field(code, b.mode_t, "s3d", scene_t)?;
    let m41 = want_field(code, mat_t, "_41", f64_)?;
    let m42 = want_field(code, mat_t, "_42", f64_)?;
    let m43 = want_field(code, mat_t, "_43", f64_)?;

    // ping__impl's fx load: Main.CACHE.loadPrefab(loader.loadCache(path, Resource), null, s3d)
    let imp = &code.functions[b.impl_fi];
    let loads: Vec<(usize, Reg, Reg)> = imp
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call4 {
                fun, arg0, arg1, ..
            } if fname(code, *fun) == "loadPrefab" => Some((i, *arg0, *arg1)),
            _ => None,
        })
        .collect();
    let [(li, cache_r, rs_r)] = loads[..] else {
        bail!("ping__impl: {} loadPrefab calls, want 1", loads.len());
    };
    let Opcode::Call4 {
        fun: load_prefab, ..
    } = imp.ops[li]
    else {
        unreachable!()
    };
    let rt = |r: Reg| imp.regs[r.0 as usize];
    let (main_r, m_cache) = match writer(imp, cache_r, li) {
        Some(Opcode::Field { obj, field, .. }) => (*obj, *field),
        _ => bail!("ping__impl: the model cache is not a field read"),
    };
    let main_g = match writer(imp, main_r, li) {
        Some(Opcode::GetGlobal { global, .. }) => *global,
        _ => bail!("ping__impl: the model cache owner is not a global"),
    };
    let hr_r = match writer(imp, rs_r, li) {
        Some(Opcode::SafeCast { src, .. }) => *src,
        _ => bail!("ping__impl: the prefab resource is not a cast"),
    };
    let (load_cache, ld_r, rc_r) = match writer(imp, hr_r, li) {
        Some(Opcode::Call3 {
            fun, arg0, arg2, ..
        }) if fname(code, *fun) == "loadCache" => (*fun, *arg0, *arg2),
        _ => bail!("ping__impl: no loadCache before loadPrefab"),
    };
    let get_loader = match writer(imp, ld_r, li) {
        Some(Opcode::Call0 { fun, .. }) => *fun,
        _ => bail!("ping__impl: the loader is not a static call"),
    };
    let rescls_g = match writer(imp, rc_r, li) {
        Some(Opcode::GetGlobal { global, .. }) => *global,
        _ => bail!("ping__impl: the resource class is not a global"),
    };
    let (main_t, cache_t, loader_t, hres_t, rescls_t, res_t) = (
        rt(main_r),
        rt(cache_r),
        rt(ld_r),
        rt(hr_r),
        rt(rc_r),
        rt(rs_r),
    );
    if code.globals.get(main_g.0) != Some(&main_t)
        || code.globals.get(rescls_g.0) != Some(&rescls_t)
    {
        bail!("ping__impl: fx load globals have unexpected types");
    }
    want(code, get_loader, "get_loader", &[], loader_t)?;
    let (la, lr) = super::ping_cell::sig(code, load_cache)?;
    if la.len() != 3 || la[0] != loader_t || la[1] != str_t || lr != hres_t {
        bail!("unexpected loadCache signature");
    }
    let (pa, pr) = super::ping_cell::sig(code, load_prefab)?;
    let prefab_t = match pa[..] {
        [c, r, p, o] if c == cache_t && r == res_t && o == obj_t && pr == obj_t => p,
        _ => bail!("unexpected loadPrefab signature"),
    };

    let (sfx, _) = super::ping_cell::vproto(code, b.ui_t, "sfx")?;
    let (sa, sr) = super::ping_cell::sig(code, sfx)?;
    if sa.len() != 3
        || !mirror::is_sub(code, b.ui_t, sa[0])
        || sa[1] != str_t
        || sa[2] != dyn_t
        || sr != void_
    {
        bail!("unexpected ui.sfx signature");
    }
    let by_name = proto(code, obj_t, "getObjectByName")?;
    want(
        code,
        by_name,
        "Object.getObjectByName",
        &[obj_t, str_t],
        obj_t,
    )?;
    let abs_pos = proto(code, obj_t, "getAbsPos")?;
    want(code, abs_pos, "Object.getAbsPos", &[obj_t], mat_t)?;
    let set_pos = proto(code, obj_t, "setPosition")?;
    want(
        code,
        set_pos,
        "Object.setPosition",
        &[obj_t, f64_, f64_, f64_],
        void_,
    )?;
    let remove = proto(code, obj_t, "remove")?;
    want(code, remove, "Object.remove", &[obj_t], void_)?;
    if !mirror::is_sub(code, scene_t, obj_t) {
        bail!("h3d.scene.Scene is not an Object");
    }

    // ForgeAction: anim names, the three hooks.
    let ctor = method(code, fa_t, "__constructor__")?;
    let hit_g = ctor_literal(code, ctor, want_field(code, fa_t, "animHit", str_t)?)?;
    let yes_g = ctor_literal(code, ctor, want_field(code, fa_t, "animSuccess", str_t)?)?;
    let meh_g = ctor_literal(code, ctor, want_field(code, fa_t, "animFail", str_t)?)?;
    let fa_m = |name: &str| -> Result<&Function> { method(code, fa_t, name) };
    let init = fa_m("init")?;
    if fun_args(code, init) != [fa_t] {
        bail!("unexpected ForgeAction.init signature");
    }
    let done = fa_m("setActionDone")?;
    let da = fun_args(code, done);
    let tier_t = match da[..] {
        [w, o, t] if w == fa_t && o == obj_t => t,
        _ => bail!("unexpected ForgeAction.setActionDone signature"),
    };
    let end = fa_m("endActivity")?;
    let ea = fun_args(code, end);
    let result_t = match ea[..] {
        [w, r] if w == fa_t => r,
        _ => bail!("unexpected ForgeAction.endActivity signature"),
    };
    let tiers = enum_constructs(code, tier_t)?;
    let results = enum_constructs(code, result_t)?;
    if tiers.len() > 16 || results.len() > 16 || tiers.len() < 3 {
        bail!("EScoreTier / ActivityResult have unexpected sizes");
    }
    let success = results
        .iter()
        .position(|n| n == "Success")
        .context("ActivityResult has no Success")? as i32;

    Ok(Plan {
        b,
        fa_t,
        scene_t,
        mat_t,
        w_game,
        w_act,
        m_s3d,
        m41,
        m42,
        m43,
        sfx,
        by_name,
        abs_pos,
        set_pos,
        remove,
        main_g,
        main_t,
        m_cache,
        cache_t,
        get_loader,
        loader_t,
        load_cache,
        hres_t,
        rescls_g,
        rescls_t,
        res_t,
        prefab_t,
        load_prefab,
        hit_g,
        yes_g,
        meh_g,
        success,
        init_fi: index_of(code, init.findex)?,
        done_fi: index_of(code, done.findex)?,
        end_fi: index_of(code, end.findex)?,
        dbg_file: debug_file(code, "src/ui/win/ForgeAction.hx")?,
    })
}

/// String globals the appended functions read.
pub(crate) struct Strs {
    send_tag: RefGlobal,
    recv_tag: RefGlobal,
    sp: RefGlobal,
    shards: RefGlobal,
    anvil: RefGlobal,
    snd: [RefGlobal; 3],
    fx_base: RefGlobal,
    fx_good: RefGlobal,
    fx_bad: RefGlobal,
    fx_perfect: RefGlobal,
    /// New String global: the worker's idle anim, taken at the start event.
    pub(crate) idle: RefGlobal,
}

fn strs(code: &mut Bytecode, p: &Plan) -> Strs {
    let g = |code: &mut Bytecode, v: &'static str| str_global(code, p.str_t, v);
    let s = Strs {
        send_tag: g(code, "mp: forge send"),
        recv_tag: g(code, "mp: forge recv"),
        sp: g(code, " "),
        shards: g(code, "allShards"),
        anvil: g(code, "anvil"),
        snd: [g(code, SND_PERFECT), g(code, SND_GOOD), g(code, SND_BAD)],
        fx_base: g(code, FX_BASE),
        fx_good: g(code, FX_GOOD),
        fx_bad: g(code, FX_BAD),
        fx_perfect: g(code, FX_PERFECT),
        idle: RefGlobal(0),
    };
    code.globals.push(p.str_t);
    Strs {
        idle: RefGlobal(code.globals.len() - 1),
        ..s
    }
}

/// `forgeSend(win, code, a, shard)`: the ping carrying one event (see the header).
fn add_send(code: &mut Bytecode, p: &Plan, st: &Strs) -> Result<RefFun> {
    let mut r = Regs(vec![p.fa_t, p.i32_, p.i32_, p.obj_t]);
    let (win, kind, arg, shard) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let (exc, game, ctrl, host, act, tgt, uid, b) = (
        r.r(p.dyn_t),
        r.r(p.game_t),
        r.r(p.ctrl_t),
        r.r(p.host_t),
        r.r(p.act_t),
        r.r(p.ent_t),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let (par, ch, len, k, raw, dv, so, t, zi, kk) = (
        r.r(p.obj_t),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.raw_arr_t),
        r.r(p.dyn_t),
        r.r(p.obj_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let (zf, yf, xf, me, v) = (
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.player_t),
        r.r(p.void_),
    );
    let sent = float_const(code, SENTINEL);
    let mut a = Asm::new();
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
    get(&mut a, game, win, p.w_game);
    get(&mut a, ctrl, game, p.g_ctrl);
    // solo: no network, nothing to mirror
    get(&mut a, host, ctrl, p.c_host);
    get(&mut a, act, win, p.w_act);
    get(&mut a, tgt, act, p.a_target);
    a.op(Opcode::Field {
        dst: uid,
        obj: tgt,
        field: p.e_uid,
    });
    int(&mut a, code, b, 0);
    a.jmp(
        Opcode::JNull {
            reg: shard,
            offset: 0,
        },
        "enc",
    );
    a.op(Opcode::Field {
        dst: par,
        obj: shard,
        field: p.o_parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: par,
            offset: 0,
        },
        "enc",
    );
    a.op(Opcode::Field {
        dst: ch,
        obj: par,
        field: p.o_children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "enc");
    a.op(Opcode::Field {
        dst: len,
        obj: ch,
        field: p.a_len,
    });
    int(&mut a, code, k, 0);
    a.loop_head("scan");
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: len,
            offset: 0,
        },
        "enc",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: ch,
        field: p.a_arr,
    });
    a.op(Opcode::GetArray {
        dst: dv,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: so, src: dv });
    a.jmp(
        Opcode::JNotEq {
            a: so,
            b: shard,
            offset: 0,
        },
        "scan",
    );
    a.op(Opcode::Mov { dst: b, src: k });
    a.label("enc");
    // z = kind + (arg << 2) + (b << 6)
    int(&mut a, code, kk, 2);
    a.op(Opcode::Shl {
        dst: t,
        a: arg,
        b: kk,
    });
    a.op(Opcode::Add {
        dst: zi,
        a: kind,
        b: t,
    });
    int(&mut a, code, kk, 6);
    a.op(Opcode::Shl {
        dst: t,
        a: b,
        b: kk,
    });
    a.op(Opcode::Add {
        dst: zi,
        a: zi,
        b: t,
    });
    a.op(Opcode::ToSFloat { dst: zf, src: zi });
    a.op(Opcode::ToSFloat { dst: yf, src: uid });
    a.op(Opcode::Float { dst: xf, ptr: sent });
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me,
    });
    a.op(Opcode::CallN {
        dst: v,
        fun: p.ping,
        args: vec![ctrl, xf, yf, zf, me],
    });
    emit_log(&mut a, &mut r, p, st.send_tag, st.sp, &[kind, arg, b, uid]);
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.fa_t, p.i32_, p.i32_, p.obj_t],
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}
/// `forgeFx(st)`, st = { e, g, a, b }: the grade's sound and particles.
fn add_fx(code: &mut Bytecode, p: &Plan, st: &Strs) -> Result<RefFun> {
    let mut r = Regs(vec![p.dynobj_t]);
    let stv = Reg(0);
    let (exc, e, game, ta, tb, ui, snd, nd, k) = (
        r.r(p.dyn_t),
        r.r(p.ent_t),
        r.r(p.game_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.ui_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.i32_),
    );
    let (mode, s3d, src, nm, sh, ch, len, idx, raw, dv, m, px, py, pz) = (
        r.r(p.mode_t),
        r.r(p.scene_t),
        r.r(p.obj_t),
        r.r(p.str_t),
        r.r(p.obj_t),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.raw_arr_t),
        r.r(p.dyn_t),
        r.r(p.mat_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
    );
    let (mainr, cache, ld, ps, rc, hr, rs, pf, fx, ev, dur, rm, v) = (
        r.r(p.main_t),
        r.r(p.cache_t),
        r.r(p.loader_t),
        r.r(p.str_t),
        r.r(p.rescls_t),
        r.r(p.hres_t),
        r.r(p.res_t),
        r.r(p.prefab_t),
        r.r(p.obj_t),
        r.r(p.ev_t),
        r.r(p.f64_),
        r.r(p.wait_cb_t),
        r.r(p.void_),
    );
    let (ke, kg, ka, kb) = (
        string_ref(code, "e"),
        string_ref(code, "g"),
        string_ref(code, "a"),
        string_ref(code, "b"),
    );
    let life = float_const(code, FX_LIFE);
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::DynGet {
        dst: e,
        obj: stv,
        field: ke,
    });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "untrap");
    a.op(Opcode::DynGet {
        dst: game,
        obj: stv,
        field: kg,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::DynGet {
        dst: ta,
        obj: stv,
        field: ka,
    });
    a.op(Opcode::DynGet {
        dst: tb,
        obj: stv,
        field: kb,
    });
    // sound: Perfect / Good / anything else Bad (feedbackOnAction's switch)
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.g_ui,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "pos");
    a.op(Opcode::GetGlobal {
        dst: snd,
        global: st.snd[2],
    });
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNotEq {
            a: ta,
            b: k,
            offset: 0,
        },
        "s1",
    );
    a.op(Opcode::GetGlobal {
        dst: snd,
        global: st.snd[0],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "snd");
    a.label("s1");
    int(&mut a, code, k, 1);
    a.jmp(
        Opcode::JNotEq {
            a: ta,
            b: k,
            offset: 0,
        },
        "snd",
    );
    a.op(Opcode::GetGlobal {
        dst: snd,
        global: st.snd[1],
    });
    a.label("snd");
    a.op(Opcode::Null { dst: nd });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.sfx,
        arg0: ui,
        arg1: snd,
        arg2: nd,
    });
    // where: the hit shard, else the anvil, else the worker
    a.label("pos");
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: s3d,
        obj: mode,
        field: p.m_s3d,
    });
    a.jmp(
        Opcode::JNull {
            reg: s3d,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Null { dst: src });
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JSLte {
            a: tb,
            b: k,
            offset: 0,
        },
        "anvil",
    );
    a.op(Opcode::GetGlobal {
        dst: nm,
        global: st.shards,
    });
    a.op(Opcode::Call2 {
        dst: sh,
        fun: p.by_name,
        arg0: s3d,
        arg1: nm,
    });
    a.jmp(Opcode::JNull { reg: sh, offset: 0 }, "anvil");
    a.op(Opcode::Field {
        dst: ch,
        obj: sh,
        field: p.o_children,
    });
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, "anvil");
    a.op(Opcode::Field {
        dst: len,
        obj: ch,
        field: p.a_len,
    });
    int(&mut a, code, k, 1);
    a.op(Opcode::Sub {
        dst: idx,
        a: tb,
        b: k,
    });
    a.jmp(
        Opcode::JSGte {
            a: idx,
            b: len,
            offset: 0,
        },
        "anvil",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: ch,
        field: p.a_arr,
    });
    a.op(Opcode::GetArray {
        dst: dv,
        array: raw,
        index: idx,
    });
    a.op(Opcode::UnsafeCast { dst: src, src: dv });
    a.label("anvil");
    a.jmp(
        Opcode::JNotNull {
            reg: src,
            offset: 0,
        },
        "at",
    );
    a.op(Opcode::GetGlobal {
        dst: nm,
        global: st.anvil,
    });
    a.op(Opcode::Call2 {
        dst: src,
        fun: p.by_name,
        arg0: s3d,
        arg1: nm,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: src,
            offset: 0,
        },
        "at",
    );
    a.op(Opcode::Field {
        dst: src,
        obj: e,
        field: p.e_obj,
    });
    a.jmp(
        Opcode::JNull {
            reg: src,
            offset: 0,
        },
        "untrap",
    );
    a.label("at");
    a.op(Opcode::Call1 {
        dst: m,
        fun: p.abs_pos,
        arg0: src,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "untrap");
    for (dst, f) in [(px, p.m41), (py, p.m42), (pz, p.m43)] {
        a.op(Opcode::Field {
            dst,
            obj: m,
            field: f,
        });
    }
    // Main.CACHE.loadPrefab(loader.loadCache(path, Resource), null, s3d) at the point
    let spawn = |a: &mut Asm, path: RefGlobal, skip: &'static str| {
        a.op(Opcode::GetGlobal {
            dst: mainr,
            global: p.main_g,
        });
        a.jmp(
            Opcode::JNull {
                reg: mainr,
                offset: 0,
            },
            skip,
        );
        a.op(Opcode::Field {
            dst: cache,
            obj: mainr,
            field: p.m_cache,
        });
        a.jmp(
            Opcode::JNull {
                reg: cache,
                offset: 0,
            },
            skip,
        );
        a.op(Opcode::Call0 {
            dst: ld,
            fun: p.get_loader,
        });
        a.jmp(Opcode::JNull { reg: ld, offset: 0 }, skip);
        a.op(Opcode::GetGlobal {
            dst: ps,
            global: path,
        });
        a.op(Opcode::GetGlobal {
            dst: rc,
            global: p.rescls_g,
        });
        a.op(Opcode::Call3 {
            dst: hr,
            fun: p.load_cache,
            arg0: ld,
            arg1: ps,
            arg2: rc,
        });
        a.op(Opcode::SafeCast { dst: rs, src: hr });
        a.jmp(Opcode::JNull { reg: rs, offset: 0 }, skip);
        a.op(Opcode::Null { dst: pf });
        a.op(Opcode::Call4 {
            dst: fx,
            fun: p.load_prefab,
            arg0: cache,
            arg1: rs,
            arg2: pf,
            arg3: s3d,
        });
        a.jmp(Opcode::JNull { reg: fx, offset: 0 }, skip);
        a.op(Opcode::CallN {
            dst: v,
            fun: p.set_pos,
            args: vec![fx, px, py, pz],
        });
        a.op(Opcode::Field {
            dst: ev,
            obj: game,
            field: p.g_event,
        });
        a.jmp(Opcode::JNull { reg: ev, offset: 0 }, skip);
        a.op(Opcode::Float {
            dst: dur,
            ptr: life,
        });
        a.op(Opcode::InstanceClosure {
            dst: rm,
            fun: p.remove,
            obj: fx,
        });
        a.op(Opcode::Call3 {
            dst: v,
            fun: p.wait,
            arg0: ev,
            arg1: dur,
            arg2: rm,
        });
        a.label(skip);
    };
    spawn(&mut a, st.fx_base, "k0");
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNotEq {
            a: ta,
            b: k,
            offset: 0,
        },
        "t1",
    );
    spawn(&mut a, st.fx_good, "k1");
    spawn(&mut a, st.fx_perfect, "k2");
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("t1");
    int(&mut a, code, k, 1);
    a.jmp(
        Opcode::JNotEq {
            a: ta,
            b: k,
            offset: 0,
        },
        "t2",
    );
    spawn(&mut a, st.fx_good, "k3");
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("t2");
    spawn(&mut a, st.fx_bad, "k4");
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.dynobj_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `forgeRecv(ctrl, x, y, z, player) -> Bool` (see the header).
fn add_recv(code: &mut Bytecode, p: &Plan, st: &Strs, idle: RefFun, fx: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.ctrl_t, p.f64_, p.f64_, p.f64_, p.player_t]);
    let (ctrl, x, y, z, player) = (Reg(0), Reg(1), Reg(2), Reg(3), Reg(4));
    let (res, fs, exc, stage, kind, ta, tb, zi, k) = (
        r.r(p.bool_),
        r.r(p.f64_),
        r.r(p.dyn_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let (game, me, mode, cls, is_pv, pv, host, ctx, refs, uid, d, au, ed, e, o) = (
        r.r(p.game_t),
        r.r(p.player_t),
        r.r(p.mode_t),
        r.r(p.pv_cls_t),
        r.r(p.bool_),
        r.r(p.pv_t),
        r.r(p.host_t),
        r.r(p.ctx_t),
        r.r(p.refs_t),
        r.r(p.i32_),
        r.r(p.dyn_t),
        r.r(p.omap_t),
        r.r(p.dyn_t),
        r.r(p.ent_t),
        r.r(p.obj_t),
    );
    let (cur, anim, ev, stv, t, fxc, v) = (
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.ev_t),
        r.r(p.dynobj_t),
        r.r(p.f64_),
        r.r(p.wait_cb_t),
        r.r(p.void_),
    );
    let sent = float_const(code, SENTINEL);
    let delay = float_const(code, FX_DELAY);
    let (ku, ke, kg, ka, kb) = (
        string_ref(code, "unitView"),
        string_ref(code, "e"),
        string_ref(code, "g"),
        string_ref(code, "a"),
        string_ref(code, "b"),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.op(Opcode::Float { dst: fs, ptr: sent });
    a.jmp(
        Opcode::JNotEq {
            a: x,
            b: fs,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.jmp(
        Opcode::JNull {
            reg: player,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: game,
        obj: ctrl,
        field: p.c_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me,
    });
    // the forging player has its own window
    a.jmp(
        Opcode::JEq {
            a: player,
            b: me,
            offset: 0,
        },
        "ret",
    );
    // kind = z & 3; a = (z >> 2) & 15; b = z >> 6
    a.op(Opcode::ToInt { dst: zi, src: z });
    int(&mut a, code, k, 3);
    a.op(Opcode::And {
        dst: kind,
        a: zi,
        b: k,
    });
    int(&mut a, code, k, 2);
    a.op(Opcode::SShr {
        dst: ta,
        a: zi,
        b: k,
    });
    int(&mut a, code, k, 15);
    a.op(Opcode::And {
        dst: ta,
        a: ta,
        b: k,
    });
    int(&mut a, code, k, 6);
    a.op(Opcode::SShr {
        dst: tb,
        a: zi,
        b: k,
    });
    int(&mut a, code, stage, 1);
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.pv_cls,
    });
    a.op(Opcode::Call2 {
        dst: is_pv,
        fun: p.check,
        arg0: cls,
        arg1: mode,
    });
    a.jmp(
        Opcode::JFalse {
            cond: is_pv,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::UnsafeCast { dst: pv, src: mode });
    int(&mut a, code, stage, 2);
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
    get(&mut a, host, ctrl, p.c_host);
    get(&mut a, ctx, host, p.h_ctx);
    get(&mut a, refs, ctx, p.x_refs);
    a.op(Opcode::ToInt { dst: uid, src: y });
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.refs_get,
        arg0: refs,
        arg1: uid,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "untrap");
    get(&mut a, au, pv, p.pv_units);
    a.op(Opcode::Call2 {
        dst: ed,
        fun: p.omap_get,
        arg0: au,
        arg1: d,
    });
    a.jmp(Opcode::JNull { reg: ed, offset: 0 }, "untrap");
    int(&mut a, code, stage, 3);
    a.op(Opcode::DynGet {
        dst: e,
        obj: ed,
        field: ku,
    });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "untrap");
    get(&mut a, o, e, p.e_obj);
    int(&mut a, code, stage, 4);
    // start: remember the worker's idle anim
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNotEq {
            a: kind,
            b: k,
            offset: 0,
        },
        "notstart",
    );
    a.op(Opcode::Field {
        dst: cur,
        obj: e,
        field: p.e_anim,
    });
    a.op(Opcode::SetGlobal {
        global: st.idle,
        src: cur,
    });
    int(&mut a, code, stage, 5);
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("notstart");
    // a start missed (joined late): the current anim is still the idle one
    a.op(Opcode::GetGlobal {
        dst: cur,
        global: st.idle,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: cur,
            offset: 0,
        },
        "hasidle",
    );
    a.op(Opcode::Field {
        dst: cur,
        obj: e,
        field: p.e_anim,
    });
    a.op(Opcode::SetGlobal {
        global: st.idle,
        src: cur,
    });
    a.label("hasidle");
    int(&mut a, code, k, 1);
    a.jmp(
        Opcode::JNotEq {
            a: kind,
            b: k,
            offset: 0,
        },
        "nothit",
    );
    a.op(Opcode::GetGlobal {
        dst: anim,
        global: p.hit_g,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "play");
    a.label("nothit");
    int(&mut a, code, k, 2);
    a.jmp(
        Opcode::JNotEq {
            a: kind,
            b: k,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::GetGlobal {
        dst: anim,
        global: p.meh_g,
    });
    int(&mut a, code, k, p.success);
    a.jmp(
        Opcode::JNotEq {
            a: ta,
            b: k,
            offset: 0,
        },
        "play",
    );
    a.op(Opcode::GetGlobal {
        dst: anim,
        global: p.yes_g,
    });
    a.label("play");
    emit_play_once(&mut a, &mut r, code, p, e, anim, idle);
    int(&mut a, code, stage, 6);
    int(&mut a, code, k, 1);
    a.jmp(
        Opcode::JNotEq {
            a: kind,
            b: k,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: ev,
        obj: game,
        field: p.g_event,
    });
    a.jmp(Opcode::JNull { reg: ev, offset: 0 }, "untrap");
    a.op(Opcode::New { dst: stv });
    for (key, src) in [(ke, e), (kg, game), (ka, ta), (kb, tb)] {
        a.op(Opcode::DynSet {
            obj: stv,
            field: key,
            src,
        });
    }
    a.op(Opcode::Float { dst: t, ptr: delay });
    a.op(Opcode::InstanceClosure {
        dst: fxc,
        fun: fx,
        obj: stv,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.wait,
        arg0: ev,
        arg1: t,
        arg2: fxc,
    });
    int(&mut a, code, stage, 7);
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    emit_log(
        &mut a,
        &mut r,
        p,
        st.recv_tag,
        st.sp,
        &[kind, ta, tb, stage],
    );
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(
        code,
        vec![p.ctrl_t, p.f64_, p.f64_, p.f64_, p.player_t],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// New functions, in this order: forgeSend, forgeIdle, forgeFx, forgeRecv.
fn apply(code: &mut Bytecode, p: &Plan) -> Result<[RefFun; 4]> {
    let st = strs(code, p);
    let send = add_send(code, p, &st)?;
    let idle = mirror::add_idle(code, p, st.idle, p.dbg_file)?;
    let fx = add_fx(code, p, &st)?;
    let recv = add_recv(code, p, &st, idle, fx)?;
    let (i0, i1, i2) = (int_const(code, 0), int_const(code, 1), int_const(code, 2));

    // ping__impl: if (forgeRecv(this, x, y, z, player)) return;
    mirror::hook_ping(code, p, recv);
    // The three senders, at the entry of each ForgeAction step.
    let hook = |f: &mut Function, kind, arg: Option<Reg>, shard: Option<Reg>| {
        let mut nr = |t: RefType| {
            f.regs.push(t);
            Reg((f.regs.len() - 1) as u32)
        };
        let (c, av, sv, v) = (nr(p.i32_), nr(p.i32_), nr(p.obj_t), nr(p.void_));
        let mut ops = vec![Opcode::Int { dst: c, ptr: kind }];
        ops.push(match arg {
            Some(value) => Opcode::EnumIndex { dst: av, value },
            None => Opcode::Int { dst: av, ptr: i0 },
        });
        let s = match shard {
            Some(s) => s,
            None => {
                ops.push(Opcode::Null { dst: sv });
                sv
            }
        };
        ops.push(Opcode::Call4 {
            dst: v,
            fun: send,
            arg0: Reg(0),
            arg1: c,
            arg2: av,
            arg3: s,
        });
        insert_ops(f, 0, ops);
    };
    hook(&mut code.functions[p.init_fi], i0, None, None);
    hook(
        &mut code.functions[p.done_fi],
        i1,
        Some(Reg(2)),
        Some(Reg(1)),
    );
    hook(&mut code.functions[p.end_fi], i2, Some(Reg(1)), None);
    eprintln!(
        "patched forge mirror: ForgeAction init / setActionDone / endActivity send start / hit / end \
         over the ping RPC (forgeSend fn@{}), ping__impl plays them on the other players' parked \
         worker (forgeRecv fn@{}, forgeIdle fn@{}, forgeFx fn@{})",
        send.0, recv.0, idle.0, fx.0
    );
    Ok([send, idle, fx, recv])
}

/// Mirrors a player's forging onto the other players' scene, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_forge_mirror(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p).map(|_| ()));
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("forge mirror skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;
    use std::collections::HashMap;

    fn same(a: &Function, b: &Function) -> bool {
        format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs
    }

    /// Four hooks, four well-typed functions appended, every other function
    /// untouched; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        for (g, want) in [
            (p.hit_g, "Forge"),
            (p.yes_g, "ForgeYes"),
            (p.meh_g, "ForgeMeh"),
        ] {
            assert_eq!(const_str(&orig, g), Some(want));
        }
        // the game itself uses every sound id and prefab path
        for v in [
            SND_PERFECT,
            SND_GOOD,
            SND_BAD,
            FX_BASE,
            FX_GOOD,
            FX_BAD,
            FX_PERFECT,
            "allShards",
            "anvil",
        ] {
            assert!(orig.strings.iter().any(|s| s.as_str() == v), "{v}");
        }
        let fa = |name: &str| method(&orig, p.fa_t, name).unwrap();
        let tier_t = fun_args(&orig, fa("setActionDone"))[2];
        // EScoreTier A / B / C: feedbackOnAction's cases 0 / 1 / 2 load these sounds in order
        assert_eq!(
            enum_constructs(&orig, tier_t).unwrap()[..3],
            ["A", "B", "C"]
        );
        let fb = fa("feedbackOnAction");
        let at = |v: &str| {
            fb.ops
                .iter()
                .position(|o| matches!(o, Opcode::GetGlobal { global, .. } if const_str(&orig, *global) == Some(v)))
                .unwrap_or_else(|| panic!("{v}"))
        };
        assert!(at(SND_PERFECT) < at(SND_GOOD) && at(SND_GOOD) < at(SND_BAD));
        assert!(fb.ops.iter().any(|o| matches!(o, Opcode::Switch { .. })));
        let mut code = read(&image);
        patch_forge_mirror(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 4);
        let sites = [p.init_fi, p.done_fi, p.end_fi, p.impl_fi];
        for i in 0..n {
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                sites.contains(&i),
                "function #{i}"
            );
        }
        let (send, idle, fx, recv) = (
            back.functions[n].findex,
            back.functions[n + 1].findex,
            back.functions[n + 2].findex,
            back.functions[n + 3].findex,
        );
        for (fi, k) in [
            (p.init_fi, 4),
            (p.done_fi, 3),
            (p.end_fi, 4),
            (p.impl_fi, 3),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let mut want = a.clone();
            want.regs = b.regs.clone();
            shifted(&want, b, 0, k);
            check_types(&back, b, 0..k);
            check_flow(b);
        }
        let callee = |fi: usize, at: usize| match &back.functions[fi].ops[at] {
            Opcode::Call4 {
                fun, arg0: Reg(0), ..
            } => Some(*fun),
            Opcode::CallN { fun, args, .. }
                if args[..] == [Reg(0), Reg(1), Reg(2), Reg(3), Reg(4)] =>
            {
                Some(*fun)
            }
            _ => None,
        };
        assert_eq!(callee(p.init_fi, 3), Some(send));
        assert_eq!(callee(p.done_fi, 2), Some(send));
        assert_eq!(callee(p.end_fi, 3), Some(send));
        assert_eq!(callee(p.impl_fi, 0), Some(recv));
        // setActionDone sends the tier of its enum and the shard; endActivity the result
        assert!(matches!(
            back.functions[p.done_fi].ops[1],
            Opcode::EnumIndex { value: Reg(2), .. }
        ));
        assert!(matches!(
            back.functions[p.done_fi].ops[2],
            Opcode::Call4 { arg3: Reg(1), .. }
        ));
        assert!(matches!(
            back.functions[p.end_fi].ops[1],
            Opcode::EnumIndex { value: Reg(1), .. }
        ));
        assert_eq!(jump_targets(&back.functions[p.impl_fi], 1), vec![3]);
        for f in &back.functions[n..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // every vanilla call made by the new functions is one the plan validated
        let allowed = [
            p.ping,
            p.check,
            p.refs_get,
            p.omap_get,
            p.wait,
            p.sfx,
            p.by_name,
            p.abs_pos,
            p.set_pos,
            p.get_loader,
            p.load_cache,
            p.load_prefab,
            p.println,
            p.std_string,
            p.str_add,
            idle,
            fx,
        ];
        for f in &back.functions[n..] {
            for op in &f.ops {
                if let Some((g, _)) = crate::diag::call_of(op) {
                    assert!(allowed.contains(&g), "fn@{} calls fn@{}", f.findex.0, g.0);
                }
            }
        }
        let mut again = read(&patched);
        assert_eq!(
            format!("{:#}", plan(&again).err().unwrap()),
            "already applied"
        );
        patch_forge_mirror(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass runs in the full pipeline (after ping_cell).
    #[test]
    fn applies_in_pipeline() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let code = read(&crate::patch_image(&image).expect("patch"));
        assert_eq!(
            format!("{:#}", plan(&code).err().unwrap()),
            "already applied"
        );
    }

    /// A mismatch skips the whole pass and leaves the image as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let ctor = index_of(
            &orig,
            method(&orig, p.fa_t, "__constructor__").unwrap().findex,
        )
        .unwrap();
        let hit_at = orig.functions[ctor]
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::GetGlobal { global, .. } if *global == p.hit_g))
            .unwrap();
        let breakers: Vec<(&str, Box<dyn Fn(&mut Bytecode)>)> = vec![
            (
                "anim literal",
                Box::new(move |c: &mut Bytecode| {
                    let Opcode::GetGlobal { dst, .. } = c.functions[ctor].ops[hit_at] else {
                        unreachable!()
                    };
                    c.functions[ctor].ops[hit_at] = Opcode::Null { dst };
                }),
            ),
            (
                "fx load",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[p.impl_fi];
                    let i = f
                        .ops
                        .iter()
                        .position(|o| matches!(o, Opcode::SafeCast { .. }))
                        .unwrap();
                    f.ops[i] = Opcode::Nop;
                }),
            ),
        ];
        for (what, brk) in breakers {
            let mut code = read(&image);
            brk(&mut code);
            assert!(plan(&code).is_err(), "{what}");
            let before = write(&code);
            patch_forge_mirror(&mut code);
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
        S(String),
        Clo(RefFun, Box<V>),
    }

    struct Sim<'a> {
        code: &'a Bytecode,
        p: &'a Plan,
        orig_n: usize,
        globals: HashMap<usize, V>,
        heap: Vec<HashMap<String, V>>,
        refs: HashMap<i32, V>,
        names: HashMap<String, V>,
        log: Vec<(&'static str, Vec<V>)>,
    }

    impl Sim<'_> {
        fn new<'a>(code: &'a Bytecode, p: &'a Plan) -> Sim<'a> {
            Sim {
                code,
                p,
                orig_n: 0,
                globals: HashMap::new(),
                heap: vec![],
                refs: HashMap::new(),
                names: HashMap::new(),
                log: vec![],
            }
        }
        fn obj(&mut self, fields: &[(RefField, V)]) -> V {
            self.heap.push(
                fields
                    .iter()
                    .map(|(f, v)| (format!("f{}", f.0), v.clone()))
                    .collect(),
            );
            V::O(self.heap.len() - 1)
        }
        fn key_get(&self, o: &V, k: &str) -> V {
            let V::O(i) = o else {
                panic!("null access {k}")
            };
            self.heap[*i].get(k).cloned().unwrap_or(V::Null)
        }
        fn key_set(&mut self, o: &V, k: String, v: V) {
            let V::O(i) = o else {
                panic!("set {k} on {o:?}")
            };
            self.heap[*i].insert(k, v);
        }
        fn get(&self, o: &V, f: RefField) -> V {
            self.key_get(o, &format!("f{}", f.0))
        }
        fn arr(&mut self, items: Vec<V>) -> V {
            let n = items.len() as i32;
            let raw = self.obj(&[]);
            for (k, v) in items.into_iter().enumerate() {
                self.key_set(&raw, format!("i{k}"), v);
            }
            let (l, a) = (self.p.a_len, self.p.a_arr);
            self.obj(&[(l, V::I(n)), (a, raw)])
        }
        fn global(&self, g: RefGlobal) -> V {
            if let Some(v) = self.globals.get(&g.0) {
                return v.clone();
            }
            match const_str(self.code, g) {
                Some(s) => V::S(s.to_string()),
                None => V::Null,
            }
        }

        fn stub(&mut self, f: RefFun, a: &[V]) -> Option<V> {
            let p = self.p;
            let v = if f == p.ping {
                self.log.push(("ping", a.to_vec()));
                V::Null
            } else if f == p.check {
                V::B(self.key_get(&a[1], "pv") == V::B(true))
            } else if f == p.refs_get {
                let V::I(u) = a[1] else { panic!() };
                self.refs.get(&u).cloned().unwrap_or(V::Null)
            } else if f == p.omap_get {
                let V::O(k) = a[1] else { panic!() };
                self.key_get(&a[0], &format!("k{k}"))
            } else if f == p.wait {
                self.log.push(("wait", a[1..].to_vec()));
                V::Null
            } else if f == p.sfx {
                self.log.push(("sfx", a[1..2].to_vec()));
                V::Null
            } else if f == p.by_name {
                let V::S(n) = &a[1] else { panic!() };
                self.names.get(n).cloned().unwrap_or(V::Null)
            } else if f == p.abs_pos {
                self.key_get(&a[0], "abs")
            } else if f == p.set_pos {
                self.log.push(("pos", a[1..].to_vec()));
                V::Null
            } else if f == p.get_loader {
                self.obj(&[])
            } else if f == p.load_cache {
                let r = self.obj(&[]);
                self.key_set(&r, "path".into(), a[1].clone());
                r
            } else if f == p.load_prefab {
                let path = self.key_get(&a[1], "path");
                self.log.push(("fx", vec![path]));
                self.obj(&[])
            } else if f == p.println {
                self.log.push(("println", a.to_vec()));
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
                    Opcode::Label | Opcode::Trap { .. } | Opcode::EndTrap { .. } => {}
                    Opcode::Bool { dst, value } => r[rr(dst)] = V::B(value.0),
                    Opcode::Int { dst, ptr } => r[rr(dst)] = V::I(code.ints[ptr.0]),
                    Opcode::Float { dst, ptr } => r[rr(dst)] = V::F(code.floats[ptr.0]),
                    Opcode::Null { dst } => r[rr(dst)] = V::Null,
                    Opcode::Mov { dst, src }
                    | Opcode::SafeCast { dst, src }
                    | Opcode::UnsafeCast { dst, src }
                    | Opcode::ToVirtual { dst, src }
                    | Opcode::ToDyn { dst, src } => r[rr(dst)] = r[rr(src)].clone(),
                    Opcode::ToInt { dst, src } => {
                        let V::F(x) = r[rr(src)] else { panic!("ToInt") };
                        r[rr(dst)] = V::I(x as i32)
                    }
                    Opcode::ToSFloat { dst, src } => r[rr(dst)] = V::F(num(&r[rr(src)]) as f64),
                    Opcode::GetGlobal { dst, global } => r[rr(dst)] = self.global(*global),
                    Opcode::SetGlobal { global, src } => {
                        self.globals.insert(global.0, r[rr(src)].clone());
                    }
                    Opcode::Field { dst, obj, field } => r[rr(dst)] = self.get(&r[rr(obj)], *field),
                    Opcode::DynGet { dst, obj, field } => {
                        r[rr(dst)] = self
                            .key_get(&r[rr(obj)], &format!("d{}", code.strings[field.0].as_str()))
                    }
                    Opcode::DynSet { obj, field, src } => {
                        let (o, v) = (r[rr(obj)].clone(), r[rr(src)].clone());
                        self.key_set(&o, format!("d{}", code.strings[field.0].as_str()), v);
                    }
                    Opcode::New { dst } => r[rr(dst)] = self.obj(&[]),
                    Opcode::GetArray { dst, array, index } => {
                        let i = num(&r[rr(index)]);
                        r[rr(dst)] = self.key_get(&r[rr(array)], &format!("i{i}"));
                    }
                    Opcode::Incr { dst } => r[rr(dst)] = V::I(num(&r[rr(dst)]) + 1),
                    Opcode::Add { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) + num(&r[rr(b)])),
                    Opcode::Sub { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) - num(&r[rr(b)])),
                    Opcode::And { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) & num(&r[rr(b)])),
                    Opcode::Shl { dst, a, b } => {
                        r[rr(dst)] = V::I(num(&r[rr(a)]) << num(&r[rr(b)]))
                    }
                    Opcode::SShr { dst, a, b } => {
                        r[rr(dst)] = V::I(num(&r[rr(a)]) >> num(&r[rr(b)]))
                    }
                    Opcode::InstanceClosure { dst, fun, obj } => {
                        r[rr(dst)] = V::Clo(*fun, Box::new(r[rr(obj)].clone()))
                    }
                    Opcode::CallMethod { dst, field, args } => {
                        assert_eq!(*field, self.p.play_pi);
                        let a: Vec<V> = args.iter().map(|x| r[rr(x)].clone()).collect();
                        self.log.push(("play", a));
                        r[rr(dst)] = V::Null;
                    }
                    Opcode::JAlways { offset } => next = jump(*offset),
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
                    Opcode::JSGte { a, b, offset } | Opcode::JSLte { a, b, offset } => {
                        let (x, y) = (num(&r[rr(a)]), num(&r[rr(b)]));
                        let t = match op {
                            Opcode::JSGte { .. } => x >= y,
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

        fn take(&mut self, what: &str) -> Vec<Vec<V>> {
            let (hit, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.log)
                .into_iter()
                .partition(|(k, _)| *k == what);
            self.log = rest;
            hit.into_iter().map(|(_, a)| a).collect()
        }
    }

    struct World {
        ctrl: V,
        game: V,
        me: V,
        other: V,
        worker: V,
        shards: V,
    }

    /// A peer in a PlaceView where `other` parks its worker at element uid 77.
    fn world(sim: &mut Sim) -> World {
        let p = sim.p;
        let (me, other) = (sim.obj(&[]), sim.obj(&[]));
        let refs = sim.obj(&[]);
        let ctx = sim.obj(&[(p.x_refs, refs)]);
        let host = sim.obj(&[(p.h_ctx, ctx)]);
        let elt = sim.obj(&[]);
        let V::O(ek) = elt else { unreachable!() };
        sim.refs.insert(77, elt.clone());
        let wobj = sim.obj(&[]);
        let par = sim.obj(&[]);
        sim.key_set(&wobj, format!("f{}", p.o_parent.0), par);
        let worker = sim.obj(&[(p.e_obj, wobj), (p.e_anim, V::S("Idle01".into()))]);
        let ed = sim.obj(&[]);
        sim.key_set(&ed, "dunitView".into(), worker.clone());
        let units = sim.obj(&[]);
        sim.key_set(&units, format!("k{ek}"), ed);
        let s3d = sim.obj(&[]);
        let mode = sim.obj(&[(p.pv_units, units), (p.m_s3d, s3d)]);
        sim.key_set(&mode, "pv".into(), V::B(true));
        let ui = sim.obj(&[]);
        let ev = sim.obj(&[]);
        let game = sim.obj(&[
            (p.g_me, me.clone()),
            (p.g_mode, mode),
            (p.g_ui, ui),
            (p.g_event, ev),
        ]);
        let ctrl = sim.obj(&[(p.c_game, game.clone()), (p.c_host, host)]);
        sim.key_set(&game, format!("f{}", p.g_ctrl.0), ctrl.clone());
        // allShards with three shards; shard k sits at (k, 10 + k, 20 + k)
        let mut items = vec![];
        for k in 0..3 {
            let m = sim.obj(&[
                (p.m41, V::F(k as f64)),
                (p.m42, V::F(10.0 + k as f64)),
                (p.m43, V::F(20.0 + k as f64)),
            ]);
            let sh = sim.obj(&[]);
            sim.key_set(&sh, "abs".into(), m);
            items.push(sh);
        }
        let ch = sim.arr(items);
        let shards = sim.obj(&[(p.o_children, ch.clone())]);
        for sh in (0..3)
            .map(|k| {
                sim.key_get(
                    &sim.key_get(&ch, &format!("f{}", p.a_arr.0)),
                    &format!("i{k}"),
                )
            })
            .collect::<Vec<_>>()
        {
            sim.key_set(&sh, format!("f{}", p.o_parent.0), shards.clone());
        }
        sim.names.insert("allShards".into(), shards.clone());
        let cache = sim.obj(&[]);
        let main = sim.obj(&[(p.m_cache, cache)]);
        sim.globals.insert(p.main_g.0, main);
        World {
            ctrl,
            game,
            me,
            other,
            worker,
            shards,
        }
    }

    fn patched() -> Option<(Bytecode, usize)> {
        let image = std::fs::read(HLBOOT).ok()?;
        let mut code = read(&image);
        let n = code.functions.len();
        patch_forge_mirror(&mut code);
        Some((code, n))
    }

    fn z(kind: i32, a: i32, b: i32) -> V {
        V::F((kind + (a << 2) + (b << 6)) as f64)
    }

    /// The receiver: start / hit / end on a peer, the sender itself, a plain ping.
    #[test]
    fn receiver_behaviour() {
        let Some((code, n)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&std::fs::read(HLBOOT).unwrap())).unwrap();
        let (idle, fx, recv) = (
            code.functions[n + 1].findex,
            code.functions[n + 2].findex,
            code.functions[n + 3].findex,
        );
        let mut sim = Sim::new(&code, &p);
        sim.orig_n = n;
        let w = world(&mut sim);
        let ev = |sim: &mut Sim, x: f64, zz: V, who: &V| {
            sim.run(
                recv,
                vec![w.ctrl.clone(), V::F(x), V::F(77.0), zz, who.clone()],
            )
        };
        let s = |t: &str| V::S(t.into());

        // a vanilla ping goes on to the marker
        assert_eq!(ev(&mut sim, 12.5, z(1, 0, 0), &w.other), V::B(false));
        assert!(sim.log.is_empty());
        // the sender's own echo: swallowed, nothing played
        assert_eq!(ev(&mut sim, SENTINEL, z(1, 0, 0), &w.me), V::B(true));
        assert!(sim.log.is_empty());

        // start: the worker's idle anim is remembered
        assert_eq!(ev(&mut sim, SENTINEL, z(0, 0, 0), &w.other), V::B(true));
        assert!(sim.take("play").is_empty());
        assert_eq!(sim.take("println"), vec![vec![s("mp: forge recv 0 0 0 5")]]);

        // hit, Perfect on shard index 2: "Forge" once, then idle; fx 0.4 s later
        assert_eq!(ev(&mut sim, SENTINEL, z(1, 0, 3), &w.other), V::B(true));
        let plays = sim.take("play");
        assert_eq!(plays.len(), 1);
        assert_eq!(plays[0][..2], [w.worker.clone(), s("Forge")]);
        let opts = plays[0][2].clone();
        assert_eq!(sim.key_get(&opts, "dloop"), V::B(false));
        assert_eq!(
            sim.key_get(&opts, "donEnd"),
            V::Clo(idle, Box::new(w.worker.clone()))
        );
        let waits = sim.take("wait");
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0][0], V::F(FX_DELAY));
        let V::Clo(f, st) = waits[0][1].clone() else {
            panic!()
        };
        assert_eq!(f, fx);
        assert_eq!(sim.take("println"), vec![vec![s("mp: forge recv 1 0 3 7")]]);

        // the delayed fx: Perfect sound, base + good + perfect particles at shard 2
        sim.run(fx, vec![*st]);
        assert_eq!(sim.take("sfx"), vec![vec![s(SND_PERFECT)]]);
        let fxs: Vec<V> = sim.take("fx").into_iter().map(|a| a[0].clone()).collect();
        assert_eq!(fxs, [s(FX_BASE), s(FX_GOOD), s(FX_PERFECT)]);
        let at = vec![V::F(2.0), V::F(12.0), V::F(22.0)];
        assert_eq!(sim.take("pos"), vec![at.clone(), at.clone(), at]);
        assert!(sim.take("wait").iter().all(|a| a[0] == V::F(FX_LIFE)));

        // Bad hit with no shard: bad sound, base + bad at the anvil (none: the worker)
        let wpos = sim.obj(&[(p.m41, V::F(5.0)), (p.m42, V::F(6.0)), (p.m43, V::F(7.0))]);
        let wobj = sim.get(&w.worker, p.e_obj);
        sim.key_set(&wobj, "abs".into(), wpos);
        ev(&mut sim, SENTINEL, z(1, 2, 0), &w.other);
        let waits = sim.take("wait");
        let V::Clo(_, st) = waits[0][1].clone() else {
            panic!()
        };
        sim.log.clear();
        sim.run(fx, vec![*st]);
        assert_eq!(sim.take("sfx"), vec![vec![s(SND_BAD)]]);
        let fxs: Vec<V> = sim.take("fx").into_iter().map(|a| a[0].clone()).collect();
        assert_eq!(fxs, [s(FX_BASE), s(FX_BAD)]);
        assert_eq!(sim.take("pos")[0], vec![V::F(5.0), V::F(6.0), V::F(7.0)]);

        // the hit anim's end: idle again, looped
        sim.log.clear();
        sim.run(idle, vec![w.worker.clone()]);
        let plays = sim.take("play");
        assert_eq!(plays[0][1], s("Idle01"));
        assert_eq!(sim.key_get(&plays[0][2], "dloop"), V::B(true));

        // end: Success -> "ForgeYes", anything else -> "ForgeMeh"; no fx
        for (a, anim) in [(p.success, "ForgeYes"), (p.success + 1, "ForgeMeh")] {
            sim.log.clear();
            ev(&mut sim, SENTINEL, z(2, a, 0), &w.other);
            let plays = sim.take("play");
            assert_eq!(plays[0][1], s(anim), "result {a}");
            assert!(sim.take("wait").is_empty());
            assert_eq!(
                sim.take("println"),
                vec![vec![s(&format!("mp: forge recv 2 {a} 0 6"))]]
            );
        }

        // outside a PlaceView: nothing played, the stage says where it stopped
        let mode = sim.get(&w.game, p.g_mode);
        sim.key_set(&mode, "pv".into(), V::B(false));
        sim.log.clear();
        assert_eq!(ev(&mut sim, SENTINEL, z(1, 0, 0), &w.other), V::B(true));
        assert!(sim.take("play").is_empty());
        assert_eq!(sim.take("println"), vec![vec![s("mp: forge recv 1 0 0 1")]]);
    }

    /// The sender: one ping with the sentinel, the element uid and the packed event.
    #[test]
    fn sender_behaviour() {
        let Some((code, n)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&std::fs::read(HLBOOT).unwrap())).unwrap();
        let send = code.functions[n].findex;
        let mut sim = Sim::new(&code, &p);
        sim.orig_n = n;
        let w = world(&mut sim);
        let tgt = sim.obj(&[(p.e_uid, V::I(77))]);
        let act = sim.obj(&[(p.a_target, tgt)]);
        let win = sim.obj(&[(p.w_game, w.game.clone()), (p.w_act, act)]);
        let ch = sim.get(&w.shards, p.o_children);
        let raw = sim.get(&ch, p.a_arr);
        let shard2 = sim.key_get(&raw, "i2");

        // hit, tier 1, shard index 2 -> b = 3
        sim.run(send, vec![win.clone(), V::I(1), V::I(1), shard2]);
        let pings = sim.take("ping");
        assert_eq!(
            pings,
            vec![vec![
                w.ctrl.clone(),
                V::F(SENTINEL),
                V::F(77.0),
                z(1, 1, 3),
                w.me.clone()
            ]]
        );
        assert_eq!(
            sim.take("println"),
            vec![vec![V::S("mp: forge send 1 1 3 77".into())]]
        );
        // start without a shard
        sim.run(send, vec![win.clone(), V::I(0), V::I(0), V::Null]);
        assert_eq!(sim.take("ping")[0][3], z(0, 0, 0));
        // a shard without a parent: b = 0 (the receiver falls back to the anvil)
        let lone = sim.obj(&[]);
        sim.run(send, vec![win.clone(), V::I(2), V::I(p.success), lone]);
        assert_eq!(sim.take("ping")[0][3], z(2, p.success, 0));
        // solo (no network host): nothing sent
        let ctrl = sim.get(&w.game, p.g_ctrl);
        sim.key_set(&ctrl, format!("f{}", p.c_host.0), V::Null);
        sim.log.clear();
        sim.run(send, vec![win, V::I(1), V::I(0), V::Null]);
        assert!(sim.log.is_empty());
    }
}
