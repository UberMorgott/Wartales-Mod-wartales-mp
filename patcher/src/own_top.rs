// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a window another player's action opens never covers this player's
// own character sheet (owner decision 2026-11-04). It opens underneath and is
// answered once the sheet is closed.
//
// Vanilla, every modal window ends in game.globalUI (Window.setModal ->
// setGlobal: `globalUI.windows.push(w)`, `globalUI.s2d.add(windowRoot, 4)`, the
// top of layer 4), so a shared ChooseUnit (the host's ruins barricade choice
// "Thief") was drawn over a guest's open UnitInfo, its full-screen click
// catcher sat over the sheet and a client's Escape stopped at it (a shared
// window: Escape is the host's). Dialogs stay in mode.windows, a scene drawn
// under globalUI, so they never covered the sheet.
//
// The fix, `otPlace(this, alive, shared)` at the end of Window.alive (alive:
// a window replicated here from another machine, after its setup placed it)
// and of Window.netSharing(b) (shared = b: on the host, a window every player
// sees; the host's sheet is modal, so its own player did not just open one):
//   if (isMulti && this.ui == globalUI && modal != None
//       && (alive || shared && isAuth)
//       && (sheet = globalUI.getWindow(UnitInfo)) != null && sheet != this
//       && indexOf(this) > (k = indexOf(sheet))) {
//     windows.remove(this); windows.insert(k, this);       // Escape order
//     for (w in windows[k + 1 ..]) if (w.windowRoot.parent == s2d) s2d.over(w.windowRoot);
//   }
// i.e. the new window moves to just under the sheet, in the list and in the
// draw order; the sheet and whatever is above it (its own confirms) keep their
// order. The sheet's own click catcher now covers the shared window (no click
// reaches it, no click-outside cancel); Escape reaches the sheet first (same
// modal group, later in the list). Nothing is hidden, deferred or closed.
// (`__host` is not set yet when a replicated window runs alive, and a client's
// barricade choice is built on the host by the dialog script after the
// client's RPC, outside its context: hence these two sites.)
// shortcut: a client counts every replicated window as another player's, the
// host every shared one built while its sheet is open (an own sub-window of
// the sheet is local, never shared).
// Inventory / chest panels live inside the GameUI window (the bottom of
// globalUI): not covered here. Gamepad layer 6 windows keep their layer.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::job_xp::str_global;
use super::*;
use hlbc::types::RefGlobal;

const LOG: &str = "mp: own sheet stays over a shared window";

