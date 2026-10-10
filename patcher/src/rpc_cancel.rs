// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A remote call the host cancels still answers its caller: the waiting result
// callback runs with default values, so whatever the caller holds until the
// answer (a battle skill request's lock) is let go.
//
// Vanilla hxbit (NetworkHost.hx, NetworkClient.processMessage):
//
//   case RPC_WITH_RESULT (host):
//       resultID = ctx.getInt(); o = refs[ctx.getUID()]; ...
//       if (o == null) {                       // object unknown to the host
//           if (!host.isAuth) logError(...);    // the host itself logs nothing
//           skip the call's bytes;
//           ctx.addByte(CANCEL_RPC = 12); ctx.addInt(resultID);
//       }
//   case RPC_RESULT (caller):
//       id = ctx.getInt(); cb = host.rpcWaits.get(id);
//       host.rpcWaits.remove(id); host.sendRate(); cb(ctx);
//   case CANCEL_RPC (caller):
//       id = ctx.getInt(); host.rpcWaits.remove(id);   // cb dropped, never run
//
// The game's only call with a result in battle is the guest's skill request:
// Battle.executeSkill does `lockCounter++` and sends
// Controller.battleExecuteSkill with `(ok) -> lockCounter--` as its result
// callback. A cancel leaves the guest's lockCounter above 0 for the rest of
// the battle: Battle.isLocked() stays true, so its clicks (onOverlayEvent),
// End Turn (canEndTurn) and damage previews all stop, while the host and the
// other players carry on. Nothing is logged on either side.
//
// The fix, for that call only (other result callbacks are not written for a
// default value: one decodes a null and throws half way):
//
//   S. Controller.battleExecuteSkill (the generated sender), right after its
//      beginRPC: `skillRpcs.set(host.rpcUID - 1, true)` (a new IntMap global,
//      replaced by a fresh one whenever the host differs from the one it was
//      made for, a second new global; beginRPC took the old rpcUID as the
//      call id and incremented it, and ids are unique per host). One entry
//      per request, so overlapping requests are told apart; an answered
//      request leaves its small entry behind. Game.dispose clears both.
//   C. At the cancel site (after `id = ctx.getInt()`), one call:
//
//   rpcCancelled(client, ctx, id):
//       if (client.host != skillHost || skillRpcs?.get(id) == null) return;
//       skillRpcs.remove(id);
//       h = client.host; w = h?.rpcWaits; cb = w?.get(id); if (cb == null) return;
//       save ctx.input / ctx.inPos; ctx.input = Bytes.alloc(256) (zeroes); ctx.inPos = 0;
//       try cb(ctx) catch (_) {}      // reads its result from the zeroes:
//                                     // false / 0 / null, as a refused call
//       restore ctx.input / ctx.inPos;
//       Sys.println("mp: net: RPC result #<id> (battle skill request) cancelled by the host; its callback ran as refused")
//
// then vanilla removes the entry. The skill request's result wrapper reads one
// byte (0 = false) and checks ctx.error; its callback only does lockCounter--.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;

const MSG_A: &str = "mp: net: RPC result #";
const MSG_B: &str = " (battle skill request) cancelled by the host; its callback ran as refused";
const ZEROES: i32 = 256;

