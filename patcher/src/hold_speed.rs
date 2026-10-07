// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Hold-to-confirm rings fill three times faster.
//
// `ui.BaseUI.holdAction(maxValue, delay, cb, anchor)` is the one place that
// draws the press-and-hold ring (ProgressDisc): its update closure adds the
// frame time to the elapsed counter and runs `cb` once it reaches `maxValue`.
// Every caller passes its own duration (NPC / entity trigger 0.45 s, gamepad
// place exit and skill-bar arrow 1.95 s), so the pass scales it once, at entry:
//
//   maxValue = maxValue / 3;
//
// The co-op "hold to force" bar of wait-all-players buttons is gone altogether
// (see coop_gates.rs: a click already forces). The gamepad long-press binding
// duration is data (cdb Const `Pad_LongPress_Duration`), not code.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

const FACTOR: f64 = 3.0;

struct Plan {
    fi: usize,
    f64_t: RefType,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let ui_t = obj_type(code, "ui.BaseUI")?;
    let f = method(code, ui_t, "holdAction")?;
    let fi = fun_index(code, f.findex)?;
    let args = fun_args(code, f);
    let Some(&f64_t) = args.get(1) else {
        bail!("holdAction: no duration argument");
    };
    if !matches!(code.types[f64_t.0], Type::F64) {
        bail!("holdAction: duration is not f64");
    }
    let set_max = method(
        code,
        obj_type(code, "ui.comp.ProgressDisc")?,
        "set_maxValue",
    )?
    .findex;
    if !f
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::Call2 { fun, .. } if *fun == set_max))
    {
        bail!("holdAction: does not set the ring's maxValue");
    }
    if let (Some(Opcode::Float { ptr, .. }), Some(Opcode::SDiv { dst, a, .. })) =
        (f.ops.first(), f.ops.get(1))
    {
        if code.floats.get(ptr.0) == Some(&FACTOR) && *dst == Reg(1) && *a == Reg(1) {
            bail!("holdAction: already applied");
        }
    }
    Ok(Plan { fi, f64_t })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let three = float_const(code, FACTOR);
    let f = &mut code.functions[p.fi];
    f.regs.push(p.f64_t);
    let k = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Float { dst: k, ptr: three },
            Opcode::SDiv {
                dst: Reg(1),
                a: Reg(1),
                b: k,
            },
        ],
    );
    eprintln!(
        "patched hold speed fn@{}: hold rings fill {FACTOR}x faster",
        f.findex.0
    );
}

/// Makes every hold ring three times faster, or leaves `code` untouched and logs why.
pub(crate) fn patch_hold_speed(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("hold speed skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{read, HLBOOT};

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let fi = plan(&orig).expect("plan").fi;
        let mut code = read(&image);
        patch_hold_speed(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 2);
        assert_eq!(b.regs.len(), a.regs.len() + 1);
        let Opcode::Float { dst: k, ptr } = b.ops[0] else {
            panic!("no Float prologue");
        };
        assert_eq!(back.floats[ptr.0], FACTOR);
        assert!(matches!(b.ops[1], Opcode::SDiv { dst: Reg(1), a: Reg(1), b } if b == k));
        assert_eq!(format!("{:?}", &b.ops[2..]), format!("{:?}", a.ops));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_hold_speed(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
