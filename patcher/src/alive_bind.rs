// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A batch of battle entities received in one flush is bound to the battle
// before any of them initialises (issue #8).
//
// `Game.initAlive` (host and guests) first binds every pending battle.Entity of
// aliveStates whose `battle` is still null to `Game.battle`, then runs its
// vanilla loop (one shift + init per state). Vanilla sets `battle.Entity.battle`
// only in that entity's own init (Entity.hx:22), one state at a time, while the
// whole batch is already in `battle.state.allUnits`: e.g. a guest receiving
// several summoned rats in one flush inits rat 1 -> genHud ->
// HealthBars.rebuild -> Unit.get_guard -> hasSkills walks allUnits ->
// getDistance on rat 2 (battle null) -> `Null access .grid`; rat 1 stays
// half-initialised and every later update / battleNextTurn throws the same
// (battle hangs). The host never queues such a batch: a unit it creates runs
// init right away, before the next one joins allUnits. Binding only what init
// itself would assign first (Game.inst.battle; initAlive runs on Game.inst)
// leaves init running once, unchanged.
//
//   if (aliveStates == null) return; if (lockAlives) <block>;   (lockAlives jumps to the block)
//   <block>: if (this.battle != null) for (st in aliveStates)
//                if (Std.isOfType(st, battle.Entity) && st.battle == null) st.battle = this.battle;
//   <vanilla loop head>
//
// Stands alone; net_guard.rs part A (initAlive before every network handler on
// a guest) makes the batch alive early, this makes its inits order-safe.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::Asm;
use super::*;
use hlbc::types::{RefGlobal, RefInt};

struct Plan {
    /// Game.initAlive's function index.
    fi: usize,
    /// Its loop head (a Label), where the block goes.
    at: usize,
    /// The `lockAlives` test that jumps to the loop head (now to the block).
    lock_jump: usize,
    /// Game.aliveStates, Game.battle (and its type), battle.Entity.battle.
    alive_states: RefField,
    g_battle: RefField,
    battle_t: RefType,
    e_battle: RefField,
    /// battle.Entity and its class global (type `battle.$Entity`).
    ent_t: RefType,
    ent_cls: RefGlobal,
    ent_cls_t: RefType,
    /// hl.BaseType.check (`Std.isOfType` for a class), as initAlive uses it.
    check: RefFun,
    /// hl.types.ArrayObj and its length / .array (and the array's type).
    arr_t: RefType,
    a_len: RefField,
    a_raw: RefField,
    a_raw_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
}

