// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a camp talk (confession, personal quest reminder) no longer starts
// while another player is busy with something, such as the banner editor.
//
// Vanilla gates every action that starts a shared dialog or mode switch on
// `Game.anyPlayerLocked(null)` (isMulti && some connected player has a
// `lockedWith`): Npc.talk, GameUI.toggleCamp, changeCampmode, Chest.action,
// PlaceView.tryClose, Tavern.askLeave, TavernCustomer.doUnitAction and the camp's
// own unit click, CampEntryEntity.doUnitAction (CampEntryEntity.hx:1343):
//
//   if (game.anyPlayerLocked(null)) return false;
//   ... confession.hasPending(u) -> game.ctrl.playCampConfession(u) ...
//
// Two other camp entry points skip it: the reserve list of the camp window
// (CampMode.hx, the unit thumbnail's click) and the party portraits
// (GameUI.hx); both call `ctrl.playCampConfession(u)` or
// `ctrl.playPersonalQuestReminder(u)` directly, and the host's
// `playCampConfession__impl` / `playPersonalQuestReminder__impl` check nothing:
// both go straight to `playConfessionSetup` -> FakeNpc.enterPlace -> the
// shared DialogOut, i.e. a mode switch for every player. A camp tool window
// (banner editor ui.win.BannerCamp, chest, craft...) is opened by
// CampEntryEntity.onAction with `me.lockedWith = tool` (CampEntryEntity.hx:1332)
// until it closes, so in co-op a click on a reserve unit started the
// confession switch while another player sat in the banner editor; that
// player's client never answered the switch and every screen stayed black
// until the barrier timed out and sent it a rejoin. Single player cannot get
// there (the banner editor is modal).
//
// The fix: both impls (they run on the host, whoever clicked) get the vanilla
// gate in front, plus a log line:
//
//   if (this.game.anyPlayerLocked(null)) { Sys.println(REFUSED); return; }
//
// i.e. the reserve list and portraits refuse exactly when the camp unit click
// refuses (silently, as vanilla does; the player clicks again once the other
// one is done). The coop_gates G6 change applies here as everywhere: an open
// window alone is no lock, an entity lock (`lockedWith`) is.
//
// Eight ops inserted at op 0 of each impl; nothing else changes. Validated
// before editing; a mismatch skips the pass (logged).

use super::*;
use crate::diag::static_fn;
use crate::job_xp::str_global;

/// Printed (shim.log "game:" line) when the gate refuses a camp talk.
const REFUSED: &str = "mp: camp talk refused: a player is busy (lockedWith)";
/// The host impls of the camp talk RPCs that start `playConfessionSetup`.
const IMPLS: [&str; 2] = [
    "playCampConfession__impl",
    "playPersonalQuestReminder__impl",
];
/// Ops of the inserted gate.
const GATE_OPS: usize = 8;

struct Plan {
    /// Function indices of the IMPLS, in order.
    fis: Vec<usize>,
    game_f: RefField,
    game_t: RefType,
    any_locked: RefFun,
    ref_bool_t: RefType,
    bool_t: RefType,
    void_t: RefType,
    str_t: RefType,
    println: RefFun,
}

