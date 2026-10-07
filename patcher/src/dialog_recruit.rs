// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a tavern recruit can be hired while another player reads its info sheet.
//
// Vanilla `Dialog.displayChoices` (Dialog.hx:1009-1013) greys every networked
// choice "waiting for <player>" while any other connected player is
// `lockedWith` the dialog's element (`Game.getPlayerLockedWith(game, elt)`).
// `Npc.inspect` (right click, or the dialog "inspect" choice) sets that lock
// until its UnitInfo closes, so while A reads merc X, B cannot click Recruit.
//
// Two edits, op count unchanged in both:
//
// 1. displayChoices: both `getPlayerLockedWith(game, elt)` calls of the
//    "waiting for" block become `dlgLockedWith(game, elt, choice, choices)`:
//
//      pl = getPlayerLockedWith(game, elt);
//      if (pl == null || !isRecruit(choice)) return pl;
//      if (!Std.isOfType(elt, Npc) || (elt:Npc).getRecruitCost() == null) return pl;
//      for (c in choices) if (isLocker(c)) return pl;   // a customize path exists
//      return null;
//
//    isRecruit: the choice's specialAction (DialogSpecialActionBuilder.build,
//    as isLocalChoice reads it) is a ctor the choice handler sends to
//    recruitUnit (Recruit, Accompany, Follow, Capture). isLocker: a ctor whose
//    handler case locks the dialog element itself (netCustomizeUnit: warpaint,
//    tattoo, customize; set_lockedWith: fief law), so a lock from such a choice
//    in the same dialog keeps the vanilla wait. Other choices keep waiting.
//
// 2. The choice handler's `recruitUnit(sa, elt, dialog)` becomes
//    `dlgRecruit(sa, elt, dialog)`, on every machine:
//
//      me = dialog.game.me;
//      if (me.lockedWith == elt)            // this player reads the recruit
//          for (w in game.globalUI.windows)  // Npc.inspect's UnitInfo lives there
//              if (w is UnitInfo && w.unit != null && w.unit.isInTroop() != true)
//                  { w.close(); break; }     // its onClose clears the lock
//      recruitUnit(sa, elt, dialog);
//
//    The close is force_leave's NPC-inspect close (guard 3: a UnitInfo of a unit
//    outside the troop, while this player is locked with the NPC), with the
//    window's own close(): onClose clears lockedWith and, for the dialog
//    inspect, unhides the dialog, before recruitUnit destroys the Npc.
//
// Double recruit stays impossible: one shared dialog, one networked Recruit
// button, onRecruit destroys the Npc, npc_talk still refuses a second dialog.
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::coop_spectate::dst_of;
use crate::diag::call_of;
use crate::force_leave::slot;

struct Plan {
    dc_fi: usize,
    /// The two displayChoices `Call2 getPlayerLockedWith(game, elt)` ops.
    sites: [usize; 2],
    choice: Reg,
    choices: Reg,
    h_fi: usize,
    /// The handler's `Call3 recruitUnit(sa, elt, dialog)` op.
    rec_at: usize,
    recruit: Vec<i32>,
    lockers: Vec<i32>,
    plw: RefFun,
    plw_args: Vec<RefType>,
    bp_t: RefType,
    choice_t: RefType,
    c_props: (RefField, RefType),
    p_sa: (RefField, RefType),
    build: RefFun,
    sa_t: RefType,
    arr_t: RefType,
    a_len: RefField,
    a_raw: (RefField, RefType),
    base_check: RefFun,
    npc_cls: (RefGlobal, RefType),
    npc_t: RefType,
    recruit_cost: RefFun,
    cost_t: RefType,
    rec_fn: RefFun,
    rec_args: Vec<RefType>,
    d_game: (RefField, RefType),
    g_me: (RefField, RefType),
    bp_locked: (RefField, RefType),
    g_gui: (RefField, RefType),
    u_windows: RefField,
    win_t: RefType,
    ui_cls: (RefGlobal, RefType),
    ui_t: RefType,
    ui_unit: (RefField, RefType),
    in_troop: RefFun,
    in_troop_t: RefType,
    w_close: RefField,
    bool_: RefType,
    i32_: RefType,
    void_: RefType,
    dyn_t: RefType,
    dbg_file: usize,
}

