// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op pings: in battle the pinged grid cell blinks orange on every player's
// screen, and the ping lands where the pinging player's cursor really is.
//
// Vanilla (Game.hx:1947, st/Controller.hx:1284):
//
//   Game.ping():                              // ping key / ping button
//       p = renderer.textures.depth.capturePixels();
//       nx = mode.s2d.mouseX / mode.s2d.width; ny = mode.s2d.mouseY / mode.s2d.height;
//       d = p.getPixelF(Std.int(this.s2d.mouseX), Std.int(this.s2d.mouseY)).x;
//       w = s3d.camera.unproject(2 * nx - 1, -(2 * ny - 1), d);
//       ctrl.ping(w.x, w.y, w.z, me);         // RPC 92: ping__impl on every peer
//   Controller.ping__impl(x, y, z, player):
//       fx = loadPrefab("prefabs/fx/ui/ping/ping.fx"), at (x, y, z), scaled by the
//       LOCAL camera distance, billboard quads (lookAt) whose "front" pass is
//       depthTest Less and "behind" pass depthTest GreaterEqual, tinted per player;
//       removed after fx.duration (1.75 s); sfx "Ping".
//
// Why everyone sees it differently: the marker is a camera-facing 3D billboard at
// a world point. That point is the pinging player's depth-buffer sample along the
// cursor ray, read at `this.s2d` mouse coordinates, i.e. UI scene units, while
// the depth texture is in render pixels. With UI scaling, render scale or a DPI
// factor the sample comes from another pixel, so the depth (and the world point)
// is wrong. The pinging player never notices (any point on the cursor ray projects
// under the cursor); the others see the marker floating, or sunk into terrain,
// props or units, somewhere else along that ray, half drawn in the "behind" tint
// or hidden by geometry, depending on their own camera.
//
// The patch:
//   1. Game.ping samples the depth texture at the same normalized cursor position
//      the unproject uses, in the texture's own pixels:
//          d = p.getPixelF(Std.int(p.width * nx), Std.int(p.height * ny)).x
//      (7 ops replaced, one NullCheck inserted; nothing else in the fn moves).
//   2. Controller.ping__impl ends with `pingCell(this, x, y)` (new function):
//          b = game.battle; if (b == null || b.grid == null) return;
//          cs = b.grid.cellSize; i = floor(x / cs); j = floor(y / cs);
//          if (i, j) outside the grid: return;
//          try {
//              o = b.addSocle(load("prefabs/huds/orangeSquare.prefab"),
//                             (i + .5) * cs, (j + .5) * cs, 0.01, cs, 0, 0.85);
//              game.globalEvent.wait(3, o.remove);
//              game.globalEvent.waitUntil(pingCellBlink.bind(o));
//          } catch (_) {}
//      pingCellBlink(o, dt): if (o.parent == null) return true;
//          o.visible = floor((hxd.Timer.lastTimeStamp * 6) % 2) == 0; return false;
//      orangeSquare.prefab is the game's own (unused) one-cell overlay: an Overlay
//      pass with depthTest Always, so the square shows over terrain, props and
//      units from any camera. addSocle is how the game places its other cell
//      squares (redSquare on a move path, the pad cursor), as its own object:
//      the player's move / attack previews are untouched.
//
// Purely visual and local to each peer: it rides the existing ping RPC (no new
// message, no game state). Outside a battle (world map, places, camp) only the
// depth fix applies; the vanilla marker stays everywhere.
//
// Each half is validated before editing; a mismatch skips that half (logged).

use super::asm::{push_fn, Asm, Regs};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

const SQUARE: &str = "prefabs/huds/orangeSquare.prefab";
const DURATION: f64 = 3.0;
/// Visibility toggles per second (3 blinks a second).
const BLINK_RATE: f64 = 6.0;
const ALPHA: f64 = 0.85;

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let t = match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => n.t,
        None => code.functions[fun_index(code, f)?].t,
    };
    let t = t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

fn fname(code: &Bytecode, f: RefFun) -> &str {
    code.functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| s(code, g.name))
        .unwrap_or("")
}

