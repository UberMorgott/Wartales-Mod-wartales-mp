// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Friendly fire for enemy area attacks.
//
// `Skill.gatherTargets` walks every unit of the battle for an area skill and,
// for each unit the area touches, calls `eval.addTarget(unit, null)`. A null
// `checkValid` means "run `Unit.isValidTarget`", which for a skill whose
// `allowedTargets` is Enemies rejects every unit on the caster's side. This pass
// makes the area loop compute `checkValid` itself:
//
//     var cv = true;
//     if (unit != null && unit != this.u && getAllowedTargets() == Enemies
//         && this.u.owner.side != this.battle.state.playerSide
//         && unit.canBeTarget(this.inf, this.u)) cv = false;
//     ... eval.addTarget(unit, &cv) ...
//
// so a non-player caster's area attack also lands on its own allies (never on
// the caster), while `canBeTarget` still filters untargetable units exactly as
// `isValidTarget` would. Primary-target choice and AI scoring call
// `isValidTarget` directly and are left untouched.

use super::*;

/// `SkillRange.allowedTargets` value for "Enemies".
const ALLOWED_ENEMIES: i32 = 0;

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

/// Virtual-table slot of method `name` on `t` or one of its ancestors.
fn proto_slot(code: &Bytecode, t: RefType, name: &str) -> Result<(RefField, RefFun)> {
    let mut cur = Some(t);
    while let Some(ct) = cur {
        let o = obj(code, ct)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == name) {
            if p.pindex < 0 {
                bail!("method {name} is not virtual");
            }
            return Ok((RefField(p.pindex as usize), p.findex));
        }
        cur = o.super_;
    }
    bail!("method {name} not found on type {}", t.0)
}

fn type_index(code: &Bytecode, want: fn(&Type) -> bool, what: &str) -> Result<RefType> {
    code.types
        .iter()
        .position(want)
        .map(RefType)
        .with_context(|| format!("{what} type not found"))
}

