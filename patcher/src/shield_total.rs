// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The shield counter of a battle unit's health bars shows the unit's total
// damage reduction, not its Guard alone.
//
// That counter (ui.comp.HealthBars.guard, a percent Counter) is the unit's
// Guard: HealthBars.sync sets it every frame in battle to
// `min(GuardMaxValue, battle.Unit.get_guard())`. Armor is not a percentage at
// all: it is a pool the hit empties before health. The game's damage formula
// (Skill.doEval, SkillTargetPreview.calcDamages) applies two reductions of the
// target's own:
//
//   dmg = ceil(base * (100 + attackerBonus + damageTakenPercent - damageReducePercent) / 100)
//   dmg = dmg - ceil(dmg * guard / 100)        (guard capped at GuardMaxValue)
//
// so against an attacker without bonuses the unit takes
// `(100 + taken - reduce) * (100 - guard) / 100` percent. The pass replaces the
// value sync passes to the counter's set_value with
//
//   100 - ceil(max(0, 100 + taken - reduce) * (100 - guard) / 100)
//
// (`guard` the capped value vanilla already computed; the ceil follows the
// game's rounding in the attacker's favour). Left out, as attacker-side: the
// attacker's damage bonuses, Guard ignore and backstab, skills that ignore
// damage reduction, damage-type resistances (they depend on the attack's
// type) and the crit-only damageTakenCritPercent. The "maxed" style still
// compares the capped Guard (one op: its `guard.value` read becomes the saved
// Guard), so it marks a Guard at its cap as before; the counter shows (as
// vanilla) when its value or the unit's initial Guard is above zero, so a unit
// with only a damage-reduction status now shows it too. rebuild (the value
// before the first sync, and the panels outside battle) and the Guard tooltip
// are unchanged. Reads only the unit's local stats: the same on host and guest.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The `guard.set_value(min)` CallMethod; the new ops go in front of it.
    at: usize,
    /// The capped Guard register (the set_value argument).
    v: Reg,
    /// The JAlways of the first `min` branch, which jumps to `at`.
    join: usize,
    /// The `x = guard.value` read of the "maxed" test.
    maxed: usize,
    i32_t: RefType,
    stats_t: RefType,
    unit_t: RefType,
    battle: RefField,
    stats: RefField,
    taken: RefField,
    reduce: RefField,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bars_t = obj_type(code, "ui.comp.HealthBars")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let stats_t = obj_type(code, "st.UnitStats")?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let battle = typed(code, bars_t, "battle", unit_t)?;
    let (guard, counter_t) = field(code, bars_t, "guard")?;
    let (value, _) = field(code, counter_t, "value")?;
    let stats = typed(code, unit_t, "stats", stats_t)?;
    let taken = typed(code, stats_t, "damageTakenPercent", i32_t)?;
    let reduce = typed(code, stats_t, "damageReducePercent", i32_t)?;
    let get_guard = method(code, unit_t, "get_guard")?.findex;
    let f = method(code, bars_t, "sync")?;
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    // `r` was last written by `this.guard` before op `k`.
    let is_guard = |r: Reg, k: usize| {
        (0..k).rev().find_map(|j| match o[j] {
            Opcode::GetThis { dst, field } if dst == r => Some(field == guard),
            _ => None,
        }) == Some(true)
    };

    // g = get_guard(battle); if (max >= g) v = g else v = max; c.set_value(v), c = this.guard
    let sites: Vec<usize> = (6..o.len())
        .filter(|&k| {
            let Opcode::CallMethod { args, .. } = &o[k] else {
                return false;
            };
            let [c, v] = args[..] else { return false };
            let Opcode::Call1 { dst: g, fun, .. } = o[k - 6] else {
                return false;
            };
            let Opcode::JSGte {
                a: max,
                b,
                offset: 2,
            } = o[k - 4]
            else {
                return false;
            };
            fun == get_guard
                && b == g
                && is_guard(c, k)
                && matches!(o[k - 3], Opcode::Mov { dst, src } if dst == v && src == max)
                && matches!(o[k - 2], Opcode::JAlways { offset: 1 })
                && matches!(o[k - 1], Opcode::Mov { dst, src } if dst == v && src == g)
        })
        .collect();
    let [at] = sites[..] else {
        // Vanilla sync never reads damageReducePercent.
        if o.iter()
            .any(|op| matches!(op, Opcode::Field { field, .. } if *field == reduce))
        {
            bail!("HealthBars.sync: already applied");
        }
        bail!(
            "HealthBars.sync: {} guard set_value(min) sites, want 1",
            sites.len()
        );
    };
    let Opcode::CallMethod { args, .. } = &o[at] else {
        unreachable!()
    };
    let v = args[1];
    let Opcode::JSGte { a: max, .. } = o[at - 4] else {
        unreachable!()
    };
    // Only the `min` branch jumps into the set_value op.
    let into: Vec<usize> = (0..o.len())
        .filter(|&i| jump_targets(f, i).contains(&at))
        .collect();
    if into != [at - 2] {
        bail!("HealthBars.sync: set_value op is a jump target of {into:?}");
    }
    // The "maxed" test: x = this.guard.value; if (x < max) ..
    let reads: Vec<usize> = (at + 1..o.len() - 1)
        .filter(|&i| {
            matches!((&o[i], &o[i + 1]),
                (Opcode::Field { dst, obj, field }, Opcode::JSLt { a, b, .. })
                if a == dst && *b == max && *field == value && is_guard(*obj, i))
        })
        .collect();
    let [maxed] = reads[..] else {
        bail!("HealthBars.sync: {} maxed tests, want 1", reads.len());
    };
    Ok(Plan {
        fi,
        at,
        v,
        join: at - 2,
        maxed,
        i32_t,
        stats_t,
        unit_t,
        battle,
        stats,
        taken,
        reduce,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let k0 = int_const(code, 0);
    let k99 = int_const(code, 99);
    let k100 = int_const(code, 100);
    let f = &mut code.functions[p.fi];
    let g = new_reg(f, p.i32_t);
    let r = new_reg(f, p.i32_t);
    let t = new_reg(f, p.i32_t);
    let u = new_reg(f, p.unit_t);
    let st = new_reg(f, p.stats_t);
    let v = p.v;
    let mut a = crate::asm::Asm::new();
    a.op(Opcode::Mov { dst: g, src: v });
    a.op(Opcode::GetThis {
        dst: u,
        field: p.battle,
    });
    a.op(Opcode::NullCheck { reg: u });
    a.op(Opcode::Field {
        dst: st,
        obj: u,
        field: p.stats,
    });
    a.op(Opcode::NullCheck { reg: st });
    // r = max(0, 100 + taken - reduce): percent of the hit left before Guard
    a.op(Opcode::Field {
        dst: r,
        obj: st,
        field: p.taken,
    });
    a.op(Opcode::Field {
        dst: t,
        obj: st,
        field: p.reduce,
    });
    a.op(Opcode::Sub { dst: r, a: r, b: t });
    a.op(Opcode::Int { dst: t, ptr: k100 });
    a.op(Opcode::Add { dst: r, a: r, b: t });
    a.op(Opcode::Int { dst: t, ptr: k0 });
    a.jmp(
        Opcode::JSGte {
            a: r,
            b: t,
            offset: 0,
        },
        "pos",
    );
    a.op(Opcode::Mov { dst: r, src: t });
    a.label("pos");
    // v = 100 - ceil(r * (100 - g) / 100)
    a.op(Opcode::Int { dst: v, ptr: k100 });
    a.op(Opcode::Sub { dst: t, a: v, b: g });
    a.op(Opcode::Mul { dst: r, a: r, b: t });
    a.op(Opcode::Int { dst: t, ptr: k99 });
    a.op(Opcode::Add { dst: r, a: r, b: t });
    a.op(Opcode::SDiv { dst: r, a: r, b: v });
    a.op(Opcode::Sub { dst: v, a: v, b: r });
    let ops = a.finish();
    let n = ops.len();
    insert_ops(f, p.at, ops);
    // The `v = max` branch jumped to set_value; it now runs the new ops too.
    let Opcode::JAlways { offset } = &mut f.ops[p.join] else {
        unreachable!()
    };
    *offset = (p.at - p.join - 1) as i32;
    let Opcode::Field { dst, .. } = f.ops[p.maxed + n] else {
        unreachable!()
    };
    f.ops[p.maxed + n] = Opcode::Mov { dst, src: g };
    eprintln!(
        "patched shield total fn@{}: guard counter shows Guard + damage reduction (op {})",
        f.findex.0, p.at
    );
}

/// Makes the battle shield counter show the unit's total damage reduction, or leaves `code` untouched and logs why.
pub(crate) fn patch_shield_total(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("shield total skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, write};
    use crate::testsim::{Sim, V};

    /// The inserted arithmetic, run in Rust: (taken, reduce, guard) -> shown %.
    fn shown(taken: i32, reduce: i32, g: i32) -> i32 {
        let r = (100 + taken - reduce).max(0);
        100 - (r * (100 - g) + 99) / 100
    }

    #[test]
    fn formula() {
        assert_eq!(shown(0, 0, 0), 0);
        assert_eq!(shown(0, 0, 30), 30);
        assert_eq!(shown(0, 20, 0), 20);
        assert_eq!(shown(0, 20, 30), 44); // 0.8 * 0.7 = 0.56 taken
        assert_eq!(shown(20, 0, 30), 16); // 1.2 * 0.7 = 0.84
        assert_eq!(shown(0, 150, 30), 100);
        assert_eq!(shown(0, 15, 25), 36); // 85 * 75 = 63.75 -> 64 taken
    }

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, join, maxed) = (p.fi, p.at, p.join, p.maxed);
        let mut code = read(&image);
        patch_shield_total(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let n = b.ops.len() - a.ops.len();
        check_flow(b);
        check_types(&back, b, at..at + n);
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        // Unchanged outside the block, but the join jump and the maxed read.
        for i in 0..a.ops.len() {
            let j = if i < at { i } else { i + n };
            if i == join || i == maxed {
                continue;
            }
            let ta: Vec<usize> = jump_targets(a, i)
                .into_iter()
                .map(|t| if t < at { t } else { t + n })
                .collect();
            if ta.is_empty() {
                assert_eq!(
                    format!("{:?}", a.ops[i]),
                    format!("{:?}", b.ops[j]),
                    "op {i}"
                );
            } else {
                assert_eq!(jump_targets(b, j), ta, "op {i}");
            }
        }
        // Both min branches reach the block, then set_value.
        assert_eq!(jump_targets(b, join), vec![at]);
        assert!(matches!(b.ops[at - 1], Opcode::Mov { .. }));
        assert!(matches!(b.ops[at + n], Opcode::CallMethod { .. }));
        assert!(matches!(b.ops[maxed + n], Opcode::Mov { .. }));
        assert!(matches!(b.ops[maxed + n + 1], Opcode::JSLt { .. }));

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_shield_total(&mut again);
        assert!(write(&again) == patched);
    }

    /// Runs the patched guard block of sync: (counter value, "maxed") for a unit
    /// with Guard `guard`, damageTakenPercent `taken`, damageReducePercent `reduce`.
    fn run(code: &Bytecode, p: &Plan, n: usize, guard: i32, taken: i32, reduce: i32) -> (V, V) {
        const MAX: f64 = 50.0;
        let b = &code.functions[p.fi];
        let unit_t = p.unit_t;
        let get_guard = method(code, unit_t, "get_guard").unwrap().findex;
        let first = b
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == get_guard))
            .unwrap();
        let stop = (p.maxed + n..b.ops.len())
            .find(|&i| matches!(b.ops[i], Opcode::Null { .. }))
            .unwrap();
        let const_t = b
            .ops
            .iter()
            .find_map(|o| match o {
                Opcode::Call1 { dst, fun, .. } if fname(code, *fun) == "getConst" => {
                    Some(b.regs[dst.0 as usize])
                }
                _ => None,
            })
            .unwrap();
        let (cval, _) = field_of_virtual(code, const_t, "value").unwrap();
        let bars_t = b.regs[0];
        let counter_t = obj_type(code, "ui.comp.Counter").unwrap();
        let value_f = field(code, counter_t, "value").unwrap().0;
        let fld = |t, s| field(code, t, s).unwrap().0;
        let mut s = Sim::new(
            code,
            code.functions.len(),
            move |c, f, a| {
                Some(match fname(code, f) {
                    "get_guard" => V::I(guard),
                    "getConst" => c.obj(&[(cval, V::F(MAX))]),
                    "set_visible" => a[1].clone(),
                    "toggleClass" => {
                        c.log.push(("maxed", vec![a[2].clone()]));
                        V::Null
                    }
                    n => panic!("unexpected call {n}@{}", f.0),
                })
            },
            move |c, _, a| {
                c.set(&a[0], value_f, a[1].clone());
                a[1].clone()
            },
        );
        let stats = s.c.obj(&[(p.taken, V::I(taken)), (p.reduce, V::I(reduce))]);
        let unit = s.c.obj(&[(p.stats, stats)]);
        let dom = s.c.obj(&[]);
        let counter = s.c.obj(&[(fld(counter_t, "dom"), dom)]);
        let this = s.c.obj(&[
            (p.battle, unit),
            (fld(bars_t, "guard"), counter.clone()),
            (fld(bars_t, "alwaysShowGuard"), V::B(false)),
            (fld(bars_t, "guardInitialValue"), V::I(guard)),
        ]);
        let mut r = vec![V::Null; b.regs.len()];
        r[0] = this;
        assert_eq!(s.span(b.findex, &mut r, first - 2, &[stop]), stop);
        let maxed = s.c.take("maxed").pop().unwrap().pop().unwrap();
        (s.c.get(&counter, value_f), maxed)
    }

    #[test]
    fn shows_formula_value() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut code = read(&image);
        let p = plan(&orig).expect("plan");
        let n = {
            let before = orig.functions[p.fi].ops.len();
            patch_shield_total(&mut code);
            code.functions[p.fi].ops.len() - before
        };
        for (g, taken, reduce) in [
            (0, 0, 0),
            (30, 0, 0),
            (30, 0, 20),
            (80, 0, 0),
            (20, 0, 40),
            (30, 20, 0),
            (0, 0, 15),
            (25, 0, 15),
            (0, 0, 150),
            (80, 0, 20),
            (0, 30, 0),
        ] {
            let gc = g.min(50);
            let (v, maxed) = run(&code, &p, n, g, taken, reduce);
            assert_eq!(
                v,
                V::I(shown(taken, reduce, gc)),
                "guard {g} taken {taken} reduce {reduce}"
            );
            // "maxed" still means the Guard itself is at its cap (vanilla: value >= max, max 0 without Guard).
            let max = if g > 0 { 50 } else { 0 };
            assert_eq!(maxed, V::B(gc >= max), "maxed for guard {g}");
        }
        assert_eq!(run(&code, &p, n, 80, 0, 20).0, V::I(60)); // 0.8 * 0.5 taken
        assert_eq!(run(&code, &p, n, 0, 30, 0).0, V::I(-30)); // vulnerability keeps its sign
    }

    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let base = write(&orig);
        // Other set_value argument source, min branch not via JAlways, maxed test on another value.
        let edits: [fn(&mut Function, &Plan); 3] = [
            |f, p| {
                if let Opcode::Mov { src, .. } = &mut f.ops[p.at - 1] {
                    *src = Reg(0);
                }
            },
            |f, p| f.ops[p.join] = Opcode::Nop,
            |f, p| {
                if let Opcode::JSLt { b, .. } = &mut f.ops[p.maxed + 1] {
                    *b = Reg(0);
                }
            },
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code.functions[p.fi], &p);
            let before = write(&code);
            assert!(before != base, "edit {k} was a no-op");
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_shield_total(&mut code);
            assert!(write(&code) == before, "edit {k} changed the code");
        }
    }
}
