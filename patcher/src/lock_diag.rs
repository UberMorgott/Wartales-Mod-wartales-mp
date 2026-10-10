// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Battle lock diagnostics, print only (behaviour unchanged).
//
// `battle.Battle.lockCounter > 0` (isLocked) makes the host drop a guest's
// skill request (netExecuteSkill returns false) and end turn
// (Controller.battleUnitEndTurn__impl does nothing), and makes any player's own
// End Turn a no-op (Battle.endTurn -> canEndTurn). None of that is logged, so a
// lock that is never released shows up only as "nothing I press works".
//
//   S. set_lockCounter(v): when lockCounter goes from 0 to v > 0, the call stack
//      (CallStack.callStack(), an array; stringified only when reported) is
//      kept in a new global: who took the lock that is still held.
//   R. lockReport(battle, what), called where a request is refused while locked:
//        netExecuteSkill        "host refused a skill request"
//        battleUnitEndTurn__impl "host refused an end turn"
//        Battle.endTurn         "end turn blocked"   (canEndTurn false and isLocked)
//      prints "mp: lock: <what>, lockCounter=<n>, isAuth=<b>, locked since: <stack>",
//      at most REPORT_CAP lines a run (a new I32 global counts them).
//
// The guest-side leak itself (a cancelled skill request) is fixed and logged
// by rpc_cancel.rs.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;

const REPORT_CAP: i32 = 40;
const PRE: &str = "mp: lock: ";
const LC: &str = ", lockCounter=";
const AUTH: &str = ", isAuth=";
const SINCE: &str = ", locked since: ";
const UNKNOWN: &str = "?";
const W_SKILL: &str = "host refused a skill request";
const W_HOST_END: &str = "host refused an end turn";
const W_END: &str = "end turn blocked";

struct Plan {
    battle_t: RefType,
    game_t: RefType,
    str_t: RefType,
    dyn_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    void_t: RefType,
    arr_t: RefType,
    lock: RefField,
    b_game: RefField,
    g_auth: RefField,
    is_locked: RefFun,
    call_stack: RefFun,
    stack_str: RefFun,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    /// set_lockCounter and its `this.lockCounter = v` op.
    set_fi: usize,
    set_at: usize,
    /// netExecuteSkill: the op after `if (!isLocked()) skip` (the `false` return).
    exec_fi: usize,
    exec_at: usize,
    /// battleUnitEndTurn__impl: the `if (locked) return` jump, its battle and flag.
    hend_fi: usize,
    hend_at: usize,
    hend_battle: Reg,
    hend_flag: Reg,
    /// Battle.endTurn: the `if (!canEndTurn) return` jump and its flag.
    end_fi: usize,
    end_at: usize,
    end_flag: Reg,
    dbg_file: usize,
}

