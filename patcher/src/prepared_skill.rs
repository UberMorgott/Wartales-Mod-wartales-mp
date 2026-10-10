// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A prepared (two-turn) skill that its script refuses on the second turn is
// cancelled instead of freezing the battle.
//
// `Battle.setCurrentSkill(sk, onEnd)` (Battle.hx:1629) runs a unit's pending
// skill straight away:
//
//   if (currentUnit.hasPendingSkill(sk.id)) {
//     currentSkill.evaluate(pending.x, pending.y);
//     executeSkill(currentSkill, _onEndCurrentSkill);   // throws when not allowed
//     return true;
//   }
//
// `executeSkill` throws "Can't execute not allowed skill" when the evaluation is
// refused (`eval.noAllowReason != null`), e.g. a skill script's onEval
// `dontAllow()` that held when the skill was prepared and no longer does
// (Remastered LucillaVengeance: no foe with Sentence left). The pending path is
// reached from `Unit.endTurn` (Unit.hx:6138), whose turn continuation is
// `onEnd`; the throw escapes the AI end-turn closure and the battle stays on
// that unit forever. The pass inserts before the `executeSkill` call:
//
//   var s = currentSkill;
//   if (s != null && s.eval != null && !s.eval.isAllowed()) {
//     if (currentUnit != null) currentUnit.cancelZone();  // Targeting, zone, pendingSkill
//     s.clearPreview();                                    // as onEndCurrentSkill
//     currentSkill = null;
//     if (onEnd != null) onEnd();                          // the turn ends as usual
//     return true;
//   }
//
// `cancelZone` is the vanilla cancel of a prepared skill (consumes the Targeting
// status, removes the zone, clears `pendingSkill`; host only). The cleanup and
// the `currentSkill = null` are what `onEndCurrentSkill` does first; its tail
// (re-entering `endTurn` / `beginAction`) is skipped: the caller's `onEnd`
// already ends the turn. An allowed skill keeps the vanilla path.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    at: usize,
    cur_skill: RefField,
    cur_unit: RefField,
    eval: RefField,
    is_allowed: RefFun,
    cancel_slot: Option<RefField>,
    cancel_fun: RefFun,
    cleanup: RefField,
    skill_t: RefType,
    eval_t: RefType,
    unit_t: RefType,
    void_r: Reg,
    bool_r: Reg,
    on_end: Reg,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let eval_t = obj_type(code, "battle.skill.SkillEval")?;
    let f = method(code, battle_t, "setCurrentSkill")?;
    let fi = fun_index(code, f.findex)?;
    let (cur_skill, skill_t) = field(code, battle_t, "currentSkill")?;
    let (cur_unit, cu_t) = field(code, battle_t, "currentUnit")?;
    if cu_t != unit_t {
        bail!("setCurrentSkill: Battle.currentUnit is not a battle.Unit");
    }
    let (eval, ev_t) = field(code, skill_t, "eval")?;
    if ev_t != eval_t {
        bail!("setCurrentSkill: Skill.eval is not a SkillEval");
    }
    let has_pending = method(code, unit_t, "hasPendingSkill")?.findex;
    let evaluate = method(code, skill_t, "evaluate")?.findex;
    let execute = method(code, battle_t, "executeSkill")?.findex;
    let is_allowed_f = method(code, eval_t, "isAllowed")?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    if fun_args(code, is_allowed_f) != [eval_t] || sig(code, is_allowed_f.findex)?.1 != bool_t {
        bail!("setCurrentSkill: SkillEval.isAllowed is not (SkillEval) -> Bool");
    }
    // executeSkill throws on exactly this test.
    let ef = &code.functions[fun_index(code, execute)?];
    if !matches!(
        (ef.ops.first(), ef.ops.get(1), ef.ops.get(3)),
        (Some(Opcode::NullCheck { .. }), Some(Opcode::Field { field, .. }), Some(Opcode::Call1 { fun, .. }))
            if *field == eval && *fun == is_allowed_f.findex
    ) {
        bail!("setCurrentSkill: executeSkill does not start with the eval.isAllowed() test");
    }

    // The arguments: this, sk, onEnd.
    let args = fun_args(code, f);
    if args.len() != 3 || args[0] != battle_t || !matches!(code.types[args[2].0], Type::Fun(_)) {
        bail!("setCurrentSkill: unexpected signature");
    }
    let on_end = Reg(2);

    let pend: Vec<usize> = f
        .ops
        .iter()
        .enumerate()
        .filter(|(_, o)| matches!(o, Opcode::Call2 { fun, .. } if *fun == has_pending))
        .map(|(i, _)| i)
        .collect();
    let [h] = pend[..] else {
        bail!("setCurrentSkill: hasPendingSkill not called once");
    };
    let Some(e) = (h..f.ops.len())
        .find(|&i| matches!(&f.ops[i], Opcode::CallN { fun, .. } if *fun == evaluate))
    else {
        bail!("setCurrentSkill: no evaluate after hasPendingSkill");
    };
    let at = e + 1;
    let (void_r, bool_r) = match (
        f.ops.get(at),
        f.ops.get(at + 1),
        f.ops.get(at + 2),
        f.ops.get(at + 3),
    ) {
        (
            Some(Opcode::GetThis { dst: s, field }),
            Some(Opcode::Call3 {
                dst,
                fun,
                arg0,
                arg1,
                ..
            }),
            Some(Opcode::Bool { dst: b, value }),
            Some(Opcode::Ret { ret }),
        ) if *field == cur_skill
            && *fun == execute
            && arg0.0 == 0
            && arg1 == s
            && value.0
            && ret == b =>
        {
            (*dst, *b)
        }
        (Some(Opcode::GetThis { field, .. }), Some(Opcode::JNull { .. }), ..)
            if *field == cur_skill =>
        {
            bail!("setCurrentSkill: already applied")
        }
        _ => {
            bail!("setCurrentSkill: the pending path is not `evaluate; executeSkill; return true`")
        }
    };
    if (0..f.ops.len()).any(|i| jump_targets(f, i).iter().any(|&t| t > e && t <= at + 3)) {
        bail!("setCurrentSkill: a jump lands inside the pending executeSkill");
    }

    // onEndCurrentSkill's cleanup: `if (currentSkill != null) currentSkill.<m>()`.
    let oe = method(code, battle_t, "onEndCurrentSkill")?;
    let cleanup = match (
        oe.ops.get(1),
        oe.ops.get(2),
        oe.ops.get(3),
        oe.ops.get(4),
        oe.ops.get(5),
    ) {
        (
            Some(Opcode::GetThis { field: f1, .. }),
            Some(Opcode::JNull { .. }),
            Some(Opcode::GetThis { dst, field: f2 }),
            Some(Opcode::NullCheck { .. }),
            Some(Opcode::CallMethod { field, args, .. }),
        ) if *f1 == cur_skill && *f2 == cur_skill && args[..] == [*dst] => *field,
        _ => bail!("setCurrentSkill: onEndCurrentSkill does not start with the skill cleanup"),
    };
    if !obj(code, skill_t)?
        .protos
        .iter()
        .any(|q| s(code, q.name) == "clearPreview" && q.pindex == cleanup.0 as i32)
    {
        bail!("setCurrentSkill: onEndCurrentSkill's cleanup is not Skill.clearPreview");
    }

    // Unit.cancelZone(): virtual when it has a vtable slot.
    let cz = method(code, unit_t, "cancelZone")?;
    if fun_args(code, cz) != [unit_t] {
        bail!("setCurrentSkill: Unit.cancelZone is not (Unit) -> Void");
    }
    let slot = obj(code, unit_t)?
        .protos
        .iter()
        .find(|p| s(code, p.name) == "cancelZone")
        .map(|p| p.pindex);
    let cancel_slot = slot.filter(|&p| p >= 0).map(|p| RefField(p as usize));

    Ok(Plan {
        fi,
        at,
        cur_skill,
        cur_unit,
        eval,
        is_allowed: is_allowed_f.findex,
        cancel_slot,
        cancel_fun: cz.findex,
        cleanup,
        skill_t,
        eval_t,
        unit_t,
        void_r,
        bool_r,
        on_end,
    })
}

