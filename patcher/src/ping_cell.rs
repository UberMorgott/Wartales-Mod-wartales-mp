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
//   2. Controller.ping__impl starts with
//          if (pingCell(this, x, y, player)) return;   // new function
//      so in battle the vanilla marker (ping.fx: camera-facing ripple rings,
//      T_ChocWave / T_circle textures) is not spawned; and its closing
//      `ui.sfx("Ping")` (Wwise Play_WT_Hover_Skill, a barely audible hover tick,
//      marked TODO in data.cdb) becomes pingSound(this).
//      pingCell(ctrl, x, y, player) -> Bool:
//          b = game.battle; if (b == null || b.grid == null) return false;
//          cs = b.grid.cellSize; i = floor(x / cs); j = floor(y / cs);
//          if (i, j) outside the grid: return false;
//          center = cell center, size = cs;
//          try {
//              u = b.getUnitThere(i, j);    // alive, visible units only
//              if (u != null && (n = u.getSocleSize()) >= 1) {
//                  // its footprint, as getUnitThere computes it
//                  p = u.getPosition(grid.pointTmp);
//                  left = floor(p.x / cs) - floor(n / 2); (top alike)
//                  center = ((left + n / 2) * cs, ...); size = n * cs;
//              }
//              c = getConst("PlayerColor" + player.getColor()).color;
//              o = b.addSocle(load("prefabs/huds/orangeSquare.prefab"), center,
//                             0.01, size, 0, 0.85);
//              every pass p of o's materials holding a ColorSet:
//                  p.removeShaders(ColorSet); p.addShader(new ColorSet(c));
//              game.globalEvent.wait(3, o.remove);
//              game.globalEvent.waitUntil(pingBlink.bind({o: o, u: u, c: c}));
//          } catch (_) { return false; }
//          pingSound(ctrl); return true;
//      pingBlink(st, dt): while st.o is attached, st.o and st.u's outline
//          (Entity.setOutline(st.c)) blink 3 times a second; once st.o is gone
//          the outline is cleared (setOutline(null), as Unit.select(false))
//          and the callback ends.
//      pingSound(ctrl): ctrl.game.ui.sfx(SOUND) unless the last one played less
//          than SOUND_GAP s ago (hxd.Timer.lastTimeStamp); the game's sfx path,
//          so the SFX volume setting applies.
//      The color is the one ent.BasePlayer.getName gives the player's nickname
//      (PlayerColor<n>, n = BasePlayer.color + 1, a networked property), so all
//      peers agree. orangeSquare.prefab is the game's own (unused) one-cell
//      overlay: an Overlay pass with depthTest Always and a ColorSet shader
//      (gfx/shader/ColorSet.hx), so the square shows over terrain, props and
//      units from any camera and takes any color. That ColorSet must not be
//      tinted in place: the prefab is cached, its DynamicShader clones copy
//      `template`, makeShader returns template.shader.clone(), and
//      hxsl.Shader.clone is `return this` (ColorSet has no override), so ONE
//      instance, synced to the prefab's orange, sits in every square. Each
//      square gets its own ColorSet(c) (amount 1) instead. addSocle is how the game
//      places its other cell squares, as its own object: the player's move /
//      attack previews are untouched.
//
// Purely visual and local to each peer: it rides the existing ping RPC (no new
// message, no game state). Outside a battle (world map, places, camp) only the
// depth fix and the sound apply; the vanilla marker stays there.
//
// Each half is validated before editing; a mismatch skips that half (logged).

use super::asm::{push_fn, string_ref, Asm, Regs};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefGlobal, ValBool};

const SQUARE: &str = "prefabs/huds/orangeSquare.prefab";
/// data.cdb sound id (Wwise Play_WT_UI_ChatReceived_Message): the game's own
/// co-op chat "message received" cue. Alternatives: NotifyNeutral
/// (Play_WT_Interface_Notification_Neutral), SelectorClick.
const SOUND: &str = "ChatReceiveMessage";
/// Minimum seconds between two ping sounds.
const SOUND_GAP: f64 = 0.3;
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

pub(crate) fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let t = match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => n.t,
        None => code.functions[fun_index(code, f)?].t,
    };
    let t = t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

pub(crate) fn fname(code: &Bytecode, f: RefFun) -> &str {
    code.functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| s(code, g.name))
        .unwrap_or("")
}

/// The class global of `name` (HL stores it 1-based) and its type `pkg.$Cls`.
pub(crate) fn class_global(code: &Bytecode, name: &str) -> Result<(RefGlobal, RefType)> {
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

// ---------- 2. player-colored cell / unit footprint (Controller.ping__impl) ----------

/// Proto `name` of class `t` or its nearest ancestor: (function, vtable index).
pub(crate) fn vproto(code: &Bytecode, t: RefType, name: &str) -> Result<(RefFun, i32)> {
    let mut cur = Some(t);
    while let Some(c) = cur {
        let o = obj(code, c)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == name) {
            return Ok((p.findex, p.pindex));
        }
        cur = o.super_;
    }
    bail!("proto {name} not found on type {} or its ancestors", t.0)
}

/// Name of a function or native.
fn any_name(code: &Bytecode, f: RefFun) -> &str {
    match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => s(code, n.name),
        None => fname(code, f),
    }
}

