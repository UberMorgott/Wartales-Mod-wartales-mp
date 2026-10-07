// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Holding the "Outlines" binding (ALT) on the world map outlines the
// interactive elements on screen (chests, gather nodes, treasure, tracks,
// signs...), the way it already does inside places. Local render state only.
//
// Vanilla: `ent.p.Element.onOver` already picks the ALT highlight color
// (`isDown("Outlines")` -> PREFS.outlineHighlightColor), but its last test
// `if (!focus && !over && noOverNoOutline()) color = null` wipes it, and
// `Element.noOverNoOutline` (Element.hx:652) is true whenever
// `game.mode == game.world`. Places poll the binding (PlaceView.update:
// `if (isDown("Outlines") != altDown) { altDown = !altDown; for (e in elements)
// e.updateOutline(); }`); world.World has no such poll.
//
// A. noOverNoOutline: the `JEq mode, world -> true` test becomes
//      if (altWorldGate(mode, world)) return true;   // mode == world && !isDown("Outlines")
//    so with ALT held the world map falls through to the DialogOutView test
//    (false there). Places, ruins and DialogOutView stay vanilla.
// B. World.update, right after `updateCulling(false)`: `altWorldEdge(this)`:
//      if (game.mode != this) return;
//      k = isDown("Outlines"); if (k == altWorldDown) return; altWorldDown = k;
//      for (s in game.state.persistStates) if (s is Element && s.obj != null
//          && (!k || (s.obj.flags & 4) == 0)) s.updateOutline();
//    Press: on-screen (not culled) elements; release: every element, so one
//    outlined then scrolled away is cleared too. One walk per ALT edge.
// C. Element.onUpdateCulling: its `Ret` becomes `altWorldCull(this); Ret`:
//      if (game.mode == game.world && isDown("Outlines") && obj != null
//          && (obj.flags & 4) == 0) this.updateOutline();
//    so an element panning into view while ALT is held outlines (after the
//    vanilla lazy interactive init it needs).
//
// updateOutline is a no-op without `interact`; onOver keeps the other
// players' hover colors. No network traffic (setOutline is local, isDown local
// input). Places (POI markers) and roaming parties are not Elements: not covered.
//
// Whole pass or nothing; validated before editing; a mismatch skips the pass
// (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs};
use crate::job_xp::const_str;

use hlbc::types::{RefGlobal, ValBool};

struct Plan {
    // A. noOverNoOutline
    nono_fi: usize,
    /// The `JEq mode, world` op; its target is `Bool true; Ret`.
    nono_at: usize,
    // B. World.update
    upd_fi: usize,
    /// The `Call2 void = updateCulling(this, _)` op.
    upd_at: usize,
    // C. Element.onUpdateCulling
    cull_fi: usize,
    // types
    bool_: RefType,
    i32_: RefType,
    dyn_t: RefType,
    void_t: RefType,
    str_t: RefType,
    game_t: RefType,
    mode_t: RefType,
    world_t: RefType,
    elem_t: RefType,
    obj_t: RefType,
    arr_t: RefType,
    gs_t: RefType,
    // fields
    w_game: RefField,
    g_mode: RefField,
    g_world: RefField,
    g_state: RefField,
    s_persist: RefField,
    a_len: RefField,
    a_raw: (RefField, RefType),
    e_game: RefField,
    e_obj: RefField,
    o_flags: RefField,
    // calls
    is_down: RefFun,
    outlines: RefGlobal,
    base_check: RefFun,
    elem_cls: RefGlobal,
    elem_cls_t: RefType,
    update_outline: RefField,
    dbg_elem: usize,
    dbg_world: usize,
}

