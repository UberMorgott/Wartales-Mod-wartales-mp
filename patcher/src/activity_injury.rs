// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A co-op client's failed activity injures its unit through the host instead of
// throwing "Not allowed" and aborting the rest of the step.
//
// `ui.win.Activity.addInjury` (Activity.hx 751) injures the activity's unit on a
// failed attempt when the activity has an `onFailStatus` or the game is on
// Extreme difficulty: `status = unit.addInjury(onFailStatus ? [it] : null)`, then
// a "NotifyHurt" notification and a floating icon. `st.Unit.addInjury`
// (Unit.hx 746) picks the injury and applies it locally: `addStatus` (and, on
// its way, `Status.cancel` of "Doped", `resetCounter` on Konrad). Those mutate
// synced hxbit state, which only the host may do: on a client hxbit throws
// "Not allowed". Vanilla reaches it on clients from three callers:
//   - `FishingAction.startState`, state `Catching(false)` on Extreme: it runs on
//     every machine (`_changeState` is an RPC broadcast by the host, which runs
//     it too). The throw leaves `_changeState__impl` before
//     `lockUpdate = false`, so the fishing client's `updateState` returns early
//     from then on: fishing freezes. The fail sfx and `setFishhook` are skipped
//     as well.
//   - `LockPick.startState`, the break state on Extreme: only on the picker's
//     machine (`update` runs the state machine for `player == game.me`). The
//     throw skips the rest of the break state.
//   - `Activity.onActionDone(Fail)` unless `noInjury`: only on the player's
//     machine. The throw skips the whole end of the activity (result, close),
//     leaving the UI parked.
//
// Patch:
//   - a new `st.Unit.addInjuryNet`, a copy of `addInjury` where both
//     `addStatus(id, null, null)` calls become the existing client->host RPC
//     `netAddStatus(id, null, <no-op callback>)` (the RPC Place.onProjectileHit
//     already uses from clients), and the "Doped" cancel and Konrad's counter
//     reset (local-only mutations without an RPC) become Nop;
//   - `Activity.addInjury`: `status = game.isAuth ? unit.addInjury(l)
//     : unit.owner == game.me ? unit.addInjuryNet(l) : null`;
//   - `FishingAction.startState`: the `activity.addInjury()` call runs only when
//     `game.isAuth` (the host runs the same state change and injures the unit
//     there; the client copy would injure it a second time).
// No new RPC, no synced field. The host's code paths are unchanged.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::Asm;
use super::*;

struct Plan {
    /// `st.Unit.addInjury` (function index, findex).
    uinj_fi: usize,
    uinj: RefFun,
    /// Op indices in `Unit.addInjury` of the two `addStatus` calls.
    adds: [usize; 2],
    cancel_at: usize,
    reset_at: usize,
    net_add: RefFun,
    cb_t: RefType,
    bool_t: RefType,
    void_t: RefType,
    /// `Activity.addInjury`: function index, op of `unit.addInjury(l)`.
    ainj_fi: usize,
    ainj_at: usize,
    act_game: RefField,
    game_t: RefType,
    is_auth: RefField,
    me: RefField,
    owner: RefField,
    player_t: RefType,
    /// `FishingAction.startState`: function index, op of `GetThis a = this.activity`.
    fish_fi: usize,
    fish_at: usize,
    fish_game: RefField,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn targets_any(f: &Function, range: std::ops::RangeInclusive<usize>) -> bool {
    (0..f.ops.len()).any(|i| jump_targets(f, i).iter().any(|t| range.contains(t)))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "st.Unit")?;
    let status_t = obj_type(code, "st.Status")?;
    let act_t = obj_type(code, "ui.win.Activity")?;
    let fish_t = obj_type(code, "ui.win.FishingAction")?;
    let game_t = obj_type(code, "Game")?;
    let str_t = obj_type(code, "String")?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;

