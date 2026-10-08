// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op host: a lobby the game already left still finishes connecting and
// crashes (vanilla race, Lobby.hx:735-770).
//
// `Lobby.create` answers each mpman lobby/create reply with
//   l = new Lobby(mpLobby); l.host.connect(id, (ok) -> {
//       trace("Connected = " + ok);
//       if (ok && l.host != null && l.host.isAuth) { onConnected(); done(l); } else done(null);
//   });
// and the connect result arrives one loop tick later (LobbyService ctor,
// haxe.Timer.delay 0). Nothing stops a second create (StartChoice's start
// button has no repeat guard), and the Lobby constructor leaves the previous
// `Lobby.inst` (mpLobby = null). When both replies land in one tick, the
// first lobby's callback then runs on a left lobby: onConnected builds a
// LobbyState whose initAssignments reads `lobby.mpLobby.data` -> Null access
// `.isLoad`; without the crash done(l) would open a CustomizeScreen for it.
//
// The fix, right after the trace: `if (l.mpLobby == null) return;` - the
// left lobby's result is ignored; the newer lobby's own callback drives the UI.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The op `JFalse ok` that follows the trace.
    at: usize,
    /// The captured-environment field holding the Lobby.
    env_field: RefField,
    construct: hlbc::types::RefEnumConstruct,
    lobby_t: RefType,
    mp_lobby: (RefField, RefType),
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let lobby_t = obj_type(code, "Lobby")?;
    let mp_lobby = field(code, lobby_t, "mpLobby")?;
    let host = field(code, lobby_t, "host")?.0;
    let lobby_file = debug_file(code, "src/Lobby.hx")?;
    let in_lobby_file = |f: &Function| {
        f.debug_info
            .as_ref()
            .and_then(|d| d.first())
            .is_some_and(|&(file, _)| file == lobby_file)
    };
    let closures = |f: &Function| -> Vec<RefFun> {
        f.ops
            .iter()
            .filter_map(|o| match o {
                Opcode::InstanceClosure { fun, .. } | Opcode::StaticClosure { fun, .. } => {
                    Some(*fun)
                }
                _ => None,
            })
            .collect()
    };
    // Static Lobby.create: the one `create` of Lobby.hx whose only closure
    // builds a Lobby.
    let creates: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| s(code, f.name) == "create" && in_lobby_file(f))
        .collect();
    let [create] = creates[..] else {
        bail!("Lobby.create: {} candidates (want 1)", creates.len());
    };
    let [reply] = closures(create)[..] else {
        bail!("Lobby.create: not exactly one reply closure");
    };
    let reply = &code.functions[fun_index(code, reply)?];
    if !reply
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::New { dst } if reply.regs[dst.0 as usize] == lobby_t))
    {
        bail!("Lobby.create reply closure does not build a Lobby");
    }
    // The connect callback: the closure taking (env, Bool).
    let mut cbs = vec![];
    for c in closures(reply) {
        let fi = fun_index(code, c)?;
        let f = &code.functions[fi];
        let args = fun_args(code, f);
        if args.len() == 2 && matches!(code.types[args[1].0], Type::Bool) {
            cbs.push(fi);
        }
    }
    let [fi] = cbs[..] else {
        bail!("Lobby.create: {} connect callbacks (want 1)", cbs.len());
    };
    let f = &code.functions[fi];
    let o = &f.ops;
    if o.iter()
        .any(|x| matches!(x, Opcode::Field { obj, field, .. } if *field == mp_lobby.0 && f.regs[obj.0 as usize] == lobby_t))
    {
        bail!("connect callback already reads mpLobby (applied)");
    }
    let lobby_reads: Vec<(hlbc::types::RefEnumConstruct, RefField)> = o
        .iter()
        .filter_map(|x| match x {
            Opcode::EnumField {
                dst,
                value: Reg(0),
                construct,
                field,
            } if f.regs[dst.0 as usize] == lobby_t => Some((*construct, *field)),
            _ => None,
        })
        .collect();
    let Some(&(construct, env_field)) = lobby_reads.first() else {
        bail!("connect callback does not read its Lobby");
    };
    if lobby_reads.iter().any(|&r| r != (construct, env_field)) {
        bail!("connect callback reads its Lobby from several places");
    }
    if !o
        .iter()
        .any(|x| matches!(x, Opcode::Field { obj, field, .. } if *field == host && f.regs[obj.0 as usize] == lobby_t))
    {
        bail!("connect callback does not read Lobby.host");
    }
    // The first jump is `JFalse ok` (arg 1) right after the trace; nothing
    // jumps onto it.
    let Some(at) = (0..o.len()).find(|&i| !jump_targets(f, i).is_empty()) else {
        bail!("connect callback has no branch");
    };
    if !matches!(o[at], Opcode::JFalse { cond: Reg(1), .. }) {
        bail!("connect callback: the first branch is not on the connect result");
    }
    if !o[..at]
        .iter()
        .any(|x| matches!(x, Opcode::CallClosure { .. }))
    {
        bail!("connect callback: no trace before the branch");
    }
    let Some(Opcode::Ret { .. }) = o.last() else {
        bail!("connect callback does not end in Ret");
    };
    Ok(Plan {
        fi,
        at,
        env_field,
        construct,
        lobby_t,
        mp_lobby,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let l = new_reg(f, p.lobby_t);
    let m = new_reg(f, p.mp_lobby.1);
    let Some(&Opcode::Ret { ret }) = f.ops.last() else {
        unreachable!("checked in plan")
    };
    // JNull at at + 3 jumps to the final Ret (old last index + 4 after insert).
    let end = f.ops.len() + 4 - 1;
    let jnull_at = p.at + 3;
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::EnumField {
                dst: l,
                value: Reg(0),
                construct: p.construct,
                field: p.env_field,
            },
            Opcode::NullCheck { reg: l },
            Opcode::Field {
                dst: m,
                obj: l,
                field: p.mp_lobby.0,
            },
            Opcode::JNull {
                reg: m,
                offset: (end - jnull_at - 1) as i32,
            },
        ],
    );
    debug_assert!(matches!(f.ops[end], Opcode::Ret { ret: r } if r == ret));
    eprintln!(
        "patched stale lobby fn@{} op {}: a left lobby's connect result is ignored",
        f.findex.0, p.at
    );
}

/// Makes the host ignore the connect result of a lobby it already left, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_stale_lobby(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("stale lobby skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    /// Only the connect callback changes: four ops before `JFalse ok`, two
    /// registers; the JNull lands on the final Ret; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at) = (p.fi, p.at);
        let mut code = read(&image);
        patch_stale_lobby(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        assert_eq!(b.regs.len(), a.regs.len() + 2);
        assert_eq!(b.ops.len(), a.ops.len() + 4);
        assert!(matches!(b.ops[at + 3], Opcode::JNull { .. }));
        assert_eq!(jump_targets(b, at + 3), [b.ops.len() - 1]);
        let map = |i: usize| if i < at { i } else { i + 4 };
        for i in 0..a.ops.len() {
            let t: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), t, "op {i} jumps");
        }
        check_types(&back, b, at..at + 4);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_stale_lobby(&mut again);
        assert!(write(&again) == patched);
    }
}
