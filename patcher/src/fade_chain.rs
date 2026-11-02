// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A fade asked for while another one runs is never lost.
//
// Vanilla `Game.fadeTo` (Game.hx:1375): when a fade is up whose `onBreak` is
// null (every fadeIn: allowBreak is always false; a fadeOut(allowBreak=false)),
// the new fade is dropped: "Interrupted fade" is logged and it returns, so the
// new fade's `onEnd` never runs. Every mode switch parks its next step in such
// an onEnd: `Controller.syncLeaveMode` / `syncEnterMode` call
// `fadeIn(cb)` and everything after (waitForUnlock -> leaveMode / onEnd ->
// onServerReady ...) is inside `cb`. A switch started while another fadeIn is
// up therefore stops for good: the host stays black with input captured
// (fadeIn's own startCapture is released only by its onEnd), the guests wait
// for an onServerReady that never comes, and the barrier sees no dead switch
// (no lock held, nothing parked). The guarded callers test `inFade()` (the
// exact drop condition) first: toggleCamp__impl (camp), changeCampmode__impl,
// Tavern.askLeave__impl, closeTavern__impl, PlaceView.tryClose. Others do not:
// RuinsHallway.update can call fadeIn (groupHidden) and gotoNextRoom ->
// syncLeaveMode in the same frame; the battle end, FiefCity / Fief leaves,
// TavernWindow.nextDay, Tavern.upgradeTo, PlaceView.leave from
// DialogOutView.setFocus(null) / CrimeCave / ForsakenVillage / Ruins; and a
// switch queued behind another one (processSyncMode) starts right after that
// one's last step, while the new mode's own fadeIn may still run.
//
// This pass makes the dropped fade wait for the running one instead:
//
//   fadeTo, the drop branch:  logError(..);  return;
//     ->                      logError(..);  (falls through to the normal path,
//                             which builds the new params `np` as usual)
//   fadeTo, before `this.fadeParams = np`:
//     if (prev != null && prev.onBreak == null) {      // the drop condition
//         prev.onEnd = fcChain.bind(new Ctx(prev.onEnd, np, this));
//         return;
//     }
//
//   fcChain(ctx): if (ctx.old != null) ctx.old();
//                 if (np.onBreak == null) {   // a fadeIn: retake its lock
//                     game.fading = true; game.sevents.startCapture(block, null, null);
//                 }
//                 fadeTo(ctx.game, np.target, np.onEnd, np.onUpdate, np.onBreak,
//                        false, np.noLock);
//
// Game.update clears fadeParams before it calls onEnd, so the chained fadeTo
// sees the screen as the running fade left it: the running fade's own onEnd
// runs first (in order, as if the second request came later), then the new
// fade starts; if that onEnd started a fade of its own, fadeTo breaks it or
// chains again behind it, by the same rules. `np.onBreak` is already resolved
// (the empty closure when allowBreak), so allowBreak = false loses nothing.
// The vanilla log line stays ("Interrupted fade" now means "queued").
// fadeIn takes its lock (fading = true, startCapture with a block-all handler)
// before fadeTo and releases it in its onEnd; the running fadeIn's onEnd
// releases it right before the queued one starts, so the chain takes it again
// for a no-break fade. Every no-break fade is a fadeIn: all vanilla fadeOut
// callers pass allowBreak null (true), fadeIn always false. `block` and the
// startCapture call are read from fadeIn itself.
// Known cosmetic limit: PlaceView.setFocus sets `game.fadeParams.target = 0.7`
// right after its fadeIn; when that fadeIn is queued the write lands on the
// running fade (vanilla does the same) and the queued one dims to 1.0.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Regs};
use super::*;
use hlbc::types::{EnumConstruct, RefEnumConstruct};

struct Plan {
    fi: usize,
    game_t: RefType,
    fp_t: RefType,
    void_t: RefType,
    /// `GetThis prev = this.fadeParams` register.
    prev: Reg,
    /// The drop branch's `Ret` (right before the normal path).
    drop_ret: usize,
    /// `SetThis this.fadeParams = np`.
    set_at: usize,
    np: Reg,
    void_r: Reg,
    on_break: (RefField, RefType),
    on_end: (RefField, RefType),
    on_update: (RefField, RefType),
    target: (RefField, RefType),
    no_lock: (RefField, RefType),
    fade_to: RefFun,
    args: Vec<RefType>,
    dbg_file: usize,
    /// fadeIn's own lock: `this.fading = true; this.sevents.startCapture(block, null, null)`.
    fading: RefField,
    sevents: (RefField, RefType),
    start_capture: RefFun,
    /// startCapture's arguments after the events object: block closure, two nulls.
    cap_args: [RefType; 3],
    block: RefFun,
}