/// Ctor indices of a `Switch` whose case block (target up to the next greater
/// target) contains an op matching `hit`.
fn cases(ops: &[Opcode], sw: usize, hit: impl Fn(&Opcode) -> bool) -> Vec<i32> {
    let Opcode::Switch { offsets, end, .. } = &ops[sw] else {
        return vec![];
    };
    let tgt = |o: i32| (sw as i64 + 1 + o as i64) as usize;
    let end = tgt(*end as i32);
    let mut starts: Vec<usize> = offsets
        .iter()
        .filter(|&&o| o != 0)
        .map(|&o| tgt(o as i32))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let block = |s: usize| {
        let e = starts.iter().copied().find(|&x| x > s).unwrap_or(end);
        (s..e.min(ops.len())).any(|i| hit(&ops[i]))
    };
    offsets
        .iter()
        .enumerate()
        .filter(|&(_, &o)| o != 0 && block(tgt(o as i32)))
        .map(|(i, _)| i as i32)
        .collect()
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let game_t = obj_type(code, "Game")?;
    let dlg_t = obj_type(code, "ui.win.Dialog")?;
    let npc_t = obj_type(code, "ent.p.Npc")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let win_t = obj_type(code, "ui.Window")?;
    let ui_t = obj_type(code, "ui.win.UnitInfo")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    if !is_sub(code, ui_t, win_t) {
        bail!("ui.win.UnitInfo is not a ui.Window");
    }
    let a_len = field(code, arr_t, "length")?;
    if a_len.1 != i32_ {
        bail!("ArrayObj.length is not an i32");
    }
    let a_len = a_len.0;
    let a_raw = field(code, arr_t, "array")?;

    // getPlayerLockedWith(game, elt) -> BasePlayer
    let plw = method(code, game_t, "getPlayerLockedWith")?.findex;
    let (plw_args, plw_ret) = sig(code, plw)?;
    if plw_args.len() != 2 || plw_args[0] != game_t || plw_ret != bp_t {
        bail!("unexpected Game.getPlayerLockedWith signature");
    }
    // isLocalChoice(dialog, choice): choice.props.specialAction -> build()
    let ilc = method(code, dlg_t, "isLocalChoice")?;
    let choice_t = *fun_args(code, ilc).get(1).context("isLocalChoice arity")?;
    let c_props = field_of_virtual(code, choice_t, "props")?;
    let p_sa = field_of_virtual(code, c_props.1, "specialAction")?;
    let builds: Vec<RefFun> = ilc
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call1 { fun, .. } => Some(*fun),
            _ => None,
        })
        .collect();
    let build = *builds.first().context("isLocalChoice calls no build")?;
    if builds.iter().any(|&b| b != build) {
        bail!("isLocalChoice calls more than one function");
    }
    let (ba, sa_t) = sig(code, build)?;
    if ba != [p_sa.1]
        || !matches!(&code.types[sa_t.0], Type::Enum { name, .. } if s(code, *name) == "DialogSpecialAction")
    {
        bail!("unexpected DialogSpecialActionBuilder.build signature");
    }

    // displayChoices: the "waiting for" block
    //   Call2 isLocalChoice(this, ch) ... Call2 r = getPlayerLockedWith(game, elt)
    //   ... Call2 r = getPlayerLockedWith(game, elt)
    let dc = method(code, dlg_t, "displayChoices")?;
    let dc_fi = fun_index(code, dc.findex)?;
    let o = &dc.ops;
    let at: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::Call2 { fun, .. } if fun == plw))
        .collect();
    let [s1, s2] = at[..] else {
        bail!(
            "displayChoices: expected 2 getPlayerLockedWith calls, found {}",
            at.len()
        );
    };
    let (Opcode::Call2 { arg1: e1, .. }, Opcode::Call2 { arg1: e2, .. }) = (&o[s1], &o[s2]) else {
        unreachable!()
    };
    if e1 != e2 || s2 - s1 > 40 {
        bail!("displayChoices: the two lock calls are not one block");
    }
    let ilc_at = (0..s1)
        .rev()
        .find(|&i| matches!(o[i], Opcode::Call2 { fun, .. } if fun == ilc.findex))
        .context("displayChoices: no isLocalChoice before the lock test")?;
    let Opcode::Call2 { arg1: choice, .. } = o[ilc_at] else {
        unreachable!()
    };
    if s1 - ilc_at > 16 || dc.regs[choice.0 as usize] != choice_t {
        bail!("displayChoices: isLocalChoice is not the lock block's choice");
    }
    if (ilc_at + 1..=s2).any(|i| dst_of(&o[i]) == Some(choice)) {
        bail!("displayChoices: the choice register changes in the lock block");
    }
    // choices: the one ArrayObj register `choice` is read from
    // (`Field raw = list.array; GetArray d = raw[i]; ToVirtual choice = d`), set once.
    let lists: Vec<Reg> = (2..o.len())
        .filter_map(|i| match (&o[i - 2], &o[i - 1], &o[i]) {
            (
                Opcode::Field { dst: r, obj, field },
                Opcode::GetArray { dst: d, array, .. },
                Opcode::ToVirtual { dst, src },
            ) if *dst == choice && src == d && array == r && *field == a_raw.0 => Some(*obj),
            _ => None,
        })
        .collect();
    let choices = *lists.first().context("displayChoices: no choice list")?;
    if lists.iter().any(|&l| l != choices)
        || dc.regs[choices.0 as usize] != arr_t
        || o.iter().filter(|x| dst_of(x) == Some(choices)).count() != 1
    {
        bail!("displayChoices: the choice list is not one fixed ArrayObj");
    }

    // The choice handler: a Switch over the special action's ctor index with a
    // `Call3 recruitUnit(sa, elt, dialog)` case.
    let rec = static_or_method(code, dlg_t, "recruitUnit")?;
    let rec_fn = rec.findex;
    let (rec_args, rec_ret) = sig(code, rec_fn)?;
    if rec_args.len() != 3 || rec_args[0] != sa_t || rec_args[2] != dlg_t || rec_ret != void_ {
        bail!("unexpected Dialog.recruitUnit signature");
    }
    let rec_set = match rec.ops.iter().find(|x| matches!(x, Opcode::Switch { .. })) {
        Some(Opcode::Switch { offsets, .. }) => offsets
            .iter()
            .enumerate()
            .filter(|&(_, &o)| o != 0)
            .map(|(i, _)| i as i32)
            .collect::<Vec<_>>(),
        _ => bail!("recruitUnit has no ctor switch"),
    };
    let n_sa = match &code.types[sa_t.0] {
        Type::Enum { constructs, .. } => constructs.len(),
        _ => unreachable!(),
    };
    let handlers: Vec<(usize, usize)> = code
        .functions
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| {
            f.ops
                .iter()
                .enumerate()
                .filter(|(_, x)| matches!(x, Opcode::Call3 { fun, .. } if *fun == rec_fn))
                .map(move |(i, _)| (fi, i))
        })
        // the choice handler: a switch over every special action ctor before it
        .filter(|&(fi, at)| {
            code.functions[fi].ops[..at]
                .iter()
                .any(|x| matches!(x, Opcode::Switch { offsets, .. } if offsets.len() == n_sa))
        })
        .collect();
    let [(h_fi, rec_at)] = handlers[..] else {
        bail!(
            "expected one recruitUnit call in a choice handler, found {}",
            handlers.len()
        );
    };
    let h = &code.functions[h_fi];
    let sw = (0..rec_at)
        .rev()
        .find(|&i| matches!(&h.ops[i], Opcode::Switch { offsets, .. } if offsets.len() == n_sa))
        .context("choice handler: no switch before recruitUnit")?;
    let recruit = cases(
        &h.ops,
        sw,
        |x| matches!(x, Opcode::Call3 { fun, .. } if *fun == rec_fn),
    );
    if recruit.is_empty() || recruit.iter().any(|c| !rec_set.contains(c)) {
        bail!("recruit ctors {recruit:?} are not within recruitUnit's {rec_set:?}");
    }
    let customize = static_or_method(code, dlg_t, "netCustomizeUnit")?.findex;
    let set_locked = proto_or_method(code, bp_t, "set_lockedWith")?;
    let lockers = cases(&h.ops, sw, |x| {
        call_of(x).is_some_and(|(f, _)| f == customize || f == set_locked)
    });
    if lockers.is_empty() || lockers.iter().any(|c| recruit.contains(c)) {
        bail!("unexpected locking ctors {lockers:?}");
    }

    let base_t = obj_type(code, "hl.BaseType")?;
    let base_check = method(code, base_t, "check")?.findex;
    let npc_cls = class_global(code, "ent.p.Npc")?;
    let ui_cls = class_global(code, "ui.win.UnitInfo")?;
    let recruit_cost = method(code, npc_t, "getRecruitCost")?.findex;
    let (ra, cost_t) = sig(code, recruit_cost)?;
    if ra != [npc_t] {
        bail!("unexpected Npc.getRecruitCost signature");
    }
    let d_game = field(code, dlg_t, "game")?;
    if d_game.1 != game_t {
        bail!("Dialog.game is not a Game");
    }
    let g_me = field(code, game_t, "me")?;
    let bp_locked = field(code, bp_t, "lockedWith")?;
    if !is_sub(code, g_me.1, bp_t) && g_me.1 != bp_t {
        bail!("Game.me is not a BasePlayer");
    }
    let g_gui = field(code, game_t, "globalUI")?;
    let (u_windows, uw_t) = field(code, g_gui.1, "windows")?;
    if uw_t != arr_t {
        bail!("globalUI.windows is not an ArrayObj");
    }
    let ui_unit = field(code, ui_t, "unit")?;
    if ui_unit.1 != unit_t {
        bail!("UnitInfo.unit is not an st.Unit");
    }
    let in_troop = method(code, unit_t, "isInTroop")?.findex;
    let in_troop_t = match sig(code, in_troop)? {
        (a, r) if a == [unit_t] && matches!(code.types[r.0], Type::Null(x) if x == bool_) => r,
        _ => bail!("unexpected Unit.isInTroop signature"),
    };
    let w_close = slot(code, win_t, "close")?;
    Ok(Plan {
        dc_fi,
        sites: [s1, s2],
        choice,
        choices,
        h_fi,
        rec_at,
        recruit,
        lockers,
        plw,
        plw_args,
        bp_t,
        choice_t,
        c_props,
        p_sa,
        build,
        sa_t,
        arr_t,
        a_len,
        a_raw,
        base_check,
        npc_cls,
        npc_t,
        recruit_cost,
        cost_t,
        rec_fn,
        rec_args,
        d_game,
        g_me,
        bp_locked,
        g_gui,
        u_windows,
        win_t,
        ui_cls,
        ui_t,
        ui_unit,
        in_troop,
        in_troop_t,
        w_close,
        bool_,
        i32_,
        void_,
        dyn_t,
        dbg_file: debug_file(code, "src/ui/win/Dialog.hx")?,
    })
}