/// Validates Game.initAlive (`if (aliveStates == null || lockAlives) return;`
/// then a Label-headed shift/init loop) and battle.Entity.init (sets
/// `this.battle` from `Game.inst.battle`).
fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let (g_cls, g_cls_t) = class_global(code, "Game")?;
    let g_inst = typed(code, g_cls_t, "inst", game_t)?;
    let f = method(code, game_t, "initAlive")?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    if fun_args(code, f) != [game_t] || f.t.as_fun(code).map(|t| t.ret) != Some(void_t) {
        bail!("unexpected Game.initAlive signature");
    }
    let fi = fun_index(code, f.findex)?;
    let o = &f.ops;
    let at = o
        .iter()
        .position(|op| matches!(op, Opcode::Label))
        .context("initAlive: no loop head")?;
    let (alive_states, as_t) = field(code, game_t, "aliveStates")?;
    if as_t != arr_t {
        bail!("Game.aliveStates is not an ArrayObj");
    }
    let lock = typed(code, game_t, "lockAlives", bool_t)?;
    let shape = at >= 5
        && matches!(o[at - 5], Opcode::GetThis { field, .. } if field == alive_states)
        && matches!(o[at - 4], Opcode::JNull { .. })
        && jump_targets(f, at - 4) == [at - 1]
        && matches!(o[at - 3], Opcode::GetThis { field, .. } if field == lock)
        && matches!(o[at - 2], Opcode::JFalse { .. })
        && jump_targets(f, at - 2) == [at]
        && matches!(o[at - 1], Opcode::Ret { .. });
    if !shape {
        bail!("initAlive: unexpected aliveStates / lockAlives test (already applied?)");
    }
    // Only the lockAlives test enters the loop head from before it; the
    // loop's own jumps come back from after it.
    let switch_in = (0..o.len())
        .any(|i| matches!(o[i], Opcode::Switch { .. }) && jump_targets(f, i).contains(&at));
    if (0..at).any(|i| i != at - 2 && jump_targets(f, i).contains(&at)) || switch_in {
        bail!("initAlive: another jump enters the loop");
    }
    // The loop: aliveStates.shift() and `check(cls, st)` before init.
    let shift = o[at..]
        .iter()
        .any(|op| matches!(op, Opcode::Call1 { fun, .. } if fname(code, *fun) == "shift"));
    let checks: Vec<RefFun> = o[at..]
        .iter()
        .filter_map(|op| match op {
            Opcode::Call2 { fun, .. } if fname(code, *fun) == "check" => Some(*fun),
            _ => None,
        })
        .collect();
    let [check] = checks[..] else {
        bail!(
            "initAlive: expected one class check, found {}",
            checks.len()
        );
    };
    let (ca, cr) = sig(code, check)?;
    if !shift || ca.len() != 2 || ca[1] != dyn_t || cr != bool_t {
        bail!("initAlive: unexpected loop");
    }
    let ent_t = obj_type(code, "battle.Entity")?;
    let (ent_cls, ent_cls_t) = class_global(code, "battle.Entity")?;
    if !is_sub(code, ent_cls_t, ca[0]) {
        bail!("battle.$Entity is not a {}", ca[0].0);
    }
    let (g_battle, battle_t) = field(code, game_t, "battle")?;
    let e_battle = typed(code, ent_t, "battle", battle_t)?;
    // battle.Entity.init: `this.battle = Game.inst.battle` first.
    let init = method(code, ent_t, "init")?;
    let io = &init.ops;
    let binds = io.len() > 4
        && matches!(io[0], Opcode::GetGlobal { global, .. } if global == g_cls)
        && matches!(io[1], Opcode::Field { field, .. } if field == g_inst)
        && matches!(io[3], Opcode::Field { field, .. } if field == g_battle)
        && matches!(io[4], Opcode::SetThis { field, .. } if field == e_battle);
    if !binds {
        bail!("battle.Entity.init does not start with this.battle = Game.inst.battle");
    }
    let (a_len, len_t) = field(code, arr_t, "length")?;
    let (a_raw, a_raw_t) = field(code, arr_t, "array")?;
    if len_t != i32_t || !matches!(code.types[a_raw_t.0], Type::Array) {
        bail!("unexpected ArrayObj fields");
    }
    Ok(Plan {
        fi,
        at,
        lock_jump: at - 2,
        alive_states,
        g_battle,
        battle_t,
        e_battle,
        ent_t,
        ent_cls,
        ent_cls_t,
        check,
        arr_t,
        a_len,
        a_raw,
        a_raw_t,
        i32_t,
        bool_t,
        dyn_t,
    })
}

