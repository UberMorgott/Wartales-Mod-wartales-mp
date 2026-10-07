// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op spectating: a shared (data.cdb `props.coop`) mini-game no longer takes
// over the other players' screens. They stay in their scene with their own
// camera and input and watch the player's worker do it there.
//
// Vanilla: the element script (`ElementApi.a_startActivity`) starts a coop
// activity with `player = game.me` on the host, so `ui.win.Activity` lives on
// the host with `persist = CurrentMode` (`Controller.startActivity__impl`) and
// is replicated to every client (`State.updateReplication`). Its `setUnit` and
// `start` RPCs reach every machine (`networkRPC` cases 2 / 3 forward and run
// the impl), and `start__impl` -> `_start` has no owner test: on every machine
// it hides the inventory (`ui.showInventory(false)`), sets
// `mode.lockCamera`, `globalUI.padCursor.locked`, saves the camera, fades and
// moves the camera to the activity camera, and from the onReady closure
// creates the mini-game window (`miniGameToCall()`), `netSharing(true)`,
// `mode.setWindow(win)` and `addHelpIcon()` (an RPC: every machine's call adds
// an icon everywhere). The end (`onActionDone`) fades through
// `ctrl.netFade(coop)`, which also fades every other player. So every player
// got the window, the zoom and the input lock. The windows themselves are
// built for spectators: they gate input on `unit.owner == game.me` and play
// the worker's anims / props / effects from their replicated state, and on the
// host (also a spectator when a client plays) their logic stays authoritative.
// shim.log (client) shows it: `activity start id=Mine auth=false` followed by
// cancel / doResult with no `done` (a client watching the host mine).
//
// The patch keeps that window alive on a spectator but out of sight, and keeps
// the spectator's camera, inventory and input. A spectator is a machine where
// `spect(act)`: the activity is replicated (`__host != null`: coop only), its
// unit is set and `unit.owner != game.me` (the player the windows give the
// input to). Unit-less activities (BoardPuzzle, a solved NinePuzzle) keep the
// vanilla shared window: every player plays those.
//   _start: entry `m = spectPre(this)`: spectator -> `changeCamera = false`
//     (no fade, no camera move, onReady runs at once) and m = 1 | old
//     lockCamera << 1 | old padCursor.locked << 2; its Ret -> spectPost(this,
//     m): lockCamera / padCursor.locked back to their old values, cameraSave =
//     null (_cancel restores no camera), ui.showInventory(prevInventory), the
//     chooseUnit full-screen Interactive (added on every client) removed.
//     The rest of _start (start cost on the host, GC entry, trait level) runs.
//   onReady closure: showTutorial(..., cb) -> spectTut: a spectator gets no
//     tutorial, cb runs at once.
//   window closure: setWindow(win) -> spectWin: a spectator's window is made
//     invisible (windowRoot and the window: no draw, no Interactive events,
//     not the mode's current window) but stays in the windows list, updated
//     and networked; addHelpIcon() -> spectHelp: not called by a spectator.
//   addHelpIcon__impl: returns at once on a spectator (the player's icon RPC).
//   onActionDone's end fade: netFade(coop) -> spectFade: no replicated fade
//     when spectators are not in the activity camera (unit + owner set).
// shim.log: `mp: spectate <activity id> <m>` on each spectator.
// Not covered: the host's unit choice (a shared ChooseUnit, chooseUnit's
// full-screen wait Interactive on clients) before the start: which player
// starts it is known on the host only (`conds`, not synced).
// All players need this build. Validated before editing; a mismatch skips the
// pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::diag::{call_of, closure_passed_to, static_fn};
use super::job_xp::str_global;
use super::ping_cell::vproto;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

pub(crate) struct Plan {
    act_t: RefType,
    game_t: RefType,
    mode_t: RefType,
    gui_t: RefType,
    pad_t: RefType,
    ui_t: RefType,
    unit_t: RefType,
    player_t: RefType,
    host_t: RefType,
    int_t: RefType,
    win_t: RefType,
    flow_t: RefType,
    ctrl_t: RefType,
    cam_t: RefType,
    inf_t: RefType,
    cb_t: RefType,
    tut_t: RefType,
    str_t: RefType,
    dyn_t: RefType,
    bool_: RefType,
    i32_: RefType,
    void_: RefType,
    a_game: RefField,
    a_host: RefField,
    a_unit: RefField,
    a_int: RefField,
    a_cc: RefField,
    a_cam: RefField,
    a_prev: RefField,
    a_inf: RefField,
    i_id: RefField,
    u_owner: RefField,
    g_me: RefField,
    g_mode: RefField,
    g_gui: RefField,
    g_ui: RefField,
    m_lock: RefField,
    gui_pad: RefField,
    pad_locked: RefField,
    w_root: RefField,
    show_inv: RefFun,
    set_window: RefFun,
    add_help: RefFun,
    show_tut: RefFun,
    net_fade: RefFun,
    set_vis: RefFun,
    set_vis_pi: i32,
    obj_remove: RefFun,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    start_fi: usize,
    start_ret: usize,
    ready_fi: usize,
    tut_at: usize,
    make_fi: usize,
    win_at: usize,
    help_at: usize,
    help_fi: usize,
    help_ret: Reg,
    fade_fi: usize,
    fade_at: usize,
    fade_act: Reg,
    dbg_file: usize,
}

fn want(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("unexpected {what} signature");
    }
    Ok(())
}

fn want_field(code: &Bytecode, t: RefType, name: &str, ty: RefType) -> Result<RefField> {
    let (f, ft) = field(code, t, name)?;
    if ft != ty {
        bail!("field {name} has type {}, want {}", ft.0, ty.0);
    }
    Ok(f)
}

