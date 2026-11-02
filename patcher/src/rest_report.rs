// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: every player reads the camp night report (ui.win.RestReport) and
// closes it on their own. The first close, by any player, starts the night's
// events once; a later close only removes that player's copy.
//
// Vanilla: the host builds the report (CampFire._onRestEnd) with `onEnd` (the
// sleep events and the after-rest step) and shares it; its Continue button
// waits for every player (`closeBtn.set_waitAllPlayers(true)`), a client's
// click goes to the host (Element.netButton -> Window.triggerClick) and the
// host's close (`onEnd(); remove`) removes every copy (Window.onRemove ->
// dispatchRemove). The host's Escape closes it for everyone at once.
//
// The patch:
//   init: Continue no longer waits for every player (`true` -> `false`; solo
//     has one player anyway); at its end `rrInit(this)`: on a co-op client
//     `this.__host = null`, so its copy is local (Continue, Escape and a click
//     outside close it there; the host's later unshare finds nothing to do).
//   close: `if (rrClose(this))` skip `onEnd()` and go on with the removal.
//     rrClose, co-op only: the host runs `onEnd` once (`onEnd` is nulled
//     first); a client sends the ping RPC with this pass's sentinel x
//     (mirror.rs: any player -> host -> every machine), whose receiver
//     `rrRecv` runs on the host the same once-only `onEnd` of its report
//     (the one in globalUI.windows whose `__uid & 0xFFFFFF` is the ping's y; nothing
//     when the host already closed it), then unshares it: a late joiner gets no
//     ended report
//     and swallows the ping everywhere.
//   Window.onRemove: no `dispatchRemove()` for a RestReport (each copy is
//     closed by its own player); `netSharing(false)` stays, so a late joiner
//     gets no stale report.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::mirror;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

/// Ping x of this pass (exact as f32, distinct from the mirrors' sentinels).
pub(crate) const SENTINEL: f64 = -1_000_000_128.0;

struct Plan {
    b: mirror::Base,
    rr_t: RefType,
    rr_cls: (RefGlobal, RefType),
    init_fi: usize,
    /// init op of `Bool true` handed to set_waitAllPlayers; its Ret.
    wait_at: usize,
    init_ret: usize,
    close_fi: usize,
    remove_fi: usize,
    /// Window.onRemove op of `Call1 dispatchRemove(this)`.
    dispatch_at: usize,
    on_end: (RefField, RefType),
    w_host: (RefField, RefType),
    /// Window.__uid (the report's id in the ping, low 24 bits: exact as f32)
    /// and Window.netSharing (the host unshares a report once it has ended).
    w_uid: RefField,
    net_sharing: RefFun,
    g_cls: (RefGlobal, RefType),
    g_inst: RefField,
    g_auth: RefField,
    g_gui: (RefField, RefType),
    u_windows: RefField,
    is_multi: RefFun,
    is_of_type: RefFun,
    dbg: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let b = mirror::base(code)?;
    let win_t = obj_type(code, "ui.Window")?;
    let rr_t = obj_type(code, "ui.win.RestReport")?;
    if !is_sub(code, rr_t, win_t) {
        bail!("ui.win.RestReport is not a ui.Window");
    }
    let rr_cls = class_global(code, "ui.win.RestReport")?;
    let game_t = b.game_t;
    let g_cls = class_global(code, "Game")?;
    let g_inst = typed(code, g_cls.1, "inst", game_t)?;
    let g_auth = typed(code, game_t, "isAuth", b.bool_)?;
    let g_gui = field(code, game_t, "globalUI")?;
    let u_windows = typed(code, obj_type(code, "ui.BaseUI")?, "windows", b.arr_t)?;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], b.bool_) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let is_of_type = crate::diag::static_fn(code, "$Std", "isOfType")?.findex;
    if sig(code, is_of_type)? != (vec![b.dyn_t, b.dyn_t], b.bool_) {
        bail!("unexpected Std.isOfType signature");
    }
    let on_end = field(code, rr_t, "onEnd")?;
    let w_host = field(code, win_t, "__host")?;
    let w_uid = typed(code, win_t, "__uid", b.i32_)?;
    let net_sharing = method(code, win_t, "netSharing")?.findex;
    if sig(code, net_sharing)? != (vec![win_t, b.bool_], b.void_) {
        bail!("unexpected Window.netSharing signature");
    }
    // init: `Bool true; Call2 set_waitAllPlayers(closeBtn, true)`, one Ret at the end
    let button_t = obj_type(code, "ui.comp.Button")?;
    let set_wait = method(code, button_t, "set_waitAllPlayers")?.findex;
    let init = method(code, rr_t, "init")?;
    let o = &init.ops;
    let calls: Vec<usize> = (1..o.len())
        .filter(|&i| matches!(o[i], Opcode::Call2 { fun, .. } if fun == set_wait))
        .collect();
    let [call] = calls[..] else {
        bail!(
            "RestReport.init: {} set_waitAllPlayers calls (want 1)",
            calls.len()
        );
    };
    let wait_at = call - 1;
    match (&o[wait_at], &o[call]) {
        (Opcode::Bool { dst, value }, Opcode::Call2 { arg1, .. }) if dst == arg1 => {
            if !value.0 {
                bail!("RestReport.init: waitAllPlayers already false (applied?)");
            }
        }
        _ => bail!("RestReport.init: set_waitAllPlayers argument is not a Bool constant"),
    }
    let rets: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::Ret { .. }))
        .collect();
    if rets != [o.len() - 1] {
        bail!("RestReport.init: not a single Ret at the end");
    }
    // close: `GetThis onEnd; NullCheck; CallClosure`, then the removal
    let close = method(code, rr_t, "close")?;
    let c = &close.ops;
    if c.len() < 4
        || !matches!(c[0], Opcode::GetThis { field, .. } if field == on_end.0)
        || !matches!(c[1], Opcode::NullCheck { .. })
        || !matches!(c[2], Opcode::CallClosure { .. })
    {
        bail!("RestReport.close: does not start with onEnd() (already applied?)");
    }
    // Window.onRemove: `Call1 dispatchRemove(this)` (host, shared window)
    let dispatch = method(code, win_t, "dispatchRemove")?.findex;
    let rm = method(code, win_t, "onRemove")?;
    let r = &rm.ops;
    let hits: Vec<usize> = (0..r.len())
        .filter(|&i| matches!(r[i], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == dispatch))
        .collect();
    let [dispatch_at] = hits[..] else {
        bail!(
            "Window.onRemove: {} dispatchRemove calls (want 1)",
            hits.len()
        );
    };
    if dispatch_at < 2 || matches!(r[dispatch_at - 1], Opcode::JTrue { .. }) {
        bail!("Window.onRemove: dispatchRemove already guarded (applied?)");
    }
    Ok(Plan {
        rr_t,
        rr_cls,
        init_fi: fun_index(code, init.findex)?,
        wait_at,
        init_ret: o.len() - 1,
        close_fi: fun_index(code, close.findex)?,
        remove_fi: fun_index(code, rm.findex)?,
        dispatch_at,
        on_end,
        w_host,
        w_uid,
        net_sharing,
        g_cls,
        g_inst,
        g_auth,
        g_gui,
        u_windows,
        is_multi,
        is_of_type,
        dbg: debug_file(code, "src/ui/win/RestReport.hx")?,
        b,
    })
}