/// Virtual-table slot of method `name` on `t` or one of its ancestors.
fn proto_slot(code: &Bytecode, t: RefType, name: &str) -> Result<RefField> {
    let mut cur = Some(t);
    while let Some(ct) = cur {
        let o = obj(code, ct)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == name) {
            let i = usize::try_from(p.pindex).with_context(|| format!("{name} is not virtual"))?;
            return Ok(RefField(i));
        }
        cur = o.super_;
    }
    bail!("method {name} not found on type {}", t.0)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let str_t = obj_type(code, "String")?;
    let game_t = obj_type(code, "Game")?;
    let mode_t = obj_type(code, "GameMode")?;
    let world_t = obj_type(code, "world.World")?;
    let elem_t = obj_type(code, "ent.p.Element")?;
    let obj_t = obj_type(code, "h3d.scene.Object")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let gs_t = obj_type(code, "st.GameState")?;
    if !is_sub(code, world_t, mode_t) {
        bail!("world.World is not a GameMode");
    }

    let w_game = typed(code, world_t, "game", game_t)?;
    let g_mode = typed(code, game_t, "mode", mode_t)?;
    let g_world = typed(code, game_t, "world", world_t)?;
    let g_state = typed(code, game_t, "state", gs_t)?;
    let s_persist = typed(code, gs_t, "persistStates", arr_t)?;
    let a_len = typed(code, arr_t, "length", i32_)?;
    let a_raw = field(code, arr_t, "array")?;
    let e_game = typed(code, elem_t, "game", game_t)?;
    let e_obj = typed(code, elem_t, "obj", obj_t)?;
    let o_flags = typed(code, obj_t, "flags", i32_)?;

    let base_t = obj_type(code, "hl.BaseType")?;
    let check = method(code, base_t, "check")?;
    if fun_args(code, check).len() != 2 || check.t.as_fun(code).map(|f| f.ret) != Some(bool_) {
        bail!("hl.BaseType.check is not (BaseType, v) -> Bool");
    }
    let base_check = check.findex;
    let (elem_cls, elem_cls_t) = class_global(code, "ent.p.Element")?;
    let update_outline = proto_slot(code, elem_t, "updateOutline")?;

    // isDown("Outlines") as Element.onOver reads it.
    let on_over = method(code, elem_t, "onOver")?;
    let o = &on_over.ops;
    let reads: Vec<(RefFun, RefGlobal)> = (0..o.len().saturating_sub(1))
        .filter_map(|i| match (&o[i], &o[i + 1]) {
            (Opcode::GetGlobal { dst, global }, Opcode::Call1 { fun, arg0, .. })
                if arg0 == dst
                    && const_str(code, *global) == Some("Outlines")
                    && fname(code, *fun) == "isDown" =>
            {
                Some((*fun, *global))
            }
            _ => None,
        })
        .collect();
    let [(is_down, outlines)] = reads[..] else {
        bail!(
            "Element.onOver: expected one isDown(\"Outlines\"), found {}",
            reads.len()
        );
    };
    if sig(code, is_down)? != (vec![str_t], bool_) || code.globals[outlines.0] != str_t {
        bail!("isDown is not (String) -> Bool");
    }

    // A. noOverNoOutline: `GetThis g = game; NullCheck g; Field m = g.mode;
    //    GetThis g = game; NullCheck g; Field w = g.world; JEq m, w -> T`,
    //    T = `Bool r = true; Ret r`.
    let nono = method(code, elem_t, "noOverNoOutline")?;
    let nono_fi = fun_index(code, nono.findex)?;
    let o = &nono.ops;
    let sites: Vec<usize> = (6..o.len())
        .filter(|&i| {
            let Opcode::JEq { a, b, offset } = o[i] else {
                return false;
            };
            let t = (i as i64 + 1 + offset as i64) as usize;
            let shape = matches!(
                (&o[i - 6], &o[i - 4], &o[i - 3], &o[i - 1]),
                (
                    Opcode::GetThis { field: f1, .. },
                    Opcode::Field { dst: m, field: fm, .. },
                    Opcode::GetThis { field: f2, .. },
                    Opcode::Field { dst: w, field: fw, .. },
                ) if *f1 == e_game && *f2 == e_game && *fm == g_mode && *fw == g_world
                    && *m == a && *w == b
            );
            shape
                && matches!(
                    (o.get(t), o.get(t + 1)),
                    (
                        Some(Opcode::Bool { dst, value: ValBool(true) }),
                        Some(Opcode::Ret { ret })
                    ) if dst == ret
                )
        })
        .collect();
    let [nono_at] = sites[..] else {
        bail!(
            "noOverNoOutline: expected one `game.mode == game.world -> true`, found {}",
            sites.len()
        );
    };
    if nono.regs[1] != bool_ {
        bail!("noOverNoOutline: reg1 is not a bool");
    }
    let lands_on =
        |f: &Function, at: usize| (0..f.ops.len()).any(|j| jump_targets(f, j).contains(&at));
    if lands_on(nono, nono_at + 1) {
        bail!("noOverNoOutline: a jump lands after the world test");
    }

    // B. World.update: one `updateCulling(this, _)` followed by `updateUI(this, dt)`.
    let upd = method(code, world_t, "update")?;
    let upd_fi = fun_index(code, upd.findex)?;
    let cull_fn = method(code, world_t, "updateCulling")?.findex;
    let o = &upd.ops;
    let sites: Vec<usize> = (0..o.len().saturating_sub(1))
        .filter(|&i| {
            matches!(&o[i], Opcode::Call2 { fun, arg0: Reg(0), dst, .. }
                if *fun == cull_fn && code.types[upd.regs[dst.0 as usize].0] == Type::Void)
                && matches!(&o[i + 1], Opcode::Call2 { fun, arg0: Reg(0), .. }
                    if fname(code, *fun) == "updateUI")
        })
        .collect();
    let [upd_at] = sites[..] else {
        bail!(
            "World.update: expected one `updateCulling; updateUI`, found {}",
            sites.len()
        );
    };
    if lands_on(upd, upd_at + 1) {
        bail!("World.update: a jump lands between updateCulling and updateUI");
    }

    // C. Element.onUpdateCulling: ends `GetThis i = interact; JNotNull i -> Ret;
    //    CallThis (lazy init); Ret`, every jump to the end lands on that Ret.
    let cull = method(code, elem_t, "onUpdateCulling")?;
    let cull_fi = fun_index(code, cull.findex)?;
    let interact = field(code, elem_t, "interact")?.0;
    let o = &cull.ops;
    let n = o.len();
    if n < 4
        || !matches!(o[n - 1], Opcode::Ret { .. })
        || !matches!(&o[n - 2], Opcode::CallThis { args, .. } if args.is_empty())
        || !matches!(o[n - 3], Opcode::JNotNull { offset: 1, .. })
        || !matches!(o[n - 4], Opcode::GetThis { field, .. } if field == interact)
    {
        bail!("Element.onUpdateCulling: unexpected tail");
    }
    if o[..n - 1].iter().any(|x| matches!(x, Opcode::Ret { .. })) {
        bail!("Element.onUpdateCulling: more than one Ret");
    }
    let Opcode::Ret { ret } = o[n - 1] else {
        unreachable!()
    };
    if code.types[cull.regs[ret.0 as usize].0] != Type::Void {
        bail!("Element.onUpdateCulling: does not return void");
    }

    Ok(Plan {
        nono_fi,
        nono_at,
        upd_fi,
        upd_at,
        cull_fi,
        bool_,
        i32_,
        dyn_t,
        void_t,
        str_t,
        game_t,
        mode_t,
        world_t,
        elem_t,
        obj_t,
        arr_t,
        gs_t,
        w_game,
        g_mode,
        g_world,
        g_state,
        s_persist,
        a_len,
        a_raw,
        e_game,
        e_obj,
        o_flags,
        is_down,
        outlines,
        base_check,
        elem_cls,
        elem_cls_t,
        update_outline,
        dbg_elem: debug_file(code, "src/ent/p/Element.hx")?,
        dbg_world: debug_file(code, "src/world/World.hx")?,
    })
}