/// A static function `name` of class `t` (its `$Cls` statics) or an instance method.
fn static_or_method<'a>(code: &'a Bytecode, t: RefType, name: &str) -> Result<&'a Function> {
    method(code, t, name).or_else(|_| {
        let cls = s(code, obj(code, t)?.name).to_string();
        let (pkg, c) = cls.rsplit_once('.').unwrap_or(("", &cls));
        let st = if pkg.is_empty() {
            format!("${c}")
        } else {
            format!("{pkg}.${c}")
        };
        crate::diag::static_fn(code, &st, name)
    })
}

/// A proto (virtual) of `t` or a plain method.
fn proto_or_method(code: &Bytecode, t: RefType, name: &str) -> Result<RefFun> {
    proto(code, t, name).or_else(|_| Ok(method(code, t, name)?.findex))
}

/// Emits `ei = ctor index of ch.props.specialAction`, jumping to `none` when any
/// link is null.
#[allow(clippy::too_many_arguments)]
fn sa_index(
    a: &mut Asm,
    p: &Plan,
    ch: Reg,
    props: Reg,
    sa: Reg,
    e: Reg,
    ei: Reg,
    none: &'static str,
) {
    a.jmp(Opcode::JNull { reg: ch, offset: 0 }, none);
    a.op(Opcode::Field {
        dst: props,
        obj: ch,
        field: p.c_props.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: props,
            offset: 0,
        },
        none,
    );
    a.op(Opcode::Field {
        dst: sa,
        obj: props,
        field: p.p_sa.0,
    });
    a.jmp(Opcode::JNull { reg: sa, offset: 0 }, none);
    a.op(Opcode::Call1 {
        dst: e,
        fun: p.build,
        arg0: sa,
    });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, none);
    a.op(Opcode::EnumIndex { dst: ei, value: e });
}