/// `game` = Game.inst; jumps to `out` when null or not co-op.
fn multi_game(a: &mut Asm, p: &Plan, gc: Reg, g: Reg, b: Reg, out: &'static str) {
    a.op(Opcode::GetGlobal {
        dst: gc,
        global: p.g_cls.0,
    });
    a.op(Opcode::Field {
        dst: g,
        obj: gc,
        field: p.g_inst,
    });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, out);
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: g,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, out);
}

/// Runs `w.onEnd` once: nulled before the call. Falls through.
fn end_once(a: &mut Asm, p: &Plan, w: Reg, cb: Reg, nul: Reg, v: Reg, done: &'static str) {
    a.op(Opcode::Field {
        dst: cb,
        obj: w,
        field: p.on_end.0,
    });
    a.jmp(Opcode::JNull { reg: cb, offset: 0 }, done);
    a.op(Opcode::Null { dst: nul });
    a.op(Opcode::SetField {
        obj: w,
        field: p.on_end.0,
        src: nul,
    });
    a.op(Opcode::CallClosure {
        dst: v,
        fun: cb,
        args: vec![],
    });
    a.label(done);
}

/// `yf` = the report's id as sent in the ping: `w.__uid & 0xFFFFFF` (exact as f32).
fn uid_f(code: &mut Bytecode, a: &mut Asm, p: &Plan, w: Reg, k: Reg, m: Reg, yf: Reg) {
    let mask = int_const(code, 0xFF_FFFF);
    a.op(Opcode::Field {
        dst: k,
        obj: w,
        field: p.w_uid,
    });
    a.op(Opcode::Int { dst: m, ptr: mask });
    a.op(Opcode::And { dst: k, a: k, b: m });
    a.op(Opcode::ToSFloat { dst: yf, src: k });
}