/// fadeIn's `fading = true` field, its `sevents` field, the `startCapture` call
/// and the static block closure it passes, read from fadeIn itself.
fn fade_in_lock(
    code: &Bytecode,
    game_t: RefType,
) -> Result<(RefField, (RefField, RefType), RefFun, [RefType; 3], RefFun)> {
    let fi = method(code, game_t, "fadeIn")?;
    let (fading, ft) = field(code, game_t, "fading")?;
    let sevents = field(code, game_t, "sevents")?;
    if !matches!(code.types[ft.0], Type::Bool) {
        bail!("Game.fading is not a Bool");
    }
    let sets_fading = fi
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::SetThis { field, .. } if *field == fading));
    let calls: Vec<(RefFun, [Reg; 4])> = fi
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call4 {
                fun,
                arg0,
                arg1,
                arg2,
                arg3,
                ..
            } if fname(code, *fun) == "startCapture" => Some((*fun, [*arg0, *arg1, *arg2, *arg3])),
            _ => None,
        })
        .collect();
    let [(start_capture, [_, blk_r, a2, a3])] = calls[..] else {
        bail!(
            "Game.fadeIn: expected one startCapture call, found {}",
            calls.len()
        );
    };
    let blocks: Vec<RefFun> = fi
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::StaticClosure { dst, fun } if *dst == blk_r => Some(*fun),
            _ => None,
        })
        .collect();
    let [block] = blocks[..] else {
        bail!("Game.fadeIn: startCapture's block is not one StaticClosure");
    };
    if !sets_fading {
        bail!("Game.fadeIn: does not set fading");
    }
    let rt = |r: Reg| fi.regs[r.0 as usize];
    Ok((
        fading,
        sevents,
        start_capture,
        [rt(blk_r), rt(a2), rt(a3)],
        block,
    ))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let game_t = obj_type(code, "Game")?;
    let (fp_f, fp_t) = field(code, game_t, "fadeParams")?;
    let ft = method(code, game_t, "fadeTo")?;
    let fade_to = ft.findex;
    let fi = fun_index(code, fade_to)?;
    let f = &code.functions[fi];
    let args = fun_args(code, f);
    let vf = |n| field_of_virtual(code, fp_t, n);
    let (on_break, on_end, on_update, target, no_lock) = (
        vf("onBreak")?,
        vf("onEnd")?,
        vf("onUpdate")?,
        vf("target")?,
        vf("noLock")?,
    );
    if args.len() != 7
        || args[0] != game_t
        || args[1] != target.1
        || args[2] != on_end.1
        || args[3] != on_update.1
        || args[4] != on_break.1
        || !matches!(code.types[args[5].0], Type::Bool)
        || !matches!(code.types[args[6].0], Type::Null(t) if t == no_lock.1)
        || !matches!(code.types[no_lock.1 .0], Type::Bool)
    {
        bail!("Game.fadeTo: unexpected signature");
    }
    // `GetThis prev = this.fadeParams; JNull prev -> normal`, the op before
    // `normal` the drop branch's Ret.
    let gets: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::GetThis { field, .. } if field == fp_f))
        .collect();
    let [g] = gets[..] else {
        bail!(
            "Game.fadeTo: expected one fadeParams read, found {}",
            gets.len()
        );
    };
    let Opcode::GetThis { dst: prev, .. } = f.ops[g] else {
        unreachable!()
    };
    let normal = match f.ops.get(g + 1) {
        Some(Opcode::JNull { reg, .. }) if *reg == prev => jump_targets(f, g + 1)[0],
        _ => bail!("Game.fadeTo: fadeParams read is not followed by JNull"),
    };
    let drop_ret = normal - 1;
    let Opcode::Ret { ret: void_r } = f.ops[drop_ret] else {
        bail!("Game.fadeTo: no Ret before the normal path (already applied?)");
    };
    if !matches!(f.ops[drop_ret - 1], Opcode::CallN { .. }) {
        bail!("Game.fadeTo: the drop branch does not end in the logError call");
    }
    let sets: Vec<usize> = (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::SetThis { field, .. } if field == fp_f))
        .collect();
    let [set_at] = sets[..] else {
        bail!(
            "Game.fadeTo: expected one fadeParams write, found {}",
            sets.len()
        );
    };
    let Opcode::SetThis { src: np, .. } = f.ops[set_at] else {
        unreachable!()
    };
    if set_at <= normal || !matches!(f.ops.get(set_at + 1), Some(Opcode::Ret { .. })) {
        bail!("Game.fadeTo: fadeParams write is not the normal path's last step");
    }
    if f.regs[np.0 as usize] != fp_t || f.regs[prev.0 as usize] != fp_t {
        bail!("Game.fadeTo: params registers have an unexpected type");
    }
    // prev must still hold the old params at the write, and nothing may jump
    // straight to the write (it would skip the inserted test).
    let w = format!("dst: Reg({})", prev.0);
    if (0..f.ops.len()).any(|i| i != g && format!("{:?}", f.ops[i]).contains(&w)) {
        bail!("Game.fadeTo: prev register is reassigned");
    }
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&set_at)) {
        bail!("Game.fadeTo: a jump targets the fadeParams write");
    }
    let void_t = f.regs[void_r.0 as usize];
    if !matches!(code.types[void_t.0], Type::Void) {
        bail!("Game.fadeTo: the drop Ret is not void");
    }
    let dbg_file = debug_file(code, "src/Game.hx")?;
    let (fading, sevents, start_capture, cap_args, block) = fade_in_lock(code, game_t)?;
    Ok(Plan {
        fading,
        sevents,
        start_capture,
        cap_args,
        block,
        fi,
        game_t,
        fp_t,
        void_t,
        prev,
        drop_ret,
        set_at,
        np,
        void_r,
        on_break,
        on_end,
        on_update,
        target,
        no_lock,
        fade_to,
        args,
        dbg_file,
    })
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<RefFun> {
    // Capture: (old onEnd, new params, game).
    code.types.push(Type::Enum {
        name: RefString(0),
        global: RefGlobal(0),
        constructs: vec![EnumConstruct {
            name: RefString(0),
            params: vec![p.on_end.1, p.fp_t, p.game_t],
        }],
    });
    let cap_t = RefType(code.types.len() - 1);
    let bool_t = p.args[5];

    // fcChain(ctx): old(); fadeTo(game, np.target, np.onEnd, np.onUpdate, np.onBreak, false, np.noLock)
    let mut r = Regs(vec![cap_t]);
    let cx = Reg(0);
    let (old, np, game, v) = (r.r(p.on_end.1), r.r(p.fp_t), r.r(p.game_t), r.r(p.void_t));
    let (tg, oe, ou, ob) = (
        r.r(p.target.1),
        r.r(p.on_end.1),
        r.r(p.on_update.1),
        r.r(p.on_break.1),
    );
    let (ab, nl, nld) = (r.r(bool_t), r.r(p.no_lock.1), r.r(p.args[6]));
    let (sev, blk, n1, n2) = (
        r.r(p.sevents.1),
        r.r(p.cap_args[0]),
        r.r(p.cap_args[1]),
        r.r(p.cap_args[2]),
    );
    let ef = |dst, i| Opcode::EnumField {
        dst,
        value: cx,
        construct: RefEnumConstruct(0),
        field: RefField(i),
    };
    let fd = |dst, f: (RefField, RefType)| Opcode::Field {
        dst,
        obj: np,
        field: f.0,
    };
    let ops = vec![
        ef(old, 0),
        Opcode::JNull {
            reg: old,
            offset: 1,
        },
        Opcode::CallClosure {
            dst: v,
            fun: old,
            args: vec![],
        },
        ef(np, 1),
        ef(game, 2),
        fd(tg, p.target),
        fd(oe, p.on_end),
        fd(ou, p.on_update),
        fd(ob, p.on_break),
        // A no-break fade is a fadeIn (every vanilla fadeOut passes allowBreak
        // null = true): the running fadeIn's onEnd just released fadeIn's lock,
        // which the queued fadeIn's own onEnd releases again; take it back.
        Opcode::JNotNull { reg: ob, offset: 7 },
        Opcode::Bool {
            dst: ab,
            value: hlbc::types::ValBool(true),
        },
        Opcode::SetField {
            obj: game,
            field: p.fading,
            src: ab,
        },
        Opcode::Field {
            dst: sev,
            obj: game,
            field: p.sevents.0,
        },
        Opcode::StaticClosure {
            dst: blk,
            fun: p.block,
        },
        Opcode::Null { dst: n1 },
        Opcode::Null { dst: n2 },
        Opcode::Call4 {
            dst: v,
            fun: p.start_capture,
            arg0: sev,
            arg1: blk,
            arg2: n1,
            arg3: n2,
        },
        Opcode::Bool {
            dst: ab,
            value: hlbc::types::ValBool(false),
        },
        fd(nl, p.no_lock),
        Opcode::ToDyn { dst: nld, src: nl },
        Opcode::CallN {
            dst: v,
            fun: p.fade_to,
            args: vec![game, tg, oe, ou, ob, ab, nld],
        },
        Opcode::Ret { ret: v },
    ];
    let chain = push_fn(code, vec![cap_t], p.void_t, r.0, ops, p.dbg_file)?;

    let f = &mut code.functions[p.fi];
    let (ob_r, old_r, cx_r, clo_r) = (
        new_reg(f, p.on_break.1),
        new_reg(f, p.on_end.1),
        new_reg(f, cap_t),
        new_reg(f, p.on_end.1),
    );
    // Later edit first: the inserted block sits after drop_ret.
    let block = vec![
        Opcode::JNull {
            reg: p.prev,
            offset: 7,
        },
        Opcode::Field {
            dst: ob_r,
            obj: p.prev,
            field: p.on_break.0,
        },
        Opcode::JNotNull {
            reg: ob_r,
            offset: 5,
        },
        Opcode::Field {
            dst: old_r,
            obj: p.prev,
            field: p.on_end.0,
        },
        Opcode::MakeEnum {
            dst: cx_r,
            construct: RefEnumConstruct(0),
            args: vec![old_r, p.np, Reg(0)],
        },
        Opcode::InstanceClosure {
            dst: clo_r,
            fun: chain,
            obj: cx_r,
        },
        Opcode::SetField {
            obj: p.prev,
            field: p.on_end.0,
            src: clo_r,
        },
        Opcode::Ret { ret: p.void_r },
    ];
    insert_ops(f, p.set_at, block);
    // The drop branch falls through to the normal path (JAlways +0, same op count).
    f.ops[p.drop_ret] = Opcode::JAlways { offset: 0 };
    Ok(chain)
}

