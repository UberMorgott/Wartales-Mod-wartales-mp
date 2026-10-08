// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The loading screen is drawn on every frame before the game state exists
// (issue #3: a joining client's loading screen flickers).
//
// `Game.render` draws `Main.loadingS2d` only inside `if (state != null)`. A
// joining client waits for the host's data between `Game.init` (which creates
// the loading scene) and `Game.start` (which sets `state`); during that time
// `hxd.System.mainLoop` keeps presenting frames while `Game.render` draws
// nothing and the renderer does not clear (`backgroundColor = null`), so stale
// back buffers are shown: the screen flickers. Now:
//
//   if (state == null) {
//     var ls = Main.loadingS2d;
//     if (ls != null) ls.render(engine);   // the call vanilla makes once a state exists
//   } else { ...vanilla... }
//
// Nothing changes once a state exists or when no loading scene is up; the
// registers of vanilla's own loading draw are reused (no new types or regs).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Plan {
    fi: usize,
    /// The `if (state == null) jump` op.
    skip: usize,
    /// Where it jumps: the code after the state block.
    end: usize,
    state_reg: Reg,
    main_g: RefGlobal,
    main_reg: Reg,
    ls_f: RefField,
    ls_reg: Reg,
    ls_render: RefFun,
    void_reg: Reg,
}

/// Ops inserted after the state test.
const N: usize = 5;

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let engine_t = obj_type(code, "h3d.Engine")?;
    let ls_t = obj_type(code, "lib.LoadingScene")?;
    let (state_f, _) = field(code, game_t, "state")?;
    let (main_g, main_gt) = class_global(code, "Main")?;
    let ls_f = typed(code, main_gt, "loadingS2d", ls_t)?;
    let ls_render = method(code, ls_t, "render")?.findex;
    let (rargs, rret) = sig(code, ls_render)?;
    if rargs[..] != [ls_t, engine_t] || !matches!(code.types[rret.0], Type::Void) {
        bail!("LoadingScene.render: unexpected signature");
    }
    let f = method(code, game_t, "render")?;
    let (args, ret) = sig(code, f.findex)?;
    if args[..] != [game_t, engine_t] || !matches!(code.types[ret.0], Type::Void) {
        bail!("Game.render: unexpected signature");
    }
    if f.regs[1] != engine_t {
        bail!("Game.render: reg1 is not the engine");
    }
    // Vanilla's loading draw, `Main.loadingS2d.render(Engine.CURRENT)`; a
    // second one is this pass.
    let draws: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::Call2 { fun, .. } if fun == ls_render))
        .collect();
    let draw = match draws[..] {
        [d] => d,
        [_, _] => bail!("Game.render: already applied"),
        _ => bail!("Game.render: expected one LoadingScene.render call"),
    };
    let Opcode::Call2 {
        dst: void_reg,
        arg0: ls_reg,
        ..
    } = f.ops[draw]
    else {
        unreachable!()
    };
    // `if (state == null) jump end` before the draw.
    let tests: Vec<usize> = (0..draw)
        .filter(|&i| {
            matches!(f.ops[i], Opcode::GetThis { field, .. } if field == state_f)
                && matches!((&f.ops[i], &f.ops[i + 1]),
                    (Opcode::GetThis { dst, .. }, Opcode::JNull { reg, .. }) if dst == reg)
        })
        .collect();
    let [t] = tests[..] else {
        bail!("Game.render: expected one `if (state == null)` test");
    };
    let skip = t + 1;
    let Opcode::JNull { reg: state_reg, .. } = f.ops[skip] else {
        unreachable!()
    };
    let [end] = jump_targets(f, skip)[..] else {
        unreachable!()
    };
    if end <= draw + 1 || jump_targets(f, draw + 1) != vec![end] {
        bail!("Game.render: the loading draw does not skip to the end of the state block");
    }
    // The `Main` register of vanilla's loading draw.
    let main_reg = (skip..draw)
        .rev()
        .find_map(|i| match f.ops[i] {
            Opcode::GetGlobal { dst, global } if global == main_g => Some(dst),
            _ => None,
        })
        .context("Game.render: no Main load before the loading draw")?;
    if f.regs[main_reg.0 as usize] != main_gt
        || f.regs[ls_reg.0 as usize] != ls_t
        || !matches!(code.types[f.regs[void_reg.0 as usize].0], Type::Void)
    {
        bail!("Game.render: loading draw registers have unexpected types");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        skip,
        end,
        state_reg,
        main_g,
        main_reg,
        ls_f,
        ls_reg,
        ls_render,
        void_reg,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    let at = p.skip + 1;
    // Offsets from op `at + i` to `end`, which moves by N.
    let to_end = |i: usize| (p.end + N - (at + i) - 1) as i32;
    let ops = vec![
        Opcode::GetGlobal {
            dst: p.main_reg,
            global: p.main_g,
        },
        Opcode::Field {
            dst: p.ls_reg,
            obj: p.main_reg,
            field: p.ls_f,
        },
        Opcode::JNull {
            reg: p.ls_reg,
            offset: to_end(2),
        },
        Opcode::Call2 {
            dst: p.void_reg,
            fun: p.ls_render,
            arg0: p.ls_reg,
            arg1: Reg(1),
        },
        Opcode::JAlways { offset: to_end(4) },
    ];
    debug_assert_eq!(ops.len(), N);
    insert_ops(f, at, ops);
    // A state now jumps over the inserted draw into vanilla's state block.
    f.ops[p.skip] = Opcode::JNotNull {
        reg: p.state_reg,
        offset: N as i32,
    };
    eprintln!(
        "patched loading draw fn@{}: the loading screen is drawn before the game state exists",
        f.findex.0
    );
}

/// Draws the loading screen on frames before the game state exists, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_loading_draw(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("loading draw skipped: {e:#}")),
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
        let (fi, skip, end) = (p.fi, p.skip, p.end);
        let mut code = read(&image);
        patch_loading_draw(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        assert_eq!(back.globals, orig.globals);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), a.ops.len() + N);
        // Vanilla ops before the test and after the insertion are unchanged
        // apart from jump offsets, and every vanilla jump keeps its target.
        assert_eq!(
            format!("{:?}", &b.ops[..skip]),
            format!("{:?}", &a.ops[..skip])
        );
        let map = |t: usize| if t <= skip { t } else { t + N };
        for i in (0..a.ops.len()).filter(|&i| i != skip) {
            let want: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), want, "op {i}");
        }
        // state != null: straight into vanilla's state block.
        assert!(matches!(b.ops[skip], Opcode::JNotNull { .. }));
        assert_eq!(jump_targets(b, skip), vec![skip + 1 + N]);
        // state == null: the draw, then past the state block.
        for j in skip + 1..skip + 1 + N {
            for t in jump_targets(b, j) {
                assert_eq!(t, end + N, "op {j}");
            }
        }
        assert!(matches!(b.ops[skip + 4], Opcode::Call2 { fun, .. } if fun == p.ls_render));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_loading_draw(&mut again);
        assert!(write(&again) == patched);
    }
}