/// The register an op writes, if any (every opcode names it `dst`).
pub(crate) fn dst_of(op: &Opcode) -> Option<Reg> {
    let d = format!("{op:?}");
    let i = d.find("dst: Reg(")? + "dst: Reg(".len();
    let n: String = d[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok().map(Reg)
}

/// The only op of `f` calling `fun`.
fn only_call(f: &Function, fun: RefFun, what: &str) -> Result<usize> {
    let at: Vec<usize> = (0..f.ops.len())
        .filter(|&i| call_of(&f.ops[i]).is_some_and(|(g, _)| g == fun))
        .collect();
    match at[..] {
        [i] => Ok(i),
        _ => bail!("fn@{}: {} calls of {what}, want 1", f.findex.0, at.len()),
    }
}

pub(crate) fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    let act_t = obj_type(code, "ui.win.Activity")?;
    let game_t = obj_type(code, "Game")?;
    let mode_t = obj_type(code, "GameMode")?;
    let gui_t = obj_type(code, "ui.GlobalUI")?;
    let pad_t = obj_type(code, "gamepad.PadCursorController")?;
    let ui_t = obj_type(code, "ui.GameUI")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let host_t = obj_type(code, "hxbit.NetworkHost")?;
    let int_t = obj_type(code, "h2d.Interactive")?;
    let win_t = obj_type(code, "ui.Window")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let obj2_t = obj_type(code, "h2d.Object")?;
    let ctrl_t = obj_type(code, "st.Controller")?;

    let a_game = want_field(code, act_t, "game", game_t)?;
    let a_host = want_field(code, act_t, "__host", host_t)?;
    let a_unit = want_field(code, act_t, "unit", unit_t)?;
    let a_int = want_field(code, act_t, "int", int_t)?;
    let a_cc = want_field(code, act_t, "changeCamera", bool_)?;
    let a_prev = want_field(code, act_t, "prevInventory", bool_)?;
    let (a_cam, cam_t) = field(code, act_t, "cameraSave")?;
    if !matches!(code.types[cam_t.0], Type::Virtual { .. }) {
        bail!("Activity.cameraSave is not a virtual");
    }
    let (a_inf, inf_t) = field(code, act_t, "inf")?;
    let (i_id, id_t) = field_of_virtual(code, inf_t, "id")?;
    if id_t != str_t {
        bail!("activity inf.id is not a String");
    }
    let u_owner = want_field(code, unit_t, "owner", player_t)?;
    let g_me = want_field(code, game_t, "me", player_t)?;
    let g_mode = want_field(code, game_t, "mode", mode_t)?;
    let g_gui = want_field(code, game_t, "globalUI", gui_t)?;
    let g_ui = want_field(code, game_t, "ui", ui_t)?;
    let m_lock = want_field(code, mode_t, "lockCamera", bool_)?;
    let gui_pad = want_field(code, gui_t, "padCursor", pad_t)?;
    let pad_locked = want_field(code, pad_t, "locked", bool_)?;
    let w_root = want_field(code, win_t, "windowRoot", flow_t)?;

    let show_inv = method(code, ui_t, "showInventory")?.findex;
    want(
        code,
        show_inv,
        "GameUI.showInventory",
        &[ui_t, bool_],
        bool_,
    )?;
    let set_window = method(code, mode_t, "setWindow")?.findex;
    want(
        code,
        set_window,
        "GameMode.setWindow",
        &[mode_t, win_t],
        void_,
    )?;
    let add_help = method(code, act_t, "addHelpIcon")?.findex;
    want(code, add_help, "Activity.addHelpIcon", &[act_t], void_)?;
    let help_impl = method(code, act_t, "addHelpIcon__impl")?;
    if fun_args(code, help_impl) != [act_t] {
        bail!("unexpected Activity.addHelpIcon__impl signature");
    }
    let help_ret = match help_impl.ops.last() {
        Some(Opcode::Ret { ret }) if help_impl.regs[ret.0 as usize] == void_ => *ret,
        _ => bail!("Activity.addHelpIcon__impl does not end in a void Ret"),
    };
    let (show_tut, tut_args) = {
        let f = method(code, ui_t, "showTutorial")?;
        (f.findex, fun_args(code, f))
    };
    let (tut_t, cb_t) = match tut_args[..] {
        [u, s, o, c] if u == ui_t && s == str_t => (o, c),
        _ => bail!("unexpected GameUI.showTutorial signature"),
    };
    match &code.types[cb_t.0] {
        Type::Fun(f) if f.args.is_empty() && f.ret == void_ => {}
        _ => bail!("showTutorial callback is not () -> Void"),
    }
    if sig(code, show_tut)?.1 != bool_ {
        bail!("showTutorial does not return Bool");
    }
    let net_fade = method(code, ctrl_t, "netFade")?.findex;
    want(
        code,
        net_fade,
        "Controller.netFade",
        &[ctrl_t, bool_, cb_t],
        void_,
    )?;
    let (set_vis, set_vis_pi) = vproto(code, obj2_t, "set_visible")?;
    want(code, set_vis, "Object.set_visible", &[obj2_t, bool_], bool_)?;
    let obj_remove = proto(code, obj2_t, "remove")?;
    want(code, obj_remove, "Object.remove", &[obj2_t], void_)?;

    let println_f = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println_f) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    let println = println_f.findex;
    let std_string = static_fn(code, "$Std", "string")?.findex;
    want(code, std_string, "Std.string", &[dyn_t], str_t)?;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    want(code, str_add, "String.__add__", &[str_t, str_t], str_t)?;

    // _start: one Ret; the vanilla prefix (start cost) is untouched.
    let start = method(code, act_t, "_start")?;
    if fun_args(code, start) != [act_t] {
        bail!("unexpected Activity._start signature");
    }
    if matches!(start.ops.first(), Some(Opcode::Call1 { arg0: Reg(0), .. })) {
        bail!("already applied");
    }
    if !matches!(start.ops.first(), Some(Opcode::GetThis { .. })) {
        bail!("Activity._start: unexpected first op");
    }
    let rets: Vec<usize> = (0..start.ops.len())
        .filter(|&i| matches!(start.ops[i], Opcode::Ret { .. }))
        .collect();
    let [start_ret] = rets[..] else {
        bail!("Activity._start: {} Rets, want 1", rets.len());
    };
    if start_ret + 1 != start.ops.len() {
        bail!("Activity._start: the Ret is not the last op");
    }
    // the vanilla start does the three things a spectator undoes
    for (what, ok) in [
        (
            "showInventory",
            start
                .ops
                .iter()
                .any(|o| call_of(o).is_some_and(|(g, _)| g == show_inv)),
        ),
        (
            "lockCamera",
            start
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::SetField { field, .. } if *field == m_lock)),
        ),
        (
            "padCursor.locked",
            start
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::SetField { field, .. } if *field == pad_locked)),
        ),
        (
            "changeCamera",
            start
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::GetThis { field, .. } if *field == a_cc)),
        ),
    ] {
        if !ok {
            bail!("Activity._start: no {what}");
        }
    }
    let start_fi = fun_index(code, start.findex)?;

    // onReady: the closure of _start (bound to this) that calls showTutorial
    let readies: Vec<RefFun> = start
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::InstanceClosure {
                fun, obj: Reg(0), ..
            } => Some(*fun),
            _ => None,
        })
        .filter(|f| {
            fun_index(code, *f)
                .map(|i| diag::calls(&code.functions[i], show_tut))
                .unwrap_or(false)
        })
        .collect();
    let [ready] = readies[..] else {
        bail!(
            "Activity._start: {} onReady closures, want 1",
            readies.len()
        );
    };
    let ready_fi = fun_index(code, ready)?;
    let rf = &code.functions[ready_fi];
    if fun_args(code, rf) != [act_t] {
        bail!("Activity onReady: unexpected signature");
    }
    let tut_at = only_call(rf, show_tut, "showTutorial")?;
    if !matches!(rf.ops[tut_at], Opcode::Call4 { .. }) {
        bail!("Activity onReady: showTutorial is not a Call4");
    }
    let make_fi = closure_passed_to(code, ready_fi, show_tut)?;
    let mf = &code.functions[make_fi];
    if fun_args(code, mf) != [act_t] {
        bail!("Activity window closure: unexpected signature");
    }
    let win_at = only_call(mf, set_window, "setWindow")?;
    if !matches!(mf.ops[win_at], Opcode::Call2 { .. }) {
        bail!("Activity window closure: setWindow is not a Call2");
    }
    let help_at = only_call(mf, add_help, "addHelpIcon")?;
    if !matches!(mf.ops[help_at], Opcode::Call1 { arg0: Reg(0), .. }) {
        bail!("Activity window closure: addHelpIcon is not on this");
    }
    if !diag::calls(mf, method(code, win_t, "netSharing")?.findex) {
        bail!("Activity window closure: no netSharing");
    }

    // onActionDone's end: the only netFade of Activity.hx, with the Activity in a register
    let dbg = debug_file(code, "src/ui/win/Activity.hx")?;
    let fades: Vec<usize> = (0..code.functions.len())
        .filter(|&i| {
            let f = &code.functions[i];
            f.debug_info
                .as_ref()
                .is_some_and(|d| d.first().is_some_and(|(file, _)| *file == dbg))
                && diag::calls(f, net_fade)
        })
        .collect();
    let [fade_fi] = fades[..] else {
        bail!(
            "Activity.hx: {} functions call netFade, want 1",
            fades.len()
        );
    };
    let ff = &code.functions[fade_fi];
    let fade_at = only_call(ff, net_fade, "netFade")?;
    if !matches!(ff.ops[fade_at], Opcode::Call3 { .. }) {
        bail!("onActionDone fade: netFade is not a Call3");
    }
    // the Activity register: last written before the call by an EnumField
    // (the closure's capture), not written again until the call
    let acts: Vec<Reg> = (0..ff.regs.len())
        .map(|r| Reg(r as u32))
        .filter(|r| ff.regs[r.0 as usize] == act_t)
        .filter(|r| {
            matches!(
                ff.ops[..fade_at]
                    .iter()
                    .rev()
                    .find(|o| dst_of(o) == Some(*r)),
                Some(Opcode::EnumField { .. })
            )
        })
        .collect();
    let [fade_act] = acts[..] else {
        bail!(
            "onActionDone fade: {} Activity registers, want 1",
            acts.len()
        );
    };

    Ok(Plan {
        act_t,
        game_t,
        mode_t,
        gui_t,
        pad_t,
        ui_t,
        unit_t,
        player_t,
        host_t,
        int_t,
        win_t,
        flow_t,
        ctrl_t,
        cam_t,
        inf_t,
        cb_t,
        tut_t,
        str_t,
        dyn_t,
        bool_,
        i32_,
        void_,
        a_game,
        a_host,
        a_unit,
        a_int,
        a_cc,
        a_cam,
        a_prev,
        a_inf,
        i_id,
        u_owner,
        g_me,
        g_mode,
        g_gui,
        g_ui,
        m_lock,
        gui_pad,
        pad_locked,
        w_root,
        show_inv,
        set_window,
        add_help,
        show_tut,
        net_fade,
        set_vis,
        set_vis_pi,
        obj_remove,
        println,
        std_string,
        str_add,
        start_fi,
        start_ret,
        ready_fi,
        tut_at,
        make_fi,
        win_at,
        help_at,
        help_fi: fun_index(code, help_impl.findex)?,
        help_ret,
        fade_fi,
        fade_at,
        fade_act,
        dbg_file: dbg,
    })
}

