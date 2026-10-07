// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op battle HUD, part 2: a compact player status list above and to the left
// of the timeline's first diamond, one row per human player with living units:
//
//   » Name     acting   (marker and nickname in the player's colour)
//   [ok] Name  done     (green check icon, nickname in the player's colour)
//   [x] Name   waiting  (red cross icon)
//
// Status, per player p (battle.Player, its ent.BasePlayer `bp = p.player`):
// - acting:  `state.startedPlaying.data.owner == bp`. Battle.doExecuteSkill
//            (any move or action) calls setCurrentUnitPlaying(currentUnit), which
//            sets startedPlaying: that locks the Player slot onto the unit.
// - waiting: p still has a turn slot in the current round, `state.timelines[0]`:
//            Player(p) or PlayerUnit(u) with u.data.owner == bp. That line holds
//            the current and coming slots: the turn end's finish callback shifts
//            the used head out, a new round shifts in a fresh line
//            (Battle.hx:2353), the same line the diamonds show.
// - done:    no slot left. (Not UnitFlag.PlayedThisRound: that bit is per unit,
//            set only on a unit's last turn and reset by the host's round-end
//            loop, so a player with units left already read as done, and any unit
//            whose bit was not cleared kept its player done into the next round.)
// Players without a living unit in `state.units` are left out.
//
// No game font has ✓ • … glyphs, so done / waiting are icons: the label's
// loadImage (a dynamic method; vanilla returns null) is set to
// FmtText.loadImgText, which resolves <img src="icon/Ok"/> / "icon/Cancel" from
// the icon sheet at 20 px. Acting keeps » (U+00BB, in eb_garamond_medium's
// charset) and a long nickname is cut with "...".
//
// `timelineHudList(ev)`, called from TimelineEvent.sync after timelineHud, on
// Player diamonds only, inside try/catch:
//
//   if (!game.isMulti || ev is not the Timeline's first diamond) { mpList?.visible = false; return; }
//   if (mpList == null) create it (HtmlText, font "default", loadImage = FmtText.loadImgText,
//                                  drop shadow, absolute);
//   sig = round * 31 + timelines[0].length; per slot sig = sig * 31 + (kind + 1
//         [+ 31 * (players index + 1) for Player(p)]): a slot converted in place
//         (updateAutoPlayed Player <-> AI) changes it too;
//   for (u in state.units) sig = sig * 31 + (1 | alive << 1);
//   if (created || sig != mpListSig || startedPlaying != mpListUnit) rebuild the text;
//   if (rebuilt || outerWidth != mpListW || absX != mpListAx) place it:
//       x = -(textWidth + 6), clamped so the list starts >= 4 px from the scene's left edge;
//       y = -(textHeight + font.lineHeight)   (above the nickname row of part 1)
//   mpList.visible = true;
//
// So per frame: one pass over state.units with integer maths; the text is
// rebuilt only on a round / startedPlaying / turn slot / living-unit change.

use super::*;
use hlbc::types::RefEnumConstruct;

/// The list's fields, appended after part 1's: name and type.
pub(super) const FIELDS: [(&str, Kind); 5] = [
    ("mpList", Kind::Html),
    ("mpListSig", Kind::I32),
    ("mpListUnit", Kind::Unit),
    ("mpListAx", Kind::F64),
    ("mpListW", Kind::I32),
];

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Html,
    I32,
    Unit,
    F64,
}

impl Kind {
    pub(super) fn of(self, p: &Plan) -> RefType {
        match self {
            Kind::Html => p.html_t,
            Kind::I32 => p.i32_,
            Kind::Unit => p.unit_t,
            Kind::F64 => p.f64_,
        }
    }
}

pub(super) struct ListFields {
    list: RefField,
    sig: RefField,
    unit: RefField,
    ax: RefField,
    w: RefField,
}

impl ListFields {
    pub(super) fn at(base: usize) -> Self {
        ListFields {
            list: RefField(base),
            sig: RefField(base + 1),
            unit: RefField(base + 2),
            ax: RefField(base + 3),
            w: RefField(base + 4),
        }
    }
}

/// Longest nickname shown whole; a longer one keeps `MAX_NAME - 1` chars + "...".
const MAX_NAME: i32 = 16;
const GAP: f64 = 6.0;
const EDGE: f64 = 4.0;
const ACTING: &str = "\u{bb} ";
const DONE: &str = "<img src=\"icon/Ok\"/> ";
const WAITING: &str = "<img src=\"icon/Cancel\"/> ";
const CUT: &str = "...";
const BR: &str = "<br/>";

pub(super) struct ListPlan {
    s_units: RefField,
    s_players: RefField,
    s_round: RefField,
    proxy_t: RefType,
    proxy_array: RefField,
    arr_dyn_t: RefType,
    pl_player: RefField,
    /// TimelineElement.PlayerUnit(u): a turn slot bound to one player unit.
    pu_idx: i32,
    is_alive: RefFun,
    o_abs_x: RefField,
    o_mat_a: RefField,
    t_font: RefField,
    f_line_h: RefField,
    set_visible: RefFun,
    get_user_name: RefFun,
    html_escape: RefFun,
    nbool_t: RefType,
    str_add: RefFun,
    str_substr: RefFun,
    str_last_index: RefFun,
    ni32_t: RefType,
    str_len: RefField,
    load_img: RefFun,
    load_img_t: RefType,
    h_load_image: RefField,
}