/// `dlgLockedWith(game, elt, choice, choices) -> BasePlayer` (see the header).
fn add_locked(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let rec_c: Vec<_> = p.recruit.iter().map(|&k| int_const(code, k)).collect();
    let lock_c: Vec<_> = p.lockers.iter().map(|&k| int_const(code, k)).collect();
    let mut r = Regs(vec![p.plw_args[0], p.plw_args[1], p.choice_t, p.arr_t]);
    let (game, elt, ch, list) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let (pl, props, sa, e, ei, k) = (
        r.r(p.bp_t),
        r.r(p.c_props.1),
        r.r(p.p_sa.1),
        r.r(p.sa_t),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let (b, nc, npc, cost, n, i, raw, d, c) = (
        r.r(p.bool_),
        r.r(p.npc_cls.1),
        r.r(p.npc_t),
        r.r(p.cost_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.choice_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call2 {
        dst: pl,
        fun: p.plw,
        arg0: game,
        arg1: elt,
    });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "ret");
    // a recruit choice
    sa_index(&mut a, p, ch, props, sa, e, ei, "ret");
    for &kc in &rec_c {
        a.op(Opcode::Int { dst: k, ptr: kc });
        a.jmp(
            Opcode::JEq {
                a: ei,
                b: k,
                offset: 0,
            },
            "npc",
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "ret");
    // of a recruit NPC
    a.label("npc");
    a.op(Opcode::GetGlobal {
        dst: nc,
        global: p.npc_cls.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: nc,
        arg1: elt,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::UnsafeCast { dst: npc, src: elt });
    a.op(Opcode::Call1 {
        dst: cost,
        fun: p.recruit_cost,
        arg0: npc,
    });
    a.jmp(
        Opcode::JNull {
            reg: cost,
            offset: 0,
        },
        "ret",
    );
    // and no choice of this dialog locks the element itself (customize...)
    a.jmp(
        Opcode::JNull {
            reg: list,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: list,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("scan");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "free",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: list,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::ToVirtual { dst: c, src: d });
    sa_index(&mut a, p, c, props, sa, e, ei, "scan");
    for &kc in &lock_c {
        a.op(Opcode::Int { dst: k, ptr: kc });
        a.jmp(
            Opcode::JEq {
                a: ei,
                b: k,
                offset: 0,
            },
            "ret",
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "scan");
    a.label("free");
    a.op(Opcode::Null { dst: pl });
    a.label("ret");
    a.op(Opcode::Ret { ret: pl });
    push_fn(
        code,
        vec![p.plw_args[0], p.plw_args[1], p.choice_t, p.arr_t],
        p.bp_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `dlgRecruit(sa, elt, dialog)` (see the header).
fn add_recruit(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let i0 = int_const(code, 0);
    let mut r = Regs(p.rec_args.clone());
    let (sa, elt, dlg) = (Reg(0), Reg(1), Reg(2));
    let (v, game, me, lw, gui, list, n, i, raw, d, w) = (
        r.r(p.void_),
        r.r(p.d_game.1),
        r.r(p.g_me.1),
        r.r(p.bp_locked.1),
        r.r(p.g_gui.1),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.win_t),
    );
    let (uc, b, ui, un, nb) = (
        r.r(p.ui_cls.1),
        r.r(p.bool_),
        r.r(p.ui_t),
        r.r(p.ui_unit.1),
        r.r(p.in_troop_t),
    );
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: dlg,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: game,
        obj: dlg,
        field: p.d_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me.0,
    });
    a.jmp(Opcode::JNull { reg: me, offset: 0 }, "call");
    a.op(Opcode::Field {
        dst: lw,
        obj: me,
        field: p.bp_locked.0,
    });
    a.jmp(Opcode::JNull { reg: lw, offset: 0 }, "call");
    a.jmp(
        Opcode::JNotEq {
            a: lw,
            b: elt,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: gui,
        obj: game,
        field: p.g_gui.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: list,
        obj: gui,
        field: p.u_windows,
    });
    a.jmp(
        Opcode::JNull {
            reg: list,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: list,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("win");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "call",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: list,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::UnsafeCast { dst: w, src: d });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "win");
    a.op(Opcode::GetGlobal {
        dst: uc,
        global: p.ui_cls.0,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: uc,
        arg1: w,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "win");
    // guard 3: the sheet of a unit outside the troop (isInTroop null or false)
    a.op(Opcode::UnsafeCast { dst: ui, src: w });
    a.op(Opcode::Field {
        dst: un,
        obj: ui,
        field: p.ui_unit.0,
    });
    a.jmp(Opcode::JNull { reg: un, offset: 0 }, "win");
    a.op(Opcode::Call1 {
        dst: nb,
        fun: p.in_troop,
        arg0: un,
    });
    a.jmp(Opcode::JNull { reg: nb, offset: 0 }, "close");
    a.op(Opcode::SafeCast { dst: b, src: nb });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "win");
    a.label("close");
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.w_close,
        args: vec![w],
    });
    a.label("call");
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.rec_fn,
        arg0: sa,
        arg1: elt,
        arg2: dlg,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        p.rec_args.clone(),
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let locked = add_locked(code, p)?;
    let recruit = add_recruit(code, p)?;
    let f = &mut code.functions[p.dc_fi];
    for &at in &p.sites {
        let Opcode::Call2 {
            dst, arg0, arg1, ..
        } = f.ops[at]
        else {
            unreachable!()
        };
        f.ops[at] = Opcode::Call4 {
            dst,
            fun: locked,
            arg0,
            arg1,
            arg2: p.choice,
            arg3: p.choices,
        };
    }
    let dc = f.findex.0;
    let h = &mut code.functions[p.h_fi];
    let Opcode::Call3 {
        dst,
        arg0,
        arg1,
        arg2,
        ..
    } = h.ops[p.rec_at]
    else {
        unreachable!()
    };
    h.ops[p.rec_at] = Opcode::Call3 {
        dst,
        fun: recruit,
        arg0,
        arg1,
        arg2,
    };
    eprintln!(
        "patched dialog recruit lock fn@{dc} ops {:?}: recruit ctors {:?} ignore another player's lock on a recruit NPC unless ctors {:?} are offered (fn@{}); recruit fn@{} op {} closes this player's NPC sheet first (fn@{})",
        p.sites,
        p.recruit,
        p.lockers,
        locked.0,
        h.findex.0,
        p.rec_at,
        recruit.0
    );
    Ok(())
}

/// Lets a co-op player recruit a merc while another player reads its sheet, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_dialog_recruit(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p));
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("dialog recruit lock skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    fn ctor_names(code: &Bytecode, t: RefType, idx: &[i32]) -> Vec<String> {
        let Type::Enum { constructs, .. } = &code.types[t.0] else {
            panic!("not an enum")
        };
        idx.iter()
            .map(|&i| s(code, constructs[i as usize].name).to_string())
            .collect()
    }

    /// The recruit and locking ctors are the expected special actions.
    #[test]
    fn ctor_sets() {
        let Some(image) = game() else { return };
        let code = read(&image);
        let p = plan(&code).expect("plan");
        assert_eq!(
            ctor_names(&code, p.sa_t, &p.recruit),
            ["Recruit", "Accompany", "Follow", "Capture"]
        );
        let l = ctor_names(&code, p.sa_t, &p.lockers);
        for want in ["WarpaintUnit", "TattooUnit", "OpenCustomize"] {
            assert!(l.iter().any(|x| x == want), "{l:?}");
        }
    }

    /// Only the two lock calls of displayChoices and the handler's recruitUnit
    /// call change; two well-typed functions are appended; a second pass is a
    /// no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_dialog_recruit(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len() + 2);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != p.dc_fi && i != p.h_fi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        let n = back.functions.len();
        let (lf, rf) = (&back.functions[n - 2], &back.functions[n - 1]);
        for (fi, at) in [
            (p.dc_fi, p.sites[0]),
            (p.dc_fi, p.sites[1]),
            (p.h_fi, p.rec_at),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(a.ops.len(), b.ops.len());
            assert_eq!(a.regs, b.regs);
            check_types(&back, b, at..at + 1);
        }
        for (i, (x, y)) in orig.functions[p.dc_fi]
            .ops
            .iter()
            .zip(&back.functions[p.dc_fi].ops)
            .enumerate()
        {
            if p.sites.contains(&i) {
                assert!(matches!(y, Opcode::Call4 { fun, .. } if *fun == lf.findex));
            } else {
                assert_eq!(format!("{x:?}"), format!("{y:?}"), "op {i}");
            }
        }
        assert!(
            matches!(back.functions[p.h_fi].ops[p.rec_at], Opcode::Call3 { fun, .. } if fun == rf.findex)
        );
        check_types(&back, lf, 0..lf.ops.len());
        check_flow(lf);
        check_types(&back, rf, 0..rf.ops.len());
        check_flow(rf);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_dialog_recruit(&mut again);
        assert!(write(&again) == patched);
    }

    /// dlgLockedWith and dlgRecruit in the interpreter: another player's lock
    /// on a recruit NPC frees its recruit choice only (and not while a locking
    /// choice is offered); on the locked player's machine the recruit closes
    /// the NPC sheet before recruitUnit runs.
    #[test]
    fn sim_lock_and_close() {
        use crate::testsim::{Sim, V};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let orig_n = orig.functions.len();
        let mut code = read(&image);
        patch_dialog_recruit(&mut code);
        let n = code.functions.len();
        let (lf, rf) = (code.functions[n - 2].findex, code.functions[n - 1].findex);
        let pp = &p;
        let mut sim = Sim::new(
            &code,
            orig_n,
            move |c, f, a| {
                let p = pp;
                Some(if f == p.plw {
                    c.key_get(&a[1], "locker")
                } else if f == p.build {
                    a[0].clone()
                } else if f == p.base_check {
                    V::B(c.key_get(&a[1], "cls") == a[0])
                } else if f == p.recruit_cost {
                    c.key_get(&a[0], "cost")
                } else if f == p.in_troop {
                    c.key_get(&a[0], "troop")
                } else if f == p.rec_fn {
                    c.log.push(("recruit", a.to_vec()));
                    V::Null
                } else {
                    return None;
                })
            },
            |c, field, a| {
                c.log
                    .push(("close", vec![V::I(field as i32), a[0].clone()]));
                V::Null
            },
        );
        sim.c.globals.insert(p.npc_cls.0 .0, V::S("npc".into()));
        sim.c.globals.insert(p.ui_cls.0 .0, V::S("ui".into()));
        let c = &mut sim.c;
        let mk_choice = |c: &mut crate::testsim::Core, k: i32| {
            let sa = c.enm(k, vec![]);
            let props = c.obj(&[(p.p_sa.0, sa)]);
            c.obj(&[(p.c_props.0, props)])
        };
        let a_pl = c.obj(&[]);
        let elt = c.obj(&[]);
        c.key_set(&elt, "cls".into(), V::S("npc".into()));
        c.key_set(&elt, "cost".into(), V::I(1));
        let game = c.obj(&[]);
        let (recruit, inspect, tattoo) = (mk_choice(c, 6), mk_choice(c, 8), mk_choice(c, 30));
        let plain = c.arr(p.a_len, p.a_raw.0, vec![recruit.clone(), inspect.clone()]);
        let custom = c.arr(
            p.a_len,
            p.a_raw.0,
            vec![recruit.clone(), inspect.clone(), tattoo],
        );
        let lock = |sim: &mut Sim, locker: &V, ch: &V, list: &V| {
            sim.c.key_set(&elt, "locker".into(), locker.clone());
            sim.run(
                lf,
                vec![game.clone(), elt.clone(), ch.clone(), list.clone()],
            )
        };
        assert_eq!(lock(&mut sim, &V::Null, &recruit, &plain), V::Null);
        assert_eq!(lock(&mut sim, &a_pl, &recruit, &plain), V::Null);
        assert_eq!(lock(&mut sim, &a_pl, &inspect, &plain), a_pl);
        assert_eq!(lock(&mut sim, &a_pl, &recruit, &custom), a_pl);
        sim.c.key_set(&elt, "cost".into(), V::Null);
        assert_eq!(lock(&mut sim, &a_pl, &recruit, &plain), a_pl);
        sim.c.key_set(&elt, "cls".into(), V::S("other".into()));
        sim.c.key_set(&elt, "cost".into(), V::I(1));
        assert_eq!(lock(&mut sim, &a_pl, &recruit, &plain), a_pl);

        // dlgRecruit: me locked with elt, its sheet (unit outside the troop) in globalUI
        let c = &mut sim.c;
        let other_w = c.obj(&[]);
        c.key_set(&other_w, "cls".into(), V::S("x".into()));
        let unit = c.obj(&[]);
        let sheet = c.obj(&[(p.ui_unit.0, unit.clone())]);
        c.key_set(&sheet, "cls".into(), V::S("ui".into()));
        let wins = c.arr(p.a_len, p.a_raw.0, vec![other_w, sheet.clone()]);
        let gui = c.obj(&[(p.u_windows, wins)]);
        let me = c.obj(&[(p.bp_locked.0, elt.clone())]);
        let g = c.obj(&[(p.g_me.0, me.clone()), (p.g_gui.0, gui)]);
        let dlg = c.obj(&[(p.d_game.0, g)]);
        let sa = c.enm(6, vec![]);
        let recruit_run = |sim: &mut Sim| {
            sim.run(rf, vec![sa.clone(), elt.clone(), dlg.clone()]);
            let closed: Vec<V> = sim
                .c
                .take("close")
                .into_iter()
                .map(|a| a[1].clone())
                .collect();
            let rec = sim.c.take("recruit");
            assert_eq!(rec.len(), 1);
            assert_eq!(rec[0], vec![sa.clone(), elt.clone(), dlg.clone()]);
            closed
        };
        assert_eq!(recruit_run(&mut sim), vec![sheet.clone()]);
        // a troop unit's sheet stays
        sim.c.key_set(&unit, "troop".into(), V::B(true));
        assert!(recruit_run(&mut sim).is_empty());
        // not this player's lock: nothing closes
        sim.c.key_set(&unit, "troop".into(), V::B(false));
        sim.c.set(&me, p.bp_locked.0, V::Null);
        assert!(recruit_run(&mut sim).is_empty());
    }

    /// A displayChoices without the two-call lock block is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let mut code = read(&image);
        code.functions[p.dc_fi].ops[p.sites[1]] = Opcode::Label;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_dialog_recruit(&mut code);
        assert!(write(&code) == before);
    }
}
