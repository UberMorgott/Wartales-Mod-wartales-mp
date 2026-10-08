// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op client's cached skill scripts read the current synced skill vars
// (issue #2: wrong cost / availability of scripted skills such as Inhalation).
//
// `ScriptedSkill.initScript` binds `unit.getSkillPersist(inf.id).vars` into the
// script once (`interp.variables["vars"]`, `ScriptInterp.checkObjUpdate`), and
// `Battle.getOrCreateSkill` caches the skill in `unit.skillsCache`. On a client
// hxbit `networkSync` replaces `unit.skillPersistValues` wholesale, so every
// cached script keeps reading (and writing) the vars object of an older map:
// `getCost` (`vars.used`) answers with stale state while `canUseSkill` reads
// `allowed` from the current map. On a client only, a skill returned by
// `getOrCreateSkill` is rebound when its binding is not the current object:
//
//   if (game != null && !game.isAuth && sk is ScriptedSkill && script/interp/variables != null) {
//     var pv = sk.u.skillPersistValues.get(sk.inf.id).vars;  // skip if missing
//     if (variables.get("vars") != pv) {
//       variables.set("vars", pv);
//       if (interp is ScriptInterp) interp.checkObjUpdate = pv;
//     }
//   }
//
// The same objects initScript would bind on a fresh skill; the cache, the
// Skill objects and their scripts are kept. The host and solo are unchanged.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct T {
    battle_game: RefField,
    game_t: RefType,
    game_auth: RefField,
    bool_t: RefType,
    skill_t: RefType,
    scripted_g: RefGlobal,
    scripted_gt: RefType,
    scripted_t: RefType,
    check: RefFun,
    script_f: RefField,
    inst_t: RefType,
    interp_f: RefField,
    interp_t: RefType,
    vars_map_f: RefField,
    map_t: RefType,
    map_get: RefFun,
    map_set: RefFun,
    dyn_t: RefType,
    str_t: RefType,
    vars_str: RefGlobal,
    inf_f: RefField,
    inf_t: RefType,
    id_f: RefField,
    u_f: RefField,
    unit_t: RefType,
    pmap_f: RefField,
    pv_t: RefType,
    pv_vars: RefField,
    si_g: RefGlobal,
    si_gt: RefType,
    si_t: RefType,
    check_obj: RefField,
    void_t: RefType,
}

struct Plan {
    fi: usize,
    at: usize,
    hit: usize,
    res: Reg,
    t: T,
}

fn vfield(code: &Bytecode, t: RefType, name: &str) -> Result<(RefField, RefType)> {
    match &code.types[t.0] {
        Type::Virtual { fields } => fields
            .iter()
            .position(|f| s(code, f.name) == name)
            .map(|i| (RefField(i), fields[i].t))
            .with_context(|| format!("virtual field {name} not found")),
        _ => bail!("type {} is not virtual", t.0),
    }
}