const NEW_OPS: usize = 16;

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let sk = new_reg(f, p.skill_t);
    let ev = new_reg(f, p.eval_t);
    let u = new_reg(f, p.unit_t);
    let cancel_zone = match p.cancel_slot {
        Some(field) => Opcode::CallMethod {
            dst: p.void_r,
            field,
            args: vec![u],
        },
        None => Opcode::Call1 {
            dst: p.void_r,
            fun: p.cancel_fun,
            arg0: u,
        },
    };
    let mut ops = vec![
        /* 0 */
        Opcode::GetThis {
            dst: sk,
            field: p.cur_skill,
        },
        /* 1 */ Opcode::JNull { reg: sk, offset: 0 },
        /* 2 */
        Opcode::Field {
            dst: ev,
            obj: sk,
            field: p.eval,
        },
        /* 3 */ Opcode::JNull { reg: ev, offset: 0 },
        /* 4 */
        Opcode::Call1 {
            dst: p.bool_r,
            fun: p.is_allowed,
            arg0: ev,
        },
        /* 5 */ Opcode::JTrue {
            cond: p.bool_r,
            offset: 0,
        },
        /* 6 */ Opcode::GetThis {
            dst: u,
            field: p.cur_unit,
        },
        /* 7 */ Opcode::JNull { reg: u, offset: 0 },
        /* 8 */ cancel_zone,
        /* 9 */
        Opcode::CallMethod {
            dst: p.void_r,
            field: p.cleanup,
            args: vec![sk],
        },
        /* 10 */ Opcode::Null { dst: sk },
        /* 11 */
        Opcode::SetThis {
            field: p.cur_skill,
            src: sk,
        },
        /* 12 */ Opcode::JNull {
            reg: p.on_end,
            offset: 0,
        },
        /* 13 */
        Opcode::CallClosure {
            dst: p.void_r,
            fun: p.on_end,
            args: vec![],
        },
        /* 14 */
        Opcode::Bool {
            dst: p.bool_r,
            value: hlbc::types::ValBool(true),
        },
        /* 15 */ Opcode::Ret { ret: p.bool_r },
    ];
    let end = ops.len();
    debug_assert_eq!(end, NEW_OPS);
    resolve_jumps(&mut ops, &[(1, end), (3, end), (5, end), (7, 9), (12, 14)]);
    insert_ops(f, p.at, ops);
    eprintln!(
        "patched prepared skill fn@{}: a refused pending skill is cancelled, the turn goes on",
        f.findex.0
    );
}