    // ---- st.Unit.addInjury(injuries) -> String
    let uf = method(code, unit_t, "addInjury")?;
    let uinj = uf.findex;
    let uinj_fi = fun_index(code, uinj)?;
    let ut = uf.t.as_fun(code).context("Unit.addInjury: no fun type")?;
    if ut.args.len() != 2 || ut.ret != str_t {
        bail!("unexpected Unit.addInjury signature");
    }
    let add_status = method(code, unit_t, "addStatus")?;
    let as_args = fun_args(code, add_status);
    if as_args.len() != 4 || as_args[1] != str_t {
        bail!("unexpected Unit.addStatus signature");
    }
    let net = method(code, unit_t, "netAddStatus")?;
    let net_args = fun_args(code, net);
    if net_args.len() != 4
        || net_args[1] != str_t
        || net_args[2] != as_args[2]
        || net.t.as_fun(code).map(|t| t.ret) != Some(void_t)
    {
        bail!("unexpected Unit.netAddStatus signature");
    }
    let cb_t = net_args[3];
    match &code.types[cb_t.0] {
        Type::Fun(t) if t.args == [bool_t] && t.ret == void_t => {}
        _ => bail!("netAddStatus callback is not (Bool) -> Void"),
    }
    let cancel = method(code, status_t, "cancel")?.findex;
    let reset = method(code, unit_t, "resetCounter")?.findex;

    let ops = &uf.ops;
    let mut adds = vec![];
    let (mut cancels, mut resets) = (vec![], vec![]);
    for (i, op) in ops.iter().enumerate() {
        match op {
            Opcode::Call4 {
                fun,
                arg0: Reg(0),
                arg2,
                arg3,
                ..
            } if *fun == add_status.findex => {
                let ok = i >= 2
                    && matches!(ops[i - 2], Opcode::Null { dst } if dst == *arg2)
                    && matches!(ops[i - 1], Opcode::Null { dst } if dst == *arg3)
                    && matches!(ops.get(i + 1), Some(Opcode::Ret { .. }));
                if !ok {
                    bail!(
                        "Unit.addInjury: addStatus at op {i} is not `Null; Null; addStatus; Ret`"
                    );
                }
                adds.push(i);
            }
            Opcode::Call2 { fun, .. } if *fun == cancel => cancels.push(i),
            Opcode::Call3 { fun, .. } if *fun == reset => resets.push(i),
            _ => {
                if super::diag::call_of(op).is_some_and(|(f, _)| f == add_status.findex) {
                    bail!("Unit.addInjury: unexpected addStatus call at op {i}");
                }
            }
        }
    }
    let [a0, a1] = adds[..] else {
        bail!("Unit.addInjury: {} addStatus calls, want 2", adds.len());
    };
    let [cancel_at] = cancels[..] else {
        bail!(
            "Unit.addInjury: {} Status.cancel calls, want 1",
            cancels.len()
        );
    };
    let [reset_at] = resets[..] else {
        bail!(
            "Unit.addInjury: {} resetCounter calls, want 1",
            resets.len()
        );
    };
    // No jump may land on an addStatus call: it would skip the callback load
    // (`StaticClosure cb`) that replaces the `Null` right before it.
    for i in [a0, a1] {
        if targets_any(uf, i..=i) {
            bail!("Unit.addInjury: a jump lands on the addStatus call at op {i}");
        }
    }