fn calls(f: &Function, target: RefFun) -> bool {
    f.ops.iter().any(|o| {
        matches!(o,
            Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. }
            | Opcode::Call3 { fun, .. } | Opcode::Call4 { fun, .. } | Opcode::CallN { fun, .. }
            if *fun == target)
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let str_t = obj_type(code, "String")?;
    let ctrl_t = obj_type(code, "st.Controller")?;
    let game_t = obj_type(code, "Game")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let (game_f, gf_t) = field(code, ctrl_t, "game")?;
    if gf_t != game_t {
        bail!("st.Controller.game is not a Game");
    }

    // Game.anyPlayerLocked(game, ref<bool>) -> Bool, the vanilla gate.
    let al = method(code, game_t, "anyPlayerLocked")?;
    let any_locked = al.findex;
    let al_t =
        al.t.as_fun(code)
            .context("anyPlayerLocked: not a function")?;
    let [a0, ref_bool_t] = al_t.args[..] else {
        bail!("anyPlayerLocked does not take (Game, ref<bool>)");
    };
    if a0 != game_t
        || al_t.ret != bool_t
        || !matches!(code.types[ref_bool_t.0], Type::Ref(t) if t == bool_t)
    {
        bail!("unexpected anyPlayerLocked signature");
    }
    // The model: the camp unit click refuses on that gate before any talk.
    let entry_t = obj_type(code, "world.camp.CampEntryEntity")?;
    let dua = method(code, entry_t, "doUnitAction")?;
    if !matches!(dua.ops.get(3), Some(Opcode::Call2 { fun, .. }) if *fun == any_locked) {
        bail!("CampEntryEntity.doUnitAction does not start with the anyPlayerLocked gate");
    }

    let setup = method(code, ctrl_t, "playConfessionSetup")?.findex;
    let mut fis = Vec::new();
    for name in IMPLS {
        let f = method(code, ctrl_t, name)?;
        if fun_args(code, f) != [ctrl_t, unit_t] || f.t.as_fun(code).map(|t| t.ret) != Some(void_t)
        {
            bail!("unexpected {name} signature");
        }
        if !calls(f, setup) {
            bail!("{name} does not call playConfessionSetup");
        }
        if calls(f, any_locked) {
            bail!("{name} already tests anyPlayerLocked");
        }
        if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
            bail!("{name}: op 0 is a jump target");
        }
        fis.push(fun_index(code, f.findex)?);
    }

    let println = static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }
    Ok(Plan {
        fis,
        game_f,
        game_t,
        any_locked,
        ref_bool_t,
        bool_t,
        void_t,
        str_t,
        println: println.findex,
    })
}

fn apply(code: &mut Bytecode, p: &Plan) {
    let msg = str_global(code, p.str_t, REFUSED);
    for (&fi, name) in p.fis.iter().zip(IMPLS) {
        let f = &mut code.functions[fi];
        let mut reg = |t: RefType| {
            f.regs.push(t);
            Reg((f.regs.len() - 1) as u32)
        };
        let (g, n, b, s, v) = (
            reg(p.game_t),
            reg(p.ref_bool_t),
            reg(p.bool_t),
            reg(p.str_t),
            reg(p.void_t),
        );
        let gate = vec![
            Opcode::GetThis {
                dst: g,
                field: p.game_f,
            },
            Opcode::NullCheck { reg: g },
            Opcode::Null { dst: n },
            Opcode::Call2 {
                dst: b,
                fun: p.any_locked,
                arg0: g,
                arg1: n,
            },
            Opcode::JFalse { cond: b, offset: 3 },
            Opcode::GetGlobal {
                dst: s,
                global: msg,
            },
            Opcode::Call1 {
                dst: v,
                fun: p.println,
                arg0: s,
            },
            Opcode::Ret { ret: v },
        ];
        debug_assert_eq!(gate.len(), GATE_OPS);
        insert_ops(f, 0, gate);
        eprintln!(
            "patched camp talk gate fn@{} ({}): refused while a co-op player is lockedWith",
            f.findex.0, name
        );
    }
}

/// Gives the camp reserve / portrait talk the vanilla co-op lock gate, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_camp_talk(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => {
            snap.restore(code);
            eprintln!("camp talk skipped: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// Only the two impls change: the gate is inserted at op 0, every original
    /// op and jump is kept, the types check; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let fis = plan(&orig).expect("plan").fis;
        assert_eq!(fis.len(), IMPLS.len());
        let mut code = read(&image);
        patch_camp_talk(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, !fis.contains(&i), "function #{i} (fn@{})", a.findex.0);
        }
        for &fi in &fis {
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            shifted(a, b, 0, GATE_OPS);
            check_types(&back, b, 0..GATE_OPS);
            check_flow(b);
            assert!(matches!(b.ops[4], Opcode::JFalse { offset: 3, .. }));
            assert!(matches!(b.ops[7], Opcode::Ret { .. }));
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_camp_talk(&mut again);
        assert!(write(&again) == patched);
    }

    /// An impl that no longer reaches playConfessionSetup is refused and the
    /// image is left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let fis = plan(&read(&image)).expect("plan").fis;
        let mut code = read(&image);
        let f = &mut code.functions[fis[0]];
        for op in f.ops.iter_mut() {
            if matches!(op, Opcode::Call3 { .. }) {
                *op = Opcode::Label;
            }
        }
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_camp_talk(&mut code);
        assert!(write(&code) == before);
    }
}