/// `altWorldGate(mode, world) -> Bool`: mode == world && !isDown("Outlines").
fn add_gate(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.mode_t, p.world_t]);
    let (mode, world) = (Reg(0), Reg(1));
    let (b, st) = (r.r(p.bool_), r.r(p.str_t));
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNotEq {
            a: mode,
            b: world,
            offset: 0,
        },
        "no",
    );
    a.op(Opcode::GetGlobal {
        dst: st,
        global: p.outlines,
    });
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_down,
        arg0: st,
    });
    a.op(Opcode::Not { dst: b, src: b });
    a.op(Opcode::Ret { ret: b });
    a.label("no");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    push_fn(
        code,
        vec![p.mode_t, p.world_t],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg_elem,
    )
}

/// `altWorldEdge(world)`: ALT press / release outline pass (see the header).
fn add_edge(code: &mut Bytecode, p: &Plan, down: RefGlobal) -> Result<RefFun> {
    let (i0, i4) = (int_const(code, 0), int_const(code, 4));
    let mut r = Regs(vec![p.world_t]);
    let world = Reg(0);
    let (game, mode, k, prev, st, gs, arr, n, i, raw, d, cls, b, e, ob, fl, z, v) = (
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.str_t),
        r.r(p.gs_t),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.a_raw.1),
        r.r(p.dyn_t),
        r.r(p.elem_cls_t),
        r.r(p.bool_),
        r.r(p.elem_t),
        r.r(p.obj_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.void_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: game,
        obj: world,
        field: p.w_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.jmp(
        Opcode::JNotEq {
            a: mode,
            b: world,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: st,
        global: p.outlines,
    });
    a.op(Opcode::Call1 {
        dst: k,
        fun: p.is_down,
        arg0: st,
    });
    a.op(Opcode::GetGlobal {
        dst: prev,
        global: down,
    });
    a.jmp(
        Opcode::JEq {
            a: k,
            b: prev,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::SetGlobal {
        global: down,
        src: k,
    });
    a.op(Opcode::Field {
        dst: gs,
        obj: game,
        field: p.g_state,
    });
    a.jmp(Opcode::JNull { reg: gs, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: arr,
        obj: gs,
        field: p.s_persist,
    });
    a.jmp(
        Opcode::JNull {
            reg: arr,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: arr,
        field: p.a_len,
    });
    a.op(Opcode::Int { dst: i, ptr: i0 });
    a.loop_head("next");
    a.jmp(
        Opcode::JSGte {
            a: i,
            b: n,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: arr,
        field: p.a_raw.0,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: i,
    });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "next");
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.elem_cls,
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.base_check,
        arg0: cls,
        arg1: d,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "next");
    a.op(Opcode::UnsafeCast { dst: e, src: d });
    a.op(Opcode::Field {
        dst: ob,
        obj: e,
        field: p.e_obj,
    });
    a.jmp(Opcode::JNull { reg: ob, offset: 0 }, "next");
    // Release: every element; press: only the ones on screen.
    a.jmp(Opcode::JFalse { cond: k, offset: 0 }, "update");
    a.op(Opcode::Field {
        dst: fl,
        obj: ob,
        field: p.o_flags,
    });
    a.op(Opcode::Int { dst: z, ptr: i4 });
    a.op(Opcode::And {
        dst: fl,
        a: fl,
        b: z,
    });
    a.op(Opcode::Int { dst: z, ptr: i0 });
    a.jmp(
        Opcode::JNotEq {
            a: fl,
            b: z,
            offset: 0,
        },
        "next",
    );
    a.label("update");
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.update_outline,
        args: vec![e],
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "next");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.world_t],
        p.void_t,
        r.0,
        a.finish(),
        p.dbg_world,
    )
}