fn int(a: &mut Asm, code: &mut Bytecode, dst: Reg, v: i32) {
    let ptr = int_const(code, v);
    a.op(Opcode::Int { dst, ptr });
}

/// `spect(act) -> Bool`: this machine watches `act` (see the header).
fn add_spect(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (res, exc, host, unit, owner, game, me) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.host_t),
        r.r(p.unit_t),
        r.r(p.player_t),
        r.r(p.game_t),
        r.r(p.player_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    for (dst, obj, field) in [
        (host, act, p.a_host),
        (unit, act, p.a_unit),
        (owner, unit, p.u_owner),
        (game, act, p.a_game),
    ] {
        a.op(Opcode::Field { dst, obj, field });
        a.jmp(
            Opcode::JNull {
                reg: dst,
                offset: 0,
            },
            "untrap",
        );
    }
    a.op(Opcode::Field {
        dst: me,
        obj: game,
        field: p.g_me,
    });
    a.jmp(
        Opcode::JEq {
            a: owner,
            b: me,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("catch");
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.act_t], p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectPre(act) -> Int`: 0 when not watching; else changeCamera = false and
/// 1 | lockCamera << 1 | padCursor.locked << 2 as they were.
fn add_pre(
    code: &mut Bytecode,
    p: &Plan,
    spect: RefFun,
    tag: RefGlobal,
    sp: RefGlobal,
) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let act = Reg(0);
    let (m, b, exc, k, game, mode, lk, gui, pad, pl) = (
        r.r(p.i32_),
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.i32_),
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.bool_),
        r.r(p.gui_t),
        r.r(p.pad_t),
        r.r(p.bool_),
    );
    let (inf, id, acc, t, d, v) = (
        r.r(p.inf_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    int(&mut a, code, m, 0);
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    int(&mut a, code, m, 1);
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_cc,
        src: b,
    });
    a.op(Opcode::Field {
        dst: game,
        obj: act,
        field: p.a_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "pad",
    );
    a.op(Opcode::Field {
        dst: lk,
        obj: mode,
        field: p.m_lock,
    });
    a.jmp(
        Opcode::JFalse {
            cond: lk,
            offset: 0,
        },
        "pad",
    );
    int(&mut a, code, k, 2);
    a.op(Opcode::Or { dst: m, a: m, b: k });
    a.label("pad");
    a.op(Opcode::Field {
        dst: gui,
        obj: game,
        field: p.g_gui,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "log",
    );
    a.op(Opcode::Field {
        dst: pad,
        obj: gui,
        field: p.gui_pad,
    });
    a.jmp(
        Opcode::JNull {
            reg: pad,
            offset: 0,
        },
        "log",
    );
    a.op(Opcode::Field {
        dst: pl,
        obj: pad,
        field: p.pad_locked,
    });
    a.jmp(
        Opcode::JFalse {
            cond: pl,
            offset: 0,
        },
        "log",
    );
    int(&mut a, code, k, 4);
    a.op(Opcode::Or { dst: m, a: m, b: k });
    // println("mp: spectate " + inf.id + " " + m)
    a.label("log");
    a.op(Opcode::GetGlobal {
        dst: acc,
        global: tag,
    });
    a.op(Opcode::Field {
        dst: inf,
        obj: act,
        field: p.a_inf,
    });
    a.jmp(
        Opcode::JNull {
            reg: inf,
            offset: 0,
        },
        "num",
    );
    a.op(Opcode::Field {
        dst: id,
        obj: inf,
        field: p.i_id,
    });
    a.jmp(Opcode::JNull { reg: id, offset: 0 }, "num");
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: id,
    });
    a.label("num");
    a.op(Opcode::GetGlobal { dst: t, global: sp });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: t,
    });
    a.op(Opcode::ToDyn { dst: d, src: m });
    a.op(Opcode::Call1 {
        dst: t,
        fun: p.std_string,
        arg0: d,
    });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: p.str_add,
        arg0: acc,
        arg1: t,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: acc,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: m });
    push_fn(code, vec![p.act_t], p.i32_, r.0, a.finish(), p.dbg_file)
}

/// `spectPost(act, m)`: undo what _start did to a spectator's camera, input and inventory.
fn add_post(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t, p.i32_]);
    let (act, m) = (Reg(0), Reg(1));
    let (exc, k, z, t, b, game, cam, mode, ui, pv, gui, pad, it, v) = (
        r.r(p.dyn_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.bool_),
        r.r(p.game_t),
        r.r(p.cam_t),
        r.r(p.mode_t),
        r.r(p.ui_t),
        r.r(p.bool_),
        r.r(p.gui_t),
        r.r(p.pad_t),
        r.r(p.int_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    int(&mut a, code, z, 0);
    a.jmp(
        Opcode::JEq {
            a: m,
            b: z,
            offset: 0,
        },
        "ret",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "ret");
    a.op(Opcode::Null { dst: cam });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_cam,
        src: cam,
    });
    a.op(Opcode::Field {
        dst: game,
        obj: act,
        field: p.a_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "int",
    );
    // bit `bit` of m as a Bool in b
    let bit = |a: &mut Asm, code: &mut Bytecode, n: i32, l1: &'static str| {
        int(a, code, k, n);
        a.op(Opcode::And { dst: t, a: m, b: k });
        a.op(Opcode::Bool {
            dst: b,
            value: ValBool(false),
        });
        a.jmp(
            Opcode::JEq {
                a: t,
                b: z,
                offset: 0,
            },
            l1,
        );
        a.op(Opcode::Bool {
            dst: b,
            value: ValBool(true),
        });
        a.label(l1);
    };
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.jmp(
        Opcode::JNull {
            reg: mode,
            offset: 0,
        },
        "inv",
    );
    bit(&mut a, code, 2, "lk");
    a.op(Opcode::SetField {
        obj: mode,
        field: p.m_lock,
        src: b,
    });
    a.label("inv");
    a.op(Opcode::Field {
        dst: ui,
        obj: game,
        field: p.g_ui,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "pad");
    a.op(Opcode::Field {
        dst: pv,
        obj: act,
        field: p.a_prev,
    });
    a.op(Opcode::Call2 {
        dst: pv,
        fun: p.show_inv,
        arg0: ui,
        arg1: pv,
    });
    a.label("pad");
    a.op(Opcode::Field {
        dst: gui,
        obj: game,
        field: p.g_gui,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "int",
    );
    a.op(Opcode::Field {
        dst: pad,
        obj: gui,
        field: p.gui_pad,
    });
    a.jmp(
        Opcode::JNull {
            reg: pad,
            offset: 0,
        },
        "int",
    );
    bit(&mut a, code, 4, "pl");
    a.op(Opcode::SetField {
        obj: pad,
        field: p.pad_locked,
        src: b,
    });
    // chooseUnit's full-screen wait Interactive
    a.label("int");
    a.op(Opcode::Field {
        dst: it,
        obj: act,
        field: p.a_int,
    });
    a.jmp(Opcode::JNull { reg: it, offset: 0 }, "untrap");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.obj_remove,
        arg0: it,
    });
    a.op(Opcode::Null { dst: it });
    a.op(Opcode::SetField {
        obj: act,
        field: p.a_int,
        src: it,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.act_t, p.i32_],
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `spectTut(ui, id, opts, cb, act) -> Bool`: a spectator gets no tutorial (false), cb runs at once.
fn add_tut(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let args = vec![p.ui_t, p.str_t, p.tut_t, p.cb_t, p.act_t];
    let mut r = Regs(args.clone());
    let (b, v) = (r.r(p.bool_), r.r(p.void_));
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: Reg(4),
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    a.op(Opcode::CallClosure {
        dst: v,
        fun: Reg(3),
        args: vec![],
    });
    // no tutorial shown
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("vanilla");
    a.op(Opcode::Call4 {
        dst: b,
        fun: p.show_tut,
        arg0: Reg(0),
        arg1: Reg(1),
        arg2: Reg(2),
        arg3: Reg(3),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(code, args, p.bool_, r.0, a.finish(), p.dbg_file)
}

/// `spectWin(mode, win, act)`: a spectator's window is hidden, never the current one.
fn add_win(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let args = vec![p.mode_t, p.win_t, p.act_t];
    let mut r = Regs(args.clone());
    let (mode, win, act) = (Reg(0), Reg(1), Reg(2));
    let (b, exc, root, f, v) = (
        r.r(p.bool_),
        r.r(p.dyn_t),
        r.r(p.flow_t),
        r.r(p.bool_),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: act,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "vanilla");
    // a failure while hiding shows the window the vanilla way
    a.jmp(Opcode::Trap { exc, offset: 0 }, "vanilla");
    a.op(Opcode::Bool {
        dst: f,
        value: ValBool(false),
    });
    let hide = |a: &mut Asm, o: Reg| {
        if p.set_vis_pi >= 0 {
            a.op(Opcode::CallMethod {
                dst: b,
                field: RefField(p.set_vis_pi as usize),
                args: vec![o, f],
            });
        } else {
            a.op(Opcode::Call2 {
                dst: b,
                fun: p.set_vis,
                arg0: o,
                arg1: f,
            });
        }
    };
    a.op(Opcode::Field {
        dst: root,
        obj: win,
        field: p.w_root,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "self",
    );
    hide(&mut a, root);
    a.label("self");
    hide(&mut a, win);
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("vanilla");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_window,
        arg0: mode,
        arg1: win,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, args, p.void_, r.0, a.finish(), p.dbg_file)
}

/// `spectHelp(act)`: addHelpIcon() unless this machine is a spectator.
fn add_help(code: &mut Bytecode, p: &Plan, spect: RefFun) -> Result<RefFun> {
    let mut r = Regs(vec![p.act_t]);
    let (b, v) = (r.r(p.bool_), r.r(p.void_));
    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: b,
        fun: spect,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.add_help,
        arg0: Reg(0),
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.act_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `spectFade(ctrl, replicated, cb, act)`: no replicated fade when the others
/// watch from their own camera (the activity has a unit with an owner).
fn add_fade(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let args = vec![p.ctrl_t, p.bool_, p.cb_t, p.act_t];
    let mut r = Regs(args.clone());
    let (ctrl, rep, cb, act) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let (exc, unit, owner, v) = (r.r(p.dyn_t), r.r(p.unit_t), r.r(p.player_t), r.r(p.void_));
    let mut a = Asm::new();
    a.jmp(
        Opcode::JFalse {
            cond: rep,
            offset: 0,
        },
        "call",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "call");
    a.op(Opcode::Field {
        dst: unit,
        obj: act,
        field: p.a_unit,
    });
    a.jmp(
        Opcode::JNull {
            reg: unit,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: owner,
        obj: unit,
        field: p.u_owner,
    });
    a.jmp(
        Opcode::JNull {
            reg: owner,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Bool {
        dst: rep,
        value: ValBool(false),
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("call");
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.net_fade,
        arg0: ctrl,
        arg1: rep,
        arg2: cb,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, args, p.void_, r.0, a.finish(), p.dbg_file)
}

/// New functions, in this order: spect, spectPre, spectPost, spectTut,
/// spectWin, spectHelp, spectFade.
fn apply(code: &mut Bytecode, p: &Plan) -> Result<[RefFun; 7]> {
    let tag = str_global(code, p.str_t, "mp: spectate ");
    let sp = str_global(code, p.str_t, " ");
    let spect = add_spect(code, p)?;
    let pre = add_pre(code, p, spect, tag, sp)?;
    let post = add_post(code, p)?;
    let tut = add_tut(code, p, spect)?;
    let win = add_win(code, p, spect)?;
    let help = add_help(code, p, spect)?;
    let fade = add_fade(code, p)?;

    // _start: m = spectPre(this) ... Ret -> JAlways tail; tail: spectPost(this, m); Ret
    {
        let f = &mut code.functions[p.start_fi];
        let Opcode::Ret { ret } = f.ops[p.start_ret] else {
            unreachable!()
        };
        f.regs.push(p.i32_);
        let m = Reg((f.regs.len() - 1) as u32);
        f.regs.push(p.void_);
        let v = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            0,
            vec![Opcode::Call1 {
                dst: m,
                fun: pre,
                arg0: Reg(0),
            }],
        );
        let at = p.start_ret + 1;
        let n = f.ops.len();
        f.ops[at] = Opcode::JAlways {
            offset: (n - at - 1) as i32,
        };
        f.ops.push(Opcode::Call2 {
            dst: v,
            fun: post,
            arg0: Reg(0),
            arg1: m,
        });
        f.ops.push(Opcode::Ret { ret });
        if let Some(d) = &mut f.debug_info {
            let line = d[at];
            d.push(line);
            d.push(line);
        }
    }
    // onReady: showTutorial(ui, id, opts, cb) -> spectTut(ui, id, opts, cb, this)
    {
        let f = &mut code.functions[p.ready_fi];
        let Opcode::Call4 {
            dst,
            arg0,
            arg1,
            arg2,
            arg3,
            ..
        } = f.ops[p.tut_at]
        else {
            unreachable!()
        };
        f.ops[p.tut_at] = Opcode::CallN {
            dst,
            fun: tut,
            args: vec![arg0, arg1, arg2, arg3, Reg(0)],
        };
    }
    // window closure: setWindow(mode, win) -> spectWin(mode, win, this);
    // addHelpIcon(this) -> spectHelp(this)
    {
        let f = &mut code.functions[p.make_fi];
        let Opcode::Call2 {
            dst, arg0, arg1, ..
        } = f.ops[p.win_at]
        else {
            unreachable!()
        };
        f.ops[p.win_at] = Opcode::Call3 {
            dst,
            fun: win,
            arg0,
            arg1,
            arg2: Reg(0),
        };
        let Opcode::Call1 { dst, .. } = f.ops[p.help_at] else {
            unreachable!()
        };
        f.ops[p.help_at] = Opcode::Call1 {
            dst,
            fun: help,
            arg0: Reg(0),
        };
    }
    // addHelpIcon__impl: if (spect(this)) return;
    {
        let f = &mut code.functions[p.help_fi];
        let ret = p.help_ret;
        f.regs.push(p.bool_);
        let b = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            0,
            vec![
                Opcode::Call1 {
                    dst: b,
                    fun: spect,
                    arg0: Reg(0),
                },
                Opcode::JFalse { cond: b, offset: 1 },
                Opcode::Ret { ret },
            ],
        );
    }
    // onActionDone's fade: netFade(ctrl, coop, cb) -> spectFade(ctrl, coop, cb, act)
    {
        let f = &mut code.functions[p.fade_fi];
        let Opcode::Call3 {
            dst,
            arg0,
            arg1,
            arg2,
            ..
        } = f.ops[p.fade_at]
        else {
            unreachable!()
        };
        f.ops[p.fade_at] = Opcode::Call4 {
            dst,
            fun: fade,
            arg0,
            arg1,
            arg2,
            arg3: p.fade_act,
        };
    }
    eprintln!(
        "patched coop spectate: Activity._start fn@{} (pre fn@{}, post fn@{}), onReady fn@{} \
         tutorial -> fn@{}, window closure fn@{} setWindow -> fn@{} / addHelpIcon -> fn@{}, \
         addHelpIcon__impl gated (spect fn@{}), end fade fn@{} -> fn@{}: other players keep \
         their scene and watch the worker",
        code.functions[p.start_fi].findex.0,
        pre.0,
        post.0,
        code.functions[p.ready_fi].findex.0,
        tut.0,
        code.functions[p.make_fi].findex.0,
        win.0,
        help.0,
        spect.0,
        code.functions[p.fade_fi].findex.0,
        fade.0
    );
    Ok([spect, pre, post, tut, win, help, fade])
}

/// Keeps shared mini-game windows off the other players' screens, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_coop_spectate(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p).map(|_| ()));
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("coop spectate skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::same;
    use crate::asm::testutil::*;
    use crate::testsim::{Core, Sim, V};

    /// Five sites edited, seven well-typed functions appended, nothing else
    /// touched; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_coop_spectate(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 7);
        let sites = [p.start_fi, p.ready_fi, p.make_fi, p.help_fi, p.fade_fi];
        for i in 0..n {
            assert_eq!(
                !same(&orig.functions[i], &back.functions[i]),
                sites.contains(&i),
                "function #{i}"
            );
        }
        let nf = |k: usize| back.functions[n + k].findex;
        let (spect, pre, post, tut, win, help, fade) =
            (nf(0), nf(1), nf(2), nf(3), nf(4), nf(5), nf(6));
        for &fi in &sites {
            let f = &back.functions[fi];
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        for f in &back.functions[n..] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // _start: one op in front, the old Ret jumps to spectPost + Ret
        let (a, b) = (&orig.functions[p.start_fi], &back.functions[p.start_fi]);
        assert!(matches!(b.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == pre));
        let k = b.ops.len();
        assert_eq!(k, a.ops.len() + 3);
        assert!(matches!(b.ops[k - 2], Opcode::Call2 { fun, arg0: Reg(0), .. } if fun == post));
        assert_eq!(
            format!("{:?}", b.ops[k - 1]),
            format!("{:?}", a.ops[p.start_ret])
        );
        assert_eq!(jump_targets(b, p.start_ret + 1), vec![k - 2]);
        for i in 0..p.start_ret {
            let want: Vec<usize> = jump_targets(a, i).into_iter().map(|t| t + 1).collect();
            assert_eq!(jump_targets(b, i + 1), want, "_start op {i}");
            if want.is_empty() {
                assert_eq!(format!("{:?}", b.ops[i + 1]), format!("{:?}", a.ops[i]));
            }
        }
        // the three call sites keep their registers, plus this
        let ready = &back.functions[p.ready_fi];
        assert!(matches!(&ready.ops[p.tut_at],
            Opcode::CallN { fun, args, .. } if *fun == tut && args.len() == 5 && args[4] == Reg(0)));
        let make = &back.functions[p.make_fi];
        assert!(
            matches!(make.ops[p.win_at], Opcode::Call3 { fun, arg2: Reg(0), .. } if fun == win)
        );
        assert!(
            matches!(make.ops[p.help_at], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == help)
        );
        let ff = &back.functions[p.fade_fi];
        assert!(
            matches!(ff.ops[p.fade_at], Opcode::Call4 { fun, arg3, .. } if fun == fade && arg3 == p.fade_act)
        );
        for (fi, at) in [
            (p.ready_fi, p.tut_at),
            (p.make_fi, p.win_at),
            (p.make_fi, p.help_at),
            (p.fade_fi, p.fade_at),
        ] {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(a.ops.len(), b.ops.len());
            for i in (0..a.ops.len())
                .filter(|&i| i != at && !(fi == p.make_fi && (i == p.win_at || i == p.help_at)))
            {
                assert_eq!(
                    format!("{:?}", a.ops[i]),
                    format!("{:?}", b.ops[i]),
                    "fn #{fi} op {i}"
                );
            }
        }
        // the fade's Activity register is the closure capture of onActionDone
        assert_eq!(ff.regs[p.fade_act.0 as usize], p.act_t);
        // addHelpIcon__impl: if (spect(this)) return;
        let hf = &back.functions[p.help_fi];
        let mut want = orig.functions[p.help_fi].clone();
        want.regs = hf.regs.clone();
        shifted(&want, hf, 0, 3);
        assert!(matches!(hf.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == spect));
        assert_eq!(jump_targets(hf, 1), vec![3]);
        // every vanilla call made by the new functions is one the plan validated
        let allowed = [
            p.show_inv,
            p.set_window,
            p.add_help,
            p.show_tut,
            p.net_fade,
            p.set_vis,
            p.obj_remove,
            p.println,
            p.std_string,
            p.str_add,
            spect,
        ];
        for f in &back.functions[n..] {
            for op in &f.ops {
                if let Some((g, _)) = call_of(op) {
                    assert!(allowed.contains(&g), "fn@{} calls fn@{}", f.findex.0, g.0);
                }
                if let Opcode::CallMethod { field, .. } = op {
                    assert_eq!(field.0 as i32, p.set_vis_pi);
                }
            }
        }
        let mut again = read(&patched);
        assert_eq!(
            format!("{:#}", plan(&again).err().unwrap()),
            "already applied"
        );
        patch_coop_spectate(&mut again);
        assert!(write(&again) == patched);
    }

    /// The pass runs in the full pipeline.
    #[test]
    fn applies_in_pipeline() {
        let Some(image) = game() else { return };
        let code = read(&crate::patch_image(&image).expect("patch"));
        assert_eq!(
            format!("{:#}", plan(&code).err().unwrap()),
            "already applied"
        );
    }

    /// A mismatch skips the whole pass and leaves the image as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let breakers: Vec<(&str, Box<dyn Fn(&mut Bytecode)>)> = vec![
            (
                "no setWindow",
                Box::new(move |c: &mut Bytecode| {
                    c.functions[p.make_fi].ops[p.win_at] = Opcode::Nop
                }),
            ),
            (
                "two Rets",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[p.start_fi];
                    let r = f.ops[p.start_ret].clone();
                    f.ops[p.start_ret - 1] = r;
                }),
            ),
            (
                "no fade capture",
                Box::new(move |c: &mut Bytecode| {
                    let f = &mut c.functions[p.fade_fi];
                    let i = f.ops[..p.fade_at]
                        .iter()
                        .rposition(|o| dst_of(o) == Some(p.fade_act))
                        .unwrap();
                    f.ops[i] = Opcode::Null { dst: p.fade_act };
                }),
            ),
        ];
        for (what, brk) in breakers {
            let mut code = read(&image);
            brk(&mut code);
            assert!(plan(&code).is_err(), "{what}");
            let before = write(&code);
            patch_coop_spectate(&mut code);
            assert!(write(&code) == before, "{what}: image changed");
        }
    }

    // ---------- behaviour ----------

    fn patched() -> Option<(Bytecode, usize, Plan)> {
        let image = std::fs::read(HLBOOT).ok()?;
        let p = plan(&read(&image)).unwrap();
        let mut code = read(&image);
        let n = code.functions.len();
        patch_coop_spectate(&mut code);
        Some((code, n, p))
    }

    fn sim<'a>(code: &'a Bytecode, n: usize, p: &'a Plan) -> Sim<'a> {
        Sim::new(
            code,
            n,
            move |c: &mut Core, f: RefFun, a: &[V]| {
                let log = |c: &mut Core, k: &'static str, v: V| {
                    c.log.push((k, a.to_vec()));
                    Some(v)
                };
                if f == p.show_inv {
                    log(c, "showInventory", V::B(true))
                } else if f == p.set_window {
                    log(c, "setWindow", V::Null)
                } else if f == p.add_help {
                    log(c, "addHelpIcon", V::Null)
                } else if f == p.show_tut {
                    log(c, "showTutorial", V::B(true))
                } else if f == p.net_fade {
                    log(c, "netFade", V::Null)
                } else if f == p.set_vis {
                    log(c, "visible", V::B(false))
                } else if f == p.obj_remove {
                    log(c, "remove", V::Null)
                } else if f == p.println {
                    log(c, "println", V::Null)
                } else if f == p.std_string {
                    match &a[0] {
                        V::I(n) => Some(V::S(n.to_string())),
                        o => panic!("Std.string {o:?}"),
                    }
                } else if f == p.str_add {
                    match (&a[0], &a[1]) {
                        (V::S(x), V::S(y)) => Some(V::S(format!("{x}{y}"))),
                        o => panic!("String add {o:?}"),
                    }
                } else {
                    None
                }
            },
            move |c: &mut Core, pi: usize, a: &[V]| {
                assert_eq!(pi as i32, p.set_vis_pi);
                c.log.push(("visible", a.to_vec()));
                V::B(false)
            },
        )
    }

    struct W {
        act: V,
        game: V,
        me: V,
        other: V,
        mode: V,
        pad: V,
        unit: V,
    }

    /// A machine (`me`) with an activity whose unit belongs to `other`.
    fn world(s: &mut Sim, p: &Plan) -> W {
        let c = &mut s.c;
        let (me, other) = (c.obj(&[]), c.obj(&[]));
        let mode = c.obj(&[(p.m_lock, V::B(false))]);
        let pad = c.obj(&[(p.pad_locked, V::B(false))]);
        let gui = c.obj(&[(p.gui_pad, pad.clone())]);
        let ui = c.obj(&[]);
        let game = c.obj(&[
            (p.g_me, me.clone()),
            (p.g_mode, mode.clone()),
            (p.g_gui, gui),
            (p.g_ui, ui),
        ]);
        let unit = c.obj(&[(p.u_owner, other.clone())]);
        let host = c.obj(&[]);
        let inf = c.obj(&[(p.i_id, V::S("Fish".into()))]);
        let act = c.obj(&[
            (p.a_game, game.clone()),
            (p.a_host, host),
            (p.a_unit, unit.clone()),
            (p.a_inf, inf),
            (p.a_cc, V::B(true)),
            (p.a_prev, V::B(true)),
        ]);
        W {
            act,
            game,
            me,
            other,
            mode,
            pad,
            unit,
        }
    }

    /// Who is a spectator: replicated activity, unit set, owned by another player.
    #[test]
    fn spectator_test() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let spect = code.functions[n].findex;
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(true));
        // the player doing it
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(false));
        s.c.set(&w.unit, p.u_owner, w.other.clone());
        // not replicated (a local, non-coop activity), no unit, no owner
        for (o, f) in [(&w.act, p.a_host), (&w.act, p.a_unit), (&w.unit, p.u_owner)] {
            let keep = s.c.get(o, f);
            s.c.set(o, f, V::Null);
            assert_eq!(s.run(spect, vec![w.act.clone()]), V::B(false));
            s.c.set(o, f, keep);
        }
        assert!(s.c.log.is_empty());
    }

    /// _start's pre / post on a spectator: no camera change, old locks and
    /// inventory back, the chooseUnit wait Interactive gone; nothing for the player.
    #[test]
    fn start_pre_post() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let (pre, post) = (code.functions[n + 1].findex, code.functions[n + 2].findex);
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let it = s.c.obj(&[]);
        s.c.set(&w.act, p.a_int, it.clone());
        let cam = s.c.obj(&[]);
        s.c.set(&w.act, p.a_cam, cam);
        // the padCursor was locked before (bit 2), the camera not
        s.c.set(&w.pad, p.pad_locked, V::B(true));
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::I(1 | 4));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(false));
        assert_eq!(
            s.c.take("println"),
            vec![vec![V::S("mp: spectate Fish 5".into())]]
        );
        // vanilla _start ran in between
        s.c.set(&w.mode, p.m_lock, V::B(true));
        s.c.set(&w.pad, p.pad_locked, V::B(true));
        s.c.set(&w.act, p.a_prev, V::B(true));
        s.run(post, vec![w.act.clone(), V::I(5)]);
        assert_eq!(s.c.get(&w.mode, p.m_lock), V::B(false));
        assert_eq!(s.c.get(&w.pad, p.pad_locked), V::B(true));
        assert_eq!(s.c.get(&w.act, p.a_cam), V::Null);
        assert_eq!(s.c.get(&w.act, p.a_int), V::Null);
        let ui = s.c.get(&w.game, p.g_ui);
        assert_eq!(s.c.take("showInventory"), vec![vec![ui, V::B(true)]]);
        assert_eq!(s.c.take("remove"), vec![vec![it]]);
        assert!(s.c.log.is_empty());

        // the player doing it: pre says 0, post does nothing
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        s.c.set(&w.act, p.a_cc, V::B(true));
        assert_eq!(s.run(pre, vec![w.act.clone()]), V::I(0));
        assert_eq!(s.c.get(&w.act, p.a_cc), V::B(true));
        s.c.set(&w.mode, p.m_lock, V::B(true));
        s.run(post, vec![w.act.clone(), V::I(0)]);
        assert_eq!(s.c.get(&w.mode, p.m_lock), V::B(true));
        assert!(s.c.log.is_empty());
    }

    /// Window, tutorial, help icon and end fade on a spectator and on the player.
    #[test]
    fn window_tutorial_help_fade() {
        let Some((code, n, p)) = patched() else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let f = |k: usize| code.functions[n + k].findex;
        let (tut, win, help, fade) = (f(3), f(4), f(5), f(6));
        let mut s = sim(&code, n, &p);
        let w = world(&mut s, &p);
        let root = s.c.obj(&[]);
        let window = s.c.obj(&[(p.w_root, root.clone())]);
        let ui = s.c.get(&w.game, p.g_ui);
        let ctrl = s.c.obj(&[]);

        // spectator: hidden, not the current window; no tutorial, no help icon
        s.run(win, vec![w.mode.clone(), window.clone(), w.act.clone()]);
        assert_eq!(
            s.c.take("visible"),
            vec![
                vec![root.clone(), V::B(false)],
                vec![window.clone(), V::B(false)]
            ]
        );
        assert!(s.c.take("setWindow").is_empty());
        // the tutorial callback (spectHelp bound to the activity) runs at once
        let cb = V::Clo(help, Box::new(w.act.clone()));
        let args = |cb: &V| {
            vec![
                ui.clone(),
                V::S("Fish".into()),
                V::Null,
                cb.clone(),
                w.act.clone(),
            ]
        };
        assert_eq!(s.run(tut, args(&cb)), V::B(false));
        assert!(s.c.take("showTutorial").is_empty());
        assert!(s.c.take("addHelpIcon").is_empty());
        s.run(help, vec![w.act.clone()]);
        assert!(s.c.take("addHelpIcon").is_empty());
        // end fade with coop = true: not replicated (the others watch from their camera)
        s.run(fade, vec![ctrl.clone(), V::B(true), V::Null, w.act.clone()]);
        assert_eq!(
            s.c.take("netFade"),
            vec![vec![ctrl.clone(), V::B(false), V::Null]]
        );
        assert!(s.c.log.is_empty());

        // the player: vanilla calls
        s.c.set(&w.unit, p.u_owner, w.me.clone());
        s.run(win, vec![w.mode.clone(), window.clone(), w.act.clone()]);
        assert_eq!(
            s.c.take("setWindow"),
            vec![vec![w.mode.clone(), window.clone()]]
        );
        assert_eq!(s.run(tut, args(&cb)), V::B(true));
        assert_eq!(s.c.take("showTutorial").len(), 1);
        s.run(help, vec![w.act.clone()]);
        assert_eq!(s.c.take("addHelpIcon"), vec![vec![w.act.clone()]]);
        // a unit-less activity keeps the shared fade; coop = false stays false
        s.c.set(&w.act, p.a_unit, V::Null);
        s.run(fade, vec![ctrl.clone(), V::B(true), V::Null, w.act.clone()]);
        s.run(
            fade,
            vec![ctrl.clone(), V::B(false), V::Null, w.act.clone()],
        );
        let fades: Vec<V> =
            s.c.take("netFade")
                .into_iter()
                .map(|a| a[1].clone())
                .collect();
        assert_eq!(fades, [V::B(true), V::B(false)]);
        assert!(s.c.log.is_empty());
    }
}