struct Plan {
    /// (fn index, op index of the Ret to hook): Window.alive's Ret right after
    /// its setup call, Window.netSharing's only Ret.
    sites: [(usize, usize); 2],
    win_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    i32_t: RefType,
    dyn_t: RefType,
    str_t: RefType,
    game: (RefGlobal, RefType),
    g_inst: (RefField, RefType),
    g_gui: (RefField, RefType),
    g_auth: RefField,
    is_multi: RefFun,
    w_ui: (RefField, RefType),
    w_modal: (RefField, RefType),
    modal_none: i32,
    w_root: (RefField, RefType),
    o_parent: (RefField, RefType),
    u_windows: (RefField, RefType),
    u_s2d: (RefField, RefType),
    a_len: RefField,
    a_raw: (RefField, RefType),
    sheet: (RefGlobal, RefType),
    get_window: RefFun,
    /// getWindow's filter argument type (passed null).
    gw_filter: RefType,
    index_of: RefFun,
    remove: RefFun,
    insert: RefFun,
    over: RefFun,
    println: RefFun,
    dbg_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let win_t = obj_type(code, "ui.Window")?;
    let game_t = obj_type(code, "Game")?;
    let base_t = obj_type(code, "ui.BaseUI")?;
    let gui_t = obj_type(code, "ui.GlobalUI")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let layers_t = obj_type(code, "h2d.Layers")?;
    let sheet_t = obj_type(code, "ui.win.UnitInfo")?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    if !is_sub(code, gui_t, base_t) || !is_sub(code, sheet_t, win_t) {
        bail!("GlobalUI / UnitInfo have unexpected parents");
    }
    let game = class_global(code, "Game")?;
    let g_inst = (typed(code, game.1, "inst", game_t)?, game_t);
    let g_gui = field(code, game_t, "globalUI")?;
    if g_gui.1 != gui_t {
        bail!("Game.globalUI is not a GlobalUI");
    }
    let g_auth = typed(code, game_t, "isAuth", bool_t)?;
    let is_multi = method(code, game_t, "get_isMulti")?.findex;
    if sig(code, is_multi)? != (vec![game_t], bool_t) {
        bail!("unexpected Game.get_isMulti signature");
    }
    let w_ui = field(code, win_t, "ui")?;
    let w_modal = field(code, win_t, "modal")?;
    let w_root = field(code, win_t, "windowRoot")?;
    let o_parent = field(code, obj_t, "parent")?;
    if w_ui.1 != base_t || !is_sub(code, w_root.1, obj_t) || o_parent.1 != obj_t {
        bail!("Window.ui / windowRoot / Object.parent have unexpected types");
    }
    let modal_none = match &code.types[w_modal.1 .0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|c| s(code, c.name) == "None" && c.params.is_empty()),
        _ => None,
    }
    .context("Window.modal is not an enum with a None construct")?;
    let u_windows = field(code, base_t, "windows")?;
    let u_s2d = field(code, base_t, "s2d")?;
    if u_windows.1 != arr_t || !is_sub(code, u_s2d.1, layers_t) {
        bail!("BaseUI.windows / s2d have unexpected types");
    }
    let a_len = typed(code, arr_t, "length", i32_t)?;
    let a_raw = field(code, arr_t, "array")?;
    let sheet = class_global(code, "ui.win.UnitInfo")?;
    let gw = method(code, base_t, "getWindow")?;
    let (gw_args, gw_ret) = sig(code, gw.findex)?;
    if gw_args.len() != 3 || gw_ret != win_t || !is_sub(code, sheet.1, gw_args[1]) {
        bail!("unexpected BaseUI.getWindow signature");
    }
    let m = |name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, arr_t, name)?.findex;
        let (a, r) = sig(code, f)?;
        if a.len() != args.len() + 1 || a[1..] != *args || r != ret {
            bail!("unexpected ArrayObj.{name} signature");
        }
        Ok(f)
    };
    let index_of = method(code, arr_t, "indexOf")?.findex;
    match sig(code, index_of)? {
        (a, r) if a.len() == 3 && a[1] == dyn_t && r == i32_t => {}
        s => bail!("unexpected ArrayObj.indexOf signature {s:?}"),
    }
    let remove = m("remove", &[dyn_t], bool_t)?;
    let insert = m("insert", &[i32_t, dyn_t], void_t)?;
    let over = method(code, layers_t, "over")?.findex;
    if sig(code, over)? != (vec![layers_t, obj_t], void_t) {
        bail!("unexpected h2d.Layers.over signature");
    }
    let println = super::diag::static_fn(code, "$Sys", "println")?.findex;
    let alive = method(code, win_t, "alive")?;
    let setup_slot = super::force_leave::slot(code, win_t, "setup")?;
    let ends: Vec<usize> = (1..alive.ops.len())
        .filter(|&i| {
            matches!(alive.ops[i], Opcode::Ret { .. })
                && matches!(&alive.ops[i - 1], Opcode::CallThis { field, args, .. } if *field == setup_slot && args.is_empty())
        })
        .collect();
    let [alive_at] = ends[..] else {
        bail!(
            "Window.alive: {} `setup(); return` ends (want 1)",
            ends.len()
        );
    };
    let share = method(code, win_t, "netSharing")?;
    if fun_args(code, share) != [win_t, bool_t] {
        bail!("unexpected Window.netSharing signature");
    }
    let rets: Vec<usize> = (0..share.ops.len())
        .filter(|&i| matches!(share.ops[i], Opcode::Ret { .. }))
        .collect();
    let [share_at] = rets[..] else {
        bail!("Window.netSharing: {} Rets (want 1)", rets.len());
    };
    if share
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call3 { arg0: Reg(0), .. }))
    {
        bail!("Window.netSharing: already hooked");
    }
    Ok(Plan {
        sites: [
            (fun_index(code, alive.findex)?, alive_at),
            (fun_index(code, share.findex)?, share_at),
        ],
        win_t,
        void_t,
        bool_t,
        i32_t,
        dyn_t,
        str_t,
        game,
        g_inst,
        g_gui,
        g_auth,
        is_multi,
        w_ui,
        w_modal,
        modal_none: i32::try_from(modal_none)?,
        w_root,
        o_parent,
        u_windows,
        u_s2d,
        a_len,
        a_raw,
        sheet,
        get_window: gw.findex,
        gw_filter: gw_args[2],
        index_of,
        remove,
        insert,
        over,
        println,
        dbg_file: debug_file(code, "src/ui/Window.hx")?,
    })
}