fn targeted(f: &Function, at: usize) -> bool {
    (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&at))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let str_t = obj_type(code, "String")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let game_t = obj_type(code, "Game")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let lock = typed(code, battle_t, "lockCounter", i32_t)?;
    let b_game = typed(code, battle_t, "game", game_t)?;
    let g_auth = typed(code, game_t, "isAuth", bool_t)?;
    let is_locked = method(code, battle_t, "isLocked")?.findex;
    let can_end = method(code, battle_t, "canEndTurn")?.findex;
    let cs = "haxe._CallStack.$CallStack_Impl_";
    let call_stack = static_fn(code, cs, "callStack")?;
    let stack_str = static_fn(code, cs, "toString")?;
    if !fun_args(code, call_stack).is_empty()
        || call_stack.t.as_fun(code).map(|t| t.ret) != Some(arr_t)
        || fun_args(code, stack_str) != [arr_t]
        || stack_str.t.as_fun(code).map(|t| t.ret) != Some(str_t)
    {
        bail!("unexpected CallStack.callStack / toString signatures");
    }
    let println = static_fn(code, "$Sys", "println")?;
    let std_string = static_fn(code, "$Std", "string")?;
    if fun_args(code, println) != [dyn_t] || fun_args(code, std_string) != [dyn_t] {
        bail!("Sys.println / Std.string do not take one Dyn");
    }
    let str_add = static_fn(code, "$String", "__add__")?.findex;

    // S: `if (v < 0) throw ...; this.lockCounter = v; return v`.
    let setf = method(code, battle_t, "set_lockCounter")?;
    let sets: Vec<usize> = (0..setf.ops.len())
        .filter(|&i| {
            matches!(setf.ops[i], Opcode::SetThis { field, src } if field == lock && src == Reg(1))
        })
        .collect();
    let [set_at] = sets[..] else {
        bail!("set_lockCounter: {} lockCounter stores, want 1", sets.len());
    };
    if setf.ops.len() != 7 || !matches!(setf.ops[set_at + 1], Opcode::Ret { .. }) {
        bail!("set_lockCounter: unexpected shape (already patched?)");
    }

    // R1: netExecuteSkill starts `if (isLocked()) return false`.
    let exec = method(code, battle_t, "netExecuteSkill")?;
    let exec_at = (0..exec.ops.len().saturating_sub(3))
        .find(|&i| {
            matches!(exec.ops[i], Opcode::Call1 { fun, arg0, .. } if fun == is_locked && arg0 == Reg(0))
        })
        .context("netExecuteSkill: no isLocked call")?
        + 2;
    match (
        &exec.ops[exec_at - 2],
        &exec.ops[exec_at - 1],
        &exec.ops[exec_at],
        &exec.ops[exec_at + 1],
    ) {
        (
            Opcode::Call1 { dst, .. },
            Opcode::JFalse { cond, .. },
            Opcode::Bool { .. },
            Opcode::Ret { .. },
        ) if dst == cond => {}
        _ => bail!("netExecuteSkill: unexpected locked return (already patched?)"),
    }

    // R2: battleUnitEndTurn__impl: `if (battle.isLocked()) return`.
    let hend = method(code, ctrl_t, "battleUnitEndTurn__impl")?;
    let sites: Vec<(usize, Reg, Reg)> = hend
        .ops
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| match (&w[0], &w[1]) {
            (Opcode::Call1 { dst, fun, arg0 }, Opcode::JTrue { cond, .. })
                if *fun == is_locked && cond == dst =>
            {
                Some((i + 1, *arg0, *dst))
            }
            _ => None,
        })
        .collect();
    let [(hend_at, hend_battle, hend_flag)] = sites[..] else {
        bail!(
            "battleUnitEndTurn__impl: {} locked returns, want 1 (already patched?)",
            sites.len()
        );
    };

    // R3: Battle.endTurn: `if (!canEndTurn(u)) return`.
    let endf = method(code, battle_t, "endTurn")?;
    let sites: Vec<(usize, Reg)> = endf
        .ops
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| match (&w[0], &w[1]) {
            (Opcode::Call3 { dst, fun, arg0, .. }, Opcode::JFalse { cond, .. })
                if *fun == can_end && *arg0 == Reg(0) && cond == dst =>
            {
                Some((i + 1, *dst))
            }
            _ => None,
        })
        .collect();
    let [(end_at, end_flag)] = sites[..] else {
        bail!(
            "Battle.endTurn: {} canEndTurn returns, want 1 (already patched?)",
            sites.len()
        );
    };
    if targeted(exec, exec_at) || targeted(hend, hend_at) || targeted(endf, end_at) {
        bail!("a report site is a jump target");
    }
    Ok(Plan {
        battle_t,
        game_t,
        str_t,
        dyn_t,
        i32_t,
        bool_t,
        void_t,
        arr_t,
        lock,
        b_game,
        g_auth,
        is_locked,
        call_stack: call_stack.findex,
        stack_str: stack_str.findex,
        println: println.findex,
        std_string: std_string.findex,
        str_add,
        set_fi: fun_index(code, setf.findex)?,
        set_at,
        exec_fi: fun_index(code, exec.findex)?,
        exec_at,
        hend_fi: fun_index(code, hend.findex)?,
        hend_at,
        hend_battle,
        hend_flag,
        end_fi: fun_index(code, endf.findex)?,
        end_at,
        end_flag,
        dbg_file: debug_file(code, "src/battle/Battle.hx")?,
    })
}

