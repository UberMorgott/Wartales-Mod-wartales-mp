// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The game's transport is chosen when the host starts (issue #7).
//
// Every lobby of our master is direct-marked (direct_lobby.rs), so the host's
// `Lobby.setupPlatform` always asks instance/get, and our master answers from
// how the guests actually joined (internal/master/starttransport.go). The
// answer callback (Lobby.hx:607-619) only knows a relay:
//
//   if (r == null) { trace("No server found"); p(null, null); return; }
//   switch getPlatform(r.serverID) {
//     case RelayP2P(host, port, ssl): p(RelayP2P.., WServer..);
//     default: trace("Invalid platform " + r.serverID); p(null, null);
//   }
//
// The pass puts two cases in front of the default branch:
//
//   Steam:   p(r.serverID, null); return;  -- what setupPlatform's own
//            isSteamOnly branch does with getUser().id: the vanilla Steam path;
//   Unknown: TitleScreen.message(r.serverID, null);  -- the master's reason why
//            the game cannot start (it never starts with a platform char),
//            then the vanilla default: p(null, null) -> cant_create_lobby.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::Asm;
use crate::diag::static_fn;

struct Plan {
    fi: usize,
    /// The op the RelayP2P test jumps to when it fails (the default branch).
    at: usize,
    /// That test.
    test: usize,
    variant: Reg,
    k: Reg,
    r: Reg,
    sid_f: RefField,
    sid: Reg,
    none: Reg,
    p: Reg,
    void: Reg,
    steam: i32,
    unknown: i32,
    message: RefFun,
    cb_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let lobby_t = obj_type(code, "Lobby")?;
    let setup = method(code, lobby_t, "setupPlatform")?;
    let message = static_fn(code, "ui.win.$TitleScreen", "message")?;
    let (margs, _) = sig(code, message.findex)?;
    let closures: Vec<RefFun> = setup
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::InstanceClosure { fun, .. } | Opcode::StaticClosure { fun, .. } => Some(*fun),
            _ => None,
        })
        .collect();
    let mut found = None;
    for f in closures {
        let fi = fun_index(code, f)?;
        if code.functions[fi]
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if fname(code, *fun) == "getPlatform"))
        {
            if found.is_some() {
                bail!("setupPlatform: two closures call getPlatform");
            }
            found = Some(fi);
        }
    }
    let fi = found.context("setupPlatform: no instance/get callback")?;
    let f = &code.functions[fi];
    // ... Field sid = r.serverID; Call1 getPlatform(sid); NullCheck; EnumIndex v;
    // Int k = 11; JNotEq v, k -> default.
    let test = f
        .ops
        .iter()
        .position(|o| matches!(o, Opcode::JNotEq { .. }))
        .context("callback: no platform test")?;
    let (
        Some(Opcode::Field {
            dst: sid,
            obj: r,
            field: sid_f,
        }),
        Some(Opcode::Call1 { fun, arg0, dst: pv }),
        Some(Opcode::NullCheck { .. }),
        Some(Opcode::EnumIndex {
            dst: variant,
            value,
        }),
        Some(Opcode::Int { dst: k, .. }),
        Opcode::JNotEq { a, b, offset },
    ) = (
        test.checked_sub(5).and_then(|i| f.ops.get(i)),
        test.checked_sub(4).and_then(|i| f.ops.get(i)),
        test.checked_sub(3).and_then(|i| f.ops.get(i)),
        test.checked_sub(2).and_then(|i| f.ops.get(i)),
        test.checked_sub(1).and_then(|i| f.ops.get(i)),
        &f.ops[test],
    )
    else {
        bail!("callback: unexpected shape before the platform test");
    };
    if fname(code, *fun) != "getPlatform"
        || arg0 != sid
        || value != pv
        || a != variant
        || b != k
        || *r != Reg(1)
    {
        bail!("callback: the platform test does not test getPlatform(r.serverID)");
    }
    let at = (test as i64 + 1 + *offset as i64) as usize;
    if matches!(f.ops.get(at), Some(Opcode::Int { .. })) {
        bail!("callback fn@{}: already applied", f.findex.0);
    }
    if (0..f.ops.len())
        .filter(|&i| jump_targets(f, i).contains(&at))
        .count()
        != 1
    {
        bail!("callback: the default branch has other entries");
    }
    // The default branch ends with p(null, null): find its two null registers.
    let Some(Opcode::CallClosure {
        dst: void,
        fun: p,
        args,
    }) = f.ops[at..].iter().find(
        |o| matches!(o, Opcode::CallClosure { fun, args, .. } if *fun == Reg(0) && args.len() == 2),
    )
    else {
        bail!("callback: no p(null, null) in the default branch");
    };
    let none = args[1];
    if *p != Reg(0) || f.regs[sid.0 as usize] != f.regs[none.0 as usize] {
        bail!("callback: p is not (String, String) -> void");
    }
    let pt = &code.types[f.regs[pv.0 as usize].0];
    let Type::Enum { constructs, .. } = pt else {
        bail!("getPlatform does not return an enum");
    };
    let idx = |name: &str| {
        constructs
            .iter()
            .position(|c| s(code, c.name) == name)
            .map(|i| i as i32)
            .with_context(|| format!("PlatformID.{name} not found"))
    };
    if idx("RelayP2P")? != 11 {
        bail!("PlatformID.RelayP2P moved");
    }
    if margs.len() != 2 || margs[0] != f.regs[sid.0 as usize] {
        bail!("TitleScreen.message: unexpected signature");
    }
    Ok(Plan {
        fi,
        at,
        test,
        variant: *variant,
        k: *k,
        r: *r,
        sid_f: *sid_f,
        sid: *sid,
        none,
        p: *p,
        void: *void,
        steam: idx("Steam")?,
        unknown: idx("Unknown")?,
        message: message.findex,
        cb_t: margs[1],
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let steam = int_const(code, p.steam);
    let unknown = int_const(code, p.unknown);
    let f = &mut code.functions[p.fi];
    let cb = new_reg(f, p.cb_t);
    let mut a = Asm::new();
    a.op(Opcode::Int {
        dst: p.k,
        ptr: steam,
    });
    a.jmp(
        Opcode::JNotEq {
            a: p.variant,
            b: p.k,
            offset: 0,
        },
        "not_steam",
    );
    a.op(Opcode::Field {
        dst: p.sid,
        obj: p.r,
        field: p.sid_f,
    });
    a.op(Opcode::Null { dst: p.none });
    a.op(Opcode::CallClosure {
        dst: p.void,
        fun: p.p,
        args: vec![p.sid, p.none],
    });
    a.op(Opcode::Ret { ret: p.void });
    a.label("not_steam");
    a.op(Opcode::Int {
        dst: p.k,
        ptr: unknown,
    });
    a.jmp(
        Opcode::JNotEq {
            a: p.variant,
            b: p.k,
            offset: 0,
        },
        "vanilla",
    );
    a.op(Opcode::Field {
        dst: p.sid,
        obj: p.r,
        field: p.sid_f,
    });
    a.op(Opcode::Null { dst: cb });
    a.op(Opcode::Call2 {
        dst: p.void,
        fun: p.message,
        arg0: p.sid,
        arg1: cb,
    });
    a.label("vanilla");
    let ops = a.finish();
    let n = ops.len();
    insert_ops(f, p.at, ops);
    // insert_ops moved the platform test past the new block; it enters it.
    if let Opcode::JNotEq { offset, .. } = &mut f.ops[p.test] {
        *offset = (p.at as i64 - p.test as i64 - 1) as i32;
    }
    eprintln!(
        "patched start transport fn@{} op {}: a Steam answer takes the Steam path, an unknown one is shown ({} ops)",
        f.findex.0, p.at, n
    );
}

/// Lets the host's game start on the transport our master chose at start, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_start_transport(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("start transport skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, test) = (p.fi, p.at, p.test);
        let mut code = read(&image);
        patch_start_transport(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let n = b.ops.len() - a.ops.len();
        assert_eq!(n, 11);
        assert_eq!(
            jump_targets(b, test),
            vec![at],
            "the platform test enters the new block"
        );
        // Apart from that test, a plain insertion.
        let mut moved = b.clone();
        if let Opcode::JNotEq { offset, .. } = &mut moved.ops[test] {
            *offset += n as i32;
        }
        shifted(a, &moved, at, n);
        check_types(&back, b, at..at + n);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_start_transport(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
