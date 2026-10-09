// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op work mirror: while a player does a local-only mini-game other than
// the forge (archery, and the progress-bar activities of ui.win.UnitAction:
// Study, MoneyLaundering, Dismantling, Altering, Snaring, Tracking, ...), the
// other players see that player's worker work: one work anim per click / shot
// and back to its idle, at the end its idle again. Their camera, UI and input
// are untouched; nothing is created on their side.
//
// Vanilla: these activities have no data.cdb `props.coop`, so the Activity and
// its window live on the player's machine only. The others see the worker the
// way Activity.setUnit__impl shows it to them: in a place, parked at the
// element (`Controller.addUnitToElement` -> `PlaceView.activityUnits.get(
// element).unitView`, only when the activity prefab's Tool is visible); in the
// camp, the unit's own camp entry entity (setUnit__impl uses it as unitView).
// The visuals of these windows: UnitAction.click plays "Attack" once on the
// worker (then its idle), its progress bar is 2D; Archery is a first-person
// bow / arrows / target scene of its own with shot sounds and no worker anim.
// So the mirror plays UnitAction's work anim ("Attack", read from click) per
// click or shot, the idle at the end.
//
// The events ride the ping RPC like the forge mirror (mirror.rs), with their
// own sentinel x = -1000000064, y = hxbit uid of the activity element (place)
// or of the unit (camp), z = code + (camp << 2) + (kind << 3); code 0 start,
// 1 hit, 2 end; kind 0 UnitAction, 1 Archery:
//   sender, `workSend(game, act, code, kind)`, co-op only (`ctrl.__host`),
//   never for a replicated (coop) activity (coop_spectate.rs shows those),
//   under a trap; start remembers the activity in `workAct`, end forgets it:
//     UnitAction.init / Archery.init entry           -> start
//     UnitAction.click / Archery.setWorldPosOnShoot entry -> hit
//     Activity._cancel__impl entry (`workEnd`): this == workAct -> end
//       (every activity end, success or cancel, runs _cancel)
//   receiver, ping__impl entry `if (workRecv(this, x, y, z, player)) return;`:
//     false unless x is this sentinel; skipped for the sender itself; worker
//     = mirrorWorker(game, y, camp); start: remember its current anim as that
//     worker's idle (`workIdleOf`, per worker, see mirror.rs); hit: the work
//     anim once, onEnd -> workIdle (that idle looped); end: the idle looped,
//     the worker's entry forgotten.
// shim.log: `mp: work send <code> <kind> <camp> <uid>` on the player's
// machine, `mp: work recv <code> <kind> <camp> <stage>` on the others (stage 3
// start, 4 hit, 5 end; 1 no worker found).
// All players need this build (an older one shows a far-away ping with its
// sound). The y (a unit / element uid) and z go over the wire as f32 too. Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};

use super::job_xp::{const_str, str_global};
use super::mirror::{self, emit_idle_now, emit_log, emit_play_once, int, writer, Base, Camp};
use super::*;
use hlbc::types::{RefGlobal, ValBool};

/// The x of a work mirror event: exact in f32 (the ping RPC sends its floats
/// as f32, see forge_mirror.rs `SENTINEL`) and not the forge's.
pub(crate) const SENTINEL: f64 = -1_000_000_064.0;
const RECV_TAG: &str = "mp: work recv";

pub(crate) struct Plan {
    pub(crate) b: Base,
    pub(crate) c: Camp,
    pub(crate) ua_act: RefField,
    pub(crate) ar_act: RefField,
    pub(crate) w_game: RefField,
    pub(crate) a_game: RefField,
    pub(crate) work_g: RefGlobal,
    /// UnitAction.init, UnitAction.click, Archery.init, Archery.setWorldPosOnShoot.
    pub(crate) sites: [usize; 4],
    pub(crate) cancel_fi: usize,
    dbg_file: usize,
}

impl std::ops::Deref for Plan {
    type Target = Base;
    fn deref(&self) -> &Base {
        &self.b
    }
}