/// The unique `CallN dst = fun(args)` in `f` whose callee is named `name`.
fn call_named(code: &Bytecode, f: &Function, name: &str) -> Result<(RefFun, Vec<Reg>, Reg)> {
    let hits: Vec<(RefFun, Vec<Reg>, Reg)> = f
        .ops
        .iter()
        .filter_map(|o| {
            let (fun, args, dst) = match o {
                Opcode::Call0 { dst, fun } => (*fun, vec![], *dst),
                Opcode::Call1 { dst, fun, arg0 } => (*fun, vec![*arg0], *dst),
                Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                } => (*fun, vec![*arg0, *arg1], *dst),
                Opcode::Call3 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                } => (*fun, vec![*arg0, *arg1, *arg2], *dst),
                Opcode::Call4 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                    arg3,
                } => (*fun, vec![*arg0, *arg1, *arg2, *arg3], *dst),
                Opcode::CallN { dst, fun, args } => (*fun, args.clone(), *dst),
                _ => return None,
            };
            (any_name(code, fun) == name).then_some((fun, args, dst))
        })
        .collect();
    let Some(first) = hits.first() else {
        bail!("fn@{}: no {name} call", f.findex.0);
    };
    if hits.iter().any(|h| h.0 != first.0) {
        bail!("fn@{}: {name} calls reach different functions", f.findex.0);
    }
    Ok(first.clone())
}

struct CellPlan {
    impl_fi: usize,
    /// The vanilla `ui.sfx("Ping", null)` call, right before the final Ret.
    sfx_at: usize,
    ret_reg: Reg,
    ctrl_t: RefType,
    player_t: RefType,
    f64_: RefType,
    i32_: RefType,
    bool_: RefType,
    void_: RefType,
    str_t: RefType,
    dyn_t: RefType,
    dynobj_t: RefType,
    c_game: (RefField, RefType),
    g_battle: (RefField, RefType),
    g_event: (RefField, RefType),
    g_ui: (RefField, RefType),
    b_grid: (RefField, RefType),
    gr_cell: RefField,
    gr_w: RefField,
    gr_h: RefField,
    gr_pt: (RefField, RefType),
    pt_x: RefField,
    pt_y: RefField,
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
    sfx: RefFun,
    // player color: getConst("PlayerColor" + player.getColor()).color
    get_color: RefFun,
    itos: RefFun,
    alloc_str: RefFun,
    add_str: RefFun,
    ref_i32: RefType,
    bytes_t: RefType,
    get_const: RefFun,
    const_t: RefType,
    cv_color: (RefField, RefType),
    // unit on the cell
    unit_t: RefType,
    unit_there: RefFun,
    arr_t: RefType,
    ref_bool: RefType,
    socle_pindex: RefField,
    get_pos: RefFun,
    set_outline: RefFun,
    // tint: each pass holding the prefab's (shared) ColorSet gets its own
    // new ColorSet(color) instead
    get_mats: RefFun,
    raw_arr_t: RefType,
    a_len: RefField,
    a_arr: RefField,
    mat_t: RefType,
    m_pass: (RefField, RefType),
    p_next: RefField,
    get_shader: RefFun,
    remove_shaders: RefFun,
    add_shader: RefFun,
    shader_t: RefType,
    cset_t: RefType,
    cset_ctor: RefFun,
    cset_cls: RefGlobal,
    cset_cls_t: RefType,
    dbg_file: usize,
}