struct Plan {
    /// NetworkClient.processMessage and the op right after the cancel's getInt.
    pm_fi: usize,
    at: usize,
    ctx: Reg,
    id: Reg,
    client_t: RefType,
    ser_t: RefType,
    host_t: RefType,
    map_t: RefType,
    bytes_t: RefType,
    cb_t: RefType,
    dyn_t: RefType,
    i32_t: RefType,
    str_t: RefType,
    void_t: RefType,
    c_host: RefField,
    h_waits: RefField,
    s_input: RefField,
    s_in_pos: RefField,
    map_get: RefFun,
    map_set: RefFun,
    map_remove: RefFun,
    map_new: RefFun,
    bool_t: RefType,
    alloc: RefFun,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    dbg_file: usize,
    /// battleExecuteSkill, the op right after its beginRPC and the host register.
    send_fi: usize,
    send_at: usize,
    send_host: Reg,
    /// Game.dispose (the two new globals are cleared there).
    dispose_fi: usize,
    h_uid: RefField,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let str_t = obj_type(code, "String")?;
    let client_t = obj_type(code, "hxbit.NetworkClient")?;
    let host_t = obj_type(code, "hxbit.NetworkHost")?;
    let ser_t = obj_type(code, "hxbit.NetworkSerializer")?;
    let map_t = obj_type(code, "haxe.ds.IntMap")?;
    let bytes_t = obj_type(code, "haxe.io.Bytes")?;
    let c_host = typed(code, client_t, "host", host_t)?;
    let h_waits = typed(code, host_t, "rpcWaits", map_t)?;
    let s_input = typed(code, ser_t, "input", bytes_t)?;
    let s_in_pos = typed(code, ser_t, "inPos", i32_t)?;
    let alloc = static_fn(code, "haxe.io.$Bytes", "alloc")?;
    if fun_args(code, alloc) != [i32_t] || alloc.t.as_fun(code).map(|t| t.ret) != Some(bytes_t) {
        bail!("Bytes.alloc: unexpected signature");
    }
    let println = static_fn(code, "$Sys", "println")?;
    let std_string = static_fn(code, "$Std", "string")?;
    if fun_args(code, println) != [dyn_t] || fun_args(code, std_string) != [dyn_t] {
        bail!("Sys.println / Std.string do not take one Dyn");
    }
    let str_add = static_fn(code, "$String", "__add__")?.findex;