/// The class global of `name` (HL stores it 1-based) and its type `pkg.$Cls`.
fn class_global(code: &Bytecode, name: &str) -> Result<(RefGlobal, RefType)> {
    let o = obj(code, obj_type(code, name)?)?;
    let g = RefGlobal(
        o.global
            .0
            .checked_sub(1)
            .with_context(|| format!("{name}: no class global"))?,
    );
    let t = *code
        .globals
        .get(g.0)
        .with_context(|| format!("{name}: class global out of range"))?;
    let (pkg, cls) = name.rsplit_once('.').unwrap_or(("", name));
    let want = format!("{pkg}.${cls}");
    if obj(code, t).ok().map(|o| s(code, o.name)) != Some(want.as_str()) {
        bail!("{name}: class global is not {want}");
    }
    Ok((g, t))
}

// ---------- 1. depth sample in texture pixels (Game.ping) ----------

struct DepthPlan {
    fi: usize,
    /// First of the 7 ops `GetThis s2d; NullCheck; Call1 mouseX; ToInt; GetThis s2d; NullCheck; Call1 mouseY`.
    at: usize,
    pix: Reg,
    ix: Reg,
    iy: Reg,
    dx: Reg,
    dy: Reg,
    nx: Reg,
    ny: Reg,
    p_width: RefField,
    p_height: RefField,
}

fn depth_plan(code: &Bytecode) -> Result<DepthPlan> {
    let game_t = obj_type(code, "Game")?;
    let pix_t = obj_type(code, "hxd.Pixels")?;
    let tex_t = obj_type(code, "h3d.mat.Texture")?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let f64_ = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let (p_width, wt) = field(code, pix_t, "width")?;
    let (p_height, ht) = field(code, pix_t, "height")?;
    if wt != i32_ || ht != i32_ {
        bail!("hxd.Pixels width/height are not i32");
    }
    let (s2d_f, _) = field(code, game_t, "s2d")?;
    let capture = proto(code, tex_t, "capturePixels")?;
    let get_px = proto(code, pix_t, "getPixelF")?;

    let ping = method(code, game_t, "ping")?;
    let fi = fun_index(code, ping.findex)?;
    let f = &code.functions[fi];
    let rt = |r: Reg| f.regs[r.0 as usize];
    let o = &f.ops;

    let caps: Vec<Reg> = o
        .iter()
        .filter_map(|x| match x {
            Opcode::Call4 { dst, fun, .. } if *fun == capture => Some(*dst),
            _ => None,
        })
        .collect();
    let [pix] = caps[..] else {
        bail!("Game.ping: {} capturePixels calls, want 1", caps.len());
    };
    let gets: Vec<(Reg, Reg, Reg)> = o
        .iter()
        .filter_map(|x| match x {
            Opcode::Call4 {
                fun,
                arg0,
                arg1,
                arg2,
                ..
            } if *fun == get_px && *arg0 == pix => Some((*arg0, *arg1, *arg2)),
            _ => None,
        })
        .collect();
    let [(_, ix, iy)] = gets[..] else {
        bail!("Game.ping: {} getPixelF(depth) calls, want 1", gets.len());
    };
    if rt(pix) != pix_t || rt(ix) != i32_ || rt(iy) != i32_ {
        bail!("Game.ping: depth sample registers have unexpected types");
    }

    let mouse = |i: usize, want: &str| -> Option<(Reg, Reg)> {
        match (&o[i], &o[i + 1], &o[i + 2]) {
            (
                Opcode::GetThis { dst: a, field },
                Opcode::NullCheck { reg: b },
                Opcode::Call1 { dst, fun, arg0 },
            ) if *field == s2d_f && a == b && arg0 == a && fname(code, *fun) == want => {
                Some((*a, *dst))
            }
            _ => None,
        }
    };
    let sites: Vec<(usize, Reg, Reg)> = (0..o.len().saturating_sub(8))
        .filter_map(|i| {
            let (_, dx) = mouse(i, "get_mouseX")?;
            let Opcode::ToInt { dst, src } = o[i + 3] else {
                return None;
            };
            if dst != ix || src != dx {
                return None;
            }
            let (_, dy) = mouse(i + 4, "get_mouseY")?;
            Some((i, dx, dy))
        })
        .collect();
    let [(at, dx, dy)] = sites[..] else {
        bail!(
            "Game.ping: {} `this.s2d` mouse depth samples, want 1",
            sites.len()
        );
    };
    if rt(dx) != f64_ || rt(dy) != f64_ {
        bail!("Game.ping: mouse registers are not f64");
    }
    // `ToInt iy = dy` later feeds getPixelF.
    if !o[at + 7..]
        .iter()
        .any(|x| matches!(x, Opcode::ToInt { dst, src } if *dst == iy && *src == dy))
    {
        bail!("Game.ping: no ToInt of the mouse Y into the sample");
    }
    // The normalized cursor: the first two Movs after the samples copy nx, ny
    // into the unproject vector, both computed by an SDiv before the samples.
    let movs: Vec<Reg> = o[at + 7..]
        .iter()
        .filter_map(|x| match x {
            Opcode::Mov { src, .. } => Some(*src),
            _ => None,
        })
        .take(2)
        .collect();
    let [nx, ny] = movs[..] else {
        bail!("Game.ping: no normalized cursor copies");
    };
    let divided = |r: Reg| {
        o[..at]
            .iter()
            .any(|x| matches!(x, Opcode::SDiv { dst, a, .. } if *dst == r && *a == r))
    };
    if rt(nx) != f64_ || rt(ny) != f64_ || !divided(nx) || !divided(ny) {
        bail!("Game.ping: normalized cursor registers have unexpected shape");
    }
    for r in [nx, ny, pix] {
        if o[at..at + 7].iter().any(|x| {
            matches!(x, Opcode::GetThis { dst, .. } | Opcode::Call1 { dst, .. } | Opcode::ToInt { dst, .. } if *dst == r)
        }) {
            bail!("Game.ping: the samples overwrite a register they need");
        }
    }
    // No jump lands inside the replaced ops.
    for i in 0..o.len() {
        if jump_targets(f, i).iter().any(|&t| t > at && t < at + 7) {
            bail!("Game.ping: a jump lands inside the mouse samples");
        }
    }
    Ok(DepthPlan {
        fi,
        at,
        pix,
        ix,
        iy,
        dx,
        dy,
        nx,
        ny,
        p_width,
        p_height,
    })
}

