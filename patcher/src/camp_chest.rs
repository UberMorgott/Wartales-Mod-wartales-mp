// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The co-op shared camp chest exists in every co-op game.
//
// The inventory side panel's shared chest (GameInventory: campChestButton,
// #chestInventory) is the camp tool "Chest": GameInventory.update shows the
// button (and lets the panel open) only while `state.camp.getTool("Chest")` is
// not null. Vanilla creates that tool in two places only: the st.player.Camp
// constructor when the game is co-op at that moment (get_isCoopGame: options
// isCoopFriendly or >= 2 players), and Camp.enter on the host
// (`if (isCoopGame && !hasTool("Chest", true)) addTool(Data.item.byId.get("Chest"))`).
// A camp created while one player was in the game (solo save, players joining
// later) has no chest until the host makes camp: no chest button, the panel
// never opens.
//
// GameInventory.update (op 0) gets `mpCampChest(this)`: on the host of a co-op
// game whose camp lacks it, the Camp.enter block above (same calls, read from
// Camp.enter); addTool replicates the tool to the clients. Once the chest
// exists the call returns after hasTool. Runs under a trap (an exception is
// dropped, update continues).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs};
use super::*;
use hlbc::types::RefGlobal;

const N_FN: &str = "mpCampChest";

struct Plan {
    upd_fi: usize,
    gi_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
    game_t: RefType,
    state_t: RefType,
    camp_t: RefType,
    str_t: RefType,
    ref_bool_t: RefType,
    data_t: RefType,
    index_t: RefType,
    map_t: RefType,
    item_t: RefType,
    gi_game: RefField,
    game_auth: RefField,
    game_state: RefField,
    data_item: RefField,
    index_by_id: RefField,
    get_camp: RefFun,
    is_coop: RefFun,
    has_tool: RefFun,
    map_get: RefFun,
    add_tool: RefFun,
    chest_s: RefGlobal,
    data_g: RefGlobal,
    dbg_file: usize,
}

fn want_sig(code: &Bytecode, f: RefFun, what: &str, args: &[RefType], ret: RefType) -> Result<()> {
    if sig(code, f)? != (args.to_vec(), ret) {
        bail!("{what}: unexpected signature");
    }
    Ok(())
}

/// The function update's op 0 calls with `this` when it is ours (it calls addTool).
fn applied<'a>(code: &'a Bytecode, upd: &Function, add_tool: RefFun) -> Option<&'a Function> {
    let (_, f, arg) = call1(&upd.ops, 0)?;
    let callee = code.functions.iter().find(|x| x.findex == f)?;
    (arg == Reg(0)
        && callee
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == add_tool)))
    .then_some(callee)
}

