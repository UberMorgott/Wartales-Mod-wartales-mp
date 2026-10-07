// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Skill tooltips show the Valor point cost outside a battle for any unit.
//
// `ui.comp.SkillTip.new(unit, stats, skill, level, ...)` (SkillIcon.hx:76) computes
//
//   var ap = unit != null && unit.isPlayer()
//       ? (bu != null ? bu.getActionPointCost(skill) : unit.getActionPointCost(null, skill))
//       : 0;
//
// and draws the cost only when `ap > 0`. `isPlayer()` is `owner != null`, so the
// new-game customize screen (its unit has no owner yet), the start-choice class
// tips (no unit at all) and every other non-player tooltip outside a battle show
// no cost although the skill data has one (FirstAid `apCost` 1). The pass extends
// the else branch, keeping enemies in battle (`bu != null`) at 0:
//
//   else { ap = 0; if (bu == null && skill != null && skill.props != null) ap = skill.props.apCost; }
//
// The data cost is read directly: `Unit.getActionPointCost` needs a unit (its
// Hypoxia check reads `this`) and `Game.inst.state` (extreme-difficulty malus),
// neither of which exists on the new-game screens. Player units keep the vanilla
// path untouched.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    at: usize,
    bu: Reg,
    skill: Reg,
    ap: Reg,
    props: RefField,
    props_t: RefType,
    ap_cost: RefField,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let tip_t = obj_type(code, "ui.comp.SkillTip")?;
    let f = method(code, tip_t, "__constructor__")?;
    let fi = fun_index(code, f.findex)?;
    let unit_cost = method(code, obj_type(code, "st.Unit")?, "getActionPointCost")?.findex;
    let bu_cost = method(code, obj_type(code, "battle.Unit")?, "getActionPointCost")?.findex;

    let mut unit_calls = f.ops.iter().enumerate().filter_map(|(i, op)| match op {
        Opcode::Call3 { dst, fun, arg2, .. } if *fun == unit_cost => Some((i, *dst, *arg2)),
        _ => None,
    });
    let Some((c, ap, skill)) = unit_calls.next() else {
        bail!("SkillTip: no Unit.getActionPointCost call");
    };
    if unit_calls.next().is_some() {
        bail!("SkillTip: Unit.getActionPointCost called twice");
    }
    let bu_calls: Vec<Reg> = f.ops[..c]
        .iter()
        .filter_map(|op| match op {
            Opcode::Call2 {
                dst,
                fun,
                arg0,
                arg1,
            } if *fun == bu_cost && *dst == ap && *arg1 == skill => Some(*arg0),
            _ => None,
        })
        .collect();
    let [bu] = bu_calls[..] else {
        bail!("SkillTip: battle.Unit.getActionPointCost call not found once before the unit one");
    };

    let at = c + 4;
    match (f.ops.get(c + 1), f.ops.get(c + 2), f.ops.get(c + 3)) {
        (
            Some(Opcode::JAlways { .. }),
            Some(Opcode::Int { dst: k, ptr }),
            Some(Opcode::ToDyn { dst, src }),
        ) if code.ints.get(ptr.0) == Some(&0) && *dst == ap && src == k => {}
        _ => bail!("SkillTip: no `ap = 0` else branch after the cost call"),
    }
    if let (
        Some(Opcode::JNotNull { reg, .. }),
        Some(Opcode::JNull { reg: s, .. }),
        Some(Opcode::Field { obj, .. }),
        Some(Opcode::JNull { .. }),
        Some(Opcode::Field { dst, .. }),
    ) = (
        f.ops.get(at),
        f.ops.get(at + 1),
        f.ops.get(at + 2),
        f.ops.get(at + 3),
        f.ops.get(at + 4),
    ) {
        if *reg == bu && *s == skill && *obj == skill && *dst == ap {
            bail!("SkillTip: already applied");
        }
    }
    if jump_targets(f, c + 1) != [at] {
        bail!("SkillTip: the cost branch does not jump past `ap = 0`");
    }
    // `ap = 0` is entered only by the failed `unit != null && unit.isPlayer()` test, and
    // only the player branch lands on `at`: the new block is the else branch's tail.
    let into = |t: usize| -> Vec<usize> {
        (0..f.ops.len())
            .filter(|&i| jump_targets(f, i).contains(&t))
            .collect()
    };
    match into(c + 2)[..] {
        [j] if matches!(f.ops[j], Opcode::JFalse { .. }) => {}
        _ => bail!("SkillTip: `ap = 0` is not entered by one JFalse"),
    }
    if !into(c + 3).is_empty() || into(at) != [c + 1] {
        bail!("SkillTip: unexpected jumps around `ap = 0`");
    }

    let skill_t = f.regs[skill.0 as usize];
    let (props, props_t) = field_of_virtual(code, skill_t, "props")?;
    let (ap_cost, ap_t) = field_of_virtual(code, props_t, "apCost")?;
    if ap_t != f.regs[ap.0 as usize] {
        bail!("SkillTip: props.apCost type differs from the cost register");
    }
    Ok(Plan {
        fi,
        at,
        bu,
        skill,
        ap,
        props,
        props_t,
        ap_cost,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    f.regs.push(p.props_t);
    let props = Reg((f.regs.len() - 1) as u32);
    let mut ops = vec![
        Opcode::JNotNull {
            reg: p.bu,
            offset: 0,
        },
        Opcode::JNull {
            reg: p.skill,
            offset: 0,
        },
        Opcode::Field {
            dst: props,
            obj: p.skill,
            field: p.props,
        },
        Opcode::JNull {
            reg: props,
            offset: 0,
        },
        Opcode::Field {
            dst: p.ap,
            obj: props,
            field: p.ap_cost,
        },
    ];
    let end = ops.len();
    resolve_jumps(&mut ops, &[(0, end), (1, end), (3, end)]);
    insert_ops(f, p.at, ops);
    eprintln!(
        "patched skill cost fn@{}: tooltips outside battle show the data Valor cost",
        f.findex.0
    );
}

/// Shows the skill data's Valor cost in tooltips of non-player units outside a
/// battle, or leaves `code` untouched and logs why.
pub(crate) fn patch_skill_cost(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("skill cost skipped: {e:#}")),
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
        let p = plan(&orig).expect("plan");
        let (fi, at, bu, skill, ap) = (p.fi, p.at, p.bu, p.skill, p.ap);
        let mut code = read(&image);
        patch_skill_cost(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 5);
        assert_eq!(b.regs.len(), a.regs.len() + 1);
        let props = Reg(a.regs.len() as u32);
        let new = format!("{:?}", &b.ops[at..at + 5]);
        assert_eq!(
            new,
            format!(
                "{:?}",
                [
                    Opcode::JNotNull { reg: bu, offset: 4 },
                    Opcode::JNull {
                        reg: skill,
                        offset: 3
                    },
                    Opcode::Field {
                        dst: props,
                        obj: skill,
                        field: field_of_virtual(&orig, a.regs[skill.0 as usize], "props")
                            .unwrap()
                            .0
                    },
                    Opcode::JNull {
                        reg: props,
                        offset: 1
                    },
                    Opcode::Field {
                        dst: ap,
                        obj: props,
                        field: field_of_virtual(&orig, b.regs[props.0 as usize], "apCost")
                            .unwrap()
                            .0
                    },
                ]
            )
        );
        // Every original op keeps its place modulo the shift, and every jump its target.
        let map = |i: usize| if i < at { i } else { i + 5 };
        for i in 0..a.ops.len() {
            let ta: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), ta, "op {i}");
            if ta.is_empty() {
                assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[map(i)]));
            }
        }
        // The player branch's jump past `ap = 0` also skips the new block.
        assert_eq!(jump_targets(b, at - 3), [at + 5]);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_skill_cost(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