/// lockReport(battle, what).
fn add_report(code: &mut Bytecode, p: &Plan, src: RefGlobal, count: RefGlobal) -> Result<RefFun> {
    let g: Vec<RefGlobal> = [PRE, LC, AUTH, SINCE, UNKNOWN]
        .into_iter()
        .map(|x| str_global(code, p.str_t, x))
        .collect();
    let (cap, one) = (int_const(code, REPORT_CAP), int_const(code, 1));
    let mut r = Regs(vec![]);
    let (b, what) = (r.r(p.battle_t), r.r(p.str_t));
    let v = r.r(p.void_t);
    let n = r.r(p.i32_t);
    let m = r.r(p.i32_t);
    let s = r.r(p.str_t);
    let t = r.r(p.str_t);
    let d = r.r(p.dyn_t);
    let game = r.r(p.game_t);
    let k = r.r(p.bool_t);
    let arr = r.r(p.arr_t);
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: n,
        global: count,
    });
    a.op(Opcode::Int { dst: m, ptr: cap });
    a.jmp(
        Opcode::JSGte {
            a: n,
            b: m,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: m, ptr: one });
    a.op(Opcode::Add { dst: n, a: n, b: m });
    a.op(Opcode::SetGlobal {
        global: count,
        src: n,
    });
    let add = |a: &mut Asm, x: Reg| {
        a.op(Opcode::Call2 {
            dst: s,
            fun: p.str_add,
            arg0: s,
            arg1: x,
        })
    };
    let text = |a: &mut Asm, gl: RefGlobal| {
        a.op(Opcode::GetGlobal { dst: t, global: gl });
        add(a, t);
    };
    a.op(Opcode::GetGlobal {
        dst: s,
        global: g[0],
    });
    add(&mut a, what);
    text(&mut a, g[1]);
    a.op(Opcode::Field {
        dst: n,
        obj: b,
        field: p.lock,
    });
    a.op(Opcode::ToDyn { dst: d, src: n });
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.std_string,
        arg0: d,
    });
    add(&mut a, t);
    text(&mut a, g[2]);
    a.op(Opcode::Field {
        dst: game,
        obj: b,
        field: p.b_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "noauth",
    );
    a.op(Opcode::Field {
        dst: k,
        obj: game,
        field: p.g_auth,
    });
    a.op(Opcode::ToDyn { dst: d, src: k });
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.std_string,
        arg0: d,
    });
    add(&mut a, t);
    a.label("noauth");
    text(&mut a, g[3]);
    a.op(Opcode::GetGlobal {
        dst: arr,
        global: src,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: arr,
            offset: 0,
        },
        "known",
    );
    a.op(Opcode::GetGlobal {
        dst: t,
        global: g[4],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "text");
    a.label("known");
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.stack_str,
        arg0: arr,
    });
    a.label("text");
    add(&mut a, t);
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.battle_t, p.str_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let src = add_global(code, p.arr_t);
    let count = add_global(code, p.i32_t);
    let report = add_report(code, p, src, count)?;
    let ws: Vec<RefGlobal> = [W_SKILL, W_HOST_END, W_END]
        .into_iter()
        .map(|x| str_global(code, p.str_t, x))
        .collect();
    let zero = int_const(code, 0);

    // S: 0 -> v > 0 keeps the stack. Jumps to the store land on the inserted block.
    let f = &mut code.functions[p.set_fi];
    let old = new_reg(f, p.i32_t);
    let z = new_reg(f, p.i32_t);
    let arr = new_reg(f, p.arr_t);
    let to = |target: i32, i: i32| target - i - 1;
    let ops = vec![
        Opcode::GetThis {
            dst: old,
            field: p.lock,
        },
        Opcode::Int { dst: z, ptr: zero },
        Opcode::JNotEq {
            a: old,
            b: z,
            offset: to(6, 2),
        },
        Opcode::JSGte {
            a: z,
            b: Reg(1),
            offset: to(6, 3),
        },
        Opcode::Call0 {
            dst: arr,
            fun: p.call_stack,
        },
        Opcode::SetGlobal {
            global: src,
            src: arr,
        },
    ];
    let n = ops.len();
    insert_ops(f, p.set_at, ops);
    for i in (0..p.set_at).chain(p.set_at + n..f.ops.len()) {
        if jump_targets(f, i) == [p.set_at + n] {
            let off = p.set_at as i32 - i as i32 - 1;
            match &mut f.ops[i] {
                Opcode::JSGte { offset, .. }
                | Opcode::JSLt { offset, .. }
                | Opcode::JTrue { offset, .. }
                | Opcode::JFalse { offset, .. }
                | Opcode::JAlways { offset } => *offset = off,
                o => bail!("set_lockCounter: unexpected jump {o:?} to the store"),
            }
        }
    }
    let set = f.findex.0;

    // R1: netExecuteSkill, before the locked `return false`.
    let f = &mut code.functions[p.exec_fi];
    let (v, s) = (new_reg(f, p.void_t), new_reg(f, p.str_t));
    insert_ops(
        f,
        p.exec_at,
        vec![
            Opcode::GetGlobal {
                dst: s,
                global: ws[0],
            },
            Opcode::Call2 {
                dst: v,
                fun: report,
                arg0: Reg(0),
                arg1: s,
            },
        ],
    );
    let exec = f.findex.0;

    // R2: battleUnitEndTurn__impl, before `if (locked) return`.
    let f = &mut code.functions[p.hend_fi];
    let (v, s) = (new_reg(f, p.void_t), new_reg(f, p.str_t));
    insert_ops(
        f,
        p.hend_at,
        vec![
            Opcode::JFalse {
                cond: p.hend_flag,
                offset: 2,
            },
            Opcode::GetGlobal {
                dst: s,
                global: ws[1],
            },
            Opcode::Call2 {
                dst: v,
                fun: report,
                arg0: p.hend_battle,
                arg1: s,
            },
        ],
    );
    let hend = f.findex.0;

    // R3: Battle.endTurn, before `if (!canEndTurn) return`.
    let f = &mut code.functions[p.end_fi];
    let (v, s, k) = (
        new_reg(f, p.void_t),
        new_reg(f, p.str_t),
        new_reg(f, p.bool_t),
    );
    insert_ops(
        f,
        p.end_at,
        vec![
            Opcode::JTrue {
                cond: p.end_flag,
                offset: 4,
            },
            Opcode::Call1 {
                dst: k,
                fun: p.is_locked,
                arg0: Reg(0),
            },
            Opcode::JFalse { cond: k, offset: 2 },
            Opcode::GetGlobal {
                dst: s,
                global: ws[2],
            },
            Opcode::Call2 {
                dst: v,
                fun: report,
                arg0: Reg(0),
                arg1: s,
            },
        ],
    );
    eprintln!(
        "patched lock diag: set_lockCounter fn@{set} op {}, netExecuteSkill fn@{exec} op {}, \
         battleUnitEndTurn__impl fn@{hend} op {}, endTurn fn@{} op {} (lockReport fn@{})",
        p.set_at, p.exec_at, p.hend_at, f.findex.0, p.end_at, report.0
    );
    Ok(())
}