fn cell_plan(code: &Bytecode) -> Result<CellPlan> {
    let f64_ = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let dynobj_t = prim_type(code, "dynobj", |t| matches!(t, Type::DynObj))?;
    let bytes_t = prim_type(code, "bytes", |t| matches!(t, Type::Bytes))?;
    let ref_i32 = prim_type(
        code,
        "ref<i32>",
        |t| matches!(t, Type::Ref(x) if *x == i32_),
    )?;
    let ref_bool = prim_type(
        code,
        "ref<bool>",
        |t| matches!(t, Type::Ref(x) if *x == bool_),
    )?;
    let str_t = obj_type(code, "String")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let player_t = obj_type(code, "ent.BasePlayer")?;
    let game_t = obj_type(code, "Game")?;
    let battle_t = obj_type(code, "battle.Battle")?;
    let grid_t = obj_type(code, "battle.Grid")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let obj_t = obj_type(code, "h3d.scene.Object")?;
    let ev_t = obj_type(code, "hxd.WaitEvent")?;
    let res_t = obj_type(code, "hrt.prefab.Resource")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let point_t = obj_type(code, "h2d.col.PointImpl")?;
    let mat_t = obj_type(code, "h3d.mat.Material")?;
    let shader_t = obj_type(code, "hxsl.Shader")?;
    let cset_t = obj_type(code, "gfx.shader.ColorSet")?;

    let c_game = field(code, ctrl_t, "game")?;
    let g_battle = field(code, game_t, "battle")?;
    let g_event = field(code, game_t, "globalEvent")?;
    let g_ui = field(code, game_t, "ui")?;
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
    let gr_pt = field(code, grid_t, "pointTmp")?;
    let (pt_x, xt) = field(code, point_t, "x")?;
    let (pt_y, yt) = field(code, point_t, "y")?;
    if gr_pt.1 != point_t || xt != f64_ || yt != f64_ {
        bail!("Grid.pointTmp / PointImpl.x / y types differ");
    }
    let o_parent = field(code, obj_t, "parent")?;
    let (a_len, lt) = field(code, arr_t, "length")?;
    let (a_arr, at) = field(code, arr_t, "array")?;
    if lt != i32_ || !matches!(code.types[at.0], Type::Array) {
        bail!("ArrayObj length / array types differ");
    }
    let raw_arr_t = at;
    let m_pass = field(code, mat_t, "passes")?;
    let (p_next, nt) = field(code, m_pass.1, "nextPass")?;
    if nt != m_pass.1 {
        bail!("Pass.nextPass is not a Pass");
    }

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
    let unit_there = proto(code, battle_t, "getUnitThere")?;
    want(
        unit_there,
        "Battle.getUnitThere",
        &[battle_t, i32_, i32_, arr_t, ref_bool],
        unit_t,
    )?;
    let remove = proto(code, obj_t, "remove")?;
    want(remove, "Object.remove", &[obj_t], void_)?;
    let set_visible = proto(code, obj_t, "set_visible")?;
    want(set_visible, "Object.set_visible", &[obj_t, bool_], bool_)?;
    let (get_mats, _) = vproto(code, obj_t, "getMaterials")?;
    want(
        get_mats,
        "Object.getMaterials",
        &[obj_t, arr_t, ref_bool],
        arr_t,
    )?;
    let get_shader = proto(code, m_pass.1, "getShader")?;
    let (ga, gr) = sig(code, get_shader)?;
    if ga.len() != 2 || ga[0] != m_pass.1 || gr != shader_t {
        bail!("unexpected Pass.getShader signature");
    }
    let (cset_cls, cset_cls_t) = class_global(code, "gfx.shader.ColorSet")?;
    let remove_shaders = proto(code, m_pass.1, "removeShaders")?;
    want(remove_shaders, "Pass.removeShaders", &[m_pass.1, ga[1]], void_)?;
    let add_shader = proto(code, m_pass.1, "addShader")?;
    want(add_shader, "Pass.addShader", &[m_pass.1, shader_t], shader_t)?;
    // new ColorSet(?color: Int): color__ = rgb(color), amount = 1
    let cset_ctor = method(code, cset_t, "__constructor__")?.findex;
    want(cset_ctor, "ColorSet.__constructor__", &[cset_t, ref_i32], void_)?;
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

    // Unit footprint, as Battle.getUnitThere computes it: socle size by its
    // virtual getSocleSize, position by getPosition(grid.pointTmp).
    let ut = &code.functions[fun_index(code, unit_there)?];
    let (socle_fn, socle_pi) = vproto(code, unit_t, "getSocleSize")?;
    want(socle_fn, "Unit.getSocleSize", &[unit_t], i32_)?;
    if socle_pi < 0
        || !ut
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::CallMethod { field, .. } if field.0 as i32 == socle_pi))
    {
        bail!("Battle.getUnitThere does not size units by the virtual getSocleSize");
    }
    let (get_pos, _, _) = call_named(code, ut, "getPosition")?;
    let (pa, pr) = sig(code, get_pos)?;
    if pa.len() != 2 || pa[1] != point_t || pr != point_t {
        bail!("unexpected getPosition signature");
    }
    let select = method(code, unit_t, "select")?;
    let (set_outline, _, _) = call_named(code, select, "setOutline")?;
    let (oa, or) = sig(code, set_outline)?;
    let null_i32 = match oa[..] {
        [_, n] if or == void_ && matches!(code.types[n.0], Type::Null(x) if x == i32_) => n,
        _ => bail!("unexpected setOutline signature"),
    };

    // The nickname color: ent.BasePlayer.getName wraps the name in
    // getConst("PlayerColor" + getColor()).color.
    let get_color = proto(code, player_t, "getColor")?;
    want(get_color, "BasePlayer.getColor", &[player_t], i32_)?;
    let gn = method(code, player_t, "getName")?;
    if !gn.ops.iter().any(|o| {
        matches!(o, Opcode::GetGlobal { global, .. } if crate::job_xp::const_str(code, *global) == Some("PlayerColor"))
    }) {
        bail!("BasePlayer.getName does not read PlayerColor");
    }
    let (itos, _, _) = call_named(code, gn, "itos")?;
    want(itos, "itos", &[i32_, ref_i32], bytes_t)?;
    let (alloc_str, _, _) = call_named(code, gn, "__alloc__")?;
    want(alloc_str, "String.__alloc__", &[bytes_t, i32_], str_t)?;
    let (add_str, _, _) = call_named(code, gn, "__add__")?;
    want(add_str, "String.__add__", &[str_t, str_t], str_t)?;
    let (get_const, _, cdst) = call_named(code, gn, "getConst")?;
    let const_t = gn.regs[cdst.0 as usize];
    want(get_const, "getConst", &[str_t], const_t)?;
    let cv_color = match &code.types[const_t.0] {
        Type::Virtual { fields } => fields
            .iter()
            .position(|f| s(code, f.name) == "color")
            .map(|i| (RefField(i), fields[i].t)),
        _ => None,
    }
    .context("getConst result has no color field")?;
    if cv_color.1 != null_i32 {
        bail!("constant color is not null<i32>");
    }

    // ping__impl(this, x, y, z, player): the fx load gives the loader, loadCache
    // and the hrt.prefab.Resource class; it ends with ui.sfx("Ping") and Ret.
    let imp = method(code, ctrl_t, "ping__impl")?;
    let a = fun_args(code, imp);
    if a.len() != 5 || a[1] != f64_ || a[2] != f64_ || a[3] != f64_ || a[4] != player_t {
        bail!("unexpected Controller.ping__impl signature");
    }
    let impl_fi = fun_index(code, imp.findex)?;
    let o = &imp.ops;
    if matches!(
        o.first(),
        Some(Opcode::Call4 {
            arg0: Reg(0),
            arg1: Reg(1),
            arg2: Reg(2),
            arg3: Reg(4),
            ..
        })
    ) {
        bail!("ping__impl: already applied");
    }
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
    let sfx_at = ret_at - 1;
    let sfx = match (&o[sfx_at - 2], &o[sfx_at]) {
        (
            Opcode::GetGlobal { dst: g, global },
            Opcode::Call3 {
                fun, arg0, arg1, ..
            },
        ) if arg1 == g
            && fname(code, *fun) == "sfx"
            && imp.regs[arg0.0 as usize] == g_ui.1
            && crate::job_xp::const_str(code, *global) == Some("Ping") =>
        {
            *fun
        }
        _ => bail!("ping__impl: no ui.sfx(\"Ping\") before the final Ret"),
    };
    want(sfx, "GameUI.sfx", &[g_ui.1, str_t, dyn_t], void_)?;
    for i in 0..o.len() {
        if jump_targets(imp, i)
            .iter()
            .any(|&t| t == sfx_at || t == ret_at)
        {
            bail!("ping__impl: a jump lands on the sfx call or the final Ret");
        }
    }
    Ok(CellPlan {
        impl_fi,
        sfx_at,
        ret_reg,
        ctrl_t,
        player_t,
        f64_,
        i32_,
        bool_,
        void_,
        str_t,
        dyn_t,
        dynobj_t,
        c_game,
        g_battle,
        g_event,
        g_ui,
        b_grid,
        gr_cell,
        gr_w,
        gr_h,
        gr_pt,
        pt_x,
        pt_y,
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
        sfx,
        get_color,
        itos,
        alloc_str,
        add_str,
        ref_i32,
        bytes_t,
        get_const,
        const_t,
        cv_color,
        unit_t,
        unit_there,
        arr_t,
        ref_bool,
        socle_pindex: RefField(socle_pi as usize),
        get_pos,
        set_outline,
        get_mats,
        raw_arr_t,
        a_len,
        a_arr,
        mat_t,
        m_pass,
        get_shader,
        shader_t,
        p_next,
        remove_shaders,
        add_shader,
        cset_t,
        cset_ctor,
        cset_cls,
        cset_cls_t,
        dbg_file: debug_file(code, "src/st/Controller.hx")?,
    })
}

