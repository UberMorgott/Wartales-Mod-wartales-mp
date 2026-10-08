// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// mpman's error log accepts any error value.
//
// `Main.initMpman` sets `mpman.Api.logError` (a `Dynamic -> Void` field) to the
// game's `String -> Void` logger; Haxe wraps it in a closure whose first op is
// `SafeCast dyn -> String`. mpman passes it raw errors: `Connection.connect`
// hands the caught value (`__exceptionMessage()`, a `SysError` for a failed
// socket) to `onError`, and `RelayHost.connect`'s handler (RelayP2PService.hx
// 105) logs it before `stop()` + `onConnect(null)`. The cast throws
// "Can't cast SysError to String", so the failure path never runs and a host
// whose relay connect fails sits on the loading screen forever.
//
// The pass turns that one cast into `Std.string(e)` (same op count, same
// registers): the error is logged with its text and the handler goes on.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    /// Position of the wrapper closure in `code.functions`.
    fi: usize,
    std_string: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let (api_g, api_t) = class_global(code, "mpman.Api")?;
    let (log_f, _) = field(code, api_t, "logError")?;
    let init = crate::diag::static_fn(code, "$Main", "initMpman")?;
    // `GetGlobal r = $Api; SetField r.logError = c`, c an InstanceClosure.
    let mut wrappers = Vec::new();
    for (i, op) in init.ops.iter().enumerate() {
        let Opcode::SetField { obj, field, src } = op else {
            continue;
        };
        if *field != log_f
            || !matches!(init.ops[..i].last(), Some(Opcode::GetGlobal { dst, global }) if dst == obj && *global == api_g)
        {
            continue;
        }
        let made = init.ops[..i].iter().rev().find_map(|o| match o {
            Opcode::InstanceClosure { dst, fun, .. } if dst == src => Some(*fun),
            _ => None,
        });
        wrappers.extend(made);
    }
    let [wrapper] = wrappers[..] else {
        bail!(
            "initMpman: expected one logError closure, found {}",
            wrappers.len()
        );
    };
    let fi = fun_index(code, wrapper)?;
    let f = &code.functions[fi];
    let str_t = obj_type(code, "String")?;
    let std_string = crate::diag::static_fn(code, "$Std", "string")?.findex;
    let (sargs, sret) = sig(code, std_string)?;
    if sargs.len() != 1 || sret != str_t {
        bail!("Std.string: unexpected signature");
    }
    match f.ops.first() {
        Some(Opcode::SafeCast { dst, src: Reg(1) })
            if f.regs.get(dst.0 as usize) == Some(&str_t)
                && matches!(code.types[f.regs[1].0], Type::Dyn) => {}
        Some(Opcode::Call1 { fun, .. }) if *fun == std_string => {
            bail!("logError wrapper fn@{}: already applied", f.findex.0)
        }
        _ => bail!(
            "logError wrapper fn@{}: first op is not SafeCast dyn -> String",
            f.findex.0
        ),
    }
    Ok(Plan { fi, std_string })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let Opcode::SafeCast { dst, src } = f.ops[0] else {
        unreachable!("validated by plan");
    };
    f.ops[0] = Opcode::Call1 {
        dst,
        fun: p.std_string,
        arg0: src,
    };
    eprintln!(
        "patched logError cast fn@{}: mpman errors are Std.string'ed, not cast",
        f.findex.0
    );
}

/// Makes mpman's `Api.logError` take any value, or leaves `code` untouched and logs why.
pub(crate) fn patch_log_error_cast(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("logError cast skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let fi = plan(&orig).expect("plan").fi;
        let mut code = read(&image);
        patch_log_error_cast(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len());
        assert_eq!(b.regs, a.regs);
        assert!(matches!(b.ops[0], Opcode::Call1 { arg0: Reg(1), .. }));
        assert_eq!(format!("{:?}", &b.ops[1..]), format!("{:?}", &a.ops[1..]));
        crate::asm::testutil::check_types(&back, b, 0..1);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_log_error_cast(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