/// Makes `Game.fadeTo` queue a fade it would drop, or leaves `code` untouched and logs why.
pub(crate) fn patch_fade_chain(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let res = plan(code).and_then(|p| {
        let fi = p.fi;
        apply(code, p).map(|c| (fi, c))
    });
    match res {
        Ok((fi, chain)) => eprintln!(
            "patched fade chain: Game.fadeTo fn@{} queues a dropped fade behind the running one (fn@{})",
            code.functions[fi].findex.0, chain.0
        ),
        Err(e) => {
            snap.restore(code);
            crate::skipped(format!("fade chain skipped: {e:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (fi, set_at, drop_ret) = (p.fi, p.set_at, p.drop_ret);
        let mut code = read(&image);
        patch_fade_chain(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 8);
        assert!(matches!(b.ops[drop_ret], Opcode::JAlways { offset: 0 }));
        check_flow(b);
        check_types(&back, b, set_at..set_at + 8);
        // Every original op but the drop Ret keeps its place and jump targets.
        let mut a2 = a.clone();
        a2.ops[drop_ret] = Opcode::JAlways { offset: 0 };
        crate::asm::testutil::shifted(&a2, b, set_at, 8);
        // The inserted test jumps to the original write.
        assert_eq!(jump_targets(b, set_at), vec![set_at + 8]);
        assert_eq!(jump_targets(b, set_at + 2), vec![set_at + 8]);
        assert!(matches!(b.ops[set_at + 8], Opcode::SetThis { .. }));

        let chain = back.functions.last().expect("chain fn");
        check_flow(chain);
        check_types(&back, chain, 0..chain.ops.len());
        assert_eq!(jump_targets(chain, 1), vec![3]);

        for (i, (x, y)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(
                crate::asm::testutil::same(x, y),
                i != fi,
                "fn@{}",
                x.findex.0
            );
        }

        behaviour(&back, &p);

        // Idempotent: a second run is refused and changes nothing.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_fade_chain(&mut again);
        assert!(write(&again) == patched);
    }

    /// Patched fadeTo run in the interpreter: a fade asked for while a no-break
    /// one is up starts once the running one ended, after its onEnd; a running
    /// fade with onBreak is still broken and replaced at once.
    fn behaviour(code: &Bytecode, p: &Plan) {
        use crate::testsim::{Sim, V};
        let gm = |n| method(code, p.game_t, n).unwrap().findex;
        let (old_end, new_end, brk) = (gm("safeFadeOut"), gm("removeAllTip"), gm("canFade"));
        let mut sim = Sim::new(
            code,
            0,
            move |c, f, a| {
                Some(match fname(code, f) {
                    "safeFadeOut" => {
                        c.log.push(("old", vec![]));
                        V::Null
                    }
                    "canFade" => {
                        c.log.push(("break", vec![]));
                        V::Null
                    }
                    "logError" => {
                        c.log.push(("logError", vec![]));
                        V::Null
                    }
                    "startCapture" => {
                        c.log.push(("capture", vec![a[1].clone()]));
                        V::Null
                    }
                    "getLogParams" | "getSave" | "callStack" | "get_isBeta" => V::Null,
                    "toString" | "__add__" => V::S(String::new()),
                    _ => return None,
                })
            },
            |_, m, _| panic!("unexpected method call {m}"),
        );
        let ft = &code.functions[p.fi];
        for op in &ft.ops {
            if let Opcode::GetGlobal { global, .. } = op {
                if crate::job_xp::const_str(code, *global).is_none() {
                    let o = sim.c.obj(&[]);
                    sim.c.globals.insert(global.0, o);
                }
            }
        }
        let (fp_f, _) = field(code, p.game_t, "fadeParams").unwrap();
        let game = sim.c.obj(&[]);
        let args = |end| {
            vec![
                game.clone(),
                V::F(1.0),
                V::Fun(end),
                V::Null,
                V::Null,
                V::B(false),
                V::Null,
            ]
        };

        // No-break fade up: the new one is queued behind it.
        let prev = sim
            .c
            .obj(&[(p.on_break.0, V::Null), (p.on_end.0, V::Fun(old_end))]);
        sim.c.set(&game, fp_f, prev.clone());
        sim.run(p.fade_to, args(new_end));
        assert_eq!(sim.c.take("logError").len(), 1);
        assert_eq!(sim.c.get(&game, fp_f), prev, "running fade kept");
        let V::Clo(chain, ctx) = sim.c.get(&prev, p.on_end.0) else {
            panic!("prev.onEnd not chained")
        };
        // Game.update: fadeParams = null, then onEnd().
        sim.c.set(&game, fp_f, V::Null);
        sim.c.set(&game, p.fading, V::B(false)); // the running fadeIn's onEnd released it
        sim.call(chain, vec![*ctx]);
        assert_eq!(sim.c.take("old").len(), 1, "running fade's onEnd ran first");
        assert_eq!(
            sim.c.get(&game, p.fading),
            V::B(true),
            "fadeIn lock retaken"
        );
        assert_eq!(sim.c.take("capture"), vec![vec![V::Fun(p.block)]]);
        let np = sim.c.get(&game, fp_f);
        assert_ne!(np, V::Null, "queued fade started");
        assert_eq!(sim.c.get(&np, p.on_end.0), V::Fun(new_end));
        assert_eq!(sim.c.get(&np, p.target.0), V::F(1.0));

        // A breakable request (fadeOut) queued: no fadeIn lock taken.
        let prev = sim.c.obj(&[(p.on_break.0, V::Null), (p.on_end.0, V::Null)]);
        sim.c.set(&game, fp_f, prev.clone());
        let mut a = args(new_end);
        a[5] = V::B(true);
        sim.run(p.fade_to, a);
        sim.c.take("logError");
        let V::Clo(chain, ctx) = sim.c.get(&prev, p.on_end.0) else {
            panic!("prev.onEnd not chained")
        };
        sim.c.set(&game, fp_f, V::Null);
        sim.c.set(&game, p.fading, V::B(false));
        sim.call(chain, vec![*ctx]);
        assert!(sim.c.take("capture").is_empty());
        assert_eq!(sim.c.get(&game, p.fading), V::B(false));
        let np = sim.c.get(&game, fp_f);
        assert!(
            matches!(sim.c.get(&np, p.on_break.0), V::Fun(_)),
            "onBreak kept"
        );

        // A fade with onBreak up: broken and replaced (vanilla).
        let prev = sim
            .c
            .obj(&[(p.on_break.0, V::Fun(brk)), (p.on_end.0, V::Fun(old_end))]);
        sim.c.set(&game, fp_f, prev.clone());
        sim.run(p.fade_to, args(new_end));
        assert_eq!(sim.c.take("break").len(), 1);
        assert!(sim.c.take("logError").is_empty());
        let np = sim.c.get(&game, fp_f);
        assert_ne!(np, prev);
        assert_eq!(sim.c.get(&np, p.on_end.0), V::Fun(new_end));
        assert_eq!(sim.c.get(&prev, p.on_end.0), V::Fun(old_end));

        // Nothing up: set at once.
        sim.c.set(&game, fp_f, V::Null);
        sim.run(p.fade_to, args(new_end));
        assert_eq!(
            sim.c.get(&sim.c.get(&game, fp_f), p.on_end.0),
            V::Fun(new_end)
        );
    }
}