    let ctrl_t = obj_type(code, "st.Controller")?;
    let h_uid = typed(code, host_t, "rpcUID", i32_t)?;
    let send = method(code, ctrl_t, "battleExecuteSkill")?;
    let begins: Vec<(usize, Reg)> = send
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::CallN { fun, args, .. }
                if fname(code, *fun) == "beginRPC"
                    && args
                        .first()
                        .is_some_and(|r| send.regs[r.0 as usize] == host_t) =>
            {
                Some((i + 1, args[0]))
            }
            _ => None,
        })
        .collect();
    let [(send_at, send_host)] = begins[..] else {
        bail!(
            "battleExecuteSkill: {} beginRPC calls, want 1",
            begins.len()
        );
    };
    if matches!(send.ops.get(send_at), Some(Opcode::GetGlobal { .. })) {
        bail!("battleExecuteSkill: already patched");
    }
    if (0..send.ops.len()).any(|i| jump_targets(send, i).contains(&send_at)) {
        bail!("battleExecuteSkill: a jump targets the op after beginRPC");
    }
    let send_fi = fun_index(code, send.findex)?;

    let pm = method(code, client_t, "processMessage")?;
    let ser_reg = |r: &Reg| pm.regs[r.0 as usize] == ser_t;
    // getInt(ctx) -> id, then `this.host.rpcWaits.get|remove(id)`.
    let waits_call = |i: usize, id: Reg| -> Option<RefFun> {
        let w = pm.ops.get(i..i + 4)?;
        match (&w[0], &w[2], &w[3]) {
            (
                Opcode::GetThis { field, .. },
                Opcode::Field {
                    field: wf, dst: wr, ..
                },
                Opcode::NullCheck { .. },
            ) if *field == c_host && *wf == h_waits => match pm.ops.get(i + 4)? {
                Opcode::Call2 {
                    fun, arg0, arg1, ..
                } if arg0 == wr && *arg1 == id => Some(*fun),
                _ => None,
            },
            _ => None,
        }
    };
    let mut cancels = vec![];
    let mut results = vec![];
    for (i, op) in pm.ops.iter().enumerate() {
        let Opcode::Call1 { dst, fun, arg0 } = op else {
            continue;
        };
        if fname(code, *fun) != "getInt" || !ser_reg(arg0) {
            continue;
        }
        match waits_call(i + 1, *dst) {
            Some(f) if fname(code, f) == "remove" => cancels.push((i + 1, *arg0, *dst)),
            Some(f) if fname(code, f) == "get" => results.push(f),
            _ => {}
        }
    }
    let [(at, ctx, id)] = cancels[..] else {
        bail!(
            "processMessage: {} rpcWaits.remove sites right after getInt, want 1 (already patched?)",
            cancels.len()
        );
    };
    let map_set = proto(code, map_t, "set")?;
    let map_remove = proto(code, map_t, "remove")?;
    let map_new = method(code, map_t, "__constructor__")?.findex;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    if sig(code, map_set)? != (vec![map_t, i32_t, dyn_t], void_t)
        || sig(code, map_remove)?.0 != [map_t, i32_t]
        || sig(code, map_new)? != (vec![map_t], void_t)
    {
        bail!("IntMap set / remove / constructor: unexpected signatures");
    }
    let [map_get] = results[..] else {
        bail!("processMessage: {} RPC result sites, want 1", results.len());
    };
    let (margs, mret) = sig(code, map_get)?;
    if margs != [map_t, i32_t] || mret != dyn_t {
        bail!("IntMap.get: unexpected signature");
    }
    if (0..pm.ops.len()).any(|i| jump_targets(pm, i).contains(&at)) {
        bail!("processMessage: a jump targets the cancel's rpcWaits read");
    }
    // The result branch's callback type: get(id) is cast to it before the call.
    let cb_t = pm
        .regs
        .iter()
        .copied()
        .find(|t| matches!(&code.types[t.0], Type::Fun(f) if f.args == [ser_t] && f.ret == void_t))
        .context("processMessage: no (NetworkSerializer) -> Void register")?;
    Ok(Plan {
        pm_fi: fun_index(code, pm.findex)?,
        at,
        ctx,
        id,
        client_t,
        ser_t,
        host_t,
        map_t,
        bytes_t,
        cb_t,
        dyn_t,
        i32_t,
        str_t,
        void_t,
        c_host,
        h_waits,
        s_input,
        s_in_pos,
        map_get,
        map_set,
        map_remove,
        map_new,
        bool_t,
        alloc: alloc.findex,
        println: println.findex,
        std_string: std_string.findex,
        str_add,
        dbg_file: debug_file(code, "hxbit/NetworkHost.hx")?,
        send_fi,
        send_at,
        send_host,
        h_uid,
        dispose_fi: game_dispose_fi(code)?,
    })
}