    // ---- Activity.addInjury: `status = unit.addInjury(l)`
    let af = method(code, act_t, "addInjury")?;
    let ainj_fi = fun_index(code, af.findex)?;
    // Applied before: a second `(st.Unit, Array) -> String` call (addInjuryNet) next to addInjury.
    let same_t = |fun: RefFun| {
        code.functions
            .iter()
            .find(|g| g.findex == fun)
            .is_some_and(|g| g.t == uf.t)
    };
    if af
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun != uinj && same_t(*fun)))
    {
        bail!("already applied");
    }
    let calls: Vec<usize> = (0..af.ops.len())
        .filter(|&i| matches!(af.ops[i], Opcode::Call2 { fun, .. } if fun == uinj))
        .collect();
    let [ainj_at] = calls[..] else {
        bail!(
            "Activity.addInjury: {} Unit.addInjury calls, want 1",
            calls.len()
        );
    };
    let Opcode::Call2 { arg0, .. } = af.ops[ainj_at] else {
        unreachable!()
    };
    if af.regs[arg0.0 as usize] != unit_t {
        bail!("Activity.addInjury: addInjury receiver is not st.Unit");
    }
    let (act_game, ag_t) = field(code, act_t, "game")?;
    if ag_t != game_t {
        bail!("Activity.game is not Game");
    }
    let (is_auth, ia_t) = field(code, game_t, "isAuth")?;
    if ia_t != bool_t {
        bail!("Game.isAuth is not Bool");
    }
    let (me, player_t) = field(code, game_t, "me")?;
    let (owner, owner_t) = field(code, unit_t, "owner")?;
    if owner_t != player_t {
        bail!("st.Unit.owner and Game.me differ in type");
    }

    // ---- FishingAction.startState: `GetThis a = this.activity; NullCheck a; a.addInjury()`
    let ff = method(code, fish_t, "startState")?;
    let fish_fi = fun_index(code, ff.findex)?;
    let (act_f, _) = field(code, fish_t, "activity")?;
    let (fish_game, fg_t) = field(code, fish_t, "game")?;
    if fg_t != game_t {
        bail!("FishingAction.game is not Game");
    }
    let sites: Vec<usize> = (2..ff.ops.len())
        .filter(|&c| match ff.ops[c] {
            Opcode::Call1 { fun, arg0, .. } if fun == af.findex => {
                matches!(ff.ops[c - 1], Opcode::NullCheck { reg } if reg == arg0)
                    && matches!(ff.ops[c - 2], Opcode::GetThis { dst, field } if dst == arg0 && field == act_f)
            }
            _ => false,
        })
        .collect();
    let [c] = sites[..] else {
        bail!(
            "FishingAction.startState: {} `this.activity.addInjury()` calls, want 1",
            sites.len()
        );
    };
    let fish_at = c - 2;
    if targets_any(ff, fish_at..=c) {
        bail!("FishingAction.startState: a jump lands inside the addInjury call");
    }

    Ok(Plan {
        uinj_fi,
        uinj,
        adds: [a0, a1],
        cancel_at,
        reset_at,
        net_add: net.findex,
        cb_t,
        bool_t,
        void_t,
        ainj_fi,
        ainj_at,
        act_game,
        game_t,
        is_auth,
        me,
        owner,
        player_t,
        fish_fi,
        fish_at,
        fish_game,
    })
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    // ---- the no-op callback `(Bool) -> Void`, then Unit.addInjuryNet
    let dbg = code.functions[p.uinj_fi]
        .debug_info
        .as_ref()
        .and_then(|d| d.first().copied())
        .unwrap_or((0, 0));
    // Function names are not stored in the bytecode (hlbc derives them from the
    // class bindings), so the new functions stay anonymous.
    let done = next_findex(code)?;
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t: p.cb_t,
        findex: done,
        regs: vec![p.bool_t, p.void_t],
        ops: vec![Opcode::Ret { ret: Reg(1) }],
        debug_info: Some(vec![dbg]),
        assigns: Some(vec![]),
        parent: None,
    });

    let clone_findex = next_findex(code)?;
    let mut f = code.functions[p.uinj_fi].clone();
    f.findex = clone_findex;
    f.name = hlbc::types::RefString(0);
    f.parent = None;
    f.regs.push(p.void_t);
    let rv = Reg((f.regs.len() - 1) as u32);
    f.regs.push(p.cb_t);
    let rcb = Reg((f.regs.len() - 1) as u32);
    for i in p.adds {
        let Opcode::Call4 {
            arg0, arg1, arg2, ..
        } = f.ops[i]
        else {
            unreachable!()
        };
        f.ops[i - 1] = Opcode::StaticClosure {
            dst: rcb,
            fun: done,
        };
        f.ops[i] = Opcode::Call4 {
            dst: rv,
            fun: p.net_add,
            arg0,
            arg1,
            arg2,
            arg3: rcb,
        };
    }
    f.ops[p.cancel_at] = Opcode::Nop;
    f.ops[p.reset_at] = Opcode::Nop;
    code.functions.push(f);
    eprintln!(
        "patched activity injury: Unit.addInjuryNet fn@{} (fn@{} with addStatus at ops {:?} -> netAddStatus, Doped cancel op {} / Konrad reset op {} -> Nop)",
        clone_findex.0, p.uinj.0, p.adds, p.cancel_at, p.reset_at
    );

    // ---- Activity.addInjury
    let f = &mut code.functions[p.ainj_fi];
    let Opcode::Call2 {
        dst, arg0, arg1, ..
    } = f.ops[p.ainj_at]
    else {
        unreachable!()
    };
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (rg, ra, rme, rown) = (
        reg(p.game_t),
        reg(p.bool_t),
        reg(p.player_t),
        reg(p.player_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetThis {
        dst: rg,
        field: p.act_game,
    });
    a.op(Opcode::NullCheck { reg: rg });
    a.op(Opcode::Field {
        dst: ra,
        obj: rg,
        field: p.is_auth,
    });
    a.jmp(
        Opcode::JTrue {
            cond: ra,
            offset: 0,
        },
        "host",
    );
    a.op(Opcode::Field {
        dst: rme,
        obj: rg,
        field: p.me,
    });
    a.op(Opcode::Field {
        dst: rown,
        obj: arg0,
        field: p.owner,
    });
    a.jmp(
        Opcode::JNotEq {
            a: rown,
            b: rme,
            offset: 0,
        },
        "skip",
    );
    a.op(Opcode::Call2 {
        dst,
        fun: clone_findex,
        arg0,
        arg1,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("skip");
    a.op(Opcode::Null { dst });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("host");
    a.op(Opcode::Call2 {
        dst,
        fun: p.uinj,
        arg0,
        arg1,
    });
    a.label("end");
    let mut block = a.finish();
    // The block's first op takes the call's place (jumps to the call reach the
    // test); the rest goes after it, so a jump past the call lands past the block.
    let first = block.remove(0);
    f.ops[p.ainj_at] = first;
    let n = block.len();
    insert_ops(f, p.ainj_at + 1, block);
    eprintln!(
        "patched activity injury fn@{}: op {} unit.addInjury -> host: addInjury, client owner: addInjuryNet, other client: none ({} ops added)",
        f.findex.0, p.ainj_at, n
    );

    // ---- FishingAction.startState: `if (game.isAuth) activity.addInjury()`
    let f = &mut code.functions[p.fish_fi];
    f.regs.push(p.game_t);
    let rg = Reg((f.regs.len() - 1) as u32);
    f.regs.push(p.bool_t);
    let ra = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        p.fish_at,
        vec![
            Opcode::GetThis {
                dst: rg,
                field: p.fish_game,
            },
            Opcode::NullCheck { reg: rg },
            Opcode::Field {
                dst: ra,
                obj: rg,
                field: p.is_auth,
            },
            // over GetThis activity, NullCheck, Call1 addInjury
            Opcode::JFalse {
                cond: ra,
                offset: 3,
            },
        ],
    );
    eprintln!(
        "patched activity injury fn@{}: fishing fail injury at op {} on the host only",
        f.findex.0, p.fish_at
    );
    Ok(())
}

/// Routes a co-op client's activity injury through the host, or leaves `code` untouched and logs why.
pub(crate) fn patch_activity_injury(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("activity injury skipped: {e:#}");
            return;
        }
    };
    let snap = super::asm::Snap::take(code);
    let saved: Vec<(usize, Function)> = [p.ainj_fi, p.fish_fi]
        .iter()
        .map(|&i| (i, code.functions[i].clone()))
        .collect();
    if let Err(e) = apply(code, p) {
        snap.restore(code);
        for (i, f) in saved {
            code.functions[i] = f;
        }
        eprintln!("activity injury skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::{check_flow, check_types, read, write, HLBOOT};
    use super::*;

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let (ufi, afi, ffi) = (p.uinj_fi, p.ainj_fi, p.fish_fi);
        let (adds, cancel_at, reset_at, ainj_at, fish_at) =
            (p.adds, p.cancel_at, p.reset_at, p.ainj_at, p.fish_at);
        let (uinj, net_add) = (p.uinj, p.net_add);
        let mut code = read(&image);
        patch_activity_injury(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 2);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != afi && i != ffi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // The no-op callback and the clone.
        let done = &back.functions[n];
        assert!(matches!(done.ops[..], [Opcode::Ret { ret: Reg(1) }]));
        let (u, c) = (&orig.functions[ufi], &back.functions[n + 1]);

        assert_eq!(c.t, u.t);
        assert_eq!(c.ops.len(), u.ops.len());
        assert_eq!(c.regs[..u.regs.len()], u.regs[..]);
        for i in 0..u.ops.len() {
            let changed =
                adds.iter().any(|&a| i == a || i == a - 1) || i == cancel_at || i == reset_at;
            assert_eq!(
                format!("{:?}", u.ops[i]) != format!("{:?}", c.ops[i]),
                changed,
                "clone op {i}"
            );
        }
        for a in adds {
            assert!(matches!(c.ops[a], Opcode::Call4 { fun, .. } if fun == net_add));
            assert!(
                matches!(c.ops[a - 1], Opcode::StaticClosure { fun, .. } if fun == done.findex)
            );
        }
        assert!(matches!(c.ops[cancel_at], Opcode::Nop));
        assert!(matches!(c.ops[reset_at], Opcode::Nop));
        for f in [done, c] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }

        // Activity.addInjury: one op replaced by the block start, the rest inserted after it.
        let (a, b) = (&orig.functions[afi], &back.functions[afi]);
        let added = b.ops.len() - a.ops.len();
        assert_eq!(added, 11);
        let m = |i: usize| if i <= ainj_at { i } else { i + added };
        for i in 0..a.ops.len() {
            if i == ainj_at {
                continue;
            }
            let ta: Vec<usize> = jump_targets(a, i).into_iter().map(m).collect();
            assert_eq!(jump_targets(b, m(i)), ta, "Activity.addInjury op {i}");
            if ta.is_empty() {
                assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[m(i)]));
            }
        }
        let blk = &b.ops[ainj_at..=ainj_at + added];
        let calls: Vec<RefFun> = blk
            .iter()
            .filter_map(|o| match o {
                Opcode::Call2 { fun, .. } => Some(*fun),
                _ => None,
            })
            .collect();
        assert_eq!(calls, vec![c.findex, uinj]);
        // Every exit of the block lands on the original op after the call.
        for i in ainj_at..=ainj_at + added {
            for t in jump_targets(b, i) {
                assert!(
                    t > ainj_at && t <= ainj_at + added + 1,
                    "block op {i} -> {t}"
                );
            }
        }
        check_types(&back, b, ainj_at..ainj_at + added + 1);
        check_flow(b);

        // FishingAction.startState: four ops before the GetThis activity.
        let (a, b) = (&orig.functions[ffi], &back.functions[ffi]);
        super::super::asm::testutil::shifted(a, b, fish_at, 4);
        assert!(matches!(b.ops[fish_at + 3], Opcode::JFalse { .. }));
        assert_eq!(jump_targets(b, fish_at + 3), vec![fish_at + 7]);
        check_types(&back, b, fish_at..fish_at + 4);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_activity_injury(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass also applies on top of every earlier pass (party_inventory edits FishingAction.startState).
    #[test]
    fn applies_in_full_chain() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let out = crate::patch_image(&image).expect("patch_image");
        let back = read(&out);
        assert!(plan(&back).is_err());
        let fish = obj_type(&back, "ui.win.FishingAction").unwrap();
        let ss = method(&back, fish, "startState").unwrap();
        let act = obj_type(&back, "ui.win.Activity").unwrap();
        let ainj = method(&back, act, "addInjury").unwrap().findex;
        let c = ss
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == ainj))
            .unwrap();
        assert!(matches!(ss.ops[c - 3], Opcode::JFalse { offset: 3, .. }));
    }

    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let base = write(&orig);
        let (ufi, a0, ffi, fat) = (p.uinj_fi, p.adds[0], p.fish_fi, p.fish_at);
        type Edit = Box<dyn Fn(&mut Bytecode)>;
        let edits: Vec<Edit> = vec![
            // addStatus without its Null arguments
            Box::new(move |c| c.functions[ufi].ops[a0 - 1] = Opcode::Nop),
            // no NullCheck between `this.activity` and the call
            Box::new(move |c| c.functions[ffi].ops[fat + 1] = Opcode::Nop),
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code);
            let before = write(&code);
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_activity_injury(&mut code);
            assert!(write(&code) == before, "edit {k} changed the code");
            assert!(before != base, "edit {k} was a no-op");
        }
    }
}