pub(crate) fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|s| s.as_str() == RECV_TAG) {
        bail!("already applied");
    }
    let b = mirror::base(code)?;
    let c = mirror::camp(code, &b)?;
    let ua_t = obj_type(code, "ui.win.UnitAction")?;
    let ar_t = obj_type(code, "ui.win.Archery")?;
    let win_t = obj_type(code, "ui.Window")?;
    for t in [ua_t, ar_t] {
        if !is_sub(code, t, win_t) {
            bail!("type {} is not a ui.Window", t.0);
        }
    }
    let ua_act = typed(code, ua_t, "activity", b.act_t)?;
    let ar_act = typed(code, ar_t, "activity", b.act_t)?;
    let w_game = typed(code, win_t, "game", b.game_t)?;
    // the same Window.game slot on both window classes
    for t in [ua_t, ar_t] {
        if typed(code, t, "game", b.game_t)? != w_game {
            bail!("Window.game moved on type {}", t.0);
        }
    }
    let a_game = typed(code, b.act_t, "game", b.game_t)?;

    let m = |t: RefType, name: &str| -> Result<usize> {
        let f = method(code, t, name)?;
        mirror::want(code, f.findex, name, &[t], b.void_)?;
        fun_index(code, f.findex)
    };
    let sites = [
        m(ua_t, "init")?,
        m(ua_t, "click")?,
        m(ar_t, "init")?,
        m(ar_t, "setWorldPosOnShoot")?,
    ];
    // UnitAction.click: activity.unitView.play("<work anim>", ...)
    let click = &code.functions[sites[1]];
    let plays: Vec<RefGlobal> = click
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::CallMethod { field, args, .. } if *field == b.play_pi && args.len() == 4 => {
                match writer(click, args[1], i) {
                    Some(Opcode::GetGlobal { global, .. })
                        if const_str(code, *global).is_some() =>
                    {
                        Some(*global)
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    let [work_g] = plays[..] else {
        bail!(
            "UnitAction.click: {} literal work anims, want 1",
            plays.len()
        );
    };
    let cancel = method(code, b.act_t, "_cancel__impl")?;
    if fun_args(code, cancel) != [b.act_t] {
        bail!("unexpected Activity._cancel__impl signature");
    }
    if !matches!(cancel.ops.last(), Some(Opcode::Ret { ret }) if cancel.regs[ret.0 as usize] == b.void_)
    {
        bail!("Activity._cancel__impl does not end in a void Ret");
    }
    Ok(Plan {
        cancel_fi: fun_index(code, cancel.findex)?,
        b,
        c,
        ua_act,
        ar_act,
        w_game,
        a_game,
        work_g,
        sites,
        dbg_file: debug_file(code, "src/ui/win/UnitAction.hx")?,
    })
}

struct G {
    send_tag: RefGlobal,
    recv_tag: RefGlobal,
    sp: RefGlobal,
    /// New globals: the activity being mirrored and its kind, the worker's idle anim.
    act: RefGlobal,
    kind: RefGlobal,
    idle: RefGlobal,
}

fn globals(code: &mut Bytecode, p: &Plan) -> G {
    let send_tag = str_global(code, p.str_t, "mp: work send");
    let recv_tag = str_global(code, p.str_t, RECV_TAG);
    let sp = str_global(code, p.str_t, " ");
    let mut new = |t: RefType| {
        code.globals.push(t);
        RefGlobal(code.globals.len() - 1)
    };
    G {
        send_tag,
        recv_tag,
        sp,
        act: new(p.act_t),
        kind: new(p.i32_),
        idle: new(p.omap_t),
    }
}

/// `workSend(game, act, code, kind)`: the ping carrying one event (see the header).
fn add_send(code: &mut Bytecode, p: &Plan, g: &G) -> Result<RefFun> {
    let mut r = Regs(vec![p.game_t, p.act_t, p.i32_, p.i32_]);
    let (game, act, kind_code, kind) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let (exc, ctrl, host, ah, mode, cls, camp, unit, tgt, uid, cb, k, z, t) = (
        r.r(p.dyn_t),
        r.r(p.ctrl_t),
        r.r(p.host_t),
        r.r(p.host_t),
        r.r(p.mode_t),
        r.r(p.c.cls_t),
        r.r(p.bool_),
        r.r(p.unit_t),
        r.r(p.ent_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let (xf, yf, zf, me, n, v) = (
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.player_t),
        r.r(p.act_t),
        r.r(p.void_),
    );
    let sent = float_const(code, SENTINEL);
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "untrap",
    );
    a.jmp(
        Opcode::JNull {
            reg: act,
            offset: 0,
        },
        "untrap",
    );
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
    get(&mut a, ctrl, game, p.g_ctrl);
    // solo: no network, nothing to mirror
    get(&mut a, host, ctrl, p.c_host);
    // a replicated (coop) activity: the others run its window themselves
    a.op(Opcode::Field {
        dst: ah,
        obj: act,
        field: p.a_host,
    });
    a.jmp(Opcode::JNotNull { reg: ah, offset: 0 }, "untrap");
    get(&mut a, mode, game, p.g_mode);
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.c.cls,
    });
    a.op(Opcode::Call2 {
        dst: camp,
        fun: p.check,
        arg0: cls,
        arg1: mode,
    });
    a.jmp(
        Opcode::JFalse {
            cond: camp,
            offset: 0,
        },
        "place",
    );
    get(&mut a, unit, act, p.a_unit);
    a.op(Opcode::Field {
        dst: uid,
        obj: unit,
        field: p.u_uid,
    });
    int(&mut a, code, cb, 4);
    a.jmp(Opcode::JAlways { offset: 0 }, "enc");
    a.label("place");
    get(&mut a, tgt, act, p.a_target);
    a.op(Opcode::Field {
        dst: uid,
        obj: tgt,
        field: p.e_uid,
    });
    int(&mut a, code, cb, 0);
    // z = code + (camp << 2) + (kind << 3)
    a.label("enc");
    int(&mut a, code, k, 3);
    a.op(Opcode::Shl {
        dst: t,
        a: kind,
        b: k,
    });
    a.op(Opcode::Add {
        dst: z,
        a: kind_code,
        b: cb,
    });
    a.op(Opcode::Add { dst: z, a: z, b: t });
    a.op(Opcode::ToSFloat { dst: zf, src: z });
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
    // start: this activity's end is mirrored; end: forgotten
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNotEq {
            a: kind_code,
            b: k,
            offset: 0,
        },
        "notstart",
    );
    a.op(Opcode::SetGlobal {
        global: g.act,
        src: act,
    });
    a.op(Opcode::SetGlobal {
        global: g.kind,
        src: kind,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "log");
    a.label("notstart");
    int(&mut a, code, k, 2);
    a.jmp(
        Opcode::JNotEq {
            a: kind_code,
            b: k,
            offset: 0,
        },
        "log",
    );
    a.op(Opcode::Null { dst: n });
    a.op(Opcode::SetGlobal {
        global: g.act,
        src: n,
    });
    a.label("log");
    int(&mut a, code, k, 2);
    a.op(Opcode::SShr {
        dst: t,
        a: cb,
        b: k,
    });
    emit_log(
        &mut a,
        &mut r,
        p,
        g.send_tag,
        g.sp,
        &[kind_code, kind, t, uid],
    );
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.game_t, p.act_t, p.i32_, p.i32_],
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `workEnd(act)`: the end event of the mirrored activity (Activity._cancel__impl).
fn add_end(code: &mut Bytecode, p: &Plan, g: &G, send: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (cur, game, c2, kind, v) = (
        r.r(p.act_t),
        r.r(p.game_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: cur,
        global: g.act,
    });
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "ret",
    );
    a.jmp(
        Opcode::JNotEq {
            a: cur,
            b: act,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: game,
        obj: act,
        field: p.a_game,
    });
    int(&mut a, code, c2, 2);
    a.op(Opcode::GetGlobal {
        dst: kind,
        global: g.kind,
    });
    a.op(Opcode::Call4 {
        dst: v,
        fun: send,
        arg0: game,
        arg1: act,
        arg2: c2,
        arg3: kind,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.act_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `workRecv(ctrl, x, y, z, player) -> Bool` (see the header).
fn add_recv(
    code: &mut Bytecode,
    p: &Plan,
    g: &G,
    worker: RefFun,
    (idle, idle_of): (RefFun, RefFun),
) -> Result<RefFun> {
    let mut r = Regs(vec![p.ctrl_t, p.f64_, p.f64_, p.f64_, p.player_t]);
    let (ctrl, x, y, z, player) = (Reg(0), Reg(1), Reg(2), Reg(3), Reg(4));
    let (res, fs, game, me, zi, k, kc, camp, kind, stage, exc, is_camp, uid, e) = (
        r.r(p.bool_),
        r.r(p.f64_),
        r.r(p.game_t),
        r.r(p.player_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.dyn_t),
        r.r(p.bool_),
        r.r(p.i32_),
        r.r(p.ent_t),
    );
    let (cur, anim) = (r.r(p.str_t), r.r(p.str_t));
    let sent = float_const(code, SENTINEL);
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
    // the working player has its own window
    a.jmp(
        Opcode::JEq {
            a: player,
            b: me,
            offset: 0,
        },
        "ret",
    );
    // code = z & 3; camp = (z >> 2) & 1; kind = z >> 3
    a.op(Opcode::ToInt { dst: zi, src: z });
    int(&mut a, code, k, 3);
    a.op(Opcode::And {
        dst: kc,
        a: zi,
        b: k,
    });
    int(&mut a, code, k, 2);
    a.op(Opcode::SShr {
        dst: camp,
        a: zi,
        b: k,
    });
    int(&mut a, code, k, 1);
    a.op(Opcode::And {
        dst: camp,
        a: camp,
        b: k,
    });
    int(&mut a, code, k, 3);
    a.op(Opcode::SShr {
        dst: kind,
        a: zi,
        b: k,
    });
    int(&mut a, code, stage, 1);
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Bool {
        dst: is_camp,
        value: ValBool(false),
    });
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JEq {
            a: camp,
            b: k,
            offset: 0,
        },
        "look",
    );
    a.op(Opcode::Bool {
        dst: is_camp,
        value: ValBool(true),
    });
    a.label("look");
    a.op(Opcode::ToInt { dst: uid, src: y });
    a.op(Opcode::Call3 {
        dst: e,
        fun: worker,
        arg0: game,
        arg1: uid,
        arg2: is_camp,
    });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "untrap");
    int(&mut a, code, stage, 2);
    // start: remember the worker's idle anim
    int(&mut a, code, k, 0);
    a.jmp(
        Opcode::JNotEq {
            a: kc,
            b: k,
            offset: 0,
        },
        "notstart",
    );
    int(&mut a, code, k, mirror::IDLE_START);
    a.op(Opcode::Call2 {
        dst: cur,
        fun: idle_of,
        arg0: e,
        arg1: k,
    });
    int(&mut a, code, stage, 3);
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("notstart");
    int(&mut a, code, k, 1);
    a.jmp(
        Opcode::JNotEq {
            a: kc,
            b: k,
            offset: 0,
        },
        "nothit",
    );
    // hit: this worker's idle (its current anim if the start was missed)
    int(&mut a, code, k, mirror::IDLE_READ);
    a.op(Opcode::Call2 {
        dst: cur,
        fun: idle_of,
        arg0: e,
        arg1: k,
    });
    a.op(Opcode::GetGlobal {
        dst: anim,
        global: p.work_g,
    });
    emit_play_once(&mut a, &mut r, code, p, e, anim, cur, idle);
    int(&mut a, code, stage, 4);
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("nothit");
    int(&mut a, code, k, 2);
    a.jmp(
        Opcode::JNotEq {
            a: kc,
            b: k,
            offset: 0,
        },
        "untrap",
    );
    // end: the idle, and the worker's entry goes
    int(&mut a, code, k, mirror::IDLE_END);
    a.op(Opcode::Call2 {
        dst: cur,
        fun: idle_of,
        arg0: e,
        arg1: k,
    });
    emit_idle_now(&mut a, &mut r, code, p, e, cur, idle);
    int(&mut a, code, stage, 5);
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    emit_log(
        &mut a,
        &mut r,
        p,
        g.recv_tag,
        g.sp,
        &[kc, kind, camp, stage],
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

/// New functions, in this order: mirrorWorker, workIdle, workIdleOf, workSend, workEnd, workRecv.
fn apply(code: &mut Bytecode, p: &Plan) -> Result<[RefFun; 5]> {
    let g = globals(code, p);
    let worker = mirror::add_worker(code, p, &p.c, p.dbg_file)?;
    let idle = mirror::add_idle(code, p, p.dbg_file)?;
    let idle_of = mirror::add_idle_of(code, p, g.idle, p.dbg_file)?;
    let send = add_send(code, p, &g)?;
    let end = add_end(code, p, &g, send)?;
    let recv = add_recv(code, p, &g, worker, (idle, idle_of))?;

    mirror::hook_ping(code, p, recv);
    forget_on_game_dispose(code, p.dispose_fi, &[g.idle]);
    // senders at the entry of the four window steps: workSend(this.game, this.activity, code, kind)
    let ints = [
        (int_const(code, 0), int_const(code, 0)),
        (int_const(code, 1), int_const(code, 0)),
        (int_const(code, 0), int_const(code, 1)),
        (int_const(code, 1), int_const(code, 1)),
    ];
    for (k, &fi) in p.sites.iter().enumerate() {
        let act_f = if k < 2 { p.ua_act } else { p.ar_act };
        let f = &mut code.functions[fi];
        let mut nr = |t: RefType| {
            f.regs.push(t);
            Reg((f.regs.len() - 1) as u32)
        };
        let (gm, ac, c, kd, v) = (
            nr(p.game_t),
            nr(p.act_t),
            nr(p.i32_),
            nr(p.i32_),
            nr(p.void_),
        );
        insert_ops(
            f,
            0,
            vec![
                Opcode::Field {
                    dst: gm,
                    obj: Reg(0),
                    field: p.w_game,
                },
                Opcode::Field {
                    dst: ac,
                    obj: Reg(0),
                    field: act_f,
                },
                Opcode::Int {
                    dst: c,
                    ptr: ints[k].0,
                },
                Opcode::Int {
                    dst: kd,
                    ptr: ints[k].1,
                },
                Opcode::Call4 {
                    dst: v,
                    fun: send,
                    arg0: gm,
                    arg1: ac,
                    arg2: c,
                    arg3: kd,
                },
            ],
        );
    }
    // Activity._cancel__impl: workEnd(this)
    {
        let f = &mut code.functions[p.cancel_fi];
        f.regs.push(p.void_);
        let v = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            0,
            vec![Opcode::Call1 {
                dst: v,
                fun: end,
                arg0: Reg(0),
            }],
        );
    }
    eprintln!(
        "patched work mirror: UnitAction init / click and Archery init / shot send start / hit, \
         Activity._cancel__impl the end, over the ping RPC (workSend fn@{}, workEnd fn@{}); \
         ping__impl plays them on the other players' worker in a place or the camp (workRecv \
         fn@{}, mirrorWorker fn@{}, workIdle fn@{})",
        send.0, end.0, recv.0, worker.0, idle.0
    );
    Ok([worker, idle, send, end, recv])
}