/// `altWorldCull(element)`: outline an element that comes on screen while ALT
/// is held on the world map (see the header).
fn add_cull(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let (i0, i4) = (int_const(code, 0), int_const(code, 4));
    let mut r = Regs(vec![p.elem_t]);
    let e = Reg(0);
    let (game, mode, world, st, k, ob, fl, z, v) = (
        r.r(p.game_t),
        r.r(p.mode_t),
        r.r(p.world_t),
        r.r(p.str_t),
        r.r(p.bool_),
        r.r(p.obj_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.void_t),
    );
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: game,
        obj: e,
        field: p.e_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::Field {
        dst: mode,
        obj: game,
        field: p.g_mode,
    });
    a.op(Opcode::Field {
        dst: world,
        obj: game,
        field: p.g_world,
    });
    a.jmp(
        Opcode::JNotEq {
            a: mode,
            b: world,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::GetGlobal {
        dst: st,
        global: p.outlines,
    });
    a.op(Opcode::Call1 {
        dst: k,
        fun: p.is_down,
        arg0: st,
    });
    a.jmp(Opcode::JFalse { cond: k, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: ob,
        obj: e,
        field: p.e_obj,
    });
    a.jmp(Opcode::JNull { reg: ob, offset: 0 }, "end");
    a.op(Opcode::Field {
        dst: fl,
        obj: ob,
        field: p.o_flags,
    });
    a.op(Opcode::Int { dst: z, ptr: i4 });
    a.op(Opcode::And {
        dst: fl,
        a: fl,
        b: z,
    });
    a.op(Opcode::Int { dst: z, ptr: i0 });
    a.jmp(
        Opcode::JNotEq {
            a: fl,
            b: z,
            offset: 0,
        },
        "end",
    );
    a.op(Opcode::CallMethod {
        dst: v,
        field: p.update_outline,
        args: vec![e],
    });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.elem_t], p.void_t, r.0, a.finish(), p.dbg_elem)
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    code.globals.push(p.bool_);
    let down = RefGlobal(code.globals.len() - 1);
    let gate = add_gate(code, p)?;
    let edge = add_edge(code, p, down)?;
    let cull = add_cull(code, p)?;

    // A. `JEq mode, world -> T` -> `Call2 r1 = altWorldGate(mode, world); JTrue r1 -> T`.
    let f = &mut code.functions[p.nono_fi];
    let Opcode::JEq { a, b, offset } = f.ops[p.nono_at] else {
        unreachable!()
    };
    f.ops[p.nono_at] = Opcode::Call2 {
        dst: Reg(1),
        fun: gate,
        arg0: a,
        arg1: b,
    };
    // Target T moves by one; the new JTrue sits at nono_at + 1.
    insert_ops(
        f,
        p.nono_at + 1,
        vec![Opcode::JTrue {
            cond: Reg(1),
            offset,
        }],
    );
    let nono = f.findex.0;

    // B. after `updateCulling(this, false)`: `altWorldEdge(this)`.
    let f = &mut code.functions[p.upd_fi];
    let Opcode::Call2 { dst, .. } = f.ops[p.upd_at] else {
        unreachable!()
    };
    insert_ops(
        f,
        p.upd_at + 1,
        vec![Opcode::Call1 {
            dst,
            fun: edge,
            arg0: Reg(0),
        }],
    );
    let upd = f.findex.0;

    // C. `Ret r` -> `Call1 r = altWorldCull(this); Ret r` (every jump to the end
    //    now runs the call).
    let f = &mut code.functions[p.cull_fi];
    let n = f.ops.len();
    let Opcode::Ret { ret } = f.ops[n - 1] else {
        unreachable!()
    };
    f.ops[n - 1] = Opcode::Call1 {
        dst: ret,
        fun: cull,
        arg0: Reg(0),
    };
    f.ops.push(Opcode::Ret { ret });
    if let Some(dbg) = &mut f.debug_info {
        let last = dbg[n - 1];
        dbg.push(last);
    }
    eprintln!(
        "patched alt world: ALT outlines on-screen world map elements (noOverNoOutline fn@{nono} -> fn@{}, World.update fn@{upd} -> fn@{}, onUpdateCulling fn@{} -> fn@{})",
        gate.0, edge.0, f.findex.0, cull.0
    );
    Ok(())
}

