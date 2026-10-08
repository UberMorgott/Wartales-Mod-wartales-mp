// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A lobby on our direct relay is never taken for a Steam-only one.
//
// `Lobby.isSteamOnly()` is true when every player id is a Steam id; the host's
// `Lobby.setupPlatform` (its only caller) then skips `instance/get` and plays
// over Steam P2P. Our master renders every lobby member by the id its own game
// calls itself (the Steam id), so a player's identity is the same on both
// transports and in its saves; which transport a lobby uses is the master's
// decision, written into the lobby id: a direct-relay lobby id ends in 'D'
// (internal/master DirectLobbyMark; ids are otherwise "L" + lowercase hex).
// The pass prepends to isSteamOnly:
//
//   var l = this.mpLobby;
//   if (l != null && l.id != null && l.id.charCodeAt(l.id.length - 1) == 'D') return false;
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::Asm;

/// The last character of a direct-relay lobby id (internal/master/lobby.go).
const MARK: i32 = b'D' as i32;

struct Plan {
    fi: usize,
    ml_f: RefField,
    ml_t: RefType,
    id_f: RefField,
    str_t: RefType,
    len_f: RefField,
    i32_t: RefType,
    char_at: RefFun,
    nint_t: RefType,
    bool_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let lobby_t = obj_type(code, "Lobby")?;
    let f = method(code, lobby_t, "isSteamOnly")?;
    let fi = fun_index(code, f.findex)?;
    let (ml_f, ml_t) = field(code, lobby_t, "mpLobby")?;
    if s(code, obj(code, ml_t)?.name) != "mpman.Lobby" {
        bail!("Lobby.mpLobby is not an mpman.Lobby");
    }
    let str_t = obj_type(code, "String")?;
    let id_f = typed(code, ml_t, "id", str_t)?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let len_f = typed(code, str_t, "length", i32_t)?;
    let char_at = method(code, str_t, "charCodeAt")?.findex;
    let (args, nint_t) = sig(code, char_at)?;
    if args != [str_t, i32_t] || !matches!(code.types[nint_t.0], Type::Null(t) if t == i32_t) {
        bail!("String.charCodeAt: unexpected signature");
    }
    let (_, ret) = sig(code, f.findex)?;
    if ret != bool_t {
        bail!("isSteamOnly does not return bool");
    }
    if !f.ops.iter().any(|o| matches!(o, Opcode::Call1 { .. })) {
        bail!("isSteamOnly: no getPlatform call");
    }
    if matches!(f.ops.first(), Some(Opcode::Field { field, .. }) if *field == ml_f) {
        bail!("isSteamOnly fn@{}: already applied", f.findex.0);
    }
    Ok(Plan {
        fi,
        ml_f,
        ml_t,
        id_f,
        str_t,
        len_f,
        i32_t,
        char_at,
        nint_t,
        bool_t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let mark = int_const(code, MARK);
    let one = int_const(code, 1);
    let f = &mut code.functions[p.fi];
    let ml = new_reg(f, p.ml_t);
    let id = new_reg(f, p.str_t);
    let n = new_reg(f, p.i32_t);
    let k = new_reg(f, p.i32_t);
    let c = new_reg(f, p.nint_t);
    let b = new_reg(f, p.bool_t);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: ml,
        obj: Reg(0),
        field: p.ml_f,
    });
    a.jmp(Opcode::JNull { reg: ml, offset: 0 }, "body");
    a.op(Opcode::Field {
        dst: id,
        obj: ml,
        field: p.id_f,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "body");
    a.op(Opcode::Field {
        dst: n,
        obj: id,
        field: p.len_f,
    });
    a.op(Opcode::Int { dst: k, ptr: one });
    a.op(Opcode::Sub { dst: n, a: n, b: k });
    a.op(Opcode::Call2 {
        dst: c,
        fun: p.char_at,
        arg0: id,
        arg1: n,
    });
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "body");
    a.op(Opcode::SafeCast { dst: n, src: c });
    a.op(Opcode::Int { dst: k, ptr: mark });
    a.jmp(
        Opcode::JNotEq {
            a: n,
            b: k,
            offset: 0,
        },
        "body",
    );
    a.op(Opcode::Bool { dst: b, value: hlbc::types::ValBool(false) });
    a.op(Opcode::Ret { ret: b });
    a.label("body");
    let ops = a.finish();
    insert_ops(f, 0, ops);
    eprintln!(
        "patched direct lobby fn@{}: isSteamOnly is false for a direct-relay lobby (id ends in 'D')",
        f.findex.0
    );
}

/// Makes `Lobby.isSteamOnly` false for a direct-relay lobby, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_direct_lobby(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("direct lobby skipped: {e:#}")),
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
        let fi = plan(&orig).expect("plan").fi;
        let mut code = read(&image);
        patch_direct_lobby(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let n = b.ops.len() - a.ops.len();
        assert_eq!(n, 14);
        shifted(a, b, 0, n);
        check_types(&back, b, 0..n);
        check_flow(b);
        assert!(matches!(b.ops[n - 2], Opcode::Bool { .. }));
        assert!(matches!(b.ops[n - 1], Opcode::Ret { .. }));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_direct_lobby(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