/// rpcCancelled(client, ctx, id): runs the waiting result callback on zeroes.
fn add_settle(
    code: &mut Bytecode,
    p: &Plan,
    pending: RefGlobal,
    owner: RefGlobal,
) -> Result<RefFun> {
    let (ga, gb) = (
        str_global(code, p.str_t, MSG_A),
        str_global(code, p.str_t, MSG_B),
    );
    let mut r = Regs(vec![]);
    let (c, ctx, id) = (r.r(p.client_t), r.r(p.ser_t), r.r(p.i32_t));
    let v = r.r(p.void_t);
    let h = r.r(p.host_t);
    let w = r.r(p.map_t);
    let d = r.r(p.dyn_t);
    let cb = r.r(p.cb_t);
    let inp = r.r(p.bytes_t);
    let zb = r.r(p.bytes_t);
    let pos = r.r(p.i32_t);
    let n = r.r(p.i32_t);
    let e = r.r(p.dyn_t);
    let s = r.r(p.str_t);
    let s2 = r.r(p.str_t);
    let k = r.r(p.bool_t);
    let h2 = r.r(p.host_t);
    let mut a = Asm::new();
    // Only a battle skill request battleExecuteSkill noted for this host.
    a.op(Opcode::Field {
        dst: h,
        obj: c,
        field: p.c_host,
    });
    a.jmp(Opcode::JNull { reg: h, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: h2,
        global: owner,
    });
    a.jmp(
        Opcode::JNotEq {
            a: h,
            b: h2,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: w,
        global: pending,
    });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "end");
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.map_get,
        arg0: w,
        arg1: id,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "end");
    a.op(Opcode::Call2 {
        dst: k,
        fun: p.map_remove,
        arg0: w,
        arg1: id,
    });
    a.op(Opcode::Field {
        dst: h,
        obj: c,
        field: p.c_host,
    });
    a.jmp(Opcode::JNull { reg: h, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: w,
        obj: h,
        field: p.h_waits,
    });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "end");
    a.jmp(
        Opcode::JNull {
            reg: ctx,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.map_get,
        arg0: w,
        arg1: id,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "end");
    a.op(Opcode::UnsafeCast { dst: cb, src: d });
    a.op(Opcode::Field {
        dst: inp,
        obj: ctx,
        field: p.s_input,
    });
    a.op(Opcode::Field {
        dst: pos,
        obj: ctx,
        field: p.s_in_pos,
    });
    a.op(Opcode::Int {
        dst: n,
        ptr: int_const(code, ZEROES),
    });
    a.op(Opcode::Call1 {
        dst: zb,
        fun: p.alloc,
        arg0: n,
    });
    a.op(Opcode::SetField {
        obj: ctx,
        field: p.s_input,
        src: zb,
    });
    a.op(Opcode::Int {
        dst: n,
        ptr: int_const(code, 0),
    });
    a.op(Opcode::SetField {
        obj: ctx,
        field: p.s_in_pos,
        src: n,
    });
    a.jmp(Opcode::Trap { exc: e, offset: 0 }, "caught");
    a.op(Opcode::CallClosure {
        dst: v,
        fun: cb,
        args: vec![ctx],
    });
    a.op(Opcode::EndTrap { exc: e });
    a.loop_head("done");
    a.op(Opcode::SetField {
        obj: ctx,
        field: p.s_input,
        src: inp,
    });
    a.op(Opcode::SetField {
        obj: ctx,
        field: p.s_in_pos,
        src: pos,
    });
    a.op(Opcode::GetGlobal { dst: s, global: ga });
    a.op(Opcode::ToDyn { dst: d, src: id });
    a.op(Opcode::Call1 {
        dst: s2,
        fun: p.std_string,
        arg0: d,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: p.str_add,
        arg0: s,
        arg1: s2,
    });
    a.op(Opcode::GetGlobal {
        dst: s2,
        global: gb,
    });
    a.op(Opcode::Call2 {
        dst: s,
        fun: p.str_add,
        arg0: s,
        arg1: s2,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    // The callback threw: ctx is restored all the same (no rethrow: the
    // cancel is consumed either way, as vanilla consumes it).
    a.label("caught");
    a.jmp(Opcode::JAlways { offset: 0 }, "done");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.client_t, p.ser_t, p.i32_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let pending = add_global(code, p.map_t);
    let owner = add_global(code, p.host_t);
    let settle = add_settle(code, p, pending, owner)?;
    forget_on_game_dispose(code, p.dispose_fi, &[pending, owner]);
    let one = int_const(code, 1);
    let f = &mut code.functions[p.send_fi];
    let (hh, m, u, k, b, d, v) = (
        new_reg(f, p.host_t),
        new_reg(f, p.map_t),
        new_reg(f, p.i32_t),
        new_reg(f, p.i32_t),
        new_reg(f, p.bool_t),
        new_reg(f, p.dyn_t),
        new_reg(f, p.void_t),
    );
    insert_ops(
        f,
        p.send_at,
        vec![
            // A new map for each host: ids are per host (its rpcUID).
            Opcode::GetGlobal {
                dst: hh,
                global: owner,
            },
            Opcode::JEq {
                a: hh,
                b: p.send_host,
                offset: 4,
            },
            Opcode::New { dst: m },
            Opcode::Call1 {
                dst: v,
                fun: p.map_new,
                arg0: m,
            },
            Opcode::SetGlobal {
                global: pending,
                src: m,
            },
            Opcode::SetGlobal {
                global: owner,
                src: p.send_host,
            },
            Opcode::GetGlobal {
                dst: m,
                global: pending,
            },
            Opcode::Field {
                dst: u,
                obj: p.send_host,
                field: p.h_uid,
            },
            Opcode::Int { dst: k, ptr: one },
            Opcode::Sub { dst: u, a: u, b: k },
            Opcode::Bool {
                dst: b,
                value: hlbc::types::ValBool(true),
            },
            Opcode::ToDyn { dst: d, src: b },
            Opcode::Call3 {
                dst: v,
                fun: p.map_set,
                arg0: m,
                arg1: u,
                arg2: d,
            },
        ],
    );
    let send = f.findex.0;
    let f = &mut code.functions[p.pm_fi];
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.at,
        vec![Opcode::CallN {
            dst: v,
            fun: settle,
            args: vec![Reg(0), p.ctx, p.id],
        }],
    );
    eprintln!(
        "patched rpc cancel fn@{} op {}: a battle skill request the host cancels runs its callback as refused (battleExecuteSkill fn@{send} op {}, rpcCancelled fn@{})",
        f.findex.0, p.at, p.send_at, settle.0
    );
    Ok(())
}

/// Makes a cancelled result call settle its callback, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_rpc_cancel(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("rpc cancel skipped: {e:#}"));
            return;
        }
    };
    let snap = crate::asm::Snap::take(code);
    let saved = code.functions[p.pm_fi].clone();
    let saved_send = code.functions[p.send_fi].clone();
    let saved_dispose = code.functions[p.dispose_fi].clone();
    if let Err(e) = apply(code, &p) {
        snap.restore(code);
        code.functions[p.pm_fi] = saved;
        code.functions[p.send_fi] = saved_send;
        code.functions[p.dispose_fi] = saved_dispose;
        crate::skipped(format!("rpc cancel skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Ops inserted after battleExecuteSkill's beginRPC.
    const SEND_OPS: usize = 13;

    /// On the installed game, with and without net_guard first: only
    /// processMessage changes (one call after the cancel's getInt), the new
    /// function is well typed with one closed trap, a second pass changes nothing.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        for guarded in [false, true] {
            let mut orig = read(&image);
            if guarded {
                crate::net_guard::patch_net_guard(&mut orig);
            }
            let base = write(&orig);
            let orig = read(&base);
            let p = plan(&orig).expect("plan");
            let mut code = read(&base);
            patch_rpc_cancel(&mut code);
            let patched = write(&code);
            let back = read(&patched);
            let nf = orig.functions.len();
            assert_eq!(back.functions.len(), nf + 1);
            for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
                assert_eq!(
                    same(a, b),
                    ![p.pm_fi, p.send_fi, p.dispose_fi].contains(&i),
                    "function #{i}"
                );
            }
            let (a, b) = (&orig.functions[p.send_fi], &back.functions[p.send_fi]);
            shifted(a, b, p.send_at, SEND_OPS);
            check_types(&back, b, p.send_at..p.send_at + SEND_OPS);
            check_flow(b);
            let (a, b) = (&orig.functions[p.pm_fi], &back.functions[p.pm_fi]);
            shifted(a, b, p.at, 1);
            check_types(&back, b, p.at..p.at + 1);
            check_flow(b);
            assert!(matches!(a.ops[p.at - 1], Opcode::Call1 { .. }));
            let f = &back.functions[nf];
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
            assert_eq!(traps_ok(f), 1);

            let mut again = read(&patched);
            assert!(plan(&again).is_err());
            patch_rpc_cancel(&mut again);
            assert!(write(&again) == patched);
        }
    }
}