/// Holding ALT ("Outlines") on the world map outlines the on-screen elements,
/// or leaves `code` untouched and logs why.
pub(crate) fn patch_alt_world(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, &p));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("alt world skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;
    use crate::testsim::{Core, Sim, V};

    struct Fns {
        gate: RefFun,
        edge: RefFun,
        cull: RefFun,
        down: RefGlobal,
    }

    fn patched(image: &[u8]) -> (Bytecode, Plan, Bytecode, Fns) {
        let orig = read(image);
        let p = plan(&orig).expect("plan");
        let mut code = read(image);
        patch_alt_world(&mut code);
        let back = read(&write(&code));
        let Opcode::Call2 { fun: gate, .. } = back.functions[p.nono_fi].ops[p.nono_at] else {
            panic!("gate call");
        };
        let Opcode::Call1 { fun: edge, .. } = back.functions[p.upd_fi].ops[p.upd_at + 1] else {
            panic!("edge call");
        };
        let cf = &back.functions[p.cull_fi];
        let Opcode::Call1 { fun: cull, .. } = cf.ops[cf.ops.len() - 2] else {
            panic!("cull call");
        };
        let down = RefGlobal(orig.globals.len());
        (
            orig,
            p,
            back,
            Fns {
                gate,
                edge,
                cull,
                down,
            },
        )
    }

    /// Only the three sites change; three well-typed functions and one bool
    /// global are appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let (orig, p, back, fns) = patched(&image);
        assert_eq!(back.functions.len(), orig.functions.len() + 3);
        assert_eq!(back.globals.len(), orig.globals.len() + 1);
        assert_eq!(back.globals[fns.down.0], p.bool_);
        let sites = [p.nono_fi, p.upd_fi, p.cull_fi];
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                !sites.contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // A: op replaced by the gate call, JTrue to the same `Bool true; Ret`.
        let (a, b) = (&orig.functions[p.nono_fi], &back.functions[p.nono_fi]);
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        let t = jump_targets(a, p.nono_at)[0];
        assert_eq!(jump_targets(b, p.nono_at + 1), vec![t + 1]);
        assert!(matches!(
            b.ops[p.nono_at + 1],
            Opcode::JTrue { cond: Reg(1), .. }
        ));
        for i in (0..a.ops.len()).filter(|&i| i != p.nono_at) {
            let m = |x: usize| if x <= p.nono_at { x } else { x + 1 };
            let tb: Vec<usize> = jump_targets(a, i).into_iter().map(m).collect();
            assert_eq!(jump_targets(b, m(i)), tb, "noOverNoOutline op {i}");
            if tb.is_empty() {
                assert_eq!(format!("{:?}", b.ops[m(i)]), format!("{:?}", a.ops[i]));
            }
        }
        check_types(&back, b, p.nono_at..p.nono_at + 2);

        // B: one call inserted after updateCulling.
        let (a, b) = (&orig.functions[p.upd_fi], &back.functions[p.upd_fi]);
        shifted(a, b, p.upd_at + 1, 1);
        check_types(&back, b, p.upd_at + 1..p.upd_at + 2);

        // C: `Ret` -> `Call1; Ret`, every other op as it was.
        let (a, b) = (&orig.functions[p.cull_fi], &back.functions[p.cull_fi]);
        let n = a.ops.len();
        assert_eq!(b.regs, a.regs);
        assert_eq!(b.ops.len(), n + 1);
        assert_eq!(
            format!("{:?}", &b.ops[..n - 1]),
            format!("{:?}", &a.ops[..n - 1])
        );
        assert_eq!(format!("{:?}", b.ops[n]), format!("{:?}", a.ops[n - 1]));
        check_types(&back, b, n - 1..n + 1);
        check_flow(b);

        for f in [fns.gate, fns.edge, fns.cull] {
            let nf = &back.functions[crate::fun_index(&back, f).unwrap()];
            check_types(&back, nf, 0..nf.ops.len());
            check_flow(nf);
        }

        // The outline slot is the one PlaceView's own ALT loop calls.
        let pv = method(&orig, obj_type(&orig, "world.PlaceView").unwrap(), "update").unwrap();
        assert!(pv
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::CallMethod { field, args, .. }
            if *field == p.update_outline && args.len() == 1)));

        let patched_img = write(&back);
        let mut again = read(&patched_img);
        assert!(plan(&again).is_err());
        patch_alt_world(&mut again);
        assert!(write(&again) == patched_img);
    }

    /// Any of the three sites in an unexpected shape: refused, image unchanged.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");
        let breaks: [(usize, usize); 3] = [
            (p.nono_fi, p.nono_at),
            (p.upd_fi, p.upd_at + 1),
            (p.cull_fi, read(&image).functions[p.cull_fi].ops.len() - 2),
        ];
        for (fi, at) in breaks {
            let mut code = read(&image);
            code.functions[fi].ops[at] = Opcode::Label;
            let before = write(&code);
            assert!(plan(&code).is_err(), "fn #{fi} op {at}");
            patch_alt_world(&mut code);
            assert!(write(&code) == before, "fn #{fi} op {at}");
        }
    }

    const ELEM: &str = "elem";

    fn sim<'a>(code: &'a Bytecode, n: usize, p: &'a Plan, fns: &Fns) -> Sim<'a> {
        let (is_down, check, slot) = (p.is_down, p.base_check, p.update_outline);
        let mut s = Sim::new(
            code,
            n,
            move |c: &mut Core, f: RefFun, a: &[V]| {
                if f == is_down {
                    assert_eq!(a[0], V::S("Outlines".into()));
                    Some(V::B(c.map("in", "alt") == V::B(true)))
                } else if f == check {
                    Some(V::B(c.key_get(&a[1], ELEM) == V::B(true)))
                } else {
                    None
                }
            },
            move |c: &mut Core, field: usize, a: &[V]| {
                assert_eq!(field, slot.0);
                c.log.push(("update", a.to_vec()));
                V::Null
            },
        );
        s.c.globals.insert(fns.down.0, V::B(false));
        s
    }

    struct World {
        world: V,
        game: V,
        on: V,
        off: V,
        no_obj: V,
    }

    fn world(s: &mut Sim, p: &Plan) -> World {
        let c = &mut s.c;
        let world = c.obj(&[]);
        let game = c.obj(&[(p.g_mode, world.clone()), (p.g_world, world.clone())]);
        c.set(&world, p.w_game, game.clone());
        let elem = |c: &mut Core, flags: Option<i32>| {
            let ob = match flags {
                Some(f) => c.obj(&[(p.o_flags, V::I(f))]),
                None => V::Null,
            };
            let e = c.obj(&[(p.e_obj, ob), (p.e_game, game.clone())]);
            c.key_set(&e, ELEM.into(), V::B(true));
            e
        };
        let on = elem(c, Some(1));
        let off = elem(c, Some(4 | 1));
        let no_obj = elem(c, None);
        let other = c.obj(&[]);
        let arr = c.arr(
            p.a_len,
            p.a_raw.0,
            vec![other, on.clone(), V::Null, off.clone(), no_obj.clone()],
        );
        let gs = c.obj(&[(p.s_persist, arr)]);
        c.set(&game, p.g_state, gs);
        World {
            world,
            game,
            on,
            off,
            no_obj,
        }
    }

    fn alt(s: &mut Sim, down: bool) {
        s.c.put("in", "alt", V::B(down));
    }

    /// The world-map gate: true (no outline) unless ALT is held; other modes false.
    #[test]
    fn gate_follows_alt_on_world_only() {
        let Some(image) = game() else { return };
        let (orig, p, back, fns) = patched(&image);
        let mut s = sim(&back, orig.functions.len(), &p, &fns);
        let w = world(&mut s, &p);
        let other_mode = s.c.obj(&[]);
        alt(&mut s, false);
        assert_eq!(
            s.run(fns.gate, vec![w.world.clone(), w.world.clone()]),
            V::B(true)
        );
        alt(&mut s, true);
        assert_eq!(
            s.run(fns.gate, vec![w.world.clone(), w.world.clone()]),
            V::B(false)
        );
        assert_eq!(
            s.run(fns.gate, vec![other_mode.clone(), w.world.clone()]),
            V::B(false)
        );
        alt(&mut s, false);
        assert_eq!(s.run(fns.gate, vec![other_mode, w.world]), V::B(false));
    }

    /// Press: on-screen elements only; hold: nothing; release: every element
    /// with an object; not the world mode: nothing, state kept.
    #[test]
    fn edge_updates_on_press_and_release() {
        let Some(image) = game() else { return };
        let (orig, p, back, fns) = patched(&image);
        let mut s = sim(&back, orig.functions.len(), &p, &fns);
        let w = world(&mut s, &p);
        let frame = |s: &mut Sim| {
            s.run(fns.edge, vec![w.world.clone()]);
            s.c.take("update")
                .into_iter()
                .map(|a| a[0].clone())
                .collect::<Vec<V>>()
        };
        alt(&mut s, false);
        assert!(frame(&mut s).is_empty(), "idle");
        alt(&mut s, true);
        assert_eq!(frame(&mut s), vec![w.on.clone()], "press");
        assert_eq!(s.c.globals[&fns.down.0], V::B(true));
        assert!(frame(&mut s).is_empty(), "held");
        alt(&mut s, false);
        assert_eq!(frame(&mut s), vec![w.on.clone(), w.off.clone()], "release");
        assert!(frame(&mut s).is_empty(), "released");
        let _ = &w.no_obj;

        // In a place (mode != world): no pass, edge state untouched.
        let place = s.c.obj(&[]);
        s.c.set(&w.game, p.g_mode, place);
        alt(&mut s, true);
        assert!(frame(&mut s).is_empty(), "place");
        assert_eq!(s.c.globals[&fns.down.0], V::B(false));
    }

    /// An element coming on screen while ALT is held on the world map updates
    /// its outline; culled, ALT up or another mode: nothing.
    #[test]
    fn cull_updates_on_screen_while_held() {
        let Some(image) = game() else { return };
        let (orig, p, back, fns) = patched(&image);
        let mut s = sim(&back, orig.functions.len(), &p, &fns);
        let w = world(&mut s, &p);
        let hit = |s: &mut Sim, e: &V| {
            s.run(fns.cull, vec![e.clone()]);
            s.c.take("update").len()
        };
        alt(&mut s, true);
        assert_eq!(hit(&mut s, &w.on), 1);
        assert_eq!(hit(&mut s, &w.off), 0);
        assert_eq!(hit(&mut s, &w.no_obj), 0);
        alt(&mut s, false);
        assert_eq!(hit(&mut s, &w.on), 0);
        alt(&mut s, true);
        let place = s.c.obj(&[]);
        s.c.set(&w.game, p.g_mode, place);
        assert_eq!(hit(&mut s, &w.on), 0);
    }
}