/// `otPlace(win, alive, shared)`: see the header.
fn add_fn(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let log = str_global(code, p.str_t, LOG);
    let none_k = int_const(code, p.modal_none);
    let k1 = int_const(code, 1);
    let null_i32 = code_null_i32(code, p)?;
    let mut r = Regs(vec![p.win_t, p.bool_t, p.bool_t]);
    let (gc, g, b, gui, u, m, mi, mk) = (
        r.r(p.game.1),
        r.r(p.g_inst.1),
        r.r(p.bool_t),
        r.r(p.g_gui.1),
        r.r(p.w_ui.1),
        r.r(p.w_modal.1),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let flt = r.r(p.gw_filter);
    let (uc, sh, lst, nul) = (
        r.r(p.sheet.1),
        r.r(p.win_t),
        r.r(p.u_windows.1),
        r.r(null_i32),
    );
    let (k, j, n, raw, d, w, root, par, s2d, s, v) = (
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.win_t),
        r.r(p.w_root.1),
        r.r(p.o_parent.1),
        r.r(p.u_s2d.1),
        r.r(p.str_t),
        r.r(p.void_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: gc,
        global: p.game.0,
    });
    a.op(Opcode::Field {
        dst: g,
        obj: gc,
        field: p.g_inst.0,
    });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, "ret");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: g,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.op(Opcode::Field {
        dst: gui,
        obj: g,
        field: p.g_gui.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: gui,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: u,
        obj: Reg(0),
        field: p.w_ui.0,
    });
    a.jmp(
        Opcode::JNotEq {
            a: u,
            b: gui,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: m,
        obj: Reg(0),
        field: p.w_modal.0,
    });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "ret");
    a.op(Opcode::EnumIndex { dst: mi, value: m });
    a.op(Opcode::Int {
        dst: mk,
        ptr: none_k,
    });
    a.jmp(
        Opcode::JEq {
            a: mi,
            b: mk,
            offset: 0,
        },
        "ret",
    );
    // another player's window: replicated here, or shared by the host
    a.jmp(
        Opcode::JTrue {
            cond: Reg(1),
            offset: 0,
        },
        "other",
    );
    a.jmp(
        Opcode::JFalse {
            cond: Reg(2),
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: g,
        field: p.g_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "ret");
    a.label("other");
    a.op(Opcode::GetGlobal {
        dst: uc,
        global: p.sheet.0,
    });
    a.op(Opcode::Null { dst: flt });
    a.op(Opcode::Call3 {
        dst: sh,
        fun: p.get_window,
        arg0: gui,
        arg1: uc,
        arg2: flt,
    });
    a.jmp(Opcode::JNull { reg: sh, offset: 0 }, "ret");
    a.jmp(
        Opcode::JEq {
            a: sh,
            b: Reg(0),
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Field {
        dst: lst,
        obj: gui,
        field: p.u_windows.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: lst,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Null { dst: nul });
    a.op(Opcode::Call3 {
        dst: k,
        fun: p.index_of,
        arg0: lst,
        arg1: sh,
        arg2: nul,
    });
    a.op(Opcode::Call3 {
        dst: j,
        fun: p.index_of,
        arg0: lst,
        arg1: Reg(0),
        arg2: nul,
    });
    // only a window above the sheet moves (k >= 0 holds: getWindow found it there)
    a.jmp(
        Opcode::JSLte {
            a: j,
            b: k,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.remove,
        arg0: lst,
        arg1: Reg(0),
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.insert,
        arg0: lst,
        arg1: k,
        arg2: Reg(0),
    });
    // redraw order: the sheet and everything above it go back over this window
    a.op(Opcode::Field {
        dst: s2d,
        obj: gui,
        field: p.u_s2d.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: s2d,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Int { dst: mk, ptr: k1 });
    a.op(Opcode::Add {
        dst: k,
        a: k,
        b: mk,
    });
    a.loop_head("loop");
    a.op(Opcode::Field {
        dst: n,
        obj: lst,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: n,
            offset: 0,
        },
        "done",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: lst,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: k,
    });
    a.op(Opcode::SafeCast { dst: w, src: d });
    a.op(Opcode::Incr { dst: k });
    a.jmp(Opcode::JNull { reg: w, offset: 0 }, "loop");
    a.op(Opcode::Field {
        dst: root,
        obj: w,
        field: p.w_root.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Field {
        dst: par,
        obj: root,
        field: p.o_parent.0,
    });
    a.jmp(
        Opcode::JNotEq {
            a: par,
            b: s2d,
            offset: 0,
        },
        "loop",
    );
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.over,
        arg0: s2d,
        arg1: root,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("done");
    a.op(Opcode::GetGlobal {
        dst: s,
        global: log,
    });
    // a String goes to println's Dyn argument as is
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: s,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.win_t, p.bool_t, p.bool_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// indexOf's optional `fromIndex` argument type (Null<Int>).
fn code_null_i32(code: &Bytecode, p: &Plan) -> Result<RefType> {
    let (a, _) = sig(code, p.index_of)?;
    if !matches!(code.types[a[2].0], Type::Null(t) if t == p.i32_t) {
        bail!("ArrayObj.indexOf fromIndex is not a Null<Int>");
    }
    Ok(a[2])
}

/// Keeps this player's own sheet over another player's shared window, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_own_top(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => return crate::skipped(format!("own top skipped: {e:#}")),
    };
    let snap = super::asm::Snap::take(code);
    let f = match add_fn(code, &p) {
        Ok(f) => f,
        Err(e) => {
            snap.restore(code);
            return crate::skipped(format!("own top skipped: {e:#}"));
        }
    };
    for (k, (fi, at)) in p.sites.into_iter().enumerate() {
        let s = &mut code.functions[fi];
        let (b0, b1, v) = (
            Reg(s.regs.len() as u32),
            Reg(s.regs.len() as u32 + 1),
            Reg(s.regs.len() as u32 + 2),
        );
        s.regs.extend([p.bool_t, p.bool_t, p.void_t]);
        // k 0: Window.alive -> (true, false); 1: Window.netSharing(b) -> (false, b)
        let mut ops = vec![
            Opcode::Bool {
                dst: b0,
                value: hlbc::types::ValBool(k == 0),
            },
            Opcode::Bool {
                dst: b1,
                value: hlbc::types::ValBool(false),
            },
        ];
        if k == 1 {
            ops.push(Opcode::Mov {
                dst: b1,
                src: Reg(1),
            });
        }
        ops.push(Opcode::Call3 {
            dst: v,
            fun: f,
            arg0: Reg(0),
            arg1: b0,
            arg2: b1,
        });
        insert_ops(s, at, ops);
        eprintln!(
            "patched own top fn@{} op {at}: another player's window opens under this player's sheet (fn@{})",
            s.findex.0, f.0
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, shifted, write};
    use crate::testsim::{Core, Sim, V};

    /// Window.alive / netSharing get one call before their Ret; the new function type-checks;
    /// a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_own_top(&mut code);
        let once = write(&code);
        let back = read(&once);
        let n = orig.functions.len();
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, p.sites.iter().all(|s| s.0 != i), "function #{i}");
        }
        for (k, (fi, at)) in p.sites.into_iter().enumerate() {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            // alive: Bool, Bool, Call3; netSharing: Bool, Bool, Mov b, Call3
            let len = 3 + k;
            shifted(a, b, at, len);
            assert!(matches!(b.ops[at], Opcode::Bool { value, .. } if value.0 == (k == 0)));
            assert!(k == 0 || matches!(b.ops[at + 2], Opcode::Mov { src: Reg(1), .. }));
            let Opcode::Call3 {
                fun, arg0: Reg(0), ..
            } = b.ops[at + len - 1]
            else {
                panic!("no hook call")
            };
            assert_eq!(fun_index(&back, fun).unwrap(), n);
            check_types(&back, b, at..at + len);
            assert!(matches!(b.ops[at + len], Opcode::Ret { .. }));
        }
        let f = &back.functions[n];
        check_types(&back, f, 0..f.ops.len());
        check_flow(f);
        let mut again = read(&once);
        assert!(plan(&again).is_err());
        patch_own_top(&mut again);
        assert!(write(&again) == once);
    }

    /// Behaviour: list [other, sheet, own confirm] + a new modal window on top
    /// -> [other, new, sheet, confirm], sheet and confirm re-raised in order,
    /// for a replicated window (alive) and a window the host shares; a window
    /// a client shares, or one not shared, stays on top.
    #[test]
    fn moves_foreign_window_under_sheet() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_own_top(&mut code);
        let n = orig.functions.len();
        let f = code.functions[n].findex;
        let (gw, io, rm, ins, over, multi, pl) = (
            p.get_window,
            p.index_of,
            p.remove,
            p.insert,
            p.over,
            p.is_multi,
            p.println,
        );
        let (a_len, a_raw) = (p.a_len, p.a_raw.0);
        let items = move |c: &mut Core, l: &V| -> Vec<V> {
            let raw = c.get(l, a_raw);
            let V::I(n) = c.get(l, a_len) else { panic!() };
            (0..n).map(|k| c.key_get(&raw, &format!("i{k}"))).collect()
        };
        let set_items = move |c: &mut Core, l: &V, v: Vec<V>| {
            let raw = c.get(l, a_raw);
            c.set(l, a_len, V::I(v.len() as i32));
            for (k, x) in v.into_iter().enumerate() {
                c.key_set(&raw, format!("i{k}"), x);
            }
        };
        let run = |alive: bool, shared: bool, auth: bool| -> (bool, Vec<Vec<V>>, Vec<V>) {
            let mut s = Sim::new(
                &code,
                n,
                move |c: &mut Core, fun: RefFun, a: &[V]| {
                    if fun == multi {
                        Some(V::B(true))
                    } else if fun == gw {
                        Some(c.map("t", "sheet"))
                    } else if fun == io {
                        let pos = items(c, &a[0]).iter().position(|x| *x == a[1]);
                        Some(V::I(pos.map_or(-1, |x| x as i32)))
                    } else if fun == rm {
                        let mut v = items(c, &a[0]);
                        v.retain(|x| *x != a[1]);
                        set_items(c, &a[0], v);
                        Some(V::B(true))
                    } else if fun == ins {
                        let mut v = items(c, &a[0]);
                        let V::I(k) = a[1] else { panic!() };
                        v.insert(k as usize, a[2].clone());
                        set_items(c, &a[0], v);
                        Some(V::Null)
                    } else if fun == over {
                        c.log.push(("over", a.to_vec()));
                        Some(V::Null)
                    } else if fun == pl {
                        Some(V::Null)
                    } else {
                        None
                    }
                },
                |_: &mut Core, _: usize, _: &[V]| panic!("no virtual call"),
            );
            let s2d = s.c.obj(&[]);
            let gui_raw = s.c.obj(&[]);
            let lst = s.c.obj(&[(a_len, V::I(0)), (a_raw, gui_raw)]);
            let gui =
                s.c.obj(&[(p.u_windows.0, lst.clone()), (p.u_s2d.0, s2d.clone())]);
            let game = s.c.obj(&[(p.g_gui.0, gui.clone()), (p.g_auth, V::B(auth))]);
            let gcls = s.c.obj(&[(p.g_inst.0, game)]);
            s.c.globals.insert(p.game.0 .0, gcls);
            let modal = s.c.enm(p.modal_none + 1, vec![]);
            let win = |s: &mut Sim| {
                let root = s.c.obj(&[(p.o_parent.0, s2d.clone())]);
                s.c.obj(&[
                    (p.w_ui.0, gui.clone()),
                    (p.w_modal.0, modal.clone()),
                    (p.w_root.0, root),
                ])
            };
            let (other, sheet, confirm, new) = (win(&mut s), win(&mut s), win(&mut s), win(&mut s));
            set_items(
                &mut s.c,
                &lst,
                vec![other.clone(), sheet.clone(), confirm.clone(), new.clone()],
            );
            s.c.put("t", "sheet", sheet.clone());
            s.run(f, vec![new.clone(), V::B(alive), V::B(shared)]);
            let roots = |s: &Sim, w: &V| s.c.get(w, p.w_root.0);
            let want = vec![roots(&s, &sheet), roots(&s, &confirm)];
            let moved = vec![other.clone(), new.clone(), sheet.clone(), confirm.clone()];
            let l = items(&mut s.c, &lst);
            (l == moved, s.c.take("over"), want)
        };
        let roots = |ov: Vec<Vec<V>>| ov.into_iter().map(|a| a[1].clone()).collect::<Vec<_>>();
        for (alive, shared, auth, moves) in [
            (true, false, false, true),  // client: replicated
            (true, false, true, true),   // host: a client's shared window
            (false, true, true, true),   // host shares it
            (false, true, false, false), // a client shares its own window
            (false, false, true, false), // netSharing(false)
        ] {
            let (moved, ov, want) = run(alive, shared, auth);
            assert_eq!(moved, moves, "{alive} {shared} {auth}");
            assert_eq!(roots(ov), if moves { want } else { vec![] });
        }
    }
}
