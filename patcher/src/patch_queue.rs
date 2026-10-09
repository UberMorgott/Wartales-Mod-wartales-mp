// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op client: a world reload no longer keeps the previous world in memory.
//
// Vanilla Game.start (Game.hx:786), on every load, host and client alike:
//
//   try new lib.CompatibilityPatcher(this).patchAfterWorldLoading() catch ...
//
// patchAfterWorldLoading pushes a closure into the static
// `CompatibilityPatcher.delayedPatches` (Array<() -> Void>); the closure holds
// the patcher, the patcher holds its Game, the Game holds the whole world. The
// queue is run and emptied only by `patchAfter`, which Game.gameplayStart and
// the load-end closure call under `if (isAuth)`: on a client nothing ever
// empties it, so every load pins one more world (seen live: 14 Game objects
// alive, exactly the 14 queue entries; ~2.7 GB per reload).
//
// The fix, one inline edit in Game.start, inside the same try, right after the
// patchAfterWorldLoading call:
//
//   if (!isAuth) CompatibilityPatcher.delayedPatches.resize(0);
//
// The client never runs these closures in vanilla (they change host-side
// state: currentCity, tavern / fief liquidation), so dropping them changes
// nothing but the memory; the host is untouched and still drains them in
// patchAfter. ArrayObj.resize(0) nulls the dropped slots.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// Op index of the EndTrap right after the patchAfterWorldLoading call.
    at: usize,
    bool_t: RefType,
    cls_t: RefType,
    arr_t: RefType,
    i32_t: RefType,
    void_t: RefType,
    game_auth: RefField,
    cls_g: RefGlobal,
    queue: RefField,
    resize: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let game_t = obj_type(code, "Game")?;
    let game_auth = typed(code, game_t, "isAuth", bool_t)?;
    let cp_t = obj_type(code, "lib.CompatibilityPatcher")?;
    let (cls_g, cls_t) = class_global(code, "lib.CompatibilityPatcher")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let queue = typed(code, cls_t, "delayedPatches", arr_t)?;
    let resize = method(code, arr_t, "resize")?;
    if fun_args(code, resize) != [arr_t, i32_t] {
        bail!("ArrayObj.resize: unexpected signature");
    }
    let resize = resize.findex;
    let after_world = method(code, cp_t, "patchAfterWorldLoading")?.findex;

    let start = method(code, game_t, "start")?;
    if fun_args(code, start) != [game_t] {
        bail!("Game.start: unexpected signature");
    }
    let sites: Vec<usize> = start
        .ops
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| match (&w[0], &w[1]) {
            (Opcode::Call1 { fun, .. }, Opcode::EndTrap { .. }) if *fun == after_world => {
                Some(i + 1)
            }
            _ => None,
        })
        .collect();
    let [at] = sites[..] else {
        bail!(
            "Game.start: {} patchAfterWorldLoading calls closing a try, want 1",
            sites.len()
        );
    };
    // Once applied, the call is followed by the inserted ops, not the EndTrap:
    // the site count above is 0 and the pass is skipped.
    if (0..start.ops.len()).any(|i| jump_targets(start, i).contains(&at)) {
        bail!("Game.start: a jump targets the EndTrap");
    }
    Ok(Plan {
        fi: fun_index(code, start.findex)?,
        at,
        bool_t,
        cls_t,
        arr_t,
        i32_t,
        void_t,
        game_auth,
        cls_g,
        queue,
        resize,
    })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    let zero = int_const(code, 0);
    let f = &mut code.functions[p.fi];
    let k = new_reg(f, p.bool_t);
    let g = new_reg(f, p.cls_t);
    let a = new_reg(f, p.arr_t);
    let n = new_reg(f, p.i32_t);
    let v = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::GetThis { dst: k, field: p.game_auth },
            // host: skip to the EndTrap (6 = the vanilla op after the insert)
            Opcode::JTrue { cond: k, offset: 6 - 1 - 1 },
            Opcode::GetGlobal { dst: g, global: p.cls_g },
            Opcode::Field { dst: a, obj: g, field: p.queue },
            Opcode::Int { dst: n, ptr: zero },
            Opcode::Call2 { dst: v, fun: p.resize, arg0: a, arg1: n },
        ],
    );
    eprintln!(
        "patched patch queue fn@{} op {}: a co-op client empties CompatibilityPatcher.delayedPatches after each load",
        f.findex.0, p.at
    );
}

/// Stops a co-op client from keeping every loaded world alive through
/// CompatibilityPatcher.delayedPatches, or leaves `code` untouched and logs why.
pub(crate) fn patch_patch_queue(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => crate::skipped(format!("patch queue skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;
    use crate::testsim::{Sim, V};

    /// Only Game.start changes, by the 6 inserted ops; the host test skips to
    /// the vanilla EndTrap; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_patch_queue(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != p.fi, "fn #{i}");
        }
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 6);
        assert_eq!(jump_targets(b, p.at + 1), [p.at + 6]);
        assert!(matches!(b.ops[p.at + 6], Opcode::EndTrap { .. }));
        shifted(a, b, p.at, 6);
        check_types(&back, b, p.at..p.at + 6);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_patch_queue(&mut again);
        assert!(write(&again) == patched);
    }

    /// Runs the inserted ops: a client empties the queue, the host leaves it.
    #[test]
    fn client_empties_host_keeps() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_patch_queue(&mut code);
        let start = code.functions[p.fi].findex;
        let arr_obj = obj(&code, p.arr_t).unwrap();
        let fld = |name: &str| {
            RefField(
                arr_obj
                    .fields
                    .iter()
                    .position(|f| s(&code, f.name) == name)
                    .unwrap(),
            )
        };
        let (len_f, arr_f) = (fld("length"), fld("array"));
        let resize = p.resize;
        for auth in [false, true] {
            let mut sim = Sim::new(
                &code,
                code.functions.len(),
                move |c, f, args| {
                    if f != resize {
                        return None;
                    }
                    c.log.push(("resize", args.to_vec()));
                    c.set(&args[0], len_f, args[1].clone());
                    Some(V::Null)
                },
                |_, _, _| V::Null,
            );
            let q = sim.c.arr(len_f, arr_f, vec![V::Fun(start), V::Fun(start)]);
            let cls = sim.c.obj(&[(p.queue, q.clone())]);
            sim.c.globals.insert(p.cls_g.0, cls);
            let game = sim.c.obj(&[(p.game_auth, V::B(auth))]);
            let mut r = vec![V::Null; code.functions[p.fi].regs.len()];
            r[0] = game;
            let pc = sim.span(start, &mut r, p.at, &[p.at + 6]);
            assert_eq!(pc, p.at + 6);
            let calls = sim.c.take("resize");
            if auth {
                assert!(calls.is_empty(), "host must not touch the queue");
                assert_eq!(sim.c.get(&q, len_f), V::I(2));
            } else {
                assert_eq!(calls, [vec![q.clone(), V::I(0)]]);
                assert_eq!(sim.c.get(&q, len_f), V::I(0));
            }
        }
    }
}