/// `rrInit(w)`: a co-op client's copy becomes local (`__host = null`).
fn add_init(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let b = &p.b;
    let mut r = Regs(vec![p.rr_t]);
    let (gc, g, t, h, v) = (
        r.r(p.g_cls.1),
        r.r(b.game_t),
        r.r(b.bool_),
        r.r(p.w_host.1),
        r.r(b.void_),
    );
    let mut a = Asm::new();
    multi_game(&mut a, p, gc, g, t, "out");
    a.op(Opcode::Field {
        dst: t,
        obj: g,
        field: p.g_auth,
    });
    a.jmp(Opcode::JTrue { cond: t, offset: 0 }, "out");
    a.op(Opcode::Null { dst: h });
    a.op(Opcode::SetField {
        obj: Reg(0),
        field: p.w_host.0,
        src: h,
    });
    a.label("out");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.rr_t], b.void_, r.0, a.finish(), p.dbg)
}

/// `rrClose(w) -> Bool`: false in solo (vanilla onEnd + removal); in co-op the
/// host runs onEnd once, a client asks the host through the ping; true.
fn add_close(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let b = &p.b;
    let sent = float_const(code, SENTINEL);
    let zero = float_const(code, 0.0);
    let mut r = Regs(vec![p.rr_t]);
    let (gc, g, t, cb, v, ctrl, xf, zf, me) = (
        r.r(p.g_cls.1),
        r.r(b.game_t),
        r.r(b.bool_),
        r.r(p.on_end.1),
        r.r(b.void_),
        r.r(b.ctrl_t),
        r.r(b.f64_),
        r.r(b.f64_),
        r.r(b.player_t),
    );
    let (nul, k, m, yf) = (r.r(p.on_end.1), r.r(b.i32_), r.r(b.i32_), r.r(b.f64_));
    let mut a = Asm::new();
    multi_game(&mut a, p, gc, g, t, "solo");
    a.op(Opcode::Field {
        dst: t,
        obj: g,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: t, offset: 0 }, "client");
    end_once(&mut a, p, Reg(0), cb, nul, v, "done");
    a.jmp(Opcode::JAlways { offset: 0 }, "yes");
    a.label("client");
    a.op(Opcode::Field {
        dst: ctrl,
        obj: g,
        field: b.g_ctrl,
    });
    a.jmp(
        Opcode::JNull {
            reg: ctrl,
            offset: 0,
        },
        "yes",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: g,
        field: b.g_me,
    });
    a.op(Opcode::Float { dst: xf, ptr: sent });
    a.op(Opcode::Float { dst: zf, ptr: zero });
    uid_f(code, &mut a, p, Reg(0), k, m, yf);
    a.op(Opcode::CallN {
        dst: v,
        fun: b.ping,
        args: vec![ctrl, xf, yf, zf, me],
    });
    a.label("yes");
    a.op(Opcode::Bool {
        dst: t,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: t });
    a.label("solo");
    a.op(Opcode::Bool {
        dst: t,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: t });
    push_fn(code, vec![p.rr_t], b.bool_, r.0, a.finish(), p.dbg)
}