/// `pingSound(ctrl)`: `ctrl.game.ui.sfx(SOUND)`, at most once per SOUND_GAP s.
fn add_sound(code: &mut Bytecode, p: &CellPlan) -> Result<RefFun> {
    let name = str_global(code, p.str_t, SOUND);
    code.globals.push(p.f64_);
    let last_g = RefGlobal(code.globals.len() - 1);
    let (fgap, f0) = (float_const(code, SOUND_GAP), float_const(code, 0.0));
    let mut r = Regs(vec![p.ctrl_t]);
    let ctrl = Reg(0);
    let (tm, t, last, d, k, game, ui, sn, nul, v) = (
        r.r(p.timer_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.c_game.1),
        r.r(p.g_ui.1),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: tm,
        global: p.timer,
    });
    a.op(Opcode::Field {
        dst: t,
        obj: tm,
        field: p.t_stamp,
    });
    a.op(Opcode::GetGlobal {
        dst: last,
        global: last_g,
    });
    a.op(Opcode::Sub {
        dst: d,
        a: t,
        b: last,
    });
    a.op(Opcode::Float { dst: k, ptr: fgap });
    a.jmp(
        Opcode::JSGte {
            a: d,
            b: k,
            offset: 0,
        },
        "play",
    );
    // the stamp went backwards (clock change): play anyway
    a.op(Opcode::Float { dst: k, ptr: f0 });
    a.jmp(
        Opcode::JSLt {
            a: d,
            b: k,
            offset: 0,
        },
        "play",
    );
    a.op(Opcode::Ret { ret: v });
    a.label("play");
    a.op(Opcode::SetGlobal {
        global: last_g,
        src: t,
    });
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
        dst: ui,
        obj: game,
        field: p.g_ui.0,
    });
    a.jmp(Opcode::JNull { reg: ui, offset: 0 }, "end");
    a.op(Opcode::GetGlobal {
        dst: sn,
        global: name,
    });
    a.op(Opcode::Null { dst: nul });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.sfx,
        arg0: ui,
        arg1: sn,
        arg2: nul,
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.ctrl_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// `pingBlink(st, dt) -> Bool`, st = { o: square, u: unit or null, c: color }:
/// the square (and the unit's outline, in the player color) blink until the
/// square is removed; then the outline is cleared and the callback ends.
fn add_blink(code: &mut Bytecode, p: &CellPlan) -> Result<RefFun> {
    let (ko, ku, kc) = (
        string_ref(code, "o"),
        string_ref(code, "u"),
        string_ref(code, "c"),
    );
    let rate = float_const(code, BLINK_RATE);
    let two = float_const(code, 2.0);
    let i0 = int_const(code, 0);
    let mut r = Regs(vec![p.dynobj_t, p.f64_]);
    let st = Reg(0);
    let (o, u, c, nc, par, b, tm, t, fr, k, zero, v) = (
        r.r(p.obj_t),
        r.r(p.unit_t),
        r.r(p.cv_color.1),
        r.r(p.cv_color.1),
        r.r(p.o_parent.1),
        r.r(p.bool_),
        r.r(p.timer_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::DynGet {
        dst: o,
        obj: st,
        field: ko,
    });
    a.op(Opcode::DynGet {
        dst: u,
        obj: st,
        field: ku,
    });
    a.op(Opcode::Null { dst: nc });
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "done");
    a.op(Opcode::Field {
        dst: par,
        obj: o,
        field: p.o_parent.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: par,
            offset: 0,
        },
        "done",
    );
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
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "live");
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "off");
    a.op(Opcode::DynGet {
        dst: c,
        obj: st,
        field: kc,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_outline,
        arg0: u,
        arg1: c,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "live");
    a.label("off");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_outline,
        arg0: u,
        arg1: nc,
    });
    a.label("live");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    a.label("done");
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "stop");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.set_outline,
        arg0: u,
        arg1: nc,
    });
    a.label("stop");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.dynobj_t, p.f64_],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `pingCell(ctrl, x, y, player) -> Bool` (see the header); true when the