fn depth_apply(code: &mut Bytecode, p: &DepthPlan) {
    let f = &mut code.functions[p.fi];
    let new = [
        Opcode::Field {
            dst: p.ix,
            obj: p.pix,
            field: p.p_width,
        },
        Opcode::ToSFloat {
            dst: p.dx,
            src: p.ix,
        },
        Opcode::Mul {
            dst: p.dx,
            a: p.dx,
            b: p.nx,
        },
        Opcode::ToInt {
            dst: p.ix,
            src: p.dx,
        },
        Opcode::Field {
            dst: p.iy,
            obj: p.pix,
            field: p.p_height,
        },
        Opcode::ToSFloat {
            dst: p.dy,
            src: p.iy,
        },
        Opcode::Mul {
            dst: p.dy,
            a: p.dy,
            b: p.ny,
        },
    ];
    for (k, op) in new.into_iter().enumerate() {
        f.ops[p.at + k] = op;
    }
    insert_ops(f, p.at, vec![Opcode::NullCheck { reg: p.pix }]);
    eprintln!(
        "patched ping fn@{} ops {}..{}: depth sampled at the normalized cursor in texture pixels",
        f.findex.0,
        p.at,
        p.at + 8
    );
}

// ---------- 2. blinking orange cell (Controller.ping__impl) ----------