/// `if (this.battle != null) for (st in aliveStates) if (check(battle.Entity, st)
/// && st.battle == null) st.battle = this.battle;` through new registers of `f`.
fn block(f: &mut Function, p: &Plan, zero: RefInt) -> Vec<Opcode> {
    let bt = new_reg(f, p.battle_t);
    let cur = new_reg(f, p.battle_t);
    let arr = new_reg(f, p.arr_t);
    let raw = new_reg(f, p.a_raw_t);
    let len = new_reg(f, p.i32_t);
    let i = new_reg(f, p.i32_t);
    let d = new_reg(f, p.dyn_t);
    let cls = new_reg(f, p.ent_cls_t);
    let ok = new_reg(f, p.bool_t);
    let e = new_reg(f, p.ent_t);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: bt,
        obj: Reg(0),
        field: p.g_battle,
    });
    a.jmp(Opcode::JNull { reg: bt, offset: 0 }, "done");
    a.op(Opcode::Field {
        dst: arr,
        obj: Reg(0),
        field: p.alive_states,
    });
    a.op(Opcode::Field {
        dst: len,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.a_raw,
    });
    a.op(Opcode::Int { dst: i, ptr: zero });
    a.loop_head("next");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: len,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.ent_cls,
    });
    a.op(Opcode::Call2 {
        dst: ok,
        fun: p.check,
        arg0: cls,
        arg1: d,
    });
    a.jmp(
        Opcode::JFalse {
            cond: ok,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::SafeCast { dst: e, src: d });
    a.op(Opcode::Field {
        dst: cur,
        obj: e,
        field: p.e_battle,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: cur,
            offset: 0,
        },
        "next",
    );
    a.op(Opcode::SetField {
        obj: e,
        field: p.e_battle,
        src: bt,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "next");
    a.label("done");
    a.finish()
}

fn apply(code: &mut Bytecode, p: Plan) {
    let zero = int_const(code, 0);
    let f = &mut code.functions[p.fi];
    let ops = block(f, &p, zero);
    let n = ops.len();
    insert_ops(f, p.at, ops);
    // The lockAlives test (the only way in from above) lands on the block; the
    // loop's own jumps still land on the head.
    let Opcode::JFalse { offset, .. } = &mut f.ops[p.lock_jump] else {
        unreachable!()
    };
    *offset = (p.at as i64 - p.lock_jump as i64 - 1) as i32;
    eprintln!(
        "patched alive bind fn@{} op {}: initAlive binds every pending battle entity first ({n} ops)",
        f.findex.0, p.at
    );
}

/// Binds a received batch of battle entities to the battle before their inits
/// run, or leaves `code` untouched and logs why.
pub(crate) fn patch_alive_bind(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("alive bind skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    /// Only initAlive changes: 18 ops at the loop head, the lockAlives test lands
    /// on them, every other jump keeps its target; idempotent.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, lock_jump) = (p.fi, p.at, p.lock_jump);
        let mut code = read(&image);
        patch_alive_bind(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.types, orig.types);
        assert_eq!(back.globals, orig.globals);
        assert_eq!(back.ints, orig.ints);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        let n = b.ops.len() - a.ops.len();
        assert_eq!(n, 18);
        let map = |i: usize| if i < at { i } else { i + n };
        for i in 0..a.ops.len() {
            let want: Vec<usize> = if i == lock_jump {
                vec![at]
            } else {
                jump_targets(a, i).into_iter().map(map).collect()
            };
            assert_eq!(jump_targets(b, map(i)), want, "initAlive op {i}");
            if want.is_empty() {
                assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]));
            }
        }
        for j in at..at + n {
            for t in jump_targets(b, j) {
                assert!(t >= at && t <= at + n, "initAlive op {j}");
            }
        }
        assert!(matches!(b.ops[at + n], Opcode::Label));
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        check_types(&back, b, at..at + n);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_alive_bind(&mut again);
        assert!(write(&again) == patched);
    }

    /// Issue #8: Game.initAlive with a batch of pending battle entities. Each
    /// entity's init sees every other pending entity already bound to the
    /// battle (vanilla: the later ones still have `battle == null`); the bind
    /// leaves an already-bound entity's battle untouched (checked before any
    /// init runs: the real init overwrites it), a non-entity state is left
    /// alone, and every entity is still initialised exactly once, in queue order.
    #[test]
    fn init_alive_binds_the_whole_batch_first() {
        use crate::testsim::{Core, Sim, V};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut code = read(&image);
        let pb = plan(&code).expect("plan");
        patch_alive_bind(&mut code);
        let init_alive = code.functions[pb.fi].findex;

        let run = |code: &Bytecode| -> (Vec<Vec<V>>, Vec<V>, V, Vec<V>) {
            let ents = std::cell::RefCell::new(Vec::<V>::new());
            // Battles of every entity as the first init starts (= right after the bind).
            let pre = std::cell::RefCell::new(None::<Vec<V>>);
            let f = &code.functions[pb.fi];
            let (a_len, a_raw, e_battle) = (pb.a_len, pb.a_raw, pb.e_battle);
            let stub = |core: &mut Core, fun: RefFun, args: &[V]| -> Option<V> {
                if fun == pb.check {
                    return Some(V::B(core.key_get(&args[1], "xent") == V::B(true)));
                }
                if fname(code, fun) == "shift" {
                    let a = &args[0];
                    let V::I(n) = core.get(a, a_len) else {
                        panic!("len")
                    };
                    let raw = core.get(a, a_raw);
                    if n == 0 {
                        return Some(V::Null);
                    }
                    let first = core.key_get(&raw, "i0");
                    for k in 1..n {
                        let v = core.key_get(&raw, &format!("i{k}"));
                        core.key_set(&raw, format!("i{}", k - 1), v);
                    }
                    core.set(a, a_len, V::I(n - 1));
                    return Some(first);
                }
                None
            };
            let method = |core: &mut Core, _field: usize, args: &[V]| -> V {
                let me = args[0].clone();
                let bound = ents
                    .borrow()
                    .iter()
                    .filter(|e| **e != me)
                    .all(|e| core.get(e, e_battle) != V::Null);
                if pre.borrow().is_none() {
                    let snap = ents
                        .borrow()
                        .iter()
                        .map(|e| core.get(e, e_battle))
                        .collect();
                    *pre.borrow_mut() = Some(snap);
                }
                core.log.push(("init", vec![me.clone(), V::B(bound)]));
                // Entity.init@8545 ops 0-4: this.battle = Game.inst.battle, unconditionally.
                let bt = core.map("t", "bt");
                core.set(&me, e_battle, bt);
                V::Null
            };
            let mut sim = Sim::new(code, code.functions.len(), stub, method);
            // The loop skips a state whose persist equals a vanilla enum global.
            for w in f.ops.windows(3) {
                if let [Opcode::Field { .. }, Opcode::GetGlobal { global, .. }, Opcode::JNotEq { .. }] =
                    w
                {
                    sim.c.globals.insert(global.0, V::I(-7));
                }
            }
            let bt = sim.c.obj(&[]);
            let other = sim.c.obj(&[]);
            sim.c.put("t", "bt", bt.clone());
            let mk = |c: &mut Core, ent: bool, battle: V| {
                let o = c.obj(&[(e_battle, battle)]);
                c.key_set(&o, "xent".into(), V::B(ent));
                o
            };
            let ea = mk(&mut sim.c, true, V::Null);
            let x = mk(&mut sim.c, false, V::Null);
            let eb = mk(&mut sim.c, true, V::Null);
            let ec = mk(&mut sim.c, true, other.clone());
            *ents.borrow_mut() = vec![ea.clone(), eb.clone(), ec.clone()];
            let states = vec![ea.clone(), x.clone(), eb.clone(), ec.clone()];
            let arr = sim.c.arr(a_len, a_raw, states);
            let game = sim
                .c
                .obj(&[(pb.alive_states, arr), (pb.g_battle, bt.clone())]);
            sim.run(init_alive, vec![game]);
            let inits = sim.c.take("init");
            let after = [ea, x, eb, ec]
                .iter()
                .map(|o| sim.c.get(o, e_battle))
                .collect();
            let pre = pre.borrow_mut().take().expect("no init ran");
            (inits, after, other, pre)
        };

        let (inits, after, other, pre) = run(&code);
        // ents = [ea, eb, ec]: the bind set the null ones, left ec's own battle.
        assert_eq!(pre[2], other, "bind leaves an already-bound entity's battle");
        assert!(pre[0] != V::Null && pre[0] == pre[1], "{pre:?}");
        assert_eq!(inits.len(), 3, "{inits:?}");
        assert!(inits.iter().all(|i| i[1] == V::B(true)), "{inits:?}");
        assert_eq!(after[1], V::Null, "non-entity state untouched");
        assert!(after[0] != V::Null && after[0] == after[2]);
        // Vanilla: the first init sees a later entity of the batch unbound.
        let (inits, _, _, _) = run(&orig);
        assert_eq!(inits.len(), 3);
        assert_eq!(inits[0][1], V::B(false));
    }
}