pub(crate) fn patch_enemy_area_friendly_fire(code: &mut Bytecode) -> Result<()> {
    let skill_t = obj_type(code, "battle.skill.Skill")?;
    let eval_t = obj_type(code, "battle.skill.SkillEval")?;
    let add_target = method(code, eval_t, "addTarget")?.findex;
    let area_hit = method(code, skill_t, "checkAreaHit")?.findex;
    let allowed_fn = method(code, skill_t, "getAllowedTargets")?;
    let (allowed_findex, allowed_t) = (
        allowed_fn.findex,
        allowed_fn
            .t
            .as_fun(code)
            .context("getAllowedTargets type")?
            .ret,
    );
    let (u_f, unit_t) = field(code, skill_t, "u")?;
    let (inf_f, inf_t) = field(code, skill_t, "inf")?;
    let (battle_f, battle_t) = field(code, skill_t, "battle")?;
    let (state_f, state_t) = field(code, battle_t, "state")?;
    let (pside_f, pside_t) = field(code, state_t, "playerSide")?;
    let (owner_f, owner_t) = field(code, unit_t, "owner")?;
    let (side_f, side_t) = field(code, owner_t, "side")?;
    if side_t != pside_t {
        bail!("owner.side and state.playerSide have different types");
    }
    let i32_t = type_index(code, |t| matches!(t, Type::I32), "i32")?;
    let (can_target_slot, can_target_fn) = proto_slot(code, unit_t, "canBeTarget")?;
    let can_target_ret = code.functions[fun_index(code, can_target_fn)?]
        .t
        .as_fun(code)
        .context("canBeTarget type")?
        .ret;

    // isValidTarget opens with the same virtual canBeTarget call; make sure the
    // slot we resolved is the one the game itself uses.
    let valid = method(code, unit_t, "isValidTarget")?;
    if !valid
        .ops
        .iter()
        .any(|op| matches!(op, Opcode::CallMethod { field, .. } if *field == can_target_slot))
    {
        bail!(
            "isValidTarget does not call canBeTarget through slot {}",
            can_target_slot.0
        );
    }

    let fi = fun_index(code, method(code, skill_t, "gatherTargets")?.findex)?;
    let f = &code.functions[fi];

    // The area-hit sites: `addTarget(eval, unit, flag)` right after `flag = null`,
    // shortly after a `checkAreaHit(_, unit, ...)` on the same unit.
    let mut sites = vec![];
    for (i, op) in f.ops.iter().enumerate() {
        let Opcode::Call3 {
            fun, arg1, arg2, ..
        } = op
        else {
            continue;
        };
        if *fun != add_target || i == 0 {
            continue;
        }
        let flag_null = matches!(f.ops[i - 1], Opcode::Null { dst } if dst == *arg2);
        let after_area_hit = f.ops[i.saturating_sub(16)..i].iter().any(
            |o| matches!(o, Opcode::CallN { fun, args, .. } if *fun == area_hit && args.get(1) == Some(arg1)),
        );
        if flag_null && after_area_hit {
            sites.push((i, *arg1, *arg2));
        }
    }
    let [(first, unit_reg, flag_reg), (last, unit_reg2, flag_reg2)] = sites[..] else {
        bail!(
            "expected exactly two area addTarget calls in gatherTargets, found {}",
            sites.len()
        );
    };
    if unit_reg != unit_reg2 || flag_reg != flag_reg2 {
        bail!("area addTarget calls use different registers");
    }
    if f.regs.first() != Some(&skill_t) || f.regs[unit_reg.0 as usize] != unit_t {
        bail!("unexpected gatherTargets register types");
    }
    let cv_t = match &code.types[f.regs[flag_reg.0 as usize].0] {
        Type::Ref(inner) => *inner,
        _ => bail!("addTarget flag argument is not a ref"),
    };
    if can_target_ret != cv_t {
        bail!("canBeTarget does not return the addTarget flag type");
    }

    // Loop head: `unit = cast units[i]; i++`; the block goes right after it.
    let at = (1..first)
        .rev()
        .find(|&k| {
            matches!(f.ops[k], Opcode::Incr { .. })
                && matches!(f.ops[k - 1], Opcode::UnsafeCast { dst, .. } if dst == unit_reg)
        })
        .context("area loop head not found")?
        + 1;
    // Every path to the two sites must run the block: nothing may jump into
    // [at, last] from outside it (a jump to `at` itself would skip the block).
    for i in 0..f.ops.len() {
        let inside = (at..=last).contains(&i);
        for t in jump_targets(f, i) {
            if t == at || (!inside && (at..=last).contains(&t)) {
                bail!("op {i} jumps into the area loop body at {t}");
            }
        }
    }

    let zero = int_const(code, ALLOWED_ENEMIES);
    let f = &mut code.functions[fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let cv = reg(cv_t);
    let caster = reg(unit_t);
    let allowed = reg(allowed_t);
    let ai = reg(i32_t);
    let enemies = reg(i32_t);
    let battle = reg(battle_t);
    let state = reg(state_t);
    let pside = reg(pside_t);
    let owner = reg(owner_t);
    let cside = reg(side_t);
    let inf = reg(inf_t);
    let ok = reg(cv_t);

    let bool_op = |dst, v| Opcode::Bool {
        dst,
        value: hlbc::types::ValBool(v),
    };
    let mut ops = vec![
        bool_op(cv, true),
        Opcode::JNull {
            reg: unit_reg,
            offset: 0,
        }, // 1 -> END
        Opcode::GetThis {
            dst: caster,
            field: u_f,
        },
        Opcode::JNull {
            reg: caster,
            offset: 0,
        }, // 3 -> END
        Opcode::JEq {
            a: unit_reg,
            b: caster,
            offset: 0,
        }, // 4 -> END
        Opcode::Call1 {
            dst: allowed,
            fun: allowed_findex,
            arg0: Reg(0),
        },
        Opcode::JNull {
            reg: allowed,
            offset: 0,
        }, // 6 -> END
        Opcode::SafeCast {
            dst: ai,
            src: allowed,
        },
        Opcode::Int {
            dst: enemies,
            ptr: zero,
        },
        Opcode::JNotEq {
            a: ai,
            b: enemies,
            offset: 0,
        }, // 9 -> END
        Opcode::GetThis {
            dst: battle,
            field: battle_f,
        },
        Opcode::JNull {
            reg: battle,
            offset: 0,
        }, // 11 -> END
        Opcode::Field {
            dst: state,
            obj: battle,
            field: state_f,
        },
        Opcode::JNull {
            reg: state,
            offset: 0,
        }, // 13 -> END
        Opcode::Field {
            dst: pside,
            obj: state,
            field: pside_f,
        },
        Opcode::Field {
            dst: owner,
            obj: caster,
            field: owner_f,
        },
        Opcode::JNull {
            reg: owner,
            offset: 0,
        }, // 16 -> END
        Opcode::Field {
            dst: cside,
            obj: owner,
            field: side_f,
        },
        Opcode::JEq {
            a: cside,
            b: pside,
            offset: 0,
        }, // 18 -> END
        Opcode::GetThis {
            dst: inf,
            field: inf_f,
        },
        Opcode::CallMethod {
            dst: ok,
            field: can_target_slot,
            args: vec![unit_reg, inf, caster],
        },
        Opcode::JFalse {
            cond: ok,
            offset: 0,
        }, // 21 -> END
        bool_op(cv, false),
    ];
    let end = ops.len();
    resolve_jumps(
        &mut ops,
        &[1, 3, 4, 6, 9, 11, 13, 16, 18, 21].map(|i| (i, end)),
    );
    let n = ops.len();

    // `flag = null` becomes `flag = &cv` at both sites (indices shift by n).
    for site in [first, last] {
        f.ops[site - 1] = Opcode::Ref {
            dst: flag_reg,
            src: cv,
        };
    }
    insert_ops(f, at, ops);
    eprintln!(
        "patched gatherTargets fn@{}: enemy area friendly fire block at op {} ({n} ops), addTarget flag = &reg{} at ops {} and {}",
        f.findex.0,
        at,
        cv.0,
        first + n,
        last + n
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    /// Patches the installed game's bytecode (skipped when it is absent) and
    /// checks the result reads back with both area sites passing `&cv`.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let out = patch_image(&image).expect("patch");
        let code = Bytecode::deserialize(&mut Cursor::new(&out)).expect("patched image reads back");
        let skill_t = obj_type(&code, "battle.skill.Skill").unwrap();
        let eval_t = obj_type(&code, "battle.skill.SkillEval").unwrap();
        let add_target = method(&code, eval_t, "addTarget").unwrap().findex;
        let f = method(&code, skill_t, "gatherTargets").unwrap();
        let by_ref = f
            .ops
            .windows(2)
            .filter(|w| {
                matches!((&w[0], &w[1]), (Opcode::Ref { dst, .. }, Opcode::Call3 { fun, arg2, .. })
                    if *fun == add_target && arg2 == dst)
            })
            .count();
        assert_eq!(by_ref, 2);
    }
}
