// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: inspecting an NPC (a hireable recruit's info window) no longer stops
// the other players from talking to a different NPC.
//
// Vanilla `ent.p.Npc.talk` (Npc.hx:299) refuses while ANY connected player is
// `lockedWith` something (Game.anyPlayerLocked(null) -> getPlayerLocked(false):
// connected && lockedWith != null):
//
//   if (game.paused || game.lockForVisitCommand) return;
//   if (game.anyPlayerLocked(null)) return;      // silent, no retry
//   ...
//
// `Npc.inspect` (right click on an NPC, also the dialog "inspect" choice) sets
// `me.lockedWith = npc` until its UnitInfo closes. So while one player reads a
// tavern recruit's info, a click on any other NPC by anyone does nothing.
//
// The fix: that one call becomes `npcTalkLocked(game, this)`:
//
//   if (!game.isMulti) return false;
//   other = false;
//   for (p in game.getPlayerLocked(false)) {    // connected && lockedWith != null
//       lw = p.lockedWith;
//       if (lw == this) return true;             // same NPC: still refused
//       if (!Std.isOfType(lw, ent.p.Npc)) return true;  // chest, craft, activity...
//       other = true;                            // busy with ANOTHER NPC
//   }
//   if (other) for (w in game.mode.windows) if (Std.isOfType(w, ui.win.Dialog)) return true;
//   return false;
//
// Kept: a lock on the same NPC (two players never open it at once), every lock
// on a non-NPC entity (chest, craft, activity, garrison...), and every NPC lock
// while a dialog window exists (the dialog's own inspect / customize choices lock
// the dialog NPC; a second dialog must not start then). Only another player's
// NPC inspection outside a dialog stops counting. A dialog shown to everyone
// while one player still has a recruit's UnitInfo open is the same window
// stacking the dialog inspect choice already produces.
//
// Same op count, jumps and registers in Npc.talk (one Call2 target and argument
// change); the new function is appended. Validated before editing; a mismatch
// skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use hlbc::types::{RefGlobal, ValBool};

struct Plan {
    talk_fi: usize,
    /// The Npc.talk op `Call2 dst = anyPlayerLocked(game, null)`.
    at: usize,
    game_t: RefType,
    npc_t: RefType,
    bool_: RefType,
    i32_: RefType,
    dyn_t: RefType,
    win_t: RefType,
    arr_t: RefType,
    a_len: RefField,
    a_raw: (RefField, RefType),
    bp_t: RefType,
    bp_locked: (RefField, RefType),
    g_mode: (RefField, RefType),
    m_windows: RefField,
    is_multi: RefFun,
    get_locked: RefFun,
    base_check: RefFun,
    npc_cls: RefGlobal,
    npc_cls_t: RefType,
    dlg_cls: RefGlobal,
    dlg_cls_t: RefType,
    dbg_file: usize,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let fun = &code.functions[fun_index(code, f)?];
    let t = fun.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

/// The class global of `name` (HL stores it 1-based) and its type `pkg.$Cls`.
fn class_global(code: &Bytecode, name: &str) -> Result<(RefGlobal, RefType)> {
    let o = obj(code, obj_type(code, name)?)?;
    let g = RefGlobal(
        o.global
            .0
            .checked_sub(1)
            .with_context(|| format!("{name}: no class global"))?,
    );
    let t = *code
        .globals
        .get(g.0)
        .with_context(|| format!("{name}: class global out of range"))?;
    let (pkg, cls) = name.rsplit_once('.').unwrap_or(("", name));
    let want = format!("{pkg}.${cls}");
    if obj(code, t).ok().map(|o| s(code, o.name)) != Some(want.as_str()) {
        bail!("{name}: class global is not {want}");
    }
    Ok((g, t))
}

fn is_sub(code: &Bytecode, t: RefType, of: RefType) -> Result<bool> {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if c == of {
            return Ok(true);
        }
        cur = obj(code, c)?.super_;
    }
    Ok(false)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let game_t = obj_type(code, "Game")?;
    let npc_t = obj_type(code, "ent.p.Npc")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let win_t = obj_type(code, "ui.Window")?;
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    if !is_sub(code, dlg_t, win_t)? {
        bail!("ui.win.Dialog is not a ui.Window");
    }