/// `Call1 dst = fun(..)` of `ops[i]`, if it is one.
fn call1(ops: &[Opcode], i: usize) -> Option<(Reg, RefFun, Reg)> {
    match ops.get(i)? {
        Opcode::Call1 { dst, fun, arg0 } => Some((*dst, *fun, *arg0)),
        _ => None,
    }
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let gi_t = obj_type(code, "ui.comp.gameUIComp.GameInventory")?;
    let upd = method(code, gi_t, "update")?;
    let upd_fi = fun_index(code, upd.findex)?;
    if (0..upd.ops.len()).any(|i| jump_targets(upd, i).contains(&0)) {
        bail!("GameInventory.update: a jump targets op 0");
    }
    let void_t = upd.t.as_fun(code).context("update type")?.ret;
    if !matches!(code.types[void_t.0], Type::Void) {
        bail!("GameInventory.update does not return void");
    }
    let dbg_file = upd
        .debug_info
        .as_ref()
        .and_then(|d| d.first())
        .map(|d| d.0)
        .context("update: no debug info")?;

    let game_t = obj_type(code, "Game")?;
    let camp_t = obj_type(code, "st.player.Camp")?;
    let state_t = obj_type(code, "st.GameState")?;
    let gi_game = typed(code, gi_t, "game", game_t)?;
    let (game_auth, bool_t) = field(code, game_t, "isAuth")?;
    if !matches!(code.types[bool_t.0], Type::Bool) {
        bail!("Game.isAuth is not a Bool");
    }
    let game_state = typed(code, game_t, "state", state_t)?;
    let get_camp = method(code, state_t, "get_camp")?.findex;
    want_sig(code, get_camp, "GameState.get_camp", &[state_t], camp_t)?;
    // update reads the camp the same way (multi branch).
    if !upd
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == get_camp))
    {
        bail!("GameInventory.update: no get_camp");
    }

    // The Camp.enter block: hasTool("Chest", true) -> addTool(Data.item.byId.get("Chest")).
    let enter = method(code, camp_t, "enter")?;
    let ops = &enter.ops;
    let mut hit = None;
    for i in 0..(ops.len() + 1).saturating_sub(16) {
        let w = &ops[i..i + 16];
        if let (
            Some((_, is_coop, _)),
            Opcode::JFalse { .. },
            Opcode::GetGlobal { global: s1, .. },
            Opcode::Bool { .. },
            Opcode::Ref { dst: rref, .. },
            Opcode::Call3 {
                fun: has_tool,
                arg0,
                ..
            },
            Opcode::JTrue { .. },
            Opcode::GetGlobal {
                global: data_g,
                dst: rdata,
            },
            Opcode::Field {
                field: data_item,
                dst: ridx,
                ..
            },
            Opcode::NullCheck { .. },
            Opcode::Field {
                field: index_by_id,
                dst: rmap,
                ..
            },
            Opcode::NullCheck { .. },
            Opcode::GetGlobal { global: s2, .. },
            Opcode::Call2 {
                fun: map_get,
                dst: rdyn,
                ..
            },
            Opcode::ToVirtual { dst: ritem, .. },
            Opcode::Call2 {
                fun: add_tool,
                arg0: a2,
                arg1,
                ..
            },
        ) = (
            call1(ops, i),
            &w[1],
            &w[2],
            &w[3],
            &w[4],
            &w[5],
            &w[6],
            &w[7],
            &w[8],
            &w[9],
            &w[10],
            &w[11],
            &w[12],
            &w[13],
            &w[14],
            &w[15],
        ) {
            if s1 == s2 && *arg0 == Reg(0) && *a2 == Reg(0) && arg1 == ritem {
                hit = Some((
                    is_coop,
                    *s1,
                    *rref,
                    *has_tool,
                    *data_g,
                    *rdata,
                    *data_item,
                    *ridx,
                    *index_by_id,
                    *rmap,
                    *map_get,
                    *rdyn,
                    *ritem,
                    *add_tool,
                ));
                break;
            }
        }
    }
    let (
        is_coop,
        chest_s,
        rref,
        has_tool,
        data_g,
        rdata,
        data_item,
        ridx,
        index_by_id,
        rmap,
        map_get,
        rdyn,
        ritem,
        add_tool,
    ) = hit.context("Camp.enter: no hasTool(\"Chest\") -> addTool block")?;
    let reg_t = |r: Reg| enter.regs[r.0 as usize];
    want_sig(code, is_coop, "Game.get_isCoopGame", &[game_t], bool_t)?;
    let str_t = code.globals[chest_s.0];
    if obj(code, str_t).map(|o| s(code, o.name)).ok() != Some("String") {
        bail!("Camp.enter: \"Chest\" is not a String global");
    }
    if fname(code, has_tool) != "hasTool" || fname(code, add_tool) != "addTool" {
        bail!("Camp.enter: unexpected hasTool / addTool");
    }
    let ref_bool_t = reg_t(rref);
    let (data_t, index_t, map_t, dyn_t, item_t) = (
        reg_t(rdata),
        reg_t(ridx),
        reg_t(rmap),
        reg_t(rdyn),
        reg_t(ritem),
    );
    if code.globals[data_g.0] != data_t {
        bail!("Camp.enter: Data global type");
    }
    want_sig(code, add_tool, "Camp.addTool", &[camp_t, item_t], void_t)?;
    if applied(code, upd, add_tool).is_some() {
        bail!("already applied");
    }
    Ok(Plan {
        upd_fi,
        gi_t,
        void_t,
        bool_t,
        dyn_t,
        game_t,
        state_t,
        camp_t,
        str_t,
        ref_bool_t,
        data_t,
        index_t,
        map_t,
        item_t,
        gi_game,
        game_auth,
        game_state,
        data_item,
        index_by_id,
        get_camp,
        is_coop,
        has_tool,
        map_get,
        add_tool,
        chest_s,
        data_g,
        dbg_file,
    })
}

