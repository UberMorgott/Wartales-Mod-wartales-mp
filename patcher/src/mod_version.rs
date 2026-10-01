// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The join gate shows wartales-mp's own "mod files differ from the host" text.
//
// `Lobby.checkJoinLobby(lobby)` (Lobby.hx:868) is the one pre-join test of both
// join by code (closure of `Lobby.joinCode`, Lobby.hx:815) and a Steam invite
// (`Lobby.tryJoinLobby`); it runs on the LobbyInfo the master answered, before
// any lobby/join:
//
//   if (lobby.data.version != Const.VERSION + beta) {
//       message(Texts.multiplayer.join_version_error, null);
//       return false;
//   }
//   return true;
//
// When a guest's mod files (winmm.dll, res1.pak) differ from the host's, the
// guest's own master (internal/master/modcheck.go) adds the field `mpModError`
// to the lobby data, a haxe-serialized string that `MPLobby.decodeData` turns
// back into a String, and also replaces `data.version`, so the vanilla test
// above blocks the join even where this pass did not apply. The pass makes the
// gate show that text instead of the generic version error. Right after
// `d = lobby.data` it inserts:
//
//   if (d != null) {
//       var m = d.mpModError;               // DynGet: null when absent
//       if (m != null) {
//           message(Std.string(m), null);
//           return false;
//       }
//   }
//
// `Std.string` never throws, whatever the field holds. No field means vanilla
// behaviour exactly; only our master writes the field (it strips one coming
// from the host). The caller then adds its generic join error on top, as it
// does after the vanilla version error.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::string_ref;
use super::*;
use hlbc::types::ValBool;

/// Lobby data key; internal/master/modcheck.go writes it under this exact name.
const FIELD: &str = "mpModError";

struct Plan {
    fi: usize,
    /// Insert before this op (the one right after `d = lobby.data`).
    at: usize,
    data: Reg,
    message: RefFun,
    std_string: RefFun,
    str_t: RefType,
    cb_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
}