/// Logs refused battle requests while locked and who took the lock, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_lock_diag(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("lock diag skipped: {e:#}"));
            return;
        }
    };
    let snap = crate::asm::Snap::take(code);
    let saved: Vec<(usize, Function)> = [p.set_fi, p.exec_fi, p.hend_fi, p.end_fi]
        .into_iter()
        .map(|i| (i, code.functions[i].clone()))
        .collect();
    if let Err(e) = apply(code, &p) {
        snap.restore(code);
        for (i, f) in saved {
            code.functions[i] = f;
        }
        crate::skipped(format!("lock diag skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only the four functions change, by the inserted ops alone; the store's
    /// guard jump now lands on the inserted block; every skip lands on the
    /// vanilla op; the report function is well typed; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_lock_diag(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        let changed = [p.set_fi, p.exec_fi, p.hend_fi, p.end_fi];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), !changed.contains(&i), "function #{i}");
        }
        let f = &back.functions[nf];
        check_types(&back, f, 0..f.ops.len());
        check_flow(f);

        let (a, b) = (&orig.functions[p.set_fi], &back.functions[p.set_fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 6);
        check_types(&back, b, p.set_at..p.set_at + 6);
        check_flow(b);
        assert_eq!(jump_targets(b, p.set_at + 2), [p.set_at + 6]);
        assert_eq!(jump_targets(b, p.set_at + 3), [p.set_at + 6]);
        // The vanilla `v >= 0` guard reaches the inserted block, not the store.
        let guard: Vec<usize> = (0..p.set_at)
            .filter(|&i| !jump_targets(a, i).is_empty())
            .collect();
        for i in guard {
            if jump_targets(a, i) == [p.set_at] {
                assert_eq!(jump_targets(b, i), [p.set_at]);
            }
        }

        for (fi, at, n) in [
            (p.exec_fi, p.exec_at, 2),
            (p.hend_fi, p.hend_at, 3),
            (p.end_fi, p.end_at, 5),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, at, n);
            check_types(&back, b, at..at + n);
            check_flow(b);
        }
        assert_eq!(
            jump_targets(&back.functions[p.hend_fi], p.hend_at),
            [p.hend_at + 3]
        );
        let e = &back.functions[p.end_fi];
        assert_eq!(jump_targets(e, p.end_at), [p.end_at + 5]);
        assert_eq!(jump_targets(e, p.end_at + 2), [p.end_at + 5]);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_lock_diag(&mut again);
        assert!(write(&again) == patched);
    }
}