struct CellPlan {
    impl_fi: usize,
    /// The final `Ret` of ping__impl.
    ret_at: usize,
    ret_reg: Reg,
    ctrl_t: RefType,
    f64_: RefType,
    i32_: RefType,
    bool_: RefType,
    void_: RefType,
    str_t: RefType,
    c_game: (RefField, RefType),
    g_battle: (RefField, RefType),
    g_event: (RefField, RefType),
    b_grid: (RefField, RefType),
    gr_cell: RefField,
    gr_w: RefField,
    gr_h: RefField,
    floor: RefFun,
    get_loader: RefFun,
    loader_t: RefType,
    load_cache: RefFun,
    hres_t: RefType,
    res_cls: RefGlobal,
    res_cls_t: RefType,
    res_t: RefType,
    add_socle: RefFun,
    obj_t: RefType,
    o_parent: (RefField, RefType),
    remove: RefFun,
    set_visible: RefFun,
    wait: RefFun,
    wait_cb_t: RefType,
    wait_until: RefFun,
    until_cb_t: RefType,
    timer: RefGlobal,
    timer_t: RefType,
    t_stamp: RefField,
    dyn_t: RefType,
    dbg_file: usize,
}

fn cell_plan(code: &Bytecode) -> Result<CellPlan> {
    let f64_ = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let game_t = obj_type(code, "Game")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let grid_t = obj_type(code, "battle.Grid")?;
    let obj_t = obj_type(code, "h3d.scene.Object")?;
    let ev_t = obj_type(code, "hxd.WaitEvent")?;
    let res_t = obj_type(code, "hrt.prefab.Resource")?;

    let c_game = field(code, ctrl_t, "game")?;
    let g_battle = field(code, game_t, "battle")?;
    let g_event = field(code, game_t, "globalEvent")?;
    let b_grid = field(code, battle_t, "grid")?;
    if c_game.1 != game_t || g_battle.1 != battle_t || g_event.1 != ev_t || b_grid.1 != grid_t {
        bail!("Controller.game / Game.battle / Game.globalEvent / Battle.grid types differ");
    }
    let (gr_cell, ct) = field(code, grid_t, "cellSize")?;
    let (gr_w, wt) = field(code, grid_t, "w")?;
    let (gr_h, ht) = field(code, grid_t, "h")?;
    if ct != f64_ || wt != i32_ || ht != i32_ {
        bail!("battle.Grid cellSize / w / h types differ");
    }
    let o_parent = field(code, obj_t, "parent")?;

    let want = |f: RefFun, what: &str, args: &[RefType], ret: RefType| -> Result<()> {
        if sig(code, f)? != (args.to_vec(), ret) {
            bail!("unexpected {what} signature");
        }
        Ok(())
    };
    let floor = {
        let hits: Vec<RefFun> = code
            .natives
            .iter()
            .filter(|n| s(code, n.name) == "math_floor" && s(code, n.lib) == "std")
            .map(|n| n.findex)
            .collect();
        let [f] = hits[..] else {
            bail!("{} std math_floor natives, want 1", hits.len());
        };
        want(f, "math_floor", &[f64_], i32_)?;
        f
    };
    let add_socle = proto(code, battle_t, "addSocle")?;
    want(
        add_socle,
        "Battle.addSocle",
        &[battle_t, res_t, f64_, f64_, f64_, f64_, f64_, f64_],
        obj_t,
    )?;
    let remove = proto(code, obj_t, "remove")?;
    want(remove, "Object.remove", &[obj_t], void_)?;
    let set_visible = proto(code, obj_t, "set_visible")?;
    want(set_visible, "Object.set_visible", &[obj_t, bool_], bool_)?;
    let wait = proto(code, ev_t, "wait")?;
    let (wa, wr) = sig(code, wait)?;
    let wait_until = proto(code, ev_t, "waitUntil")?;
    let (ua, ur) = sig(code, wait_until)?;
    if wa.len() != 3
        || wa[0] != ev_t
        || wa[1] != f64_
        || wr != void_
        || ua.len() != 2
        || ur != void_
    {
        bail!("unexpected WaitEvent.wait / waitUntil signature");
    }
    let (wait_cb_t, until_cb_t) = (wa[2], ua[1]);
    match (&code.types[wait_cb_t.0], &code.types[until_cb_t.0]) {
        (Type::Fun(a), Type::Fun(b))
            if a.args.is_empty() && a.ret == void_ && b.args == [f64_] && b.ret == bool_ => {}
        _ => bail!("WaitEvent callbacks are not () -> Void / Float -> Bool"),
    }
    let (timer, timer_t) = class_global(code, "hxd.Timer")?;
    let (t_stamp, st) = field(code, timer_t, "lastTimeStamp")?;
    if st != f64_ {
        bail!("hxd.Timer.lastTimeStamp is not f64");
    }

    // ping__impl(this, x, y, z, player): the fx load gives the loader, loadCache
    // and the hrt.prefab.Resource class; it ends with sfx("Ping") and Ret.
    let imp = method(code, ctrl_t, "ping__impl")?;
    let a = fun_args(code, imp);
    if a.len() != 5 || a[1] != f64_ || a[2] != f64_ || a[3] != f64_ {
        bail!("unexpected Controller.ping__impl signature");
    }
    let impl_fi = fun_index(code, imp.findex)?;
    let o = &imp.ops;
    let loads: Vec<usize> = (0..o.len().saturating_sub(5))
        .filter(|&i| {
            matches!(
                (&o[i], &o[i + 2], &o[i + 3], &o[i + 4], &o[i + 5]),
                (
                    Opcode::Call0 { dst: l, .. },
                    Opcode::GetGlobal { dst: p, global },
                    Opcode::GetGlobal { dst: c, .. },
                    Opcode::Call3 { arg0, arg1, arg2, dst, .. },
                    Opcode::SafeCast { src, dst: r },
                ) if arg0 == l && arg1 == p && arg2 == c && src == dst
                    && imp.regs[r.0 as usize] == res_t
                    && crate::job_xp::const_str(code, *global) == Some("prefabs/fx/ui/ping/ping.fx")
            )
        })
        .collect();
    let [li] = loads[..] else {
        bail!("ping__impl: {} ping.fx loads, want 1", loads.len());
    };
    let (
        Opcode::Call0 {
            fun: get_loader,
            dst: ld,
        },
        Opcode::GetGlobal {
            global: res_cls,
            dst: rc,
        },
        Opcode::Call3 {
            fun: load_cache,
            dst: hr,
            ..
        },
    ) = (&o[li], &o[li + 3], &o[li + 4])
    else {
        unreachable!()
    };
    let (loader_t, res_cls_t, hres_t) = (
        imp.regs[ld.0 as usize],
        imp.regs[rc.0 as usize],
        imp.regs[hr.0 as usize],
    );
    want(*get_loader, "get_loader", &[], loader_t)?;
    let (la, lr) = sig(code, *load_cache)?;
    if la.len() != 3 || la[0] != loader_t || la[1] != str_t || lr != hres_t {
        bail!("unexpected loadCache signature");
    }
    if fname(code, *load_cache) != "loadCache" {
        bail!("ping__impl: the fx load is not loadCache");
    }

    let ret_at = o.len() - 1;
    let Opcode::Ret { ret: ret_reg } = o[ret_at] else {
        bail!("ping__impl does not end in Ret");
    };
    if imp.regs[ret_reg.0 as usize] != void_ {
        bail!("ping__impl: Ret register is not void");
    }
    match &o[ret_at - 1] {
        Opcode::Call3 {
            arg0: Reg(0),
            arg1: Reg(1),
            arg2: Reg(2),
            ..
        } => bail!("ping__impl: already applied"),
        Opcode::Call3 { fun, .. } if fname(code, *fun) == "sfx" => {}
        _ => bail!("ping__impl: no sfx call before the final Ret"),
    }
    for i in 0..o.len() {
        if jump_targets(imp, i).contains(&ret_at) {
            bail!("ping__impl: a jump lands on the final Ret");
        }
    }
    Ok(CellPlan {
        impl_fi,
        ret_at,
        ret_reg,
        ctrl_t,
        f64_,
        i32_,
        bool_,
        void_,
        str_t,
        c_game,
        g_battle,
        g_event,
        b_grid,
        gr_cell,
        gr_w,
        gr_h,
        floor,
        get_loader: *get_loader,
        loader_t,
        load_cache: *load_cache,
        hres_t,
        res_cls: *res_cls,
        res_cls_t,
        res_t,
        add_socle,
        obj_t,
        o_parent,
        remove,
        set_visible,
        wait,
        wait_cb_t,
        wait_until,
        until_cb_t,
        timer,
        timer_t,
        t_stamp,
        dyn_t,
        dbg_file: debug_file(code, "src/st/Controller.hx")?,
    })
}