    let bp_locked = field(code, bp_t, "lockedWith")?;
    if !is_sub(code, npc_t, bp_locked.1)? {
        bail!("ent.p.Npc is not a BasePlayer.lockedWith type");
    }
    let g_mode = field(code, game_t, "mode")?;
    let (m_windows, mw_t) = field(code, g_mode.1, "windows")?;
    if mw_t != arr_t {
        bail!("Game.mode.windows is not an ArrayObj");
    }
    let (a_len, al_t) = field(code, arr_t, "length")?;
    if al_t != i32_ {
        bail!("ArrayObj.length is not an i32");
    }
    let a_raw = field(code, arr_t, "array")?;

    let m = |o: RefType, name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, o, name)?.findex;
        if sig(code, f)? != (args.to_vec(), ret) {
            bail!("unexpected {name} signature");
        }
        Ok(f)
    };
    let is_multi = m(game_t, "get_isMulti", &[game_t], bool_)?;
    let get_locked = m(game_t, "getPlayerLocked", &[game_t, bool_], arr_t)?;
    let any_locked = method(code, game_t, "anyPlayerLocked")?.findex;
    // anyPlayerLocked(game, &checkWindow) == isMulti && getPlayerLocked(checkWindow).length > 0
    let al = &code.functions[fun_index(code, any_locked)?];
    let calls = |f: RefFun| {
        al.ops.iter().any(
            |o| matches!(o, Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. } if *fun == f),
        )
    };
    if !calls(is_multi) || !calls(get_locked) {
        bail!("Game.anyPlayerLocked is not isMulti && getPlayerLocked");
    }
    let base_t = obj_type(code, "hl.BaseType")?;
    let check = method(code, base_t, "check")?;
    if fun_args(code, check).len() != 2 || check.t.as_fun(code).map(|f| f.ret) != Some(bool_) {
        bail!("hl.BaseType.check is not (BaseType, v) -> Bool");
    }
    let base_check = check.findex;
    let (npc_cls, npc_cls_t) = class_global(code, "ent.p.Npc")?;
    let (dlg_cls, dlg_cls_t) = class_global(code, "ui.win.Dialog")?;

    // Npc.talk: `Call2 r = anyPlayerLocked(game, null); JFalse r +1; Ret`.
    let talk = method(code, npc_t, "talk")?;
    if fun_args(code, talk).first() != Some(&npc_t) {
        bail!("unexpected Npc.talk signature");
    }
    let talk_fi = fun_index(code, talk.findex)?;
    let o = &talk.ops;
    let sites: Vec<usize> = (0..o.len().saturating_sub(2))
        .filter(|&i| {
            matches!(
                (&o[i], &o[i + 1], &o[i + 2]),
                (
                    Opcode::Call2 { dst, fun, arg0, .. },
                    Opcode::JFalse { cond, offset: 1 },
                    Opcode::Ret { .. },
                ) if *fun == any_locked && cond == dst && talk.regs[arg0.0 as usize] == game_t
            )
        })
        .collect();
    let [at] = sites[..] else {
        bail!(
            "Npc.talk: expected one `if (anyPlayerLocked) return`, found {}",
            sites.len()
        );
    };
    if !o.iter().any(|x| {
        matches!(x, Opcode::Call1 { fun, .. }
        if code.functions.iter().any(|f| f.findex == *fun && s(code, f.name) == "canTalk"))
    }) {
        bail!("Npc.talk: the lock test is not followed by canTalk");
    }
    Ok(Plan {
        talk_fi,
        at,
        game_t,
        npc_t,
        bool_,
        i32_,
        dyn_t,
        win_t,
        arr_t,
        a_len,
        a_raw,
        bp_t,
        bp_locked,
        g_mode,
        m_windows,
        is_multi,
        get_locked,
        base_check,
        npc_cls,
        npc_cls_t,
        dlg_cls,
        dlg_cls_t,
        dbg_file: debug_file(code, "src/ent/p/Npc.hx")?,
    })
}