pub(super) fn plan(code: &Bytecode, p: &Plan) -> Result<ListPlan> {
    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("{name}: unexpected field type");
        }
        Ok(f)
    };
    let m = |o: RefType, name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, o, name)?.findex;
        if sig(code, f)? != (args.to_vec(), ret) {
            bail!("unexpected {name} signature");
        }
        Ok(f)
    };
    let s_units = typed(p.state_t, "units", p.arr_t)?;
    let (s_players, proxy_t) = field(code, p.state_t, "players")?;
    if s(code, obj(code, proxy_t)?.name) != "hxbit.ArrayProxyData" {
        bail!("State.players is not an hxbit.ArrayProxyData");
    }
    let s_round = typed(p.state_t, "round", p.i32_)?;
    let arr_dyn_t = obj_type(code, "hl.types.ArrayDyn")?;
    let proxy_array = typed(proxy_t, "array", arr_dyn_t)?;
    let pl_player = typed(p.player_t, "player", p.bp_t)?;
    // The round's turn slots: TimelineElement.Player(p) (construct 0, checked by
    // the hud plan) and PlayerUnit(u).
    let pu_idx = match &code.types[p.elt_t.0] {
        Type::Enum { constructs, .. } => constructs
            .iter()
            .position(|c| s(code, c.name) == "PlayerUnit" && c.params == [p.unit_t]),
        _ => None,
    }
    .context("TimelineElement has no PlayerUnit(battle.Unit)")? as i32;
    let is_alive = m(p.unit_t, "isAlive", &[p.unit_t], p.bool_)?;
    let o_abs_x = typed(p.obj_t, "absX", p.f64_)?;
    let o_mat_a = typed(p.obj_t, "matA", p.f64_)?;
    let text_t = obj_type(code, "h2d.Text")?;
    let t_font = typed(text_t, "font", p.font_t)?;
    let f_line_h = typed(p.font_t, "lineHeight", p.f64_)?;
    let set_visible = proto(code, p.obj_t, "set_visible")?;
    if sig(code, set_visible)? != (vec![p.obj_t, p.bool_], p.bool_) {
        bail!("unexpected set_visible signature");
    }
    for t in [text_t, p.html_t] {
        if obj(code, t)?
            .protos
            .iter()
            .any(|x| s(code, x.name) == "set_visible")
        {
            bail!("{} overrides set_visible", s(code, obj(code, t)?.name));
        }
    }
    let get_user_name = m(p.bp_t, "getUserName", &[p.bp_t], p.str_t)?;
    let esc: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "htmlEscape"
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args.len() == 2 && t.args[0] == p.str_t && t.ret == p.str_t)
        })
        .collect();
    let [esc] = esc[..] else {
        bail!("expected one htmlEscape(String, ?Bool) -> String");
    };
    let nbool_t = fun_args(code, esc)[1];
    // `?quotes:Bool` compiles to ref<bool>; callers pass null for "not given".
    if !matches!(code.types[nbool_t.0], Type::Ref(t) if t == p.bool_) {
        bail!("htmlEscape's second argument is not ref<Bool>");
    }
    let html_escape = esc.findex;
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![p.str_t, p.str_t], p.str_t) {
        bail!("unexpected String.__add__ signature");
    }
    let str_substr = proto(code, p.str_t, "substr")?;
    let (sa, sr) = sig(code, str_substr)?;
    let ni32_t = *sa.get(2).context("String.substr arity")?;
    if sa != [p.str_t, p.i32_, ni32_t]
        || sr != p.str_t
        || !matches!(code.types[ni32_t.0], Type::Null(t) if t == p.i32_)
    {
        bail!("unexpected String.substr signature");
    }
    let str_last_index = proto(code, p.str_t, "lastIndexOf")?;
    if sig(code, str_last_index)? != (vec![p.str_t, p.str_t, ni32_t], p.i32_) {
        bail!("unexpected String.lastIndexOf signature");
    }
    let str_len = typed(p.str_t, "length", p.i32_)?;
    // Vanilla HtmlText.loadImage (a dynamic method) returns null: no <img>.
    // FmtText.loadImgText resolves "icon/<id>" from the icon sheet, 20 px.
    let tile_t = obj_type(code, "h2d.Tile")?;
    let fmt_file = debug_file(code, "src/ui/comp/FmtText.hx")?;
    let hits: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "loadImgText"
                && f.debug_info
                    .as_ref()
                    .and_then(|d| d.first())
                    .is_some_and(|&(file, _)| file == fmt_file)
        })
        .collect();
    let [load_img_fn] = hits[..] else {
        bail!("expected one FmtText.loadImgText, found {}", hits.len());
    };
    let load_img = load_img_fn.findex;
    if sig(code, load_img)? != (vec![p.str_t], tile_t) {
        bail!("unexpected FmtText.loadImgText signature");
    }
    let load_img_t = load_img_fn.t;
    let h_load_image = typed(p.html_t, "loadImage", load_img_t)?;
    Ok(ListPlan {
        s_units,
        s_players,
        s_round,
        proxy_t,
        proxy_array,
        arr_dyn_t,
        pl_player,
        pu_idx,
        is_alive,
        o_abs_x,
        o_mat_a,
        t_font,
        f_line_h,
        set_visible,
        get_user_name,
        html_escape,
        nbool_t,
        str_add,
        str_substr,
        str_last_index,
        ni32_t,
        str_len,
        load_img,
        load_img_t,
        h_load_image,
    })
}