/// `mpCampChest(gi)`: the Camp.enter chest block for the host of a co-op game.
fn add_fn(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.gi_t]);
    let gi = Reg(0);
    let (v, exc, game, b, state, camp, st) = (
        r.r(p.void_t),
        r.r(p.dyn_t),
        r.r(p.game_t),
        r.r(p.bool_t),
        r.r(p.state_t),
        r.r(p.camp_t),
        r.r(p.str_t),
    );
    let (rb, data, idx, map, d, item) = (
        r.r(p.ref_bool_t),
        r.r(p.data_t),
        r.r(p.index_t),
        r.r(p.map_t),
        r.r(p.dyn_t),
        r.r(p.item_t),
    );
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::Field {
        dst: game,
        obj: gi,
        field: p.gi_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: game,
        field: p.game_auth,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_coop,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "out");
    a.op(Opcode::Field {
        dst: state,
        obj: game,
        field: p.game_state,
    });
    a.jmp(
        Opcode::JNull {
            reg: state,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call1 {
        dst: camp,
        fun: p.get_camp,
        arg0: state,
    });
    a.jmp(
        Opcode::JNull {
            reg: camp,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::GetGlobal {
        dst: st,
        global: p.chest_s,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: hlbc::types::ValBool(true),
    });
    a.op(Opcode::Ref { dst: rb, src: b });
    a.op(Opcode::Call3 {
        dst: b,
        fun: p.has_tool,
        arg0: camp,
        arg1: st,
        arg2: rb,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "out");
    a.op(Opcode::GetGlobal {
        dst: data,
        global: p.data_g,
    });
    a.jmp(
        Opcode::JNull {
            reg: data,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: idx,
        obj: data,
        field: p.data_item,
    });
    a.jmp(
        Opcode::JNull {
            reg: idx,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Field {
        dst: map,
        obj: idx,
        field: p.index_by_id,
    });
    a.jmp(
        Opcode::JNull {
            reg: map,
            offset: 0,
        },
        "out",
    );
    a.op(Opcode::Call2 {
        dst: d,
        fun: p.map_get,
        arg0: map,
        arg1: st,
    });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "out");
    a.op(Opcode::ToVirtual { dst: item, src: d });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.add_tool,
        arg0: camp,
        arg1: item,
    });
    a.label("out");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.gi_t], p.void_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let f = add_fn(code, &p)?;
    super::window_drag::name_fn(code, f, N_FN);
    let upd = &mut code.functions[p.upd_fi];
    let v = new_reg(upd, p.void_t);
    insert_ops(
        upd,
        0,
        vec![Opcode::Call1 {
            dst: v,
            fun: f,
            arg0: Reg(0),
        }],
    );
    eprintln!(
        "patched camp chest fn@{}: a co-op host's camp always has the shared chest",
        upd.findex.0
    );
    Ok(())
}

/// Gives a co-op host's camp the shared chest tool when it lacks it, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_camp_chest(code: &mut Bytecode) {
    let snap = super::asm::Snap::take(code);
    let r = plan(code).and_then(|p| apply(code, p));
    if let Err(e) = r {
        snap.restore(code);
        crate::skipped(format!("camp chest skipped: {e:#}"));
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
        let fi = p.upd_fi;
        let mut code = read(&image);
        let p2 = plan(&code).expect("plan");
        apply(&mut code, p2).expect("apply");
        let back = read(&write(&code));
        let f = applied(&back, &back.functions[fi], p.add_tool).expect("appended");
        check_types(&back, f, 0..f.ops.len());
        check_flow(f);
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 1);
        assert!(matches!(b.ops[0], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == f.findex));
        assert_eq!(format!("{:?}", &b.ops[1..]), format!("{:?}", a.ops));
        // The appended body calls what Camp.enter calls.
        for fun in [p.is_coop, p.get_camp, p.has_tool, p.map_get, p.add_tool] {
            assert!(
                f.ops.iter().any(|o| matches!(o,
                    Opcode::Call1 { fun: x, .. } | Opcode::Call2 { fun: x, .. } | Opcode::Call3 { fun: x, .. }
                    if *x == fun)),
                "calls fn@{}",
                fun.0
            );
        }
        // Idempotent: a second run changes nothing.
        let once = write(&code);
        let mut again = read(&once);
        assert!(plan(&again).is_err());
        patch_camp_chest(&mut again);
        assert!(write(&again) == once);
    }
}