/// Cancels a unit's prepared skill that its script refuses when it fires,
/// instead of throwing out of the end of turn, or leaves `code` untouched and
/// logs why.
pub(crate) fn patch_prepared_skill(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("prepared skill skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted};
    use crate::testsim::{Sim, V};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at) = (p.fi, p.at);
        // cancelZone is virtual (Beast overrides it): dispatched, not called directly.
        assert!(p.cancel_slot.is_some());

        let mut code = read(&image);
        patch_prepared_skill(&mut code);
        let patched = crate::asm::testutil::write(&code);
        let back = read(&patched);
        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        shifted(a, b, at, NEW_OPS);
        assert_eq!(b.regs.len(), a.regs.len() + 3);
        check_types(&back, b, at..at + NEW_OPS);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_prepared_skill(&mut again);
        assert!(crate::asm::testutil::write(&again) == patched);
    }

    /// Runs the patched setCurrentSkill with a pending skill whose evaluation is
    /// refused or allowed; returns (result, calls in order).
    fn run(code: &Bytecode, refused: bool, with_on_end: bool) -> (V, Vec<&'static str>, V) {
        let battle_t = obj_type(code, "battle.Battle").unwrap();
        let unit_t = obj_type(code, "battle.Unit").unwrap();
        let eval_t = obj_type(code, "battle.skill.SkillEval").unwrap();
        let f = method(code, battle_t, "setCurrentSkill").unwrap();
        let (cur_skill, skill_t) = field(code, battle_t, "currentSkill").unwrap();
        let (cur_unit, _) = field(code, battle_t, "currentUnit").unwrap();
        let (eval, _) = field(code, skill_t, "eval").unwrap();
        let (pending, _) = field(code, unit_t, "pendingSkill").unwrap();
        let (reason, _) = field(code, eval_t, "noAllowReason").unwrap();
        let m = |t, n| method(code, t, n).unwrap().findex;
        let (has_pending, create, execute, weapon, is_allowed, cancel_zone) = (
            m(unit_t, "hasPendingSkill"),
            m(battle_t, "createSkill"),
            m(battle_t, "executeSkill"),
            m(unit_t, "getWeaponObject"),
            m(eval_t, "isAllowed"),
            m(unit_t, "cancelZone"),
        );
        let cz_slot = obj(code, unit_t)
            .unwrap()
            .protos
            .iter()
            .find(|q| s(code, q.name) == "cancelZone")
            .map(|q| q.pindex as usize);
        let on_end_fn = RefFun(usize::MAX);
        let mut sim = Sim::new(
            code,
            code.functions.len(),
            move |c, g, a| {
                let hit = |c: &mut crate::testsim::Core, k: &'static str| {
                    c.log.push((k, a.to_vec()));
                };
                if g == weapon {
                    Some(V::Null)
                } else if g == has_pending {
                    Some(V::B(true))
                } else if g == create {
                    Some(c.map("t", "skill"))
                } else if g == is_allowed {
                    Some(V::B(c.get(&a[0], reason) == V::Null))
                } else if g == execute {
                    hit(c, "execute");
                    Some(V::Null)
                } else if g == cancel_zone {
                    hit(c, "cancelZone");
                    Some(V::Null)
                } else if g == on_end_fn {
                    hit(c, "onEnd");
                    Some(V::Null)
                } else if fname(code, g) == "evaluate" {
                    hit(c, "evaluate");
                    Some(V::Null)
                } else if fname(code, g) == "check" {
                    Some(V::B(false))
                } else {
                    None
                }
            },
            move |c, slot, a| {
                if Some(slot) == cz_slot {
                    c.log.push(("cancelZone", a.to_vec()));
                } else {
                    c.log.push(("cleanup", a.to_vec()));
                }
                V::Null
            },
        );
        let c = &mut sim.c;
        let why = if refused { c.enm(0, vec![]) } else { V::Null };
        let ev = c.obj(&[(reason, why)]);
        let skill = c.obj(&[(eval, ev)]);
        c.put("t", "skill", skill);
        let pend = c.obj(&[]);
        let unit = c.obj(&[(pending, pend)]);
        let battle = c.obj(&[(cur_unit, unit)]);
        let inf = c.obj(&[]);
        let on_end = if with_on_end {
            V::Fun(on_end_fn)
        } else {
            V::Null
        };
        let ret = sim.run(f.findex, vec![battle.clone(), inf, on_end]);
        let calls = sim.c.log.iter().map(|(k, _)| *k).collect();
        let cur = sim.c.get(&battle, cur_skill);
        (ret, calls, cur)
    }

    #[test]
    fn refused_pending_skill_is_cancelled_and_the_turn_ends() {
        let Some(image) = game() else { return };
        let mut code = read(&image);
        patch_prepared_skill(&mut code);

        let (ret, calls, cur) = run(&code, true, true);
        assert_eq!(ret, V::B(true));
        assert_eq!(calls, ["evaluate", "cancelZone", "cleanup", "onEnd"]);
        assert_eq!(cur, V::Null, "the refused skill is not left current");

        let (ret, calls, _) = run(&code, true, false);
        assert_eq!(ret, V::B(true));
        assert_eq!(calls, ["evaluate", "cancelZone", "cleanup"]);

        // Allowed: vanilla, executeSkill runs and nothing is cancelled.
        let (ret, calls, cur) = run(&code, false, true);
        assert_eq!(ret, V::B(true));
        assert_eq!(calls, ["evaluate", "execute"]);
        assert_ne!(cur, V::Null);
    }
}