/// `timelineHudList(ev)` (see the header).
pub(super) fn add_list(
    code: &mut Bytecode,
    p: &Plan,
    lp: &ListPlan,
    fl: &ListFields,
    report: RefFun,
    name_font: RefFun,
) -> Result<RefFun> {
    let s_err = str_global(code, p.str_t, "mp: timelineHudList error: ");
    let c0 = int_const(code, 0);
    let c1 = int_const(code, 1);
    let c2 = int_const(code, 2);
    let c31 = int_const(code, 31);
    let c_m1 = int_const(code, -1);
    let c_pu = int_const(code, lp.pu_idx);
    let c_max = int_const(code, MAX_NAME);
    let c_cut = int_const(code, MAX_NAME - 1);
    let f_one = float_const(code, 1.0);
    let f_alpha = float_const(code, 0.8);
    let f_gap = float_const(code, GAP);
    let f_edge = float_const(code, EDGE);
    let f_zero = float_const(code, 0.0);
    let s_empty = str_global(code, p.str_t, "");
    let s_acting = str_global(code, p.str_t, ACTING);
    let s_done = str_global(code, p.str_t, DONE);
    let s_waiting = str_global(code, p.str_t, WAITING);
    let s_cut = str_global(code, p.str_t, CUT);
    let s_br = str_global(code, p.str_t, BR);

    let mut r = Regs(vec![p.ev_t]);
    let ev = Reg(0);
    let (v, b, zero, one, idx, elt, exc) = (
        r.r(p.void_),
        r.r(p.bool_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.elt_t),
        r.r(p.dyn_t),
    );
    let (lbl, font, props, sh, fl64, ci) = (
        r.r(p.html_t),
        r.r(p.font_t),
        r.r(p.fprops_t),
        r.r(p.shadow_t),
        r.r(p.f64_),
        r.r(p.i32_),
    );
    let (game, bat, st) = (r.r(p.game_t), r.r(p.battle_t), r.r(p.state_t));
    let (par, tlc, tl, arr, n, raw, d, first) = (
        r.r(p.obj_t),
        r.r(p.tl_cls_t),
        r.r(p.tl_t),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.raw_t),
        r.r(p.dyn_t),
        r.r(p.ev_t),
    );
    let (force, h, k, t, units, nu, u) = (
        r.r(p.bool_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.unit_t),
    );
    // the current round's turn slots, timelines[0]
    let (tls, cur, e, ei, sp, slots) = (
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.elt_t),
        r.r(p.i32_),
        r.r(p.player_t),
        r.r(p.i32_),
    );
    let (started, old_sig, old_unit, sbp, sdata, owner, text, lines) = (
        r.r(p.unit_t),
        r.r(p.i32_),
        r.r(p.unit_t),
        r.r(p.bp_t),
        r.r(p.sunit_t),
        r.r(p.bp_t),
        r.r(p.str_t),
        r.r(p.i32_),
    );
    let (side, ps, players, ad, pa, j, np, pl, bp) = (
        r.r(p.side_t),
        r.r(p.side_t),
        r.r(lp.proxy_t),
        r.r(lp.arr_dyn_t),
        r.r(p.arr_t),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.player_t),
        r.r(p.bp_t),
    );
    let (alive, done, raw_name, esc, short, full, at, nb, ni) = (
        r.r(p.bool_),
        r.r(p.bool_),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.i32_),
        r.r(lp.nbool_t),
        r.r(lp.ni32_t),
    );
    let (inner, line, tmp) = (r.r(p.str_t), r.r(p.str_t), r.r(p.str_t));
    let loader = r.r(lp.load_img_t);
    let (wi, w_old, ax, ax_old, mat, x, xmin, tw, th, lh, fz) = (
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
    );

    let mut a = Asm::new();
    let str_op = |a: &mut Asm, dst: Reg, global| a.op(Opcode::GetGlobal { dst, global });
    let concat = |a: &mut Asm, dst: Reg, x: Reg, y: Reg| {
        a.op(Opcode::Call2 {
            dst,
            fun: lp.str_add,
            arg0: x,
            arg1: y,
        })
    };

    // Player diamonds only.
    a.op(Opcode::GetThis {
        dst: elt,
        field: p.ev_elt,
    });
    a.jmp(
        Opcode::JNull {
            reg: elt,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::EnumIndex {
        dst: idx,
        value: elt,
    });
    a.op(Opcode::Int { dst: zero, ptr: c0 });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: zero,
            offset: 0,
        },
        "ret",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetThis {
        dst: lbl,
        field: fl.list,
    });

    // Co-op only, and only on the Timeline's first diamond.
    a.op(Opcode::GetThis {
        dst: game,
        field: p.ev_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "hide",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "hide");
    a.op(Opcode::GetThis {
        dst: bat,
        field: p.ev_battle,
    });
    a.jmp(
        Opcode::JNull {
            reg: bat,
            offset: 0,
        },
        "hide",
    );
    a.op(Opcode::Field {
        dst: st,
        obj: bat,
        field: p.b_state,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "hide");
    a.op(Opcode::Field {
        dst: units,
        obj: st,
        field: lp.s_units,
    });
    a.jmp(
        Opcode::JNull {
            reg: units,
            offset: 0,
        },
        "hide",
    );
    super::emit_first_check(
        &mut a,
        p,
        super::FirstRegs {
            par,
            tlc,
            b,
            tl,
            arr,
            n,
            raw,
            d,
            first,
            zero,
        },
        "hide",
    );

    // Created once per diamond, empty.
    a.op(Opcode::Bool {
        dst: force,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JNotNull {
            reg: lbl,
            offset: 0,
        },
        "sig",
    );
    a.op(Opcode::Call0 {
        dst: font,
        fun: name_font,
    });
    a.op(Opcode::New { dst: lbl });
    a.op(Opcode::Call3 {
        dst: v,
        fun: p.html_ctor,
        arg0: lbl,
        arg1: font,
        arg2: ev,
    });
    // lbl.loadImage = FmtText.loadImgText: <img src="icon/Ok"/> etc.
    a.op(Opcode::StaticClosure {
        dst: loader,
        fun: lp.load_img,
    });
    a.op(Opcode::SetField {
        obj: lbl,
        field: lp.h_load_image,
        src: loader,
    });
    a.op(Opcode::Call2 {
        dst: props,
        fun: p.get_props,
        arg0: ev,
        arg1: lbl,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_absolute,
        arg0: props,
        arg1: b,
    });
    a.op(Opcode::New { dst: sh });
    a.op(Opcode::Float {
        dst: fl64,
        ptr: f_one,
    });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_dx,
        src: fl64,
    });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_dy,
        src: fl64,
    });
    a.op(Opcode::Int { dst: ci, ptr: c0 });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_color,
        src: ci,
    });
    a.op(Opcode::Float {
        dst: fl64,
        ptr: f_alpha,
    });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_alpha,
        src: fl64,
    });
    a.op(Opcode::SetField {
        obj: lbl,
        field: p.t_shadow,
        src: sh,
    });
    a.op(Opcode::SetThis {
        field: fl.list,
        src: lbl,
    });
    a.op(Opcode::Bool {
        dst: force,
        value: ValBool(true),
    });

    // sig = round * 31 + timelines[0].length (-1: none), then per unit
    // sig * 31 + (1 | alive << 1). A turn slot used (shifted out of
    // timelines[0]) or a new round changes it.
    a.label("sig");
    a.op(Opcode::Int { dst: one, ptr: c1 });
    a.op(Opcode::Field {
        dst: h,
        obj: st,
        field: lp.s_round,
    });
    a.op(Opcode::Null { dst: cur });
    a.op(Opcode::Int { dst: t, ptr: c_m1 });
    a.op(Opcode::Field {
        dst: tls,
        obj: st,
        field: p.s_timelines,
    });
    a.jmp(
        Opcode::JNull {
            reg: tls,
            offset: 0,
        },
        "tl",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: tls,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: n,
            offset: 0,
        },
        "tl",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: tls,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: zero,
    });
    a.op(Opcode::UnsafeCast { dst: cur, src: d });
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "tl",
    );
    a.op(Opcode::Field {
        dst: t,
        obj: cur,
        field: p.a_len,
    });
    a.label("tl");
    a.op(Opcode::Int { dst: nu, ptr: c31 });
    a.op(Opcode::Mul {
        dst: h,
        a: h,
        b: nu,
    });
    a.op(Opcode::Add { dst: h, a: h, b: t });
    // per slot: sig * 31 + (kind + 1), a Player(p) slot also + 31 * (p's
    // position in state.players + 1): a slot converted in place (vanilla
    // updateAutoPlayed: Player <-> AI(unit)) changes it without a length change.
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "tsig",
    );
    a.op(Opcode::Int { dst: k, ptr: c0 });
    a.loop_head("ts");
    a.op(Opcode::Field {
        dst: n,
        obj: cur,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: n,
            offset: 0,
        },
        "tsig",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: cur,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: e, src: d });
    a.op(Opcode::Int { dst: t, ptr: c0 });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "tmix");
    a.op(Opcode::EnumIndex { dst: ei, value: e });
    a.op(Opcode::Add {
        dst: t,
        a: ei,
        b: one,
    });
    a.jmp(
        Opcode::JNotEq {
            a: ei,
            b: zero,
            offset: 0,
        },
        "tmix",
    );
    a.op(Opcode::EnumField {
        dst: sp,
        value: e,
        construct: RefEnumConstruct(0),
        field: RefField(0),
    });
    a.op(Opcode::Field {
        dst: players,
        obj: st,
        field: lp.s_players,
    });
    a.jmp(
        Opcode::JNull {
            reg: players,
            offset: 0,
        },
        "tmix",
    );
    a.op(Opcode::Field {
        dst: ad,
        obj: players,
        field: lp.proxy_array,
    });
    a.op(Opcode::SafeCast { dst: pa, src: ad });
    a.jmp(Opcode::JNull { reg: pa, offset: 0 }, "tmix");
    a.op(Opcode::Int { dst: j, ptr: c0 });
    a.loop_head("tp");
    a.op(Opcode::Field {
        dst: np,
        obj: pa,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: np,
            offset: 0,
        },
        "tmix",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: pa,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: j,
    });
    a.op(Opcode::Incr { dst: j });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.jmp(
        Opcode::JNotEq {
            a: pl,
            b: sp,
            offset: 0,
        },
        "tp",
    );
    a.op(Opcode::Mul {
        dst: np,
        a: j,
        b: nu,
    });
    a.op(Opcode::Add {
        dst: t,
        a: t,
        b: np,
    });
    a.label("tmix");
    a.op(Opcode::Mul {
        dst: h,
        a: h,
        b: nu,
    });
    a.op(Opcode::Add { dst: h, a: h, b: t });
    a.jmp(Opcode::JAlways { offset: 0 }, "ts");
    a.label("tsig");
    a.op(Opcode::Int { dst: k, ptr: c0 });
    a.loop_head("su");
    a.op(Opcode::Field {
        dst: nu,
        obj: units,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: nu,
            offset: 0,
        },
        "sdone",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: units,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: u, src: d });
    a.op(Opcode::Mov { dst: idx, src: one });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "smix");
    a.op(Opcode::Call1 {
        dst: b,
        fun: lp.is_alive,
        arg0: u,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "smix");
    a.op(Opcode::Int { dst: t, ptr: c2 });
    a.op(Opcode::Add {
        dst: idx,
        a: idx,
        b: t,
    });
    a.label("smix");
    a.op(Opcode::Int { dst: t, ptr: c31 });
    a.op(Opcode::Mul { dst: h, a: h, b: t });
    a.op(Opcode::Add {
        dst: h,
        a: h,
        b: idx,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "su");
    a.label("sdone");

    // Rebuild only on a change.
    a.op(Opcode::Field {
        dst: started,
        obj: st,
        field: p.s_started,
    });
    a.jmp(
        Opcode::JTrue {
            cond: force,
            offset: 0,
        },
        "rebuild",
    );
    a.op(Opcode::GetThis {
        dst: old_sig,
        field: fl.sig,
    });
    a.jmp(
        Opcode::JNotEq {
            a: h,
            b: old_sig,
            offset: 0,
        },
        "rebuild",
    );
    a.op(Opcode::GetThis {
        dst: old_unit,
        field: fl.unit,
    });
    a.jmp(
        Opcode::JNotEq {
            a: started,
            b: old_unit,
            offset: 0,
        },
        "rebuild",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "layout");

    a.label("rebuild");
    a.op(Opcode::SetThis {
        field: fl.sig,
        src: h,
    });
    a.op(Opcode::SetThis {
        field: fl.unit,
        src: started,
    });
    a.op(Opcode::Bool {
        dst: force,
        value: ValBool(true),
    });
    // sbp = startedPlaying?.data?.owner
    a.op(Opcode::Null { dst: sbp });
    a.jmp(
        Opcode::JNull {
            reg: started,
            offset: 0,
        },
        "rows",
    );
    a.op(Opcode::Field {
        dst: sdata,
        obj: started,
        field: p.u_data,
    });
    a.jmp(
        Opcode::JNull {
            reg: sdata,
            offset: 0,
        },
        "rows",
    );
    a.op(Opcode::Field {
        dst: sbp,
        obj: sdata,
        field: p.su_owner,
    });
    a.label("rows");
    str_op(&mut a, text, s_empty);
    a.op(Opcode::Int {
        dst: lines,
        ptr: c0,
    });
    a.op(Opcode::Null { dst: nb });
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Field {
        dst: side,
        obj: st,
        field: p.s_side,
    });
    a.op(Opcode::Field {
        dst: players,
        obj: st,
        field: lp.s_players,
    });
    a.jmp(
        Opcode::JNull {
            reg: players,
            offset: 0,
        },
        "settext",
    );
    a.op(Opcode::Field {
        dst: ad,
        obj: players,
        field: lp.proxy_array,
    });
    a.op(Opcode::SafeCast { dst: pa, src: ad });
    a.jmp(Opcode::JNull { reg: pa, offset: 0 }, "settext");
    a.op(Opcode::Int { dst: j, ptr: c0 });
    a.loop_head("pl");
    a.op(Opcode::Field {
        dst: np,
        obj: pa,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: j,
            b: np,
            offset: 0,
        },
        "settext",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: pa,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: j,
    });
    a.op(Opcode::Incr { dst: j });
    a.op(Opcode::UnsafeCast { dst: pl, src: d });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "pnext");
    a.op(Opcode::Field {
        dst: bp,
        obj: pl,
        field: lp.pl_player,
    });
    a.jmp(Opcode::JNull { reg: bp, offset: 0 }, "pnext");
    a.op(Opcode::Field {
        dst: ps,
        obj: pl,
        field: p.p_side,
    });
    a.jmp(
        Opcode::JNotEq {
            a: ps,
            b: side,
            offset: 0,
        },
        "pnext",
    );

    // alive / done over bp's units.
    a.op(Opcode::Bool {
        dst: alive,
        value: ValBool(false),
    });
    a.op(Opcode::Bool {
        dst: done,
        value: ValBool(false),
    });
    a.op(Opcode::Int { dst: k, ptr: c0 });
    a.loop_head("uu");
    a.op(Opcode::Field {
        dst: nu,
        obj: units,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: nu,
            offset: 0,
        },
        "udone",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: units,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: u, src: d });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "unext");
    a.op(Opcode::Field {
        dst: sdata,
        obj: u,
        field: p.u_data,
    });
    a.jmp(
        Opcode::JNull {
            reg: sdata,
            offset: 0,
        },
        "unext",
    );
    a.op(Opcode::Field {
        dst: owner,
        obj: sdata,
        field: p.su_owner,
    });
    a.jmp(
        Opcode::JNotEq {
            a: owner,
            b: bp,
            offset: 0,
        },
        "unext",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: lp.is_alive,
        arg0: u,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "unext");
    a.op(Opcode::Bool {
        dst: alive,
        value: ValBool(true),
    });
    a.label("unext");
    a.jmp(Opcode::JAlways { offset: 0 }, "uu");
    a.label("udone");
    a.jmp(
        Opcode::JFalse {
            cond: alive,
            offset: 0,
        },
        "pnext",
    );

    // done = no turn slot of this player left in the round: timelines[0]
    // holds the current and coming slots (a used one is shifted out, a new
    // round brings a fresh line), Player(pl) or PlayerUnit(u) with u.data.owner == bp.
    a.op(Opcode::Int {
        dst: slots,
        ptr: c0,
    });
    a.jmp(
        Opcode::JNull {
            reg: cur,
            offset: 0,
        },
        "slots",
    );
    a.op(Opcode::Int { dst: k, ptr: c0 });
    a.loop_head("sl");
    a.op(Opcode::Field {
        dst: nu,
        obj: cur,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: nu,
            offset: 0,
        },
        "slots",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: cur,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: k,
    });
    a.op(Opcode::Incr { dst: k });
    a.op(Opcode::UnsafeCast { dst: e, src: d });
    a.jmp(Opcode::JNull { reg: e, offset: 0 }, "sl");
    a.op(Opcode::EnumIndex { dst: ei, value: e });
    a.jmp(
        Opcode::JNotEq {
            a: ei,
            b: zero,
            offset: 0,
        },
        "sl_unit",
    );
    a.op(Opcode::EnumField {
        dst: sp,
        value: e,
        construct: RefEnumConstruct(0),
        field: RefField(0),
    });
    a.jmp(
        Opcode::JNotEq {
            a: sp,
            b: pl,
            offset: 0,
        },
        "sl",
    );
    a.op(Opcode::Incr { dst: slots });
    a.jmp(Opcode::JAlways { offset: 0 }, "sl");
    a.label("sl_unit");
    a.op(Opcode::Int { dst: t, ptr: c_pu });
    a.jmp(
        Opcode::JNotEq {
            a: ei,
            b: t,
            offset: 0,
        },
        "sl",
    );
    a.op(Opcode::EnumField {
        dst: u,
        value: e,
        construct: RefEnumConstruct(lp.pu_idx as usize),
        field: RefField(0),
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "sl");
    a.op(Opcode::Field {
        dst: sdata,
        obj: u,
        field: p.u_data,
    });
    a.jmp(
        Opcode::JNull {
            reg: sdata,
            offset: 0,
        },
        "sl",
    );
    a.op(Opcode::Field {
        dst: owner,
        obj: sdata,
        field: p.su_owner,
    });
    a.jmp(
        Opcode::JNotEq {
            a: owner,
            b: bp,
            offset: 0,
        },
        "sl",
    );
    a.op(Opcode::Incr { dst: slots });
    a.jmp(Opcode::JAlways { offset: 0 }, "sl");
    a.label("slots");
    a.op(Opcode::Bool {
        dst: done,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JNotEq {
            a: slots,
            b: zero,
            offset: 0,
        },
        "named0",
    );
    a.op(Opcode::Bool {
        dst: done,
        value: ValBool(true),
    });
    a.label("named0");

    // short = escaped nickname, cut to MAX_NAME - 1 chars + "..." when longer.
    a.op(Opcode::Call1 {
        dst: raw_name,
        fun: lp.get_user_name,
        arg0: bp,
    });
    a.jmp(
        Opcode::JNull {
            reg: raw_name,
            offset: 0,
        },
        "pnext",
    );
    a.op(Opcode::Call2 {
        dst: esc,
        fun: lp.html_escape,
        arg0: raw_name,
        arg1: nb,
    });
    a.op(Opcode::Mov {
        dst: short,
        src: esc,
    });
    a.op(Opcode::Field {
        dst: t,
        obj: raw_name,
        field: lp.str_len,
    });
    a.op(Opcode::Int {
        dst: at,
        ptr: c_max,
    });
    a.jmp(
        Opcode::JSGte {
            a: at,
            b: t,
            offset: 0,
        },
        "named",
    );
    a.op(Opcode::Int { dst: t, ptr: c_cut });
    a.op(Opcode::ToDyn { dst: ni, src: t });
    a.op(Opcode::Call3 {
        dst: tmp,
        fun: lp.str_substr,
        arg0: raw_name,
        arg1: zero,
        arg2: ni,
    });
    a.op(Opcode::Null { dst: ni });
    a.op(Opcode::Call2 {
        dst: short,
        fun: lp.html_escape,
        arg0: tmp,
        arg1: nb,
    });
    str_op(&mut a, tmp, s_cut);
    concat(&mut a, short, short, tmp);
    a.label("named");

    // inner = (acting ? "» " : "") + short, put where getName() has the escaped name.
    a.op(Opcode::Mov {
        dst: inner,
        src: short,
    });
    a.jmp(
        Opcode::JNotEq {
            a: sbp,
            b: bp,
            offset: 0,
        },
        "colour",
    );
    str_op(&mut a, tmp, s_acting);
    concat(&mut a, inner, tmp, inner);
    a.label("colour");
    a.op(Opcode::Call1 {
        dst: full,
        fun: p.bp_get_name,
        arg0: bp,
    });
    a.op(Opcode::Mov {
        dst: line,
        src: inner,
    });
    a.jmp(
        Opcode::JNull {
            reg: full,
            offset: 0,
        },
        "marker",
    );
    a.op(Opcode::Call3 {
        dst: at,
        fun: lp.str_last_index,
        arg0: full,
        arg1: esc,
        arg2: ni,
    });
    a.jmp(
        Opcode::JSGt {
            a: zero,
            b: at,
            offset: 0,
        },
        "marker",
    );
    a.op(Opcode::ToDyn { dst: ni, src: at });
    a.op(Opcode::Call3 {
        dst: line,
        fun: lp.str_substr,
        arg0: full,
        arg1: zero,
        arg2: ni,
    });
    a.op(Opcode::Null { dst: ni });
    concat(&mut a, line, line, inner);
    a.op(Opcode::Field {
        dst: t,
        obj: esc,
        field: lp.str_len,
    });
    a.op(Opcode::Add {
        dst: at,
        a: at,
        b: t,
    });
    a.op(Opcode::Call3 {
        dst: tmp,
        fun: lp.str_substr,
        arg0: full,
        arg1: at,
        arg2: ni,
    });
    concat(&mut a, line, line, tmp);

    // Marker: acting is inside the colour already; done green check; waiting red cross.
    a.label("marker");
    a.jmp(
        Opcode::JEq {
            a: sbp,
            b: bp,
            offset: 0,
        },
        "append",
    );
    str_op(&mut a, tmp, s_waiting);
    a.jmp(
        Opcode::JFalse {
            cond: done,
            offset: 0,
        },
        "prefix",
    );
    str_op(&mut a, tmp, s_done);
    a.label("prefix");
    concat(&mut a, line, tmp, line);
    a.label("append");
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: lines,
            offset: 0,
        },
        "first_row",
    );
    str_op(&mut a, tmp, s_br);
    concat(&mut a, text, text, tmp);
    a.label("first_row");
    concat(&mut a, text, text, line);
    a.op(Opcode::Incr { dst: lines });
    a.label("pnext");
    a.jmp(Opcode::JAlways { offset: 0 }, "pl");
    a.label("settext");
    a.op(Opcode::Call2 {
        dst: text,
        fun: p.html_set_text,
        arg0: lbl,
        arg1: text,
    });

    // Place only when the text, the diamond's width or its screen x changed.
    a.label("layout");
    a.op(Opcode::Call1 {
        dst: wi,
        fun: p.outer_width,
        arg0: ev,
    });
    a.op(Opcode::Field {
        dst: ax,
        obj: ev,
        field: lp.o_abs_x,
    });
    a.jmp(
        Opcode::JTrue {
            cond: force,
            offset: 0,
        },
        "place",
    );
    a.op(Opcode::GetThis {
        dst: w_old,
        field: fl.w,
    });
    a.jmp(
        Opcode::JNotEq {
            a: wi,
            b: w_old,
            offset: 0,
        },
        "place",
    );
    a.op(Opcode::GetThis {
        dst: ax_old,
        field: fl.ax,
    });
    a.jmp(
        Opcode::JNotEq {
            a: ax,
            b: ax_old,
            offset: 0,
        },
        "place",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "show");
    a.label("place");
    a.op(Opcode::SetThis {
        field: fl.w,
        src: wi,
    });
    a.op(Opcode::SetThis {
        field: fl.ax,
        src: ax,
    });
    // x = -(textWidth + GAP), but not left of EDGE px on screen: (EDGE - absX) / matA.
    a.op(Opcode::Call1 {
        dst: tw,
        fun: p.text_width,
        arg0: lbl,
    });
    a.op(Opcode::Float {
        dst: fl64,
        ptr: f_gap,
    });
    a.op(Opcode::Add {
        dst: x,
        a: tw,
        b: fl64,
    });
    a.op(Opcode::Neg { dst: x, src: x });
    a.op(Opcode::Field {
        dst: mat,
        obj: ev,
        field: lp.o_mat_a,
    });
    a.op(Opcode::Float {
        dst: fz,
        ptr: f_zero,
    });
    a.jmp(
        Opcode::JSGte {
            a: fz,
            b: mat,
            offset: 0,
        },
        "setx",
    );
    a.op(Opcode::Float {
        dst: xmin,
        ptr: f_edge,
    });
    a.op(Opcode::Sub {
        dst: xmin,
        a: xmin,
        b: ax,
    });
    a.op(Opcode::SDiv {
        dst: xmin,
        a: xmin,
        b: mat,
    });
    a.jmp(
        Opcode::JSGte {
            a: x,
            b: xmin,
            offset: 0,
        },
        "setx",
    );
    a.op(Opcode::Mov { dst: x, src: xmin });
    a.label("setx");
    a.op(Opcode::Call2 {
        dst: x,
        fun: p.set_x,
        arg0: lbl,
        arg1: x,
    });
    // y = -(textHeight + font.lineHeight): above part 1's nickname row.
    a.op(Opcode::Call1 {
        dst: th,
        fun: p.text_height,
        arg0: lbl,
    });
    a.op(Opcode::Field {
        dst: font,
        obj: lbl,
        field: lp.t_font,
    });
    a.jmp(
        Opcode::JNull {
            reg: font,
            offset: 0,
        },
        "sety",
    );
    a.op(Opcode::Field {
        dst: lh,
        obj: font,
        field: lp.f_line_h,
    });
    a.op(Opcode::Add {
        dst: th,
        a: th,
        b: lh,
    });
    a.label("sety");
    a.op(Opcode::Neg { dst: th, src: th });
    a.op(Opcode::Call2 {
        dst: th,
        fun: p.set_y,
        arg0: lbl,
        arg1: th,
    });
    a.label("show");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: lp.set_visible,
        arg0: lbl,
        arg1: b,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "untrap");
    a.label("hide");
    a.jmp(
        Opcode::JNull {
            reg: lbl,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: lp.set_visible,
        arg0: lbl,
        arg1: b,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    str_op(&mut a, text, s_err);
    a.op(Opcode::Call3 {
        dst: v,
        fun: report,
        arg0: ev,
        arg1: text,
        arg2: exc,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.ev_t], p.void_, r.0, a.finish(), p.dbg_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// The text markers are in the "default" font's charset (eb_garamond_medium.fnt
    /// has U+00BB, no U+2713 / U+2022 / U+2026); done / waiting are icon <img> tags.
    #[test]
    fn markers_are_latin1() {
        for m in [ACTING, DONE, WAITING, CUT] {
            assert!(m.chars().all(|c| (c as u32) < 0x100), "{m}");
        }
        assert!(DONE.contains("\"icon/Ok\""), "{DONE}");
        assert!(WAITING.contains("\"icon/Cancel\""), "{WAITING}");
    }

    /// The list label gets FmtText.loadImgText as its loadImage, and the icon
    /// markers are emitted.
    #[test]
    fn list_loads_icons() {
        let Some(image) = game() else { return };
        let mut code = read(&image);
        let p = super::super::plan(&code).expect("base plan");
        let lp = plan(&code, &p).expect("list plan");
        super::super::patch_timeline_hud(&mut code);
        assert!(code.functions.iter().any(|f| {
            f.ops.windows(2).any(|w| {
                matches!(w[0], Opcode::StaticClosure { fun, .. } if fun == lp.load_img)
                    && matches!(w[1], Opcode::SetField { field, .. } if field == lp.h_load_image)
            })
        }));
        for m in [DONE, WAITING] {
            assert!(code.strings.iter().any(|x| x.as_str() == m), "{m}");
        }
    }

    /// A State without `players`, or a TimelineElement without
    /// PlayerUnit(battle.Unit), is refused and the image left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let mut code = read(&image);
        let st = obj_type(&code, "battle.State").expect("state");
        let (f, _) = field(&code, st, "players").expect("players");
        let other = string_ref(&mut code, "mpNotPlayers");
        code.types[st.0].get_type_obj_mut().expect("obj").fields[f.0].name = other;
        let before = write(&code);
        let p = super::super::plan(&code).expect("base plan");
        let e = plan(&code, &p).err().expect("refused");
        assert!(format!("{e:#}").contains("players"), "{e:#}");
        super::super::patch_timeline_hud(&mut code);
        assert!(write(&code) == before);

        let mut code = read(&image);
        let p = super::super::plan(&code).expect("base plan");
        let other = string_ref(&mut code, "mpNotPlayerUnit");
        let Type::Enum { constructs, .. } = &mut code.types[p.elt_t.0] else {
            unreachable!()
        };
        let pu = constructs
            .iter()
            .position(|c| s_of(&image, c.name) == "PlayerUnit")
            .expect("PlayerUnit");
        constructs[pu].name = other;
        let e = plan(&code, &p).err().expect("refused");
        assert!(format!("{e:#}").contains("PlayerUnit"), "{e:#}");
        let before = write(&code);
        super::super::patch_timeline_hud(&mut code);
        assert!(write(&code) == before);
    }

    fn s_of(image: &[u8], i: hlbc::types::RefString) -> String {
        read(image).strings[i.0].as_str().to_string()
    }

    /// timelineHudList in the interpreter over a co-op round rollover: two
    /// players A and B, each with one living unit. A row is waiting while its
    /// player still has a turn slot in timelines[0] (Player(p), or PlayerUnit(u)
    /// of one of its units), done once none is left, acting while one of its
    /// units plays; a new round's line puts everyone back to waiting.
    #[test]
    fn round_rollover_resets_marks() {
        use crate::testsim::{Sim, V};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = super::super::plan(&orig).expect("base plan");
        let lp = plan(&orig, &p).expect("list plan");
        let orig_n = orig.functions.len();
        let mut code = read(&image);
        super::super::patch_timeline_hud(&mut code);
        let n = code.functions.len();
        let (name_font, list) = (code.functions[n - 3].findex, code.functions[n - 1].findex);
        let (pp, lpp) = (&p, &lp);
        let s = |v: &V| match v {
            V::S(x) => x.clone(),
            o => panic!("not a string: {o:?}"),
        };
        let mut sim = Sim::new(
            &code,
            orig_n,
            move |c, f, a| {
                let (p, lp) = (pp, lpp);
                Some(if f == p.is_multi {
                    V::B(true)
                } else if f == p.base_check {
                    V::B(c.key_get(&a[1], "is_tl") == V::B(true))
                } else if f == name_font {
                    c.obj(&[])
                } else if f == p.get_props {
                    c.obj(&[])
                } else if f == p.html_ctor || f == p.set_absolute {
                    V::Null
                } else if f == lp.is_alive {
                    V::B(c.key_get(&a[0], "alive") == V::B(true))
                } else if f == lp.get_user_name || f == p.bp_get_name {
                    c.key_get(&a[0], "name")
                } else if f == lp.html_escape {
                    a[0].clone()
                } else if f == lp.str_add {
                    V::S(format!("{}{}", s(&a[0]), s(&a[1])))
                } else if f == lp.str_last_index {
                    let (h, n) = (s(&a[0]), s(&a[1]));
                    V::I(h.rfind(&n).map_or(-1, |i| i as i32))
                } else if f == lp.str_substr {
                    let x = s(&a[0]);
                    let V::I(pos) = a[1] else {
                        panic!("substr pos")
                    };
                    let len = match a[2] {
                        V::I(l) => l as usize,
                        _ => x.len(),
                    };
                    V::S(x.chars().skip(pos as usize).take(len).collect())
                } else if f == p.html_set_text {
                    c.log.push(("text", a.to_vec()));
                    a[1].clone()
                } else if f == p.outer_width {
                    V::I(40)
                } else if f == p.text_width || f == p.text_height {
                    V::F(20.0)
                } else if f == p.set_x || f == p.set_y {
                    a[1].clone()
                } else if f == lp.set_visible {
                    a[1].clone()
                } else {
                    return None;
                })
            },
            |_, _, _| panic!("no virtual call expected"),
        );
        let c = &mut sim.c;
        let side = c.obj(&[]);
        let mk_bp = |c: &mut crate::testsim::Core, name: &str| {
            let bp = c.obj(&[]);
            c.key_set(&bp, "name".into(), V::S(name.into()));
            bp
        };
        let (bpa, bpb) = (mk_bp(c, "A"), mk_bp(c, "B"));
        let pla = c.obj(&[(lp.pl_player, bpa.clone()), (p.p_side, side.clone())]);
        let plb = c.obj(&[(lp.pl_player, bpb.clone()), (p.p_side, side.clone())]);
        let mk_unit = |c: &mut crate::testsim::Core, bp: &V| {
            let data = c.obj(&[(p.su_owner, bp.clone())]);
            let u = c.obj(&[(p.u_data, data)]);
            c.key_set(&u, "alive".into(), V::B(true));
            u
        };
        let (ua, ub) = (mk_unit(c, &bpa), mk_unit(c, &bpb));
        let units = c.arr(p.a_len, p.a_raw, vec![ua.clone(), ub.clone()]);
        let players_arr = c.arr(p.a_len, p.a_raw, vec![pla.clone(), plb.clone()]);
        let players = c.obj(&[(lp.proxy_array, players_arr)]);
        let st = c.obj(&[
            (lp.s_units, units),
            (lp.s_players, players),
            (lp.s_round, V::I(1)),
            (p.s_side, side),
        ]);
        let battle = c.obj(&[(p.b_state, st.clone())]);
        let game = c.obj(&[]);
        let tl = c.obj(&[]);
        c.key_set(&tl, "is_tl".into(), V::B(true));
        let head = c.enm(0, vec![pla.clone()]);
        assert_eq!(lp.str_len.0, 1, "String.length is field 1 (testsim)");
        let ev = c.obj(&[
            (p.ev_elt, head),
            (p.ev_game, game),
            (p.ev_battle, battle),
            (p.o_parent, tl.clone()),
            (lp.o_abs_x, V::F(100.0)),
            (lp.o_mat_a, V::F(1.0)),
        ]);
        let events = c.arr(p.a_len, p.a_raw, vec![ev.clone()]);
        c.set(&tl, p.tl_events, events);

        // one frame with timelines = [line]: the list text, if rebuilt
        let frame = |sim: &mut Sim, round: i32, line: Vec<V>, started: V| -> Option<String> {
            let c = &mut sim.c;
            let line = c.arr(p.a_len, p.a_raw, line);
            let tls = c.arr(p.a_len, p.a_raw, vec![line]);
            c.set(&st, p.s_timelines, tls);
            c.set(&st, lp.s_round, V::I(round));
            c.set(&st, p.s_started, started);
            sim.run(list, vec![ev.clone()]);
            let mut t = sim.c.take("text");
            assert!(t.len() <= 1);
            t.pop().map(|a| s(&a[1]))
        };
        let (wait, done, br) = (WAITING, DONE, BR);
        let pa = |c: &mut crate::testsim::Core| c.enm(0, vec![pla.clone()]);
        let pb = |c: &mut crate::testsim::Core| c.enm(0, vec![plb.clone()]);
        let pub_ = |c: &mut crate::testsim::Core| c.enm(lp.pu_idx, vec![ub.clone()]);
        let ai = |c: &mut crate::testsim::Core| c.enm(1, vec![V::Null]);

        // round 1 start: A, AI, B (fixed unit slot) to come -> both waiting
        let line = vec![pa(&mut sim.c), ai(&mut sim.c), pub_(&mut sim.c)];
        assert_eq!(
            frame(&mut sim, 1, line.clone(), V::Null).as_deref(),
            Some(format!("{wait}A{br}{wait}B").as_str())
        );
        // same state next frame: no rebuild
        assert_eq!(frame(&mut sim, 1, line, V::Null), None);
        // A's unit acting
        let line = vec![pa(&mut sim.c), ai(&mut sim.c), pub_(&mut sim.c)];
        assert_eq!(
            frame(&mut sim, 1, line, ua.clone()).as_deref(),
            Some(format!("{ACTING}A{br}{wait}B").as_str())
        );
        // A's slot used (shifted out): A done, B waiting
        let line = vec![ai(&mut sim.c), pub_(&mut sim.c)];
        assert_eq!(
            frame(&mut sim, 1, line, V::Null).as_deref(),
            Some(format!("{done}A{br}{wait}B").as_str())
        );
        // B's slot used too: both done
        assert_eq!(
            frame(&mut sim, 1, vec![], V::Null).as_deref(),
            Some(format!("{done}A{br}{done}B").as_str())
        );
        // round 2: a fresh line -> both waiting again
        let line = vec![pb(&mut sim.c), pa(&mut sim.c)];
        assert_eq!(
            frame(&mut sim, 2, line, V::Null).as_deref(),
            Some(format!("{wait}A{br}{wait}B").as_str())
        );
        // A's slot turned into an AI(unit) slot in place (updateAutoPlayed):
        // same length, round and acting unit, still rebuilt: A done
        let line = vec![pb(&mut sim.c), ai(&mut sim.c)];
        assert_eq!(
            frame(&mut sim, 2, line, V::Null).as_deref(),
            Some(format!("{done}A{br}{wait}B").as_str())
        );
        // and back, then the two Player slots swapped: same kinds, other owners
        let line = vec![pb(&mut sim.c), pa(&mut sim.c)];
        assert!(frame(&mut sim, 2, line, V::Null).is_some());
        let line = vec![pa(&mut sim.c), pb(&mut sim.c)];
        assert!(frame(&mut sim, 2, line, V::Null).is_some());
    }
}