/// `pingCellBlink(o, dt) -> Bool` (see the header).
fn add_blink(code: &mut Bytecode, p: &CellPlan) -> Result<RefFun> {
    let rate = float_const(code, BLINK_RATE);
    let two = float_const(code, 2.0);
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.obj_t, p.f64_]);
    let o = Reg(0);
    let (par, b, tm, t, k, zero) = (
        r.r(p.o_parent.1),
        r.r(p.bool_),
        r.r(p.timer_t),
        r.r(p.f64_),
        r.r(p.i32_),
        r.r(p.i32_),
    );
    let fr = r.r(p.f64_);
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: par,
        obj: o,
        field: p.o_parent.0,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: par,
            offset: 0,
        },
        "live",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("live");
    a.op(Opcode::GetGlobal {
        dst: tm,
        global: p.timer,
    });
    a.op(Opcode::Field {
        dst: t,
        obj: tm,
        field: p.t_stamp,
    });
    a.op(Opcode::Float { dst: fr, ptr: rate });
    a.op(Opcode::Mul {
        dst: t,
        a: t,
        b: fr,
    });
    // The stamp is wall-clock seconds (~1.8e9): fmod by 2 before the i32 floor.
    a.op(Opcode::Float { dst: fr, ptr: two });
    a.op(Opcode::SMod {
        dst: t,
        a: t,
        b: fr,
    });
    a.op(Opcode::Call1 {
        dst: k,
        fun: p.floor,
        arg0: t,
    });
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.jmp(
        Opcode::JEq {
            a: k,
            b: zero,
            offset: 0,
        },
        "set",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.label("set");
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_visible,
        arg0: o,
        arg1: b,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.obj_t, p.f64_],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `pingCell(ctrl, x, y)` (see the header).