/// `npcTalkLocked(game, npc) -> Bool` (see the header).
fn add_locked(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.game_t, p.npc_t]);
    let (game, npc) = (Reg(0), Reg(1));
    let (b, other, arr, n, i, raw, d, pl, lw) = (
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.bp_t),
        r.r(p.bp_locked.1),
    );
    let (nc, mode, w, dc) = (
        r.r(p.npc_cls_t),
        r.r(p.g_mode.1),
        r.r(p.win_t),
        r.r(p.dlg_cls_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "free");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: arr,
        fun: p.get_locked,
        arg0: game,
        arg1: b,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Bool {
        dst: other,
        value: ValBool(false),
    });
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("players");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "players");
    a.op(Opcode::Field {
        dst: lw,
        obj: pl,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "players");
    a.jmp(
        Opcode::JEq {
            a: lw,
            b: npc,
            offset: 0,
        },
        "busy",
    );
    a.op(Opcode::GetGlobal {
        dst: nc,
        global: p.npc_cls,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: nc,
        arg1: lw,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "busy");
    a.op(Opcode::Bool {
        dst: other,
        value: ValBool(true),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "players");
    // Only other-NPC locks: refused while a dialog window exists.
    a.label("end");
    a.jmp(
        Opcode::JFalse {
            cond: other,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: arr,
        obj: mode,
        field: p.m_windows,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("windows");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: w, src: d });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "windows");
    a.op(Opcode::GetGlobal {
        dst: dc,
        global: p.dlg_cls,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: dc,
        arg1: w,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "windows");
    a.label("busy");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("free");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.game_t, p.npc_t],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let locked = add_locked(code, p)?;
    let f = &mut code.functions[p.talk_fi];
    let Opcode::Call2 { dst, arg0, .. } = f.ops[p.at] else {
        unreachable!()
    };
    f.ops[p.at] = Opcode::Call2 {
        dst,
        fun: locked,
        arg0,
        arg1: Reg(0),
    };
    eprintln!(
        "patched npc talk fn@{} op {}: another player's NPC inspection no longer blocks a talk (lock test fn@{})",
        f.findex.0, p.at, locked.0
    );
    Ok(())
}

/// Lets a co-op player talk to an NPC while another player inspects a different
/// NPC, or leaves `code` untouched and logs why.
pub(crate) fn patch_npc_talk(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p));
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("npc talk skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only Npc.talk's lock call changes (target and second argument); one
    /// well-typed function is appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at) = (p.talk_fi, p.at);
        let mut code = read(&image);
        patch_npc_talk(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 1);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let nf = back.functions.last().expect("new fn");
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), a.ops.len());
        for i in 0..a.ops.len() {
            if i == at {
                let Opcode::Call2 { dst, arg0, .. } = a.ops[i] else {
                    panic!("site is not Call2");
                };
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!(
                        "{:?}",
                        Opcode::Call2 {
                            dst,
                            fun: nf.findex,
                            arg0,
                            arg1: Reg(0)
                        }
                    )
                );
            } else {
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", a.ops[i]),
                    "op {i}"
                );
            }
        }
        check_types(&back, b, at..at + 1);
        check_types(&back, nf, 0..nf.ops.len());
        check_flow(nf);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_npc_talk(&mut again);
        assert!(write(&again) == patched);
    }

    /// A talk whose lock test is not `if (anyPlayerLocked) return` is refused
    /// and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (fi, at) = (p.talk_fi, p.at);
        let mut code = read(&image);
        code.functions[fi].ops[at + 2] = Opcode::Label;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_npc_talk(&mut code);
        assert!(write(&code) == before);
    }
}
