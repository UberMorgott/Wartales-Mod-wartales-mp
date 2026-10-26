// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Vanilla bug: colour-blind mode leaks a full-screen render target on every
// frame the game scene is not drawn (a save loading, a client joining).
//
// `Game.render` opens the colour-blind pass whenever `ui != null`:
//
//   var cb = ui != null;
//   var t = cb ? initColorBlind(s2d.ctx) : null;  // s2d.ctx.textures.allocTarget("postFilter", w, h)
//   if (state != null) {
//     if (Main.loadingS2d != null) Main.loadingS2d.render(...);  // own context
//     else { ...s2d draw... }     // s2d.ctx.begin -> textures.begin() rewinds the cache
//   }
//   if (cb) renderColorBlind(t, s2d, ...);
//
// `s2d`'s texture cache is rewound only by the s2d draw, so with no state or
// with a loading scene up each frame allocates a new screen-sized target:
// video memory grows by gigabytes in seconds (Proton/DXVK then fails
// allocations with an access violation every frame: the game freezes). The
// pass is now opened only when the s2d draw runs:
//
//   var cb = ui != null && state != null && Main.loadingS2d == null;
//
// Five ops, on registers the state block writes before reading. Loading
// screens are shown without the filter. The title screen renders its own
// pass (`TitleScreen.render`) and is untouched.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use hlbc::types::ValBool;

struct Plan {
    fi: usize,
    state_f: RefField,
    state_reg: Reg,
    main_g: RefGlobal,
    main_reg: Reg,
    ls_f: RefField,
    ls_reg: Reg,
}

/// Ops inserted after `if (ui == null)`.
const N: usize = 5;
/// Where they go.
const AT: usize = 2;
/// The vanilla `cb = false` op.
const FALSE: usize = 4;

/// `op` names register `r` (any operand).
fn uses(op: &Opcode, r: Reg) -> bool {
    let op = format!("{op:?}");
    op.contains(&format!("Reg({})", r.0))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let app_t = obj_type(code, "AppBase")?;
    let ls_t = obj_type(code, "lib.LoadingScene")?;
    let (ui_f, _) = field(code, game_t, "ui")?;
    let (state_f, _) = field(code, game_t, "state")?;
    let (main_g, main_gt) = class_global(code, "Main")?;
    let ls_f = typed(code, main_gt, "loadingS2d", ls_t)?;
    let init = method(code, app_t, "initColorBlind")?.findex;
    let render_cb = method(code, app_t, "renderColorBlind")?.findex;
    let f = method(code, game_t, "render")?;
    if matches!(f.ops.get(AT), Some(Opcode::GetThis { field, .. }) if *field == state_f) {
        bail!("Game.render: already applied");
    }
    // 0: ui; 1: if null -> 4; 2: cb = true; 3: -> 5; 4: cb = false; 5: if !cb -> skip init
    let (
        Some(Opcode::GetThis { dst: ui_reg, field }),
        Some(Opcode::JNull { reg, .. }),
        Some(Opcode::Bool {
            dst: cb,
            value: ValBool(true),
        }),
        Some(Opcode::JAlways { .. }),
        Some(Opcode::Bool {
            dst: cb2,
            value: ValBool(false),
        }),
        Some(Opcode::JFalse { cond, .. }),
    ) = (
        f.ops.first(),
        f.ops.get(1),
        f.ops.get(2),
        f.ops.get(3),
        f.ops.get(FALSE),
        f.ops.get(5),
    )
    else {
        bail!("Game.render: unexpected colour-blind test");
    };
    if *field != ui_f || reg != ui_reg || cb2 != cb || cond != cb {
        bail!("Game.render: unexpected colour-blind test");
    }
    if jump_targets(f, 1) != vec![FALSE] || jump_targets(f, 3) != vec![5] {
        bail!("Game.render: unexpected colour-blind test jumps");
    }
    // The opened pass: one init (under `if (!cb)`) and one render (after another).
    let inits: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::Call2 { fun, .. } if fun == init))
        .collect();
    let renders: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::Call4 { fun, .. } if fun == render_cb))
        .collect();
    let ([i], [r]) = (&inits[..], &renders[..]) else {
        bail!("Game.render: expected one initColorBlind and one renderColorBlind call");
    };
    if jump_targets(f, 5).first().is_none_or(|&t| t <= *i) {
        bail!("Game.render: initColorBlind is not under the colour-blind test");
    }
    let gate = (0..*r)
        .rev()
        .find(|&k| matches!(f.ops[k], Opcode::JFalse { cond: c, .. } if c == *cb))
        .filter(|&k| k > *i && jump_targets(f, k).first().is_some_and(|&t| t > *r))
        .context("Game.render: renderColorBlind is not under the colour-blind test")?;
    // `cb` is only written by ops 2 and 4.
    if (0..gate).any(|k| {
        k != 2 && k != FALSE && format!("{:?}", f.ops[k]).contains(&format!("dst: Reg({})", cb.0))
    }) {
        bail!("Game.render: the colour-blind flag is rewritten");
    }
    // The state block's `if (state == null)` (`loading_draw` turns it into a
    // JNotNull) and the first `Main.loadingS2d` load after it.
    let test = (6..f.ops.len() - 1)
        .find(|&k| {
            matches!((&f.ops[k], &f.ops[k + 1]),
                (Opcode::GetThis { dst, field }, Opcode::JNull { reg, .. } | Opcode::JNotNull { reg, .. })
                    if *field == state_f && dst == reg)
        })
        .context("Game.render: no `if (state == null)` test")?;
    let Opcode::GetThis { dst: state_reg, .. } = f.ops[test] else {
        unreachable!()
    };
    let load = (test + 2..f.ops.len() - 1)
        .find(|&k| {
            matches!((&f.ops[k], &f.ops[k + 1]),
                (Opcode::GetGlobal { dst, global }, Opcode::Field { obj, field, .. })
                    if *global == main_g && obj == dst && *field == ls_f)
        })
        .context("Game.render: no Main.loadingS2d load in the state block")?;
    let (Opcode::GetGlobal { dst: main_reg, .. }, Opcode::Field { dst: ls_reg, .. }) =
        (&f.ops[load], &f.ops[load + 1])
    else {
        unreachable!()
    };
    if f.regs[main_reg.0 as usize] != main_gt || f.regs[ls_reg.0 as usize] != ls_t {
        bail!("Game.render: loading scene registers have unexpected types");
    }
    // Every reused register is first written by its own load: no vanilla
    // value lives across the inserted ops.
    if (0..test).any(|k| uses(&f.ops[k], state_reg))
        || (0..load).any(|k| uses(&f.ops[k], *main_reg))
        || (0..=load).any(|k| uses(&f.ops[k], *ls_reg))
    {
        bail!("Game.render: a reused register is live before its load");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        state_f,
        state_reg,
        main_g,
        main_reg: *main_reg,
        ls_f,
        ls_reg: *ls_reg,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    // Offsets from inserted op `AT + k` to `cb = false`, at FALSE + N after the insertion.
    let to_false = |k: usize| (FALSE + N - (AT + k) - 1) as i32;
    let ops = vec![
        Opcode::GetThis {
            dst: p.state_reg,
            field: p.state_f,
        },
        Opcode::JNull {
            reg: p.state_reg,
            offset: to_false(1),
        },
        Opcode::GetGlobal {
            dst: p.main_reg,
            global: p.main_g,
        },
        Opcode::Field {
            dst: p.ls_reg,
            obj: p.main_reg,
            field: p.ls_f,
        },
        Opcode::JNotNull {
            reg: p.ls_reg,
            offset: to_false(4),
        },
    ];
    debug_assert_eq!(ops.len(), N);
    insert_ops(f, AT, ops);
    eprintln!(
        "patched colour-blind pass fn@{}: opened only when the game scene is drawn",
        f.findex.0
    );
}