fn add_cell(code: &mut Bytecode, p: &CellPlan, blink: RefFun) -> Result<RefFun> {
    let path = str_global(code, p.str_t, SQUARE);
    let (f0, fh, fz, fa, fd) = (
        float_const(code, 0.0),
        float_const(code, 0.5),
        float_const(code, 0.01),
        float_const(code, ALPHA),
        float_const(code, DURATION),
    );
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.ctrl_t, p.f64_, p.f64_]);
    let (ctrl, x, y) = (Reg(0), Reg(1), Reg(2));
    let (game, battle, grid, cs, zf, q, i, j, zero, lim, cx, cy, half) = (
        r.r(p.c_game.1),
        r.r(p.g_battle.1),
        r.r(p.b_grid.1),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
    );
    let (exc, loader, ps, cls, hres, res, z, rot, alpha, ob, ev, dur, rm, bl, v) = (
        r.r(p.dyn_t),
        r.r(p.loader_t),
        r.r(p.str_t),
        r.r(p.res_cls_t),
        r.r(p.hres_t),
        r.r(p.res_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.obj_t),
        r.r(p.g_event.1),
        r.r(p.f64_),
        r.r(p.wait_cb_t),
        r.r(p.until_cb_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: game,
        obj: ctrl,
        field: p.c_game.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: battle,
        obj: game,
        field: p.g_battle.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: battle,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: grid,
        obj: battle,
        field: p.b_grid.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: grid,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: cs,
        obj: grid,
        field: p.gr_cell,
    });
    a.op(Opcode::Float { dst: zf, ptr: f0 });
    a.jmp(
        Opcode::JSLte {
            a: cs,
            b: zf,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.op(Opcode::Float { dst: half, ptr: fh });
    // i = floor(x / cs), inside [0, w)
    a.op(Opcode::SDiv {
        dst: q,
        a: x,
        b: cs,
    });
    a.op(Opcode::Call1 {
        dst: i,
        fun: p.floor,
        arg0: q,
    });
    a.jmp(
        Opcode::JSLt {
            a: i,
            b: zero,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: lim,
        obj: grid,
        field: p.gr_w,
    });
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: lim,
            offset: 0,
        },
        "end",
    );
    // j = floor(y / cs), inside [0, h)
    a.op(Opcode::SDiv {
        dst: q,
        a: y,
        b: cs,
    });
    a.op(Opcode::Call1 {
        dst: j,
        fun: p.floor,
        arg0: q,
    });
    a.jmp(
        Opcode::JSLt {
            a: j,
            b: zero,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: lim,
        obj: grid,
        field: p.gr_h,
    });
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: lim,
            offset: 0,
        },
        "end",
    );
    // cell center (i + .5) * cs, (j + .5) * cs
    a.op(Opcode::ToSFloat { dst: cx, src: i });
    a.op(Opcode::Add {
        dst: cx,
        a: cx,
        b: half,
    });
    a.op(Opcode::Mul {
        dst: cx,
        a: cx,
        b: cs,
    });
    a.op(Opcode::ToSFloat { dst: cy, src: j });
    a.op(Opcode::Add {
        dst: cy,
        a: cy,
        b: half,
    });
    a.op(Opcode::Mul {
        dst: cy,
        a: cy,
        b: cs,
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Call0 {
        dst: loader,
        fun: p.get_loader,
    });
    a.op(Opcode::GetGlobal {
        dst: ps,
        global: path,
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.res_cls,
    });
    a.op(Opcode::Call3 {
        dst: hres,
        fun: p.load_cache,
        arg0: loader,
        arg1: ps,
        arg2: cls,
    });
    a.op(Opcode::SafeCast {
        dst: res,
        src: hres,
    });
    a.jmp(
        Opcode::JNull {
            reg: res,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Float { dst: z, ptr: fz });
    a.op(Opcode::Float { dst: rot, ptr: f0 });
    a.op(Opcode::Float {
        dst: alpha,
        ptr: fa,
    });
    a.op(Opcode::CallN {
        dst: ob,
        fun: p.add_socle,
        args: vec![battle, res, cx, cy, z, cs, rot, alpha],
    });
    a.jmp(Opcode::JNull { reg: ob, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: ev,
        obj: game,
        field: p.g_event.0,
    });
    a.jmp(Opcode::JNull { reg: ev, offset: 0 }, "untrap");
    a.op(Opcode::Float { dst: dur, ptr: fd });
    a.op(Opcode::InstanceClosure {
        dst: rm,
        fun: p.remove,
        obj: ob,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.wait,
        arg0: ev,
        arg1: dur,
        arg2: rm,
    });
    a.op(Opcode::InstanceClosure {
        dst: bl,
        fun: blink,
        obj: ob,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.wait_until,
        arg0: ev,
        arg1: bl,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.ctrl_t, p.f64_, p.f64_],
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn cell_apply(code: &mut Bytecode, p: &CellPlan) -> Result<()> {
    let blink = add_blink(code, p)?;
    let cell = add_cell(code, p, blink)?;
    let f = &mut code.functions[p.impl_fi];
    insert_ops(
        f,
        p.ret_at,
        vec![Opcode::Call3 {
            dst: p.ret_reg,
            fun: cell,
            arg0: Reg(0),
            arg1: Reg(1),
            arg2: Reg(2),
        }],
    );
    eprintln!(
        "patched ping fn@{} op {}: the pinged battle cell blinks orange (pingCell fn@{}, blink fn@{})",
        f.findex.0, p.ret_at, cell.0, blink.0
    );
    Ok(())
}

/// Fixes the ping's depth sample and makes the pinged battle cell blink orange
/// on every peer; each half that does not validate is skipped and logged.
pub(crate) fn patch_ping_cell(code: &mut Bytecode) {
    match depth_plan(code) {
        Ok(p) => depth_apply(code, &p),
        Err(e) => eprintln!("ping depth skipped: {e:#}"),
    }
    let snap = crate::asm::Snap::take(code);
    let r = cell_plan(code).and_then(|p| cell_apply(code, &p));
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("ping cell skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Game.ping: 7 ops replaced + 1 inserted; ping__impl: one call before Ret;
    /// two well-typed functions appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let dp = depth_plan(&orig).expect("depth plan");
        let cp = cell_plan(&orig).expect("cell plan");
        let mut code = read(&image);
        patch_ping_cell(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 2);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != dp.fi && i != cp.impl_fi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        let n = back.functions.len();
        let (blink, cell) = (&back.functions[n - 2], &back.functions[n - 1]);

        // Game.ping
        let (a, b) = (&orig.functions[dp.fi], &back.functions[dp.fi]);
        assert_eq!(a.regs, b.regs);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        assert!(matches!(b.ops[dp.at], Opcode::NullCheck { reg } if reg == dp.pix));
        let map = |t: usize| if t < dp.at { t } else { t + 1 };
        for i in 0..a.ops.len() {
            let ta: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            let j = map(i);
            assert_eq!(jump_targets(b, j), ta, "op {i}");
            if !(dp.at..dp.at + 7).contains(&i) && ta.is_empty() {
                assert_eq!(
                    format!("{:?}", b.ops[j]),
                    format!("{:?}", a.ops[i]),
                    "op {i}"
                );
            }
        }
        check_types(&back, b, dp.at..dp.at + 8);
        check_flow(b);

        // ping__impl
        let (a, b) = (&orig.functions[cp.impl_fi], &back.functions[cp.impl_fi]);
        assert_eq!(a.regs, b.regs);
        shifted(a, b, cp.ret_at, 1);
        assert!(matches!(
            b.ops[cp.ret_at],
            Opcode::Call3 { fun, arg0: Reg(0), arg1: Reg(1), arg2: Reg(2), .. } if fun == cell.findex
        ));
        check_types(&back, b, cp.ret_at..cp.ret_at + 1);

        for f in [blink, cell] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }

        let mut again = read(&patched);
        assert!(depth_plan(&again).is_err());
        assert!(cell_plan(&again).is_err());
        patch_ping_cell(&mut again);
        assert!(write(&again) == patched);
    }

    /// Unexpected shapes skip the half they belong to and leave it as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let dp = depth_plan(&orig).expect("depth plan");
        let cp = cell_plan(&orig).expect("cell plan");

        // Depth: the mouse Y sample is not a get_mouseY call.
        let mut code = read(&image);
        code.functions[dp.fi].ops[dp.at + 6] = Opcode::Nop;
        assert!(depth_plan(&code).is_err());
        let fi_ops = format!("{:?}", code.functions[dp.fi].ops);
        patch_ping_cell(&mut code);
        assert_eq!(format!("{:?}", code.functions[dp.fi].ops), fi_ops);
        assert_eq!(code.functions.len(), orig.functions.len() + 2);

        // Cell: no sfx call before the final Ret.
        let mut code = read(&image);
        code.functions[cp.impl_fi].ops[cp.ret_at - 1] = Opcode::Nop;
        assert!(cell_plan(&code).is_err());
        let impl_ops = format!("{:?}", code.functions[cp.impl_fi].ops);
        let (nt, nf, ns, ng) = (
            code.types.len(),
            code.functions.len(),
            code.strings.len(),
            code.globals.len(),
        );
        patch_ping_cell(&mut code);
        assert_eq!(format!("{:?}", code.functions[cp.impl_fi].ops), impl_ops);
        assert_eq!(
            (
                code.types.len(),
                code.functions.len(),
                code.strings.len(),
                code.globals.len()
            ),
            (nt, nf, ns, ng)
        );
    }
}