fn types(code: &Bytecode) -> Result<T> {
    let battle_t = obj_type(code, "battle.Battle")?;
    let game_t = obj_type(code, "Game")?;
    let battle_game = typed(code, battle_t, "game", game_t)?;
    let (game_auth, bool_t) = field(code, game_t, "isAuth")?;
    if !matches!(code.types[bool_t.0], Type::Bool) {
        bail!("Game.isAuth is not a Bool");
    }
    let skill_t = obj_type(code, "battle.skill.Skill")?;
    let scripted_t = obj_type(code, "battle.skill.ScriptedSkill")?;
    let (scripted_g, scripted_gt) = class_global(code, "battle.skill.ScriptedSkill")?;
    let check = method(code, obj_type(code, "hl.BaseType")?, "check")?.findex;
    let (script_f, inst_t) = field(code, scripted_t, "script")?;
    let (interp_f, interp_t) = field(code, inst_t, "interp")?;
    let (vars_map_f, map_t) = field(code, interp_t, "variables")?;
    let map_get = method(code, map_t, "get")?.findex;
    let map_set = method(code, map_t, "set")?.findex;
    let (_, dyn_t) = sig(code, map_get)?;
    let str_t = obj_type(code, "String")?;
    let vars_str = existing_str(code, str_t, "vars")?;
    let (inf_f, inf_t) = field(code, skill_t, "inf")?;
    let (id_f, id_t) = vfield(code, inf_t, "id")?;
    if id_t != str_t {
        bail!("skill id is not a String");
    }
    let unit_t = obj_type(code, "battle.Unit")?;
    let u_f = typed(code, skill_t, "u", unit_t)?;
    let persist = method(code, unit_t, "getSkillPersist")?.findex;
    let (pargs, pv_t) = sig(code, persist)?;
    if pargs[..] != [unit_t, str_t] {
        bail!("Unit.getSkillPersist: unexpected signature");
    }
    let (pv_vars, pv_vars_t) = vfield(code, pv_t, "vars")?;
    // The map getSkillPersist reads (not the getter: it creates missing entries).
    let pmap_f = typed(code, unit_t, "skillPersistValues", map_t)?;
    let (si_g, si_gt) = class_global(code, "script.ScriptInterp")?;
    let si_t = obj_type(code, "script.ScriptInterp")?;
    let (check_obj, check_obj_t) = field(code, si_t, "checkObjUpdate")?;
    if !is_sub(code, si_t, interp_t) {
        bail!("ScriptInterp is not an hscript.Interp");
    }
    for t in [dyn_t, pv_vars_t, check_obj_t] {
        if !matches!(code.types[t.0], Type::Dyn) {
            bail!("vars binding types are not dynamic");
        }
    }
    let (_, void_t) = sig(code, map_set)?;
    Ok(T {
        battle_game,
        game_t,
        game_auth,
        bool_t,
        skill_t,
        scripted_g,
        scripted_gt,
        scripted_t,
        check,
        script_f,
        inst_t,
        interp_f,
        interp_t,
        vars_map_f,
        map_t,
        map_get,
        map_set,
        dyn_t,
        str_t,
        vars_str,
        inf_f,
        inf_t,
        id_f,
        u_f,
        unit_t,
        pmap_f,
        pv_t,
        pv_vars,
        si_g,
        si_gt,
        si_t,
        check_obj,
        void_t,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let t = types(code)?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let f = method(code, battle_t, "getOrCreateSkill")?;
    if f.ops.iter().any(
        |o| matches!(o, Opcode::SetField { obj, field, .. } if *field == t.check_obj && f.regs[obj.0 as usize] == t.si_t),
    ) {
        bail!("getOrCreateSkill: already applied");
    }
    let at = f.ops.len() - 1;
    let Opcode::Ret { ret: res } = f.ops[at] else {
        bail!("getOrCreateSkill: does not end in Ret");
    };
    if f.regs[res.0 as usize] != t.skill_t {
        bail!("getOrCreateSkill: does not return a Skill");
    }
    // The cache hit: the one jump to the final Ret, `if (sk != null) return sk`.
    let hits: Vec<usize> = (0..at)
        .filter(|&i| jump_targets(f, i).contains(&at))
        .collect();
    let [hit] = hits[..] else {
        bail!("getOrCreateSkill: expected one jump to the final Ret");
    };
    if !matches!(f.ops[hit], Opcode::JNotNull { reg, .. } if reg == res) {
        bail!("getOrCreateSkill: the cache hit is not `if (sk != null) return sk`");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        at,
        hit,
        res,
        t,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let t = &p.t;
    let f = &mut code.functions[p.fi];
    let g = new_reg(f, t.game_t);
    let k = new_reg(f, t.bool_t);
    let cg = new_reg(f, t.scripted_gt);
    let ss = new_reg(f, t.scripted_t);
    let inst = new_reg(f, t.inst_t);
    let it = new_reg(f, t.interp_t);
    let vm = new_reg(f, t.map_t);
    let key = new_reg(f, t.str_t);
    let cur = new_reg(f, t.dyn_t);
    let inf = new_reg(f, t.inf_t);
    let id = new_reg(f, t.str_t);
    let u = new_reg(f, t.unit_t);
    let pm = new_reg(f, t.map_t);
    let d = new_reg(f, t.dyn_t);
    let pv = new_reg(f, t.pv_t);
    let pvars = new_reg(f, t.dyn_t);
    let v = new_reg(f, t.void_t);
    let sg = new_reg(f, t.si_gt);
    let si = new_reg(f, t.si_t);
    let sk = p.res;
    let (b, battle_f) = (Reg(0), t.battle_game);
    // op i skips to the final Ret, which follows the N inserted ops.
    const N: i32 = 35;
    let end = |i: i32| N - 1 - i;
    let ops = vec![
        Opcode::Field {
            dst: g,
            obj: b,
            field: battle_f,
        },
        Opcode::JNull {
            reg: g,
            offset: end(1),
        },
        Opcode::Field {
            dst: k,
            obj: g,
            field: t.game_auth,
        },
        Opcode::JTrue {
            cond: k,
            offset: end(3),
        },
        Opcode::JNull {
            reg: sk,
            offset: end(4),
        },
        Opcode::GetGlobal {
            dst: cg,
            global: t.scripted_g,
        },
        Opcode::Call2 {
            dst: k,
            fun: t.check,
            arg0: cg,
            arg1: sk,
        },
        Opcode::JFalse {
            cond: k,
            offset: end(7),
        },
        Opcode::UnsafeCast { dst: ss, src: sk },
        Opcode::Field {
            dst: inst,
            obj: ss,
            field: t.script_f,
        },
        Opcode::JNull {
            reg: inst,
            offset: end(10),
        },
        Opcode::Field {
            dst: it,
            obj: inst,
            field: t.interp_f,
        },
        Opcode::JNull {
            reg: it,
            offset: end(12),
        },
        Opcode::Field {
            dst: vm,
            obj: it,
            field: t.vars_map_f,
        },
        Opcode::JNull {
            reg: vm,
            offset: end(14),
        },
        Opcode::GetGlobal {
            dst: key,
            global: t.vars_str,
        },
        Opcode::Call2 {
            dst: cur,
            fun: t.map_get,
            arg0: vm,
            arg1: key,
        },
        Opcode::Field {
            dst: inf,
            obj: ss,
            field: t.inf_f,
        },
        Opcode::JNull {
            reg: inf,
            offset: end(18),
        },
        Opcode::Field {
            dst: id,
            obj: inf,
            field: t.id_f,
        },
        Opcode::Field {
            dst: u,
            obj: ss,
            field: t.u_f,
        },
        Opcode::JNull {
            reg: u,
            offset: end(21),
        },
        Opcode::Field {
            dst: pm,
            obj: u,
            field: t.pmap_f,
        },
        Opcode::JNull {
            reg: pm,
            offset: end(23),
        },
        Opcode::Call2 {
            dst: d,
            fun: t.map_get,
            arg0: pm,
            arg1: id,
        },
        Opcode::JNull {
            reg: d,
            offset: end(25),
        },
        Opcode::ToVirtual { dst: pv, src: d },
        Opcode::Field {
            dst: pvars,
            obj: pv,
            field: t.pv_vars,
        },
        Opcode::JEq {
            a: cur,
            b: pvars,
            offset: end(28),
        },
        Opcode::Call3 {
            dst: v,
            fun: t.map_set,
            arg0: vm,
            arg1: key,
            arg2: pvars,
        },
        Opcode::GetGlobal {
            dst: sg,
            global: t.si_g,
        },
        Opcode::Call2 {
            dst: k,
            fun: t.check,
            arg0: sg,
            arg1: it,
        },
        Opcode::JFalse {
            cond: k,
            offset: end(32),
        },
        Opcode::UnsafeCast { dst: si, src: it },
        Opcode::SetField {
            obj: si,
            field: t.check_obj,
            src: pvars,
        },
    ];
    debug_assert_eq!(ops.len(), N as usize);
    insert_ops(f, p.at, ops);
    // The cache hit now runs the rebind too, instead of jumping past it.
    match &mut f.ops[p.hit] {
        Opcode::JNotNull { offset, .. } => *offset -= N,
        _ => unreachable!(),
    }
    eprintln!(
        "patched skill vars fn@{}: a client's cached skill scripts follow the synced vars",
        f.findex.0
    );
}

/// Rebinds a client's cached skill scripts to the current synced vars, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_skill_vars(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("skill vars skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, at, hit) = (p.fi, p.at, p.hit);
        let mut code = read(&image);
        patch_skill_vars(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 35);
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        assert_eq!(
            format!("{:?}", &b.ops[..hit]),
            format!("{:?}", &a.ops[..hit])
        );
        assert_eq!(jump_targets(b, hit), vec![at]);
        let ret = b.ops.len() - 1;
        for j in at..ret {
            for t in jump_targets(b, j) {
                assert_eq!(t, ret, "op {j}");
            }
        }
        assert!(matches!(b.ops[ret], Opcode::Ret { .. }));
        assert!(b.ops[at..]
            .iter()
            .any(|o| matches!(o, Opcode::ToVirtual { .. })));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_skill_vars(&mut again);
        assert!(write(&again) == patched);
    }
}