/// Opens the colour-blind pass only when the game scene is drawn, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_colorblind_load(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("colour-blind load skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, same, shifted, write};

    fn check(image: &[u8]) {
        let orig = &read(image);
        let p = plan(orig).expect("plan");
        let fi = p.fi;
        let mut code = read(image);
        patch_colorblind_load(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        assert_eq!(back.globals, orig.globals);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        shifted(a, b, AT, N);
        assert_eq!(b.regs, a.regs);
        // ui != null && state != null && Main.loadingS2d == null, else `cb = false`.
        assert!(
            matches!(b.ops[AT], Opcode::GetThis { dst, field } if dst == p.state_reg && field == p.state_f)
        );
        assert!(matches!(b.ops[AT + 1], Opcode::JNull { reg, .. } if reg == p.state_reg));
        assert!(
            matches!(b.ops[AT + 3], Opcode::Field { dst, field, .. } if dst == p.ls_reg && field == p.ls_f)
        );
        assert!(matches!(b.ops[AT + 4], Opcode::JNotNull { reg, .. } if reg == p.ls_reg));
        for j in [1, AT + 1, AT + 4] {
            assert_eq!(jump_targets(b, j), vec![FALSE + N], "op {j}");
        }
        assert!(matches!(
            b.ops[FALSE + N],
            Opcode::Bool {
                value: ValBool(false),
                ..
            }
        ));
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_colorblind_load(&mut again);
        assert!(write(&again) == patched);
    }

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        check(&image);
    }

    /// Composes with `loading_draw`, which runs first and edits the state block.
    #[test]
    fn patches_after_loading_draw() {
        let Some(image) = game() else { return };
        let mut code = read(&image);
        crate::loading_draw::patch_loading_draw(&mut code);
        let game_t = obj_type(&code, "Game").unwrap();
        let f = method(&code, game_t, "render").unwrap();
        let ls_t = obj_type(&code, "lib.LoadingScene").unwrap();
        let ls_render = method(&code, ls_t, "render").unwrap().findex;
        let draws = f
            .ops
            .iter()
            .filter(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == ls_render))
            .count();
        assert_eq!(draws, 2, "loading_draw applied");
        check(&write(&code));
    }
}