/// Mirrors a player's archery / progress-bar work onto the other players'
/// scene, or leaves `code` untouched and logs why.
pub(crate) fn patch_work_mirror(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p).map(|_| ()));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("work mirror skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::same;
    use crate::asm::testutil::*;
    use crate::diag::call_of;
    use crate::testsim::{Core, Sim, V};

    /// Six sites edited, five well-typed functions appended, nothing else
    /// touched; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(const_str(&orig, p.work_g), Some("Attack"));
        let mut code = read(&image);
        patch_work_mirror(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 6);
        let mut sites = p.sites.to_vec();
        sites.extend([p.cancel_fi, p.impl_fi, p.dispose_fi]);
        for i in 0..n {
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                sites.contains(&i),
                "function #{i}"
            );
        }
        let nf = |k: usize| back.functions[n + k].findex;
        let (worker, idle, idle_of, send, end, recv) = (nf(0), nf(1), nf(2), nf(3), nf(4), nf(5));
        for (k, &fi) in p.sites.iter().enumerate() {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let mut want = a.clone();
            want.regs = b.regs.clone();
            shifted(&want, b, 0, 5);
            check_types(&back, b, 0..b.ops.len());
            check_flow(b);
            let Opcode::Call4 {
                fun,
                arg0,
                arg1,
                arg2,
                arg3,
                ..
            } = b.ops[4]
            else {
                panic!("site {k}")
            };
            assert_eq!(fun, send);
            assert!(
                matches!(b.ops[0], Opcode::Field { dst, obj: Reg(0), field } if dst == arg0 && field == p.w_game)
            );
            let act_f = if k < 2 { p.ua_act } else { p.ar_act };
            assert!(
                matches!(b.ops[1], Opcode::Field { dst, obj: Reg(0), field } if dst == arg1 && field == act_f)
            );
            let val = |op: &Opcode, r: Reg| match op {
                Opcode::Int { dst, ptr } if *dst == r => back.ints[ptr.0],
                o => panic!("{o:?}"),
            };
            // (code, kind): init start, click / shot hit; UnitAction 0, Archery 1
            assert_eq!(
                (val(&b.ops[2], arg2), val(&b.ops[3], arg3)),
                [(0, 0), (1, 0), (0, 1), (1, 1)][k]
            );
        }
        for (fi, k, callee) in [(p.cancel_fi, 1, end), (p.impl_fi, 3, recv)] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            let mut want = a.clone();
            want.regs = b.regs.clone();
            shifted(&want, b, 0, k);
            check_types(&back, b, 0..b.ops.len());
            check_flow(b);
            assert_eq!(call_of(&b.ops[0]).map(|c| c.0), Some(callee));
        }
        assert_eq!(jump_targets(&back.functions[p.impl_fi], 1), vec![3]);
        for f in &back.functions[n..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        let allowed = [
            p.ping,
            p.check,
            p.refs_get,
            p.omap_get,
            p.omap_set,
            p.omap_remove,
            p.omap_new,
            p.println,
            p.std_string,
            p.str_add,
            worker,
            idle,
            idle_of,
            send,
        ];
        for f in &back.functions[n..] {
            for op in &f.ops {
                if let Some((g, _)) = call_of(op) {
                    assert!(allowed.contains(&g), "fn@{} calls fn@{}", f.findex.0, g.0);
                }
                if let Opcode::CallMethod { field, .. } = op {
                    assert_eq!(*field, p.play_pi);
                }
            }
        }
        let mut again = read(&patched);
        assert_eq!(
            format!("{:#}", plan(&again).err().unwrap()),
            "already applied"
        );
        patch_work_mirror(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass runs in the full pipeline, next to the forge mirror.
    #[test]
    fn applies_in_pipeline() {
        let Some(image) = game() else { return };
        let code = read(&crate::patch_image(&image).expect("patch"));
        assert_eq!(
            format!("{:#}", plan(&code).err().unwrap()),
            "already applied"
        );
        let b = mirror::base(&code).unwrap();
        let names: Vec<String> = mirror::ping_hooks(&code, &b)
            .into_iter()
            .map(|f| {
                let g = &code.functions[fun_index(&code, f).unwrap()];
                // each receiver logs its own tag
                g.ops
                    .iter()
                    .find_map(|o| match o {
                        Opcode::GetGlobal { global, .. } => const_str(&code, *global)
                            .filter(|s| s.ends_with(" recv"))
                            .map(str::to_string),
                        _ => None,
                    })
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"mp: forge recv".to_string()));
        assert!(names.contains(&"mp: work recv".to_string()));
    }

    /// A mismatch skips the whole pass and leaves the image as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let click = p.sites[1];
        let su = fun_index(
            &orig,
            method(&orig, p.act_t, "setUnit__impl").unwrap().findex,
        )
        .unwrap();
        let breakers: Vec<(&str, Box<dyn Fn(&mut Bytecode)>)> = vec![
            (
                "work anim literal",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[click];
                    let wg = p.work_g;
                    let i = f
                        .ops
                        .iter()
                        .position(
                            |o| matches!(o, Opcode::GetGlobal { global, .. } if *global == wg),
                        )
                        .unwrap();
                    let Opcode::GetGlobal { dst, .. } = f.ops[i] else {
                        unreachable!()
                    };
                    f.ops[i] = Opcode::Null { dst };
                }),
            ),
            (
                "camp loop",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[su];
                    let i = f
                        .ops
                        .iter()
                        .position(|o| matches!(o, Opcode::EnumField { .. }))
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
            patch_work_mirror(&mut code);
            assert!(write(&code) == before, "{what}: image changed");
        }
    }

    // ---------- behaviour ----------

    fn patched() -> Option<(Bytecode, usize, Plan)> {
        let image = std::fs::read(HLBOOT).ok()?;
        let p = plan(&read(&image)).unwrap();
        let mut code = read(&image);
        let n = code.functions.len();
        patch_work_mirror(&mut code);
        Some((code, n, p))
    }

    fn sim<'a>(code: &'a Bytecode, n: usize, p: &'a Plan) -> Sim<'a> {
        let mut s = Sim::new(
            code,
            n,
            move |c: &mut Core, f: RefFun, a: &[V]| {
                if f == p.ping || f == p.println {
                    c.log
                        .push((if f == p.ping { "ping" } else { "println" }, a.to_vec()));
                    Some(V::Null)
                } else if f == p.check {
                    // class globals hold their name; the mode says what it is
                    let V::S(cls) = &a[0] else {
                        panic!("check {a:?}")
                    };
                    Some(V::B(c.key_get(&a[1], cls) == V::B(true)))
                } else if f == p.refs_get {
                    let V::I(u) = a[1] else { panic!() };
                    Some(c.map("refs", &u.to_string()))
                } else if f == p.omap_get {
                    let V::O(k) = a[1] else { panic!() };
                    Some(c.key_get(&a[0], &format!("k{k}")))
                } else if f == p.omap_set || f == p.omap_remove {
                    let V::O(k) = a[1] else { panic!() };
                    let v = if f == p.omap_set {
                        a[2].clone()
                    } else {
                        V::Null
                    };
                    c.key_set(&a[0], format!("k{k}"), v);
                    Some(if f == p.omap_set { V::Null } else { V::B(true) })
                } else if f == p.omap_new {
                    Some(V::Null)
                } else if f == p.std_string {
                    match &a[0] {
                        V::I(n) => Some(V::S(n.to_string())),
                        o => panic!("Std.string {o:?}"),
                    }
                } else if f == p.str_add {
                    match (&a[0], &a[1]) {
                        (V::S(x), V::S(y)) => Some(V::S(format!("{x}{y}"))),
                        o => panic!("String add {o:?}"),
                    }
                } else {
                    None
                }
            },
            move |c: &mut Core, pi: usize, a: &[V]| {
                assert_eq!(pi, p.play_pi.0);
                c.log.push(("play", a.to_vec()));
                V::Null
            },
        );
        s.c.globals.insert(p.pv_cls.0, V::S("pv".into()));
        s.c.globals.insert(p.c.cls.0, V::S("camp".into()));
        s
    }

    struct W {
        ctrl: V,
        game: V,
        me: V,
        other: V,
        mode: V,
        elt: V,
        unit: V,
        parked: V,
        camper: V,
        act: V,
    }

    /// A machine (`me`) in a place where `other`'s unit (uid 9) is parked at
    /// element uid 77; in the camp the same unit has an entry entity.
    fn world(s: &mut Sim, p: &Plan) -> W {
        let c = &mut s.c;
        let (me, other) = (c.obj(&[]), c.obj(&[]));
        let host = c.obj(&[]);
        let ctx = c.obj(&[(p.h_ctx, V::Null)]);
        let refs = c.obj(&[]);
        c.set(&ctx, p.x_refs, refs);
        c.set(&host, p.h_ctx, ctx);
        let elt = c.obj(&[(p.e_uid, V::I(77))]);
        let unit = c.obj(&[(p.u_uid, V::I(9))]);
        c.put("refs", "77", elt.clone());
        c.put("refs", "9", unit.clone());
        let wobj = c.obj(&[]);
        let par = c.obj(&[]);
        c.set(&wobj, p.o_parent, par);
        let parked = c.obj(&[(p.e_obj, wobj.clone()), (p.e_anim, V::S("IdlePose".into()))]);
        let ed = c.obj(&[]);
        c.key_set(&ed, "dunitView".into(), parked.clone());
        let units = c.obj(&[]);
        let V::O(ek) = elt else { unreachable!() };
        c.key_set(&units, format!("k{ek}"), ed);
        // camp entries: someone else's unit, then this unit
        let mk = |c: &mut Core, u: V, anim: &str| {
            let content = c.enm(p.c.unit_idx, vec![u]);
            let entry = c.obj(&[(p.c.en_content, content)]);
            c.obj(&[
                (p.c.ce_entry, entry),
                (p.e_obj, wobj.clone()),
                (p.e_anim, V::S(anim.into())),
            ])
        };
        let stranger = c.obj(&[]);
        let e1 = mk(c, stranger, "Walk");
        let camper = mk(c, unit.clone(), "CampIdle");
        let ents = c.arr(p.a_len, p.a_arr, vec![e1, camper.clone()]);
        let mode = c.obj(&[(p.pv_units, units), (p.c.cm_ents, ents)]);
        c.key_set(&mode, "pv".into(), V::B(true));
        let ev = c.obj(&[]);
        let game = c.obj(&[
            (p.g_me, me.clone()),
            (p.g_mode, mode.clone()),
            (p.g_event, ev),
        ]);
        let ctrl = c.obj(&[(p.c_game, game.clone()), (p.c_host, host)]);
        c.set(&game, p.g_ctrl, ctrl.clone());
        let act = c.obj(&[
            (p.a_target, elt.clone()),
            (p.a_unit, unit.clone()),
            (p.a_game, game.clone()),
        ]);
        W {
            ctrl,
            game,
            me,
            other,
            mode,
            elt,
            unit,
            parked,
            camper,
            act,
        }
    }

    fn z(code: i32, camp: i32, kind: i32) -> V {
        V::F((code + (camp << 2) + (kind << 3)) as f64)
    }

    /// The player's machine: one ping per event, element uid in a place, unit
    /// uid in the camp; the end only for the activity it started; solo and
    /// coop activities send nothing.
    #[test]
    fn sender_behaviour() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let f = |k: usize| code.functions[n + k].findex;
        let (send, end) = (f(3), f(4));
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let ping = |s: &mut Sim| -> Vec<Vec<V>> { s.c.take("ping") };
        // UnitAction start in a place
        s.run(send, vec![w.game.clone(), w.act.clone(), V::I(0), V::I(0)]);
        assert_eq!(
            ping(&mut s),
            vec![vec![
                w.ctrl.clone(),
                V::F(SENTINEL),
                V::F(77.0),
                z(0, 0, 0),
                w.me.clone()
            ]]
        );
        assert_eq!(
            s.c.take("println"),
            vec![vec![V::S("mp: work send 0 0 0 77".into())]]
        );
        // an Archery shot in the camp: the unit's uid
        s.c.key_set(&w.mode, "pv".into(), V::B(false));
        s.c.key_set(&w.mode, "camp".into(), V::B(true));
        s.run(send, vec![w.game.clone(), w.act.clone(), V::I(1), V::I(1)]);
        assert_eq!(ping(&mut s)[0][2..4], [V::F(9.0), z(1, 1, 1)]);
        s.c.log.clear();
        // the end: only for the started activity, with its kind (0 here)
        let other_act = s.c.obj(&[(p.a_game, w.game.clone())]);
        s.run(end, vec![other_act]);
        assert!(s.c.log.is_empty());
        s.run(end, vec![w.act.clone()]);
        assert_eq!(ping(&mut s)[0][3], z(2, 1, 0));
        s.c.log.clear();
        // forgotten after the end
        s.run(end, vec![w.act.clone()]);
        assert!(s.c.log.is_empty());
        // a replicated (coop) activity, then solo: nothing
        let h = s.c.obj(&[]);
        s.c.set(&w.act, p.a_host, h);
        s.run(send, vec![w.game.clone(), w.act.clone(), V::I(0), V::I(0)]);
        assert!(s.c.log.is_empty());
        s.c.set(&w.act, p.a_host, V::Null);
        s.c.set(&w.ctrl, p.c_host, V::Null);
        s.run(send, vec![w.game.clone(), w.act.clone(), V::I(0), V::I(0)]);
        assert!(s.c.log.is_empty());
    }

    /// The other players: start / hit / end on the parked worker (place) and
    /// on the camp entity; the sender's echo, other pings and unknown uids.
    #[test]
    fn receiver_behaviour() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let f = |k: usize| code.functions[n + k].findex;
        let (idle, recv) = (f(1), f(5));
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let ev = |s: &mut Sim, x: f64, y: f64, zz: V, who: &V| {
            s.run(
                recv,
                vec![w.ctrl.clone(), V::F(x), V::F(y), zz, who.clone()],
            )
        };
        let st = |t: &str| V::S(t.into());
        // a vanilla ping and the forge's go on
        assert_eq!(ev(&mut s, 12.5, 77.0, z(1, 0, 0), &w.other), V::B(false));
        assert_eq!(
            ev(
                &mut s,
                crate::forge_mirror::SENTINEL,
                77.0,
                z(1, 0, 0),
                &w.other
            ),
            V::B(false)
        );
        // the sender's own echo: swallowed, nothing played
        assert_eq!(ev(&mut s, SENTINEL, 77.0, z(1, 0, 0), &w.me), V::B(true));
        assert!(s.c.log.is_empty());

        // place: start remembers the idle, a hit plays the work anim once
        assert_eq!(ev(&mut s, SENTINEL, 77.0, z(0, 0, 0), &w.other), V::B(true));
        assert!(s.c.take("play").is_empty());
        assert_eq!(s.c.take("println"), vec![vec![st("mp: work recv 0 0 0 3")]]);
        ev(&mut s, SENTINEL, 77.0, z(1, 0, 0), &w.other);
        let plays = s.c.take("play");
        assert_eq!(plays.len(), 1);
        assert_eq!(plays[0][..2], [w.parked.clone(), st("Attack")]);
        assert_eq!(s.c.key_get(&plays[0][2], "dloop"), V::B(false));
        let V::Clo(f_end, bound) = s.c.key_get(&plays[0][2], "donEnd") else {
            panic!("onEnd")
        };
        assert_eq!(f_end, idle);
        assert_eq!(s.c.key_get(&bound, "de"), w.parked);
        assert_eq!(s.c.key_get(&bound, "danim"), st("IdlePose"));
        assert_eq!(s.c.take("println"), vec![vec![st("mp: work recv 1 0 0 4")]]);
        // the anim's end and the event's end: the idle, looped
        s.c.set(&w.parked, p.e_anim, st("Attack"));
        s.run(idle, vec![*bound]);
        ev(&mut s, SENTINEL, 77.0, z(2, 0, 1), &w.other);
        let plays = s.c.take("play");
        assert_eq!(plays.len(), 2);
        for pl in &plays {
            assert_eq!(pl[..2], [w.parked.clone(), st("IdlePose")]);
            assert_eq!(s.c.key_get(&pl[2], "dloop"), V::B(true));
        }
        assert_eq!(s.c.take("println"), vec![vec![st("mp: work recv 2 1 0 5")]]);

        // camp: the unit's entry entity (not the first entry)
        s.c.key_set(&w.mode, "pv".into(), V::B(false));
        s.c.key_set(&w.mode, "camp".into(), V::B(true));
        ev(&mut s, SENTINEL, 9.0, z(0, 1, 0), &w.other);
        ev(&mut s, SENTINEL, 9.0, z(1, 1, 0), &w.other);
        let plays = s.c.take("play");
        assert_eq!(plays[0][..2], [w.camper.clone(), st("Attack")]);
        let bound = s.c.key_get(&plays[0][2], "donEnd");
        let V::Clo(_, bound) = bound else { panic!() };
        s.run(idle, vec![*bound]);
        assert_eq!(s.c.take("play")[0][1], st("CampIdle"));
        s.c.log.clear();
        // an unknown uid, or a place uid while in the camp: nothing, stage 1
        for (y, camp) in [(5.0, 1), (77.0, 0)] {
            ev(&mut s, SENTINEL, y, z(1, camp, 0), &w.other);
            assert!(s.c.take("play").is_empty());
            assert_eq!(
                s.c.take("println"),
                vec![vec![st(&format!("mp: work recv 1 0 {camp} 1"))]]
            );
        }
        let _ = (&w.elt, &w.unit);
    }

    /// Two workers at once each get their own idle back (a single shared one
    /// gave the first worker the second one's); after its end a worker's idle
    /// is taken afresh, not left over from the previous activity.
    #[test]
    fn idle_is_per_worker() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let f = |k: usize| code.functions[n + k].findex;
        let (idle, recv) = (f(1), f(5));
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let st = |t: &str| V::S(t.into());
        // a second worker, parked at element uid 78
        let c = &mut s.c;
        let elt2 = c.obj(&[(p.e_uid, V::I(78))]);
        c.put("refs", "78", elt2.clone());
        let wobj = c.get(&w.parked, p.e_obj);
        let parked2 = c.obj(&[(p.e_obj, wobj), (p.e_anim, st("IdleB"))]);
        let ed = c.obj(&[]);
        c.key_set(&ed, "dunitView".into(), parked2.clone());
        let units = c.get(&w.mode, p.pv_units);
        let V::O(ek) = elt2 else { unreachable!() };
        c.key_set(&units, format!("k{ek}"), ed);
        let ev = |s: &mut Sim, y: f64, code: i32| {
            s.run(
                recv,
                vec![
                    w.ctrl.clone(),
                    V::F(SENTINEL),
                    V::F(y),
                    z(code, 0, 0),
                    w.other.clone(),
                ],
            );
        };
        let hit_idle = |s: &mut Sim, y: f64| -> V {
            ev(s, y, 1);
            let plays = s.c.take("play");
            let V::Clo(_, o) = s.c.key_get(&plays[0][2], "donEnd") else {
                panic!("onEnd")
            };
            s.run(idle, vec![*o]);
            s.c.take("play")[0][1].clone()
        };
        ev(&mut s, 77.0, 0);
        ev(&mut s, 78.0, 0);
        assert_eq!(hit_idle(&mut s, 77.0), st("IdlePose"));
        assert_eq!(hit_idle(&mut s, 78.0), st("IdleB"));
        // 77 ends; its next activity, start missed, takes its anim then
        ev(&mut s, 77.0, 2);
        assert_eq!(s.c.take("play")[0][1], st("IdlePose"));
        s.c.set(&w.parked, p.e_anim, st("Sit"));
        assert_eq!(hit_idle(&mut s, 77.0), st("Sit"));
        assert_eq!(hit_idle(&mut s, 78.0), st("IdleB"));
    }
}