fn fun_type(code: &Bytecode, f: &Function) -> Result<TypeFun> {
    f.t.as_fun(code)
        .cloned()
        .with_context(|| format!("fn@{} has no function type", f.findex.0))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let lobby_t = obj_type(code, "mpman.Lobby")?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let hits: Vec<usize> = (0..code.functions.len())
        .filter(|&i| {
            let f = &code.functions[i];
            s(code, f.name) == "checkJoinLobby"
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == [lobby_t] && t.ret == bool_t)
        })
        .collect();
    let [fi] = hits[..] else {
        bail!(
            "expected one checkJoinLobby(mpman.Lobby):Bool, found {}",
            hits.len()
        );
    };
    let f = &code.functions[fi];
    if f.ops.iter().any(
        |o| matches!(o, Opcode::DynGet { field, .. } if code.strings[field.0].as_str() == FIELD),
    ) {
        bail!("checkJoinLobby: already applied");
    }

    // `d = lobby.data` (after the NullCheck of the argument), with d dynamic.
    let (data_f, _) = field(code, lobby_t, "data")?;
    let at = f
        .ops
        .iter()
        .position(|o| matches!(o, Opcode::Field { obj: Reg(0), field, .. } if *field == data_f))
        .context("checkJoinLobby: no read of lobby.data")?
        + 1;
    let Opcode::Field { dst: data, .. } = f.ops[at - 1] else {
        unreachable!()
    };
    let dyn_t = f.regs[data.0 as usize];
    if !matches!(code.types[dyn_t.0], Type::Dyn) {
        bail!("checkJoinLobby: lobby.data is not dynamic");
    }
    if at > 2
        || !f.ops[..at - 1]
            .iter()
            .all(|o| matches!(o, Opcode::NullCheck { reg: Reg(0) }))
    {
        bail!("checkJoinLobby: lobby.data is not read first");
    }
    for i in 0..f.ops.len() {
        if jump_targets(f, i).iter().any(|&t| t <= at) {
            bail!("checkJoinLobby: op {i} jumps to the function start");
        }
    }

    // The gate's own popup: the one call `message(String, cb) -> Void`.
    let calls: Vec<RefFun> = f
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call2 { fun, .. } => Some(*fun),
            _ => None,
        })
        .filter(|&g| {
            code.functions
                .iter()
                .find(|h| h.findex == g)
                .is_some_and(|h| s(code, h.name) == "message")
        })
        .collect();
    let [message] = calls[..] else {
        bail!(
            "checkJoinLobby: expected one message call, found {}",
            calls.len()
        );
    };
    let mt = fun_type(
        code,
        code.functions
            .iter()
            .find(|h| h.findex == message)
            .expect("found"),
    )?;
    let [str_t, cb_t] = mt.args[..] else {
        bail!("message takes {} arguments, not 2", mt.args.len());
    };
    if !matches!(&code.types[str_t.0], Type::Obj(o) if s(code, o.name) == "String")
        || !matches!(code.types[cb_t.0], Type::Fun(_))
        || !matches!(code.types[mt.ret.0], Type::Void)
    {
        bail!("message is not (String, callback) -> Void");
    }

    // Std.string(Dynamic) -> String.
    let strs: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|h| {
            s(code, h.name) == "string"
                && h.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == [dyn_t] && t.ret == str_t)
        })
        .map(|h| h.findex)
        .collect();
    let [std_string] = strs[..] else {
        bail!(
            "expected one Std.string(Dynamic):String, found {}",
            strs.len()
        );
    };
    Ok(Plan {
        fi,
        at,
        data,
        message,
        std_string,
        str_t,
        cb_t,
        void_t: mt.ret,
        bool_t,
        dyn_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let name = string_ref(code, FIELD);
    let f = &mut code.functions[p.fi];
    let mut add = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (m, text, cb, void, no) = (
        add(p.dyn_t),
        add(p.str_t),
        add(p.cb_t),
        add(p.void_t),
        add(p.bool_t),
    );
    // Both JNulls skip the 6 ops after the second one: to the original op.
    let ops = vec![
        Opcode::JNull {
            reg: p.data,
            offset: 7,
        },
        Opcode::DynGet {
            dst: m,
            obj: p.data,
            field: name,
        },
        Opcode::JNull { reg: m, offset: 5 },
        Opcode::Call1 {
            dst: text,
            fun: p.std_string,
            arg0: m,
        },
        Opcode::Null { dst: cb },
        Opcode::Call2 {
            dst: void,
            fun: p.message,
            arg0: text,
            arg1: cb,
        },
        Opcode::Bool {
            dst: no,
            value: ValBool(false),
        },
        Opcode::Ret { ret: no },
    ];
    insert_ops(f, p.at, ops);
    eprintln!(
        "patched mod version fn@{}: the join gate shows the mod-files mismatch text",
        f.findex.0
    );
}

/// Makes `Lobby.checkJoinLobby` show and refuse on `data.mpModError`, or leaves `code` untouched and logs why.
pub(crate) fn patch_mod_version(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("mod version skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::{check_flow, check_types, read, shifted, write, HLBOOT};
    use super::*;

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, data) = (p.fi, p.at, p.data);
        let mut code = read(&image);
        patch_mod_version(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        shifted(a, b, at, 8);
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());
        // Both guards land on the first original op after the insertion.
        assert_eq!(jump_targets(b, at), vec![at + 8]);
        assert_eq!(jump_targets(b, at + 2), vec![at + 8]);
        assert!(matches!(b.ops[at], Opcode::JNull { reg, .. } if reg == data));
        assert!(matches!(&b.ops[at + 1], Opcode::DynGet { obj, field, .. }
            if *obj == data && back.strings[field.0].as_str() == FIELD));
        assert!(matches!(b.ops[at + 7], Opcode::Ret { .. }));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_mod_version(&mut again);
        assert!(write(&again) == patched);
    }
}