/// battle marker was placed (the caller then skips the vanilla fx).
fn add_cell(code: &mut Bytecode, p: &CellPlan, blink: RefFun, sound: RefFun) -> Result<RefFun> {
    let path = str_global(code, p.str_t, SQUARE);
    let pfx = str_global(code, p.str_t, "PlayerColor");
    let (ko, ku, kc) = (
        string_ref(code, "o"),
        string_ref(code, "u"),
        string_ref(code, "c"),
    );
    let (f0, fh, fz, fa, fd) = (
        float_const(code, 0.0),
        float_const(code, 0.5),
        float_const(code, 0.01),
        float_const(code, ALPHA),
        float_const(code, DURATION),
    );
    let (i0, i1) = (int_const(code, 0), int_const(code, 1));
    let mut r = Regs(vec![p.ctrl_t, p.f64_, p.f64_, p.player_t]);
    let (ctrl, x, y, player) = (Reg(0), Reg(1), Reg(2), Reg(3));
    let res = r.r(p.bool_);
    let (game, battle, grid, cs, zf, q, i, j, zero, lim, cx, cy, half, sc) = (
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
        r.r(p.f64_),
    );
    let (color, exc, nul_arr, nul_ref, u, n, one, pt, nf, hn, l) = (
        r.r(p.i32_),
        r.r(p.dyn_t),
        r.r(p.arr_t),
        r.r(p.ref_bool),
        r.r(p.unit_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.gr_pt.1),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.i32_),
    );
    let (ps, cc, rcc, bys, s2, key, cv, oc, ci) = (
        r.r(p.str_t),
        r.r(p.i32_),
        r.r(p.ref_i32),
        r.r(p.bytes_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.const_t),
        r.r(p.cv_color.1),
        r.r(p.i32_),
    );
    let (loader, cls, hres, rs, z, rot, alpha, ob) = (
        r.r(p.loader_t),
        r.r(p.res_cls_t),
        r.r(p.hres_t),
        r.r(p.res_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.obj_t),
    );
    let (tb, rtb, mats, ki, len, raw, dv, m, pass, ccls, shd, cset) = (
        r.r(p.bool_),
        r.r(p.ref_bool),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.raw_arr_t),
        r.r(p.dyn_t),
        r.r(p.mat_t),
        r.r(p.m_pass.1),
        r.r(p.cset_cls_t),
        r.r(p.shader_t),
        r.r(p.cset_t),
    );
    let (stv, ev, dur, rm, bl, v) = (
        r.r(p.dynobj_t),
        r.r(p.g_event.1),
        r.r(p.f64_),
        r.r(p.wait_cb_t),
        r.r(p.until_cb_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JNull {
            reg: player,
            offset: 0,
        },
        "end",
    );
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
    // one cell: center (i + .5) * cs, (j + .5) * cs, size cs
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
    a.op(Opcode::Mov { dst: sc, src: cs });
    a.op(Opcode::Call1 {
        dst: color,
        fun: p.get_color,
        arg0: player,
    });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    // A unit on the cell (visible, alive): its whole footprint instead.
    a.op(Opcode::Null { dst: nul_arr });
    a.op(Opcode::Null { dst: nul_ref });
    a.op(Opcode::CallN {
        dst: u,
        fun: p.unit_there,
        args: vec![battle, i, j, nul_arr, nul_ref],
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "square");
    a.op(Opcode::CallMethod {
        dst: n,
        field: p.socle_pindex,
        args: vec![u],
    });
    a.op(Opcode::Int { dst: one, ptr: i1 });
    a.jmp(
        Opcode::JSLt {
            a: n,
            b: one,
            offset: 0,
        },
        "square",
    );
    a.op(Opcode::Field {
        dst: pt,
        obj: grid,
        field: p.gr_pt.0,
    });
    a.op(Opcode::Call2 {
        dst: pt,
        fun: p.get_pos,
        arg0: u,
        arg1: pt,
    });
    a.jmp(Opcode::JNull { reg: pt, offset: 0 }, "square");
    a.op(Opcode::ToSFloat { dst: nf, src: n });
    a.op(Opcode::Mul {
        dst: hn,
        a: nf,
        b: half,
    });
    a.op(Opcode::Call1 {
        dst: lim,
        fun: p.floor,
        arg0: hn,
    });
    // left = floor(p.x / cs) - floor(n / 2); cx = (left + n / 2) * cs
    for (f, c) in [(p.pt_x, cx), (p.pt_y, cy)] {
        a.op(Opcode::Field {
            dst: q,
            obj: pt,
            field: f,
        });
        a.op(Opcode::SDiv {
            dst: q,
            a: q,
            b: cs,
        });
        a.op(Opcode::Call1 {
            dst: l,
            fun: p.floor,
            arg0: q,
        });
        a.op(Opcode::Sub {
            dst: l,
            a: l,
            b: lim,
        });
        a.op(Opcode::ToSFloat { dst: c, src: l });
        a.op(Opcode::Add {
            dst: c,
            a: c,
            b: hn,
        });
        a.op(Opcode::Mul {
            dst: c,
            a: c,
            b: cs,
        });
    }
    a.op(Opcode::Mul {
        dst: sc,
        a: nf,
        b: cs,
    });
    a.label("square");
    // oc = getConst("PlayerColor" + color).color
    a.op(Opcode::GetGlobal {
        dst: ps,
        global: pfx,
    });
    a.op(Opcode::Mov {
        dst: cc,
        src: color,
    });
    a.op(Opcode::Ref { dst: rcc, src: cc });
    a.op(Opcode::Call2 {
        dst: bys,
        fun: p.itos,
        arg0: cc,
        arg1: rcc,
    });
    a.op(Opcode::Call2 {
        dst: s2,
        fun: p.alloc_str,
        arg0: bys,
        arg1: cc,
    });
    a.op(Opcode::Call2 {
        dst: key,
        fun: p.add_str,
        arg0: ps,
        arg1: s2,
    });
    a.op(Opcode::Call1 {
        dst: cv,
        fun: p.get_const,
        arg0: key,
    });
    a.jmp(Opcode::JNull { reg: cv, offset: 0 }, "untrap");
    a.op(Opcode::Field {
        dst: oc,
        obj: cv,
        field: p.cv_color.0,
    });
    a.jmp(Opcode::JNull { reg: oc, offset: 0 }, "untrap");
    a.op(Opcode::SafeCast { dst: ci, src: oc });
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
    a.op(Opcode::SafeCast { dst: rs, src: hres });
    a.jmp(Opcode::JNull { reg: rs, offset: 0 }, "untrap");
    a.op(Opcode::Float { dst: z, ptr: fz });
    a.op(Opcode::Float { dst: rot, ptr: f0 });
    a.op(Opcode::Float {
        dst: alpha,
        ptr: fa,
    });
    a.op(Opcode::CallN {
        dst: ob,
        fun: p.add_socle,
        args: vec![battle, rs, cx, cy, z, sc, rot, alpha],
    });
    a.jmp(Opcode::JNull { reg: ob, offset: 0 }, "untrap");
    // tint: every pass of the square's materials that holds a ColorSet (the
    // prefab's, one instance shared by all squares) gets its own
    // new ColorSet(color) in its place
    a.op(Opcode::Null { dst: nul_arr });
    a.op(Opcode::Bool {
        dst: tb,
        value: ValBool(true),
    });
    a.op(Opcode::Ref { dst: rtb, src: tb });
    a.op(Opcode::Call3 {
        dst: mats,
        fun: p.get_mats,
        arg0: ob,
        arg1: nul_arr,
        arg2: rtb,
    });
    a.jmp(
        Opcode::JNull {
            reg: mats,
            offset: 0,
        },
        "tinted",
    );
    a.op(Opcode::Int { dst: ki, ptr: i0 });
    a.loop_head("mat");
    a.op(Opcode::Field {
        dst: len,
        obj: mats,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: ki,
            b: len,
            offset: 0,
        },
        "tinted",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: mats,
        field: p.a_arr,
    });
    a.op(Opcode::GetArray {
        dst: dv,
        array: raw,
        index: ki,
    });
    a.op(Opcode::Incr { dst: ki });
    a.op(Opcode::UnsafeCast { dst: m, src: dv });
    a.jmp(Opcode::JNull { reg: m, offset: 0 }, "mat");
    a.op(Opcode::Field {
        dst: pass,
        obj: m,
        field: p.m_pass.0,
    });
    a.loop_head("pass");
    a.jmp(
        Opcode::JNull {
            reg: pass,
            offset: 0,
        },
        "mat",
    );
    a.op(Opcode::GetGlobal {
        dst: ccls,
        global: p.cset_cls,
    });
    a.op(Opcode::Call2 {
        dst: shd,
        fun: p.get_shader,
        arg0: pass,
        arg1: ccls,
    });
    a.jmp(Opcode::JNull { reg: shd, offset: 0 }, "next");
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.remove_shaders,
        arg0: pass,
        arg1: ccls,
    });
    a.op(Opcode::New { dst: cset });
    a.op(Opcode::Mov { dst: cc, src: ci });
    a.op(Opcode::Ref { dst: rcc, src: cc });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.cset_ctor,
        arg0: cset,
        arg1: rcc,
    });
    a.op(Opcode::Call2 {
        dst: shd,
        fun: p.add_shader,
        arg0: pass,
        arg1: cset,
    });
    a.label("next");
    a.op(Opcode::Field {
        dst: pass,
        obj: pass,
        field: p.p_next,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "pass");
    a.label("tinted");
    // blink state { o, u, c }; removed after DURATION, blinking until then
    a.op(Opcode::New { dst: stv });
    a.op(Opcode::DynSet {
        obj: stv,
        field: ko,
        src: ob,
    });
    a.op(Opcode::DynSet {
        obj: stv,
        field: ku,
        src: u,
    });
    a.op(Opcode::DynSet {
        obj: stv,
        field: kc,
        src: oc,
    });
    a.op(Opcode::Field {
        dst: ev,
        obj: game,
        field: p.g_event.0,
    });
    a.jmp(Opcode::JNotNull { reg: ev, offset: 0 }, "timed");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: ob,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("timed");
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
        obj: stv,
    });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.wait_until,
        arg0: ev,
        arg1: bl,
    });
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Call1 {
        dst: v,
        fun: sound,
        arg0: ctrl,
    });
    a.op(Opcode::Bool {
        dst: res,
        value: ValBool(true),
    });
    a.op(Opcode::Ret { ret: res });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("end");
    a.op(Opcode::Ret { ret: res });
    a.label("catch");
    a.op(Opcode::Ret { ret: res });
    push_fn(
        code,
        vec![p.ctrl_t, p.f64_, p.f64_, p.player_t],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

fn cell_apply(code: &mut Bytecode, p: &CellPlan) -> Result<()> {
    let sound = add_sound(code, p)?;
    let blink = add_blink(code, p)?;
    let cell = add_cell(code, p, blink, sound)?;
    let bool_ = p.bool_;
    let f = &mut code.functions[p.impl_fi];
    // vanilla ui.sfx("Ping") -> the throttled ping sound
    f.ops[p.sfx_at] = Opcode::Call1 {
        dst: p.ret_reg,
        fun: sound,
        arg0: Reg(0),
    };
    // if (pingCell(this, x, y, player)) return;
    f.regs.push(bool_);
    let hb = Reg((f.regs.len() - 1) as u32);
    insert_ops(
        f,
        0,
        vec![
            Opcode::Call4 {
                dst: hb,
                fun: cell,
                arg0: Reg(0),
                arg1: Reg(1),
                arg2: Reg(2),
                arg3: Reg(4),
            },
            Opcode::JFalse {
                cond: hb,
                offset: 1,
            },
            Opcode::Ret { ret: p.ret_reg },
        ],
    );
    eprintln!(
        "patched ping fn@{}: battle pings mark the cell / unit footprint in the player color \
         instead of the vanilla fx (pingCell fn@{}, blink fn@{}, sound fn@{})",
        f.findex.0, cell.0, blink.0, sound.0
    );
    Ok(())
}

/// Fixes the ping's depth sample and replaces the battle ping marker; each half
/// that does not validate is skipped and logged.
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

    /// Game.ping: 7 ops replaced + 1 inserted; ping__impl: 3 ops in front, the
    /// sfx call swapped; three well-typed functions appended; a second pass is a no-op.
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

        assert_eq!(back.functions.len(), orig.functions.len() + 3);
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
        let (sound, blink, cell) = (
            &back.functions[n - 3],
            &back.functions[n - 2],
            &back.functions[n - 1],
        );

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

        // ping__impl: the vanilla fx only runs when pingCell declined
        let (a, b) = (&orig.functions[cp.impl_fi], &back.functions[cp.impl_fi]);
        assert_eq!(b.regs.len(), a.regs.len() + 1);
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
        assert_eq!(b.regs[a.regs.len()], cp.bool_);
        let mut want = a.clone();
        want.ops[cp.sfx_at] = Opcode::Call1 {
            dst: cp.ret_reg,
            fun: sound.findex,
            arg0: Reg(0),
        };
        want.regs = b.regs.clone();
        shifted(&want, b, 0, 3);
        assert!(matches!(
            b.ops[0],
            Opcode::Call4 { fun, arg0: Reg(0), arg1: Reg(1), arg2: Reg(2), arg3: Reg(4), .. }
                if fun == cell.findex
        ));
        assert_eq!(jump_targets(b, 1), vec![3]);
        assert!(matches!(b.ops[2], Opcode::Ret { ret } if ret == cp.ret_reg));
        check_types(&back, b, 0..3);
        check_types(&back, b, cp.sfx_at + 3..cp.sfx_at + 4);
        check_flow(b);

        for f in [sound, blink, cell] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // pingCell asks for the unit there, tints ColorSet, and plays the sound
        // only on success.
        let calls = |f: &Function, g: RefFun| {
            f.ops
                .iter()
                .filter(|o| match o {
                    Opcode::Call1 { fun, .. }
                    | Opcode::Call2 { fun, .. }
                    | Opcode::Call3 { fun, .. }
                    | Opcode::Call4 { fun, .. }
                    | Opcode::CallN { fun, .. } => *fun == g,
                    _ => false,
                })
                .count()
        };
        assert_eq!(calls(cell, cp.unit_there), 1);
        assert_eq!(calls(cell, cp.get_shader), 1);
        assert_eq!(calls(cell, cp.remove_shaders), 1);
        assert_eq!(calls(cell, cp.cset_ctor), 1);
        assert_eq!(calls(cell, cp.add_shader), 1);
        assert_eq!(calls(cell, sound.findex), 1);
        assert_eq!(calls(blink, cp.set_outline), 3);
        assert_eq!(calls(sound, cp.sfx), 1);
        assert!(cell
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::CallMethod { field, args, .. } if *field == cp.socle_pindex && args.len() == 1)));

        let mut again = read(&patched);
        assert!(depth_plan(&again).is_err());
        assert!(cell_plan(&again).is_err());
        patch_ping_cell(&mut again);
        assert!(write(&again) == patched);
    }

    /// pingCell run in the interpreter, for pingers 1..4: each square's pass
    /// that holds the prefab's (shared) ColorSet gets it removed and its own
    /// new ColorSet(PlayerColor<n>) added; passes without one are untouched;
    /// the shared instance is never written.
    #[test]
    fn square_gets_own_color_per_pinger() {
        use crate::testsim::{Sim, V};
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let cp = cell_plan(&orig).expect("cell plan");
        let orig_n = orig.functions.len();
        let mut code = read(&image);
        patch_ping_cell(&mut code);
        let n = code.functions.len();
        let (sound, cell) = (code.functions[n - 3].findex, code.functions[n - 1].findex);
        let floor = cp.floor;
        // data.cdb PlayerColor1..4
        let colors = [0x85c1e3, 0xd3b25e, 0xafc073, 0xb5aade];
        for (k, want) in colors.into_iter().enumerate() {
            let pinger = k as i32 + 1;
            let p = &cp;
            let mut sim = Sim::new(
                &code,
                orig_n,
                move |c, f, a| {
                    let name = |s: &'static str, a: &[V], c: &mut crate::testsim::Core| {
                        c.log.push((s, a.to_vec()));
                    };
                    Some(if f == floor {
                        let V::F(x) = a[0] else { panic!("floor {a:?}") };
                        V::I(x.floor() as i32)
                    } else if f == p.get_color {
                        V::I(pinger)
                    } else if f == p.itos {
                        let V::I(x) = a[0] else { panic!("itos") };
                        V::S(x.to_string())
                    } else if f == p.alloc_str {
                        a[0].clone()
                    } else if f == p.add_str {
                        let (V::S(x), V::S(y)) = (&a[0], &a[1]) else { panic!("add {a:?}") };
                        V::S(format!("{x}{y}"))
                    } else if f == p.get_const {
                        let V::S(key) = &a[0] else { panic!("getConst") };
                        let i = key.strip_prefix("PlayerColor").unwrap().parse::<usize>().unwrap();
                        let cv = c.obj(&[(p.cv_color.0, V::I(colors[i - 1]))]);
                        name("getConst", a, c);
                        cv
                    } else if f == p.unit_there {
                        V::Null
                    } else if f == p.get_loader || f == p.load_cache {
                        c.obj(&[])
                    } else if f == p.add_socle {
                        // material: overlay pass (shared ColorSet) -> second pass (none)
                        let shared = c.map("t", "shared");
                        let p2 = c.obj(&[]);
                        let p1 = c.obj(&[(p.p_next, p2.clone())]);
                        c.key_set(&p1, "cs".into(), shared);
                        let m = c.obj(&[(p.m_pass.0, p1.clone())]);
                        let mats = c.arr(p.a_len, p.a_arr, vec![m]);
                        let o = c.obj(&[]);
                        c.put("t", "mats", mats);
                        c.put("t", "p1", p1);
                        c.put("t", "p2", p2);
                        o
                    } else if f == p.get_mats {
                        c.map("t", "mats")
                    } else if f == p.get_shader {
                        c.key_get(&a[0], "cs")
                    } else if f == p.remove_shaders {
                        name("removeShaders", a, c);
                        c.key_set(&a[0], "cs".into(), V::Null);
                        V::Null
                    } else if f == p.cset_ctor {
                        name("ColorSet", a, c);
                        c.key_set(&a[0], "color".into(), a[1].clone());
                        V::Null
                    } else if f == p.add_shader {
                        name("addShader", a, c);
                        c.key_set(&a[0], "cs".into(), a[1].clone());
                        a[1].clone()
                    } else if f == p.wait || f == p.wait_until || f == sound {
                        V::Null
                    } else {
                        return None;
                    })
                },
                |_, _, _| panic!("no virtual call expected"),
            );
            let shared = sim.c.obj(&[]);
            sim.c.put("t", "shared", shared.clone());
            let grid = sim.c.obj(&[
                (cp.gr_cell, V::F(1.0)),
                (cp.gr_w, V::I(10)),
                (cp.gr_h, V::I(10)),
            ]);
            let battle = sim.c.obj(&[(cp.b_grid.0, grid)]);
            let ev = sim.c.obj(&[]);
            let game = sim.c.obj(&[(cp.g_battle.0, battle), (cp.g_event.0, ev)]);
            let ctrl = sim.c.obj(&[(cp.c_game.0, game)]);
            let player = sim.c.obj(&[]);
            let r = sim.run(cell, vec![ctrl, V::F(2.5), V::F(3.5), player]);
            assert_eq!(r, V::B(true), "pinger {pinger}");

            let key = sim.c.take("getConst");
            assert_eq!(key, vec![vec![V::S(format!("PlayerColor{pinger}"))]]);
            let (p1, p2) = (sim.c.map("t", "p1"), sim.c.map("t", "p2"));
            assert_eq!(sim.c.take("removeShaders").len(), 1);
            let made = sim.c.take("ColorSet");
            assert_eq!(made.len(), 1, "pinger {pinger}");
            assert_eq!(made[0][1], V::I(want), "pinger {pinger}: color passed");
            let own = made[0][0].clone();
            assert_ne!(own, shared);
            assert_eq!(sim.c.take("addShader"), vec![vec![p1.clone(), own.clone()]]);
            assert_eq!(sim.c.key_get(&p1, "cs"), own);
            assert_eq!(sim.c.key_get(&p2, "cs"), V::Null);
            // the shared prefab instance is never written
            assert!(sim.c.heap[match shared {
                V::O(i) => i,
                _ => unreachable!(),
            }]
            .is_empty());
        }
    }

    /// The color source is the one the nickname uses, and the sound id exists.
    #[test]
    fn color_and_sound_sources() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let code = read(&image);
        let cp = cell_plan(&code).expect("cell plan");
        assert_eq!(any_name(&code, cp.get_color), "getColor");
        assert_eq!(any_name(&code, cp.get_const), "getConst");
        assert_eq!(any_name(&code, cp.set_outline), "setOutline");
        assert_eq!(any_name(&code, cp.unit_there), "getUnitThere");
        assert!(code.strings.iter().any(|s| s.as_str() == "PlayerColor1"));
        // the game itself plays SOUND (chat box), so the id resolves
        assert!(code.strings.iter().any(|s| s.as_str() == SOUND));
        // the socle size is battle.Unit's virtual getSocleSize
        let (f, pi) = vproto(&code, cp.unit_t, "getSocleSize").unwrap();
        assert_eq!(pi as usize, cp.socle_pindex.0);
        assert_eq!(any_name(&code, f), "getSocleSize");
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
        assert_eq!(code.functions.len(), orig.functions.len() + 3);

        // Cell: no sfx call before the final Ret.
        let mut code = read(&image);
        code.functions[cp.impl_fi].ops[cp.sfx_at] = Opcode::Nop;
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