/// `rrRecv(ctrl, x, y, z, player) -> Bool`: false unless x is SENTINEL; the
/// host runs the once-only onEnd of its open RestReport.
fn add_recv(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let b = &p.b;
    let sent = float_const(code, SENTINEL);
    let k0 = int_const(code, 0);
    let mut r = Regs(vec![b.ctrl_t, b.f64_, b.f64_, b.f64_, b.player_t]);
    let (res, fs, g, t, gui, lst, raw, n, i, d, w, cls, cb, nul, v) = (
        r.r(b.bool_),
        r.r(b.f64_),
        r.r(b.game_t),
        r.r(b.bool_),
        r.r(p.g_gui.1),
        r.r(b.arr_t),
        r.r(b.raw_arr_t),
        r.r(b.i32_),
        r.r(b.i32_),
        r.r(b.dyn_t),
        r.r(p.rr_t),
        r.r(p.rr_cls.1),
        r.r(p.on_end.1),
        r.r(p.on_end.1),
        r.r(b.void_),
    );
    let (k, m, yf, f) = (r.r(b.i32_), r.r(b.i32_), r.r(b.f64_), r.r(b.bool_));
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.op(Opcode::Float { dst: fs, ptr: sent });
    a.jmp(
        Opcode::JNotEq {
            a: Reg(1),
            b: fs,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.op(Opcode::Field {
        dst: g,
        obj: Reg(0),
        field: b.c_game,
    });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: t,
        obj: g,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: t, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: gui,
        obj: g,
        field: p.g_gui.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: lst,
        obj: gui,
        field: p.u_windows,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: lst,
        field: b.a_len,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: lst,
        field: b.a_arr,
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.rr_cls.0,
    });
    a.op(Opcode::Int { dst: i, ptr: k0 });
    a.loop_head("loop");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::Call2 {
        dst: t,
        fun: p.is_of_type,
        arg0: d,
        arg1: cls,
    });
    a.jmp(Opcode::JFalse { cond: t, offset: 0 }, "loop");
    a.op(Opcode::SafeCast { dst: w, src: d });
    // the report the closing client had (a stale one of an earlier night does not match)
    uid_f(code, &mut a, p, w, k, m, yf);
    a.jmp(
        Opcode::JNotEq {
            a: yf,
            b: Reg(2),
            offset: 0,
        },
        "loop",
    );
    // already ended (an earlier close): nothing more to do
    a.op(Opcode::Field {
        dst: cb,
        obj: w,
        field: p.on_end.0,
    });
    a.jmp(Opcode::JNull { reg: cb, offset: 0 }, "ret");
    end_once(&mut a, p, w, cb, nul, v, "ended");
    // ended: a late joiner gets no copy of it (the open ones stay)
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.net_sharing,
        arg0: w,
        arg1: f,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: res });
    push_fn(
        code,
        vec![b.ctrl_t, b.f64_, b.f64_, b.f64_, b.player_t],
        b.bool_,
        r.0,
        a.finish(),
        p.dbg,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let init = add_init(code, p)?;
    let close = add_close(code, p)?;
    let recv = add_recv(code, p)?;
    let (void_, bool_) = (p.b.void_, p.b.bool_);
    // init: Continue waits for nobody; rrInit(this) before the Ret
    let f = &mut code.functions[p.init_fi];
    if let Opcode::Bool { dst, .. } = f.ops[p.wait_at] {
        f.ops[p.wait_at] = Opcode::Bool {
            dst,
            value: ValBool(false),
        };
    }
    f.regs.push(void_);
    let v = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        p.init_ret,
        vec![Opcode::Call1 {
            dst: v,
            fun: init,
            arg0: Reg(0),
        }],
    );
    // close: if (rrClose(this)) skip onEnd() (ops 0..2, now 2..4)
    let f = &mut code.functions[p.close_fi];
    f.regs.push(bool_);
    let t = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Call1 {
                dst: t,
                fun: close,
                arg0: Reg(0),
            },
            Opcode::JTrue { cond: t, offset: 3 },
        ],
    );
    // Window.onRemove: no dispatchRemove for a RestReport
    let f = &mut code.functions[p.remove_fi];
    f.regs.extend([p.rr_cls.1, bool_]);
    let (c, t) = (
        Reg((f.regs.len() - 2) as u32),
        Reg((f.regs.len() - 1) as u32),
    );
    insert_ops(
        f,
        p.dispatch_at,
        vec![
            Opcode::GetGlobal {
                dst: c,
                global: p.rr_cls.0,
            },
            Opcode::Call2 {
                dst: t,
                fun: p.is_of_type,
                arg0: Reg(0),
                arg1: c,
            },
            Opcode::JTrue { cond: t, offset: 1 },
        ],
    );
    mirror::hook_ping(code, &p.b, recv);
    eprintln!(
        "patched rest report: init fn@{} (Continue per player, rrInit fn@{}), close -> rrClose fn@{}, Window.onRemove keeps other copies, ping -> rrRecv fn@{}",
        code.functions[p.init_fi].findex.0, init.0, close.0, recv.0
    );
    Ok(())
}

/// Makes the co-op night report per player, or leaves `code` untouched and
/// logs why.
pub(crate) fn patch_rest_report(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => {
            let snap = crate::asm::Snap::take(code);
            if let Err(e) = apply(code, &p) {
                snap.restore(code);
                crate::skipped(format!("rest report skipped: {e:#}"));
            }
        }
        Err(e) => crate::skipped(format!("rest report skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_rest_report(&mut code);
        let once = write(&code);
        let back = read(&once);
        let touched = [p.init_fi, p.close_fi, p.remove_fi, p.b.impl_fi];
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 3);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, !touched.contains(&i), "function #{i}");
        }
        for &fi in &touched {
            check_types(&back, &back.functions[fi], 0..back.functions[fi].ops.len());
            check_flow(&back.functions[fi]);
        }
        for f in &back.functions[nf..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // close skips exactly the vanilla onEnd() call
        assert_eq!(jump_targets(&back.functions[p.close_fi], 1), [5]);
        // onRemove skips exactly dispatchRemove
        let rm = &back.functions[p.remove_fi];
        assert_eq!(jump_targets(rm, p.dispatch_at + 2), [p.dispatch_at + 4]);
        // Idempotent.
        let mut again = read(&once);
        assert!(plan(&again).is_err());
        patch_rest_report(&mut again);
        assert!(write(&again) == once);
    }
}
