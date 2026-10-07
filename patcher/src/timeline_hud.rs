// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op battle HUD: while a player unit acts, the first timeline diamond
// (bottom left) shows that unit's portrait instead of the crossed swords, and the
// nickname of the player who controls it sits above the diamond, in that
// player's colour. Display only: nothing is sent over the network.
//
// The diamond is `battle.ui.win.TimelineEvent` (Timeline.hx). Its element is
// `TimelineElement.Player(p)` during the player side's turn: the constructor
// draws `getIcon("TimelineIcon")` (the swords) into the `event` bitmap and, unlike
// the unit kinds (AI, PlayerUnit, ...), removes `maskBitmap` instead of masking
// `event` with it:
//
//   switch (elt) { case AI, Event, AreaEffect, PlayerUnit: masked = true;
//                  case Player, EndRound: masked = false; }
//   if (masked) event.filter = new h2d.filter.Mask(maskBitmap);
//   else maskBitmap.remove();
//
// Two edits and one new function:
//
// 1. Constructor: `Player` takes the masked branch (one Switch offset), so a
//    Player diamond keeps its (absolutely positioned, hidden by the Mask's Hide
//    filter) mask and a portrait drawn into `event` is cut to the diamond exactly
//    like the AI / PlayerUnit portraits (same `.event` box from style.css).
// 2. `TimelineEvent.sync` calls `timelineHud(this)` right after the super sync.
// 3. `timelineHud(ev)`, Player diamonds only, inside try/catch:
//
//      if (ev.mpHudLabel == null) {          // once per diamond
//          if (ev.event.filter != null) ev.event.filter.enable = false;  // swords unmasked, as vanilla
//          ev.mpHudLabel = new h2d.HtmlText(nameFont(), ev);   // players panel font
//          ev.getProperties(label).isAbsolute = true;
//          label.dropShadow = { dx: 1, dy: 1, color: 0, alpha: 0.8 };
//      }
//      u = state.startedPlaying, kept only when it is on the player side and
//          ev is the Timeline's first diamond (eventsElts[0]); else null
//      if (startedPlaying == null && ev.mpHudUnit != null && ev is still first
//          && ev.elt == state.timelines[0][0]) u = ev.mpHudUnit;   // why 6
//          End turn clears startedPlaying (closure fn@29542) before the
//          finish callback shifts timelines[0] and Timeline.update rebuilds
//          the diamonds (ui.Window.rebuild fn@3427: new TimelineEvents);
//          keeping the portrait while ev.elt is still the live head (same
//          enum value) removes the swords flash. A shifted head, an instance
//          reused for another slot (Timeline.update rebuilds only when an
//          entry differs by type_enum_eq) or a new round's genTimeline values
//          are other enum values, so those show the swords.
//      if (u == ev.mpHudUnit) return;        // act only on change
//      ev.mpHudUnit = u;
//      if (u == null) { tile = get_api().getIcon("TimelineIcon"); text = ""; mask = false; }
//      else { tile = u.data.getIcon();       // the AI / PlayerUnit portrait
//             text = isMulti && u.data.owner != null ? u.data.owner.getName() : "";
//             mask = true; }                 // getName: nickname in <font color=player colour>,
//                                            // the markup the players panel shows too
//      ev.event.tile = tile; ev.event.filter.enable = mask;
//      label.text = text;
//      centre: ev.mpHudW = ev.outerWidth;
//              label.x = (ev.outerWidth - label.textWidth) / 2; label.y = -label.textHeight;
//
//    Same unit as last frame: only `centre` runs, and only when the diamond's
//    outerWidth differs from mpHudW (a window / UI rescale), so the nickname
//    stays centred without per-frame layout work.
// 4. `TimelineEvent.sync` also calls `timelineHudList(ev)`: the co-op player
//    status list above-left of the first diamond (see timeline_list.rs).
//
//    `mpHudUnit` / `mpHudLabel` / `mpHudW` and the list's fields are appended
//    to TimelineEvent (it has no subclass, so no field index moves).
//
// 5. Diagnostics (shim log, via Sys.println -> hl_sys_print):
//    - "mp: timelineHud: why=<n> started=<unit name|null> shown=<portrait|swords>"
//      whenever startedPlaying or the gate result changes (why: 0 shown,
//      1 no battle/state, 2 nobody acting, 3 no owner, 4 not the player side,
//      5 not the Timeline's first diamond, 6 kept after turn end);
//    - "mp: timelineHud error: <exception>" / "mp: timelineHudList error: ..."
//      from the catch blocks (timelineHudReport, at most once a second per diamond);
//    - "mp: timelineHud: portrait reset by the game, re-applied" when the event
//      bitmap's tile no longer is the portrait set (mpHudTile) while the same
//      unit acts; the portrait is then put back.
//    mpHudUnit is stored only after a successful swap, so a failure is retried.
//
// Every String the new functions use is a constant global (GetGlobal, as the
// game's own code does). The `String` opcode yields the raw UTF-16 bytes, and
// the JIT trusts the register type: the hud4/hud5 builds put those bytes in
// String registers, so the first message (and the "default" font name) raised
// an access violation inside the trap, the report did the same inside its own
// trap, and nothing was ever printed or drawn. asm::check_types now refuses a
// `String` op into anything but a bytes register.
//
// Validated before editing; a mismatch skips the pass (logged).

#[path = "timeline_list.rs"]
mod list;

use super::*;
use crate::asm::{push_fn, string_ref, Asm, Regs};
use crate::job_xp::str_global;
use hlbc::types::{ObjField, RefGlobal, ValBool};

const UNIT_FIELD: &str = "mpHudUnit";
const LABEL_FIELD: &str = "mpHudLabel";
const WIDTH_FIELD: &str = "mpHudW";

struct Plan {
    ctor_fi: usize,
    /// The constructor's `Switch` choosing between masking `event` and removing `maskBitmap`.
    mask_switch: usize,
    sync_fi: usize,
    /// `sync` op 0's void destination (the super sync result).
    sync_void: Reg,
    ev_t: RefType,
    void_: RefType,
    bool_: RefType,
    i32_: RefType,
    f64_: RefType,
    dyn_t: RefType,
    str_t: RefType,
    elt_t: RefType,
    unit_t: RefType,
    html_t: RefType,
    bitmap_t: RefType,
    filter_t: RefType,
    font_t: RefType,
    fprops_t: RefType,
    shadow_t: RefType,
    battle_t: RefType,
    state_t: RefType,
    player_t: RefType,
    side_t: RefType,
    obj_t: RefType,
    tl_t: RefType,
    arr_t: RefType,
    raw_t: RefType,
    tile_t: RefType,
    api_t: RefType,
    sunit_t: RefType,
    bp_t: RefType,
    game_t: RefType,
    ev_elt: RefField,
    ev_battle: RefField,
    ev_event: RefField,
    ev_game: RefField,
    o_parent: RefField,
    o_filter: RefField,
    b_state: RefField,
    s_started: RefField,
    s_side: RefField,
    /// `battle.State.timelines`: the live turn order, head = `timelines[0][0]`.
    s_timelines: RefField,
    u_owner: RefField,
    u_data: RefField,
    p_side: RefField,
    su_owner: RefField,
    tl_events: RefField,
    a_len: RefField,
    a_raw: RefField,
    t_shadow: RefField,
    sh_alpha: RefField,
    sh_color: RefField,
    sh_dx: RefField,
    sh_dy: RefField,
    tl_cls: RefGlobal,
    tl_cls_t: RefType,
    /// Name font chain (see `add_name_font`).
    ldr_cls: RefGlobal,
    ldr_cls_t: RefType,
    ldr_cur: RefField,
    loader_t: RefType,
    res_load: RefFun,
    any_t: RefType,
    any_to: RefFun,
    resource_t: RefType,
    bf_t: RefType,
    bf_cls: RefGlobal,
    bf_cls_t: RefType,
    to_sdf: RefFun,
    nint_t: RefType,
    ref_i32_t: RefType,
    ref_f64_t: RefType,
    base_check: RefFun,
    set_enable: RefFun,
    load_font: RefFun,
    html_ctor: RefFun,
    html_set_text: RefFun,
    get_props: RefFun,
    set_absolute: RefFun,
    set_tile: RefFun,
    get_api: RefFun,
    api_get_icon: RefFun,
    unit_get_icon: RefFun,
    is_multi: RefFun,
    bp_get_name: RefFun,
    outer_width: RefFun,
    /// `Sys.println(Dyn)`: reaches the shim log through `hl_sys_print`.
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    sys_time: RefFun,
    /// `h2d.Bitmap.tile` (read to detect a reset by the style system).
    bm_tile: RefField,
    /// `st.Unit.name`.
    su_name: RefField,
    text_width: RefFun,
    text_height: RefFun,
    set_x: RefFun,
    set_y: RefFun,
    dbg_file: usize,
}

/// The only function `name` with exactly this signature (statics included).
fn by_sig(code: &Bytecode, name: &str, args: &[RefType], ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == name
                && f.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == args && t.ret == ret)
        })
        .map(|f| f.findex)
        .collect();
    let [f] = hits[..] else {
        bail!(
            "{name}: expected one function with the expected signature, found {}",
            hits.len()
        );
    };
    Ok(f)
}

fn virtual_field(code: &Bytecode, t: RefType, name: &str, want: RefType) -> Result<RefField> {
    let Type::Virtual { fields } = &code.types[t.0] else {
        bail!("type {} is not a virtual", t.0);
    };
    let i = fields
        .iter()
        .position(|f| s(code, f.name) == name && f.t == want)
        .with_context(|| format!("virtual field {name} not found"))?;
    Ok(RefField(i))
}

/// The constructor's `Switch (elt) { ... }` that picks `masked = true / false`,
/// followed by `if (masked) event.filter = new h2d.filter.Mask(maskBitmap)`.
/// Returns its op index; the Player case must still go to `masked = false`.
fn find_mask_switch(ctor: &Function, mask_t: RefType) -> Result<usize> {
    let o = &ctor.ops;
    let at = |i: usize, off: i32| (i as i64 + 1 + off as i64) as usize;
    let mut hits = vec![];
    for (i, op) in o.iter().enumerate() {
        let Opcode::Switch { offsets, end, .. } = op else {
            continue;
        };
        if offsets.len() != 6 {
            continue;
        }
        let join = at(i, *end);
        let (Some(Opcode::Bool { dst: b0, value: v0 }), Some(Opcode::Bool { dst: b1, value: v1 })) =
            (o.get(at(i, offsets[0])), o.get(at(i, offsets[1])))
        else {
            continue;
        };
        let masked_test = matches!(o.get(join), Some(Opcode::JFalse { cond, .. }) if cond == b0);
        let builds_mask = o.get(join..join + 4).is_some_and(|w| {
            w.iter()
                .any(|x| matches!(x, Opcode::New { dst } if ctor.regs[dst.0 as usize] == mask_t))
        });
        if b0 == b1 && masked_test && builds_mask {
            hits.push((i, v0.0, v1.0));
        }
    }
    let [(i, player_masked, ai_masked)] = hits[..] else {
        bail!(
            "TimelineEvent constructor: expected one mask switch, found {}",
            hits.len()
        );
    };
    if player_masked || !ai_masked {
        bail!("TimelineEvent constructor: the Player case is already masked");
    }
    Ok(i)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let void_ = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let bool_ = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_ = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let f64_ = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let ev_t = obj_type(code, "battle.ui.win.TimelineEvent")?;
    let tl_t = obj_type(code, "battle.ui.win.Timeline")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let state_t = obj_type(code, "battle.State")?;
    let player_t = obj_type(code, "battle.Player")?;
    let sunit_t = obj_type(code, "st.Unit")?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let game_t = obj_type(code, "Game")?;
    let api_t = obj_type(code, "ui.BaseUIApi")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let text_t = obj_type(code, "h2d.Text")?;
    let html_t = obj_type(code, "h2d.HtmlText")?;
    let font_t = obj_type(code, "h2d.Font")?;
    let fprops_t = obj_type(code, "h2d.FlowProperties")?;
    let filter_t = obj_type(code, "h2d.filter.Filter")?;
    let mask_t = obj_type(code, "h2d.filter.Mask")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let str_t = obj_type(code, "String")?;
    if !is_sub(code, ev_t, flow_t) || !is_sub(code, html_t, text_t) || !is_sub(code, flow_t, obj_t)
    {
        bail!("unexpected TimelineEvent / HtmlText hierarchy");
    }
    if !is_sub(code, mask_t, filter_t) {
        bail!("h2d.filter.Mask is not a Filter");
    }
    // Appending fields is only safe when no subclass inherits the layout.
    if code
        .types
        .iter()
        .any(|t| matches!(t, Type::Obj(o) if o.super_ == Some(ev_t)))
    {
        bail!("TimelineEvent has a subclass");
    }
    let ev_o = obj(code, ev_t)?;
    if ev_o.fields.iter().any(|f| {
        [UNIT_FIELD, LABEL_FIELD, WIDTH_FIELD]
            .iter()
            .chain(list::FIELDS.iter().map(|(n, _)| n))
            .any(|n| *n == s(code, f.name))
    }) {
        bail!("TimelineEvent already has the HUD fields");
    }

    let (ev_elt, elt_t) = field(code, ev_t, "elt")?;
    if !matches!(&code.types[elt_t.0], Type::Enum { constructs, .. }
        if constructs.first().is_some_and(|c| s(code, c.name) == "Player" && c.params == [player_t]))
    {
        bail!("TimelineElement construct 0 is not Player(battle.Player)");
    }
    let (ev_battle, battle_t) = field(code, ev_t, "battle")?;
    let (ev_event, bitmap_t) = field(code, ev_t, "event")?;
    let (_, mask_bmp_t) = field(code, ev_t, "maskBitmap")?;
    if mask_bmp_t != bitmap_t || s(code, obj(code, bitmap_t)?.name) != "h2d.Bitmap" {
        bail!("TimelineEvent.event is not an h2d.Bitmap");
    }
    let (ev_game, eg_t) = field(code, ev_t, "game")?;
    let (o_parent, op_t) = field(code, obj_t, "parent")?;
    let (o_filter, of_t) = field(code, obj_t, "filter")?;
    let (b_state, bs_t) = field(code, battle_t, "state")?;
    let (s_started, ss_t) = field(code, state_t, "startedPlaying")?;
    let (s_side, side_t) = field(code, state_t, "playerSide")?;
    let (s_timelines, stl_t) = field(code, state_t, "timelines")?;
    let (u_owner, uo_t) = field(code, unit_t, "owner")?;
    let (u_data, ud_t) = field(code, unit_t, "data")?;
    let (p_side, ps_t) = field(code, player_t, "side")?;
    let (su_owner, suo_t) = field(code, sunit_t, "owner")?;
    let (tl_events, te_t) = field(code, tl_t, "eventsElts")?;
    let (a_len, al_t) = field(code, arr_t, "length")?;
    let (a_raw, raw_t) = field(code, arr_t, "array")?;
    let (t_shadow, shadow_t) = field(code, text_t, "dropShadow")?;
    if eg_t != game_t
        || op_t != obj_t
        || of_t != filter_t
        || bs_t != state_t
        || ss_t != unit_t
        || uo_t != player_t
        || ud_t != sunit_t
        || ps_t != side_t
        || suo_t != bp_t
        || te_t != arr_t
        || stl_t != arr_t
        || al_t != i32_
    {
        bail!("unexpected field types on the timeline / unit / player path");
    }
    let sh_alpha = virtual_field(code, shadow_t, "alpha", f64_)?;
    let sh_color = virtual_field(code, shadow_t, "color", i32_)?;
    let sh_dx = virtual_field(code, shadow_t, "dx", f64_)?;
    let sh_dy = virtual_field(code, shadow_t, "dy", f64_)?;

    let m = |o: RefType, name: &str, args: &[RefType], ret: RefType| -> Result<RefFun> {
        let f = method(code, o, name)?.findex;
        if sig(code, f)? != (args.to_vec(), ret) {
            bail!("unexpected {name} signature");
        }
        Ok(f)
    };
    let tile_t = obj_type(code, "h2d.Tile")?;
    let set_enable = m(filter_t, "set_enable", &[filter_t, bool_], bool_)?;
    let html_ctor = m(html_t, "__constructor__", &[html_t, font_t, obj_t], void_)?;
    let html_set_text = m(html_t, "set_text", &[html_t, str_t], str_t)?;
    let get_props = m(flow_t, "getProperties", &[flow_t, obj_t], fprops_t)?;
    let set_absolute = m(fprops_t, "set_isAbsolute", &[fprops_t, bool_], bool_)?;
    let set_tile = m(bitmap_t, "set_tile", &[bitmap_t, tile_t], tile_t)?;
    let api_get_icon = m(api_t, "getIcon", &[api_t, str_t], tile_t)?;
    let unit_get_icon = m(sunit_t, "getIcon", &[sunit_t], tile_t)?;
    let is_multi = m(game_t, "get_isMulti", &[game_t], bool_)?;
    let bp_get_name = m(bp_t, "getName", &[bp_t], str_t)?;
    let outer_width = m(flow_t, "get_outerWidth", &[flow_t], i32_)?;
    let text_width = m(text_t, "get_textWidth", &[text_t], f64_)?;
    let text_height = m(text_t, "get_textHeight", &[text_t], f64_)?;
    let set_x = m(obj_t, "set_x", &[obj_t, f64_], f64_)?;
    let set_y = m(obj_t, "set_y", &[obj_t, f64_], f64_)?;
    let println = crate::diag::static_fn(code, "$Sys", "println")?.findex;
    if sig(code, println)? != (vec![dyn_t], void_) {
        bail!("Sys.println is not Dyn -> Void");
    }
    let std_string = crate::diag::static_fn(code, "$Std", "string")?.findex;
    if sig(code, std_string)? != (vec![dyn_t], str_t) {
        bail!("Std.string is not Dyn -> String");
    }
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t) {
        bail!("String.__add__ is not (String, String) -> String");
    }
    let sys_time = {
        let hits: Vec<RefFun> = code
            .natives
            .iter()
            .filter(|n| {
                s(code, n.name) == "sys_time"
                    && n.t
                        .as_fun(code)
                        .is_some_and(|t| t.args.is_empty() && t.ret == f64_)
            })
            .map(|n| n.findex)
            .collect();
        let [f] = hits[..] else {
            bail!("expected one native sys_time, found {}", hits.len());
        };
        f
    };
    let (bm_tile, bt_t) = field(code, bitmap_t, "tile")?;
    let (su_name, sn_t) = field(code, sunit_t, "name")?;
    if bt_t != tile_t || sn_t != str_t {
        bail!("unexpected h2d.Bitmap.tile / st.Unit.name types");
    }
    let load_font = by_sig(code, "loadFont", &[str_t], font_t)?;
    let get_api = by_sig(code, "get_api", &[], api_t)?;
    let base_t = obj_type(code, "hl.BaseType")?;
    let check = method(code, base_t, "check")?;
    if fun_args(code, check).len() != 2 || check.t.as_fun(code).map(|f| f.ret) != Some(bool_) {
        bail!("hl.BaseType.check is not (BaseType, v) -> Bool");
    }
    let base_check = check.findex;
    let (tl_cls, tl_cls_t) = class_global(code, "battle.ui.win.Timeline")?;

    // The players panel's name font, `font: 'ui/fonts/eb_garamond_medium.fnt'
    // 16 multi 0.5 0.45` (style.css, players-panel .players-list .player text),
    // loaded the way h2d.domkit.CustomParser.parseFont does:
    // Loader.currentInstance.load(path).to(BitmapFont).toSdfFont(size, channel, cutoff, smooth).
    let (ldr_cls, ldr_cls_t) = class_global(code, "hxd.res.Loader")?;
    let (ldr_cur, loader_t) = field(code, ldr_cls_t, "currentInstance")?;
    let res_load = method(code, loader_t, "load")?.findex;
    let (la, any_t) = sig(code, res_load)?;
    if la != [loader_t, str_t] {
        bail!("unexpected hxd.res.Loader.load signature");
    }
    let any_to = method(code, any_t, "to")?.findex;
    let (ta, resource_t) = sig(code, any_to)?;
    let bf_t = obj_type(code, "hxd.res.BitmapFont")?;
    let (bf_cls, bf_cls_t) = class_global(code, "hxd.res.BitmapFont")?;
    if ta.len() != 2 || ta[0] != any_t {
        bail!("unexpected hxd.res.Any.to signature");
    }
    let to_sdf = method(code, bf_t, "toSdfFont")?.findex;
    let (sa, sr) = sig(code, to_sdf)?;
    let (nint_t, ref_i32_t, ref_f64_t) = match sa[..] {
        [b, n, ri, rc, rs]
            if b == bf_t
                && sr == font_t
                && rc == rs
                && matches!(code.types[n.0], Type::Null(x) if x == i32_)
                && matches!(code.types[ri.0], Type::Ref(x) if x == i32_)
                && matches!(code.types[rc.0], Type::Ref(x) if x == f64_) =>
        {
            (n, ri, rc)
        }
        _ => bail!("unexpected BitmapFont.toSdfFont signature"),
    };

    // Constructor: the Player case of the mask switch.
    let ctor = method(code, ev_t, "__constructor__")?;
    let ctor_fi = fun_index(code, ctor.findex)?;
    let mask_switch = find_mask_switch(ctor, mask_t)?;
    if !ctor
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == api_get_icon))
    {
        bail!("TimelineEvent constructor draws no getIcon tile");
    }
    if !ctor
        .ops
        .iter()
        .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == unit_get_icon))
    {
        bail!("TimelineEvent constructor draws no unit portrait");
    }

    // sync: `Call2 v = Element.sync(this, ctx); GetThis elt = this.elt; ...`.
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let super_sync = proto(code, elem_t, "sync")?;
    let sync = code
        .functions
        .iter()
        .find(|f| f.findex == proto(code, ev_t, "sync").unwrap_or(RefFun(usize::MAX)))
        .context("TimelineEvent.sync not found")?;
    let sync_fi = fun_index(code, sync.findex)?;
    let sync_void = match (&sync.ops.first(), &sync.ops.get(1)) {
        (
            Some(Opcode::Call2 {
                dst,
                fun,
                arg0: Reg(0),
                arg1: Reg(1),
            }),
            Some(Opcode::GetThis { field, .. }),
        ) if *fun == super_sync && *field == ev_elt && sync.regs[dst.0 as usize] == void_ => *dst,
        _ => bail!("TimelineEvent.sync does not start with `super.sync(ctx); this.elt`"),
    };

    Ok(Plan {
        ctor_fi,
        mask_switch,
        sync_fi,
        sync_void,
        ev_t,
        void_,
        bool_,
        i32_,
        f64_,
        dyn_t,
        str_t,
        elt_t,
        unit_t,
        html_t,
        bitmap_t,
        filter_t,
        font_t,
        fprops_t,
        shadow_t,
        battle_t,
        state_t,
        player_t,
        side_t,
        obj_t,
        tl_t,
        arr_t,
        raw_t,
        tile_t,
        api_t,
        sunit_t,
        bp_t,
        game_t,
        ev_elt,
        ev_battle,
        ev_event,
        ev_game,
        o_parent,
        o_filter,
        b_state,
        s_started,
        s_side,
        s_timelines,
        u_owner,
        u_data,
        p_side,
        su_owner,
        tl_events,
        a_len,
        a_raw,
        t_shadow,
        sh_alpha,
        sh_color,
        sh_dx,
        sh_dy,
        tl_cls,
        tl_cls_t,
        ldr_cls,
        ldr_cls_t,
        ldr_cur,
        loader_t,
        res_load,
        any_t,
        any_to,
        resource_t,
        bf_t,
        bf_cls,
        bf_cls_t,
        to_sdf,
        nint_t,
        ref_i32_t,
        ref_f64_t,
        base_check,
        set_enable,
        load_font,
        html_ctor,
        html_set_text,
        get_props,
        set_absolute,
        set_tile,
        get_api,
        api_get_icon,
        unit_get_icon,
        is_multi,
        bp_get_name,
        outer_width,
        println,
        std_string,
        str_add,
        sys_time,
        bm_tile,
        su_name,
        text_width,
        text_height,
        set_x,
        set_y,
        dbg_file: debug_file(code, "src/battle/ui/win/Timeline.hx")?,
    })
}

/// Scratch registers of [`emit_first_check`].
struct FirstRegs {
    par: Reg,
    tlc: Reg,
    b: Reg,
    tl: Reg,
    arr: Reg,
    n: Reg,
    raw: Reg,
    d: Reg,
    first: Reg,
    /// Holds int 0 already.
    zero: Reg,
}

/// Falls through when `ev` (register 0) is its Timeline's first diamond
/// (`eventsElts[0]`), else jumps to `fail`. Uses labels "up", "next", "found".
///
/// ```text
/// par = ev.parent;
/// while (par != null) { if (Std.isOfType(par, Timeline)) break; par = par.parent; }
/// if (par == null || tl.eventsElts == null || tl.eventsElts.length <= 0 || eventsElts[0] != ev) goto fail;
/// ```
fn emit_first_check(a: &mut Asm, p: &Plan, r: FirstRegs, fail: &'static str) {
    let ev = Reg(0);
    a.op(Opcode::Field {
        dst: r.par,
        obj: ev,
        field: p.o_parent,
    });
    a.loop_head("up");
    a.jmp(
        Opcode::JNull {
            reg: r.par,
            offset: 0,
        },
        "found",
    );
    a.op(Opcode::GetGlobal {
        dst: r.tlc,
        global: p.tl_cls,
    });
    a.op(Opcode::Call2 {
        dst: r.b,
        fun: p.base_check,
        arg0: r.tlc,
        arg1: r.par,
    });
    a.jmp(
        Opcode::JFalse {
            cond: r.b,
            offset: 0,
        },
        "next",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "found");
    a.label("next");
    a.op(Opcode::Field {
        dst: r.par,
        obj: r.par,
        field: p.o_parent,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "up");
    a.label("found");
    a.jmp(
        Opcode::JNull {
            reg: r.par,
            offset: 0,
        },
        fail,
    );
    a.op(Opcode::UnsafeCast {
        dst: r.tl,
        src: r.par,
    });
    a.op(Opcode::Field {
        dst: r.arr,
        obj: r.tl,
        field: p.tl_events,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.arr,
            offset: 0,
        },
        fail,
    );
    a.op(Opcode::Field {
        dst: r.n,
        obj: r.arr,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: r.zero,
            b: r.n,
            offset: 0,
        },
        fail,
    );
    a.op(Opcode::Field {
        dst: r.raw,
        obj: r.arr,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: r.d,
        array: r.raw,
        index: r.zero,
    });
    a.op(Opcode::UnsafeCast {
        dst: r.first,
        src: r.d,
    });
    a.jmp(
        Opcode::JNotEq {
            a: r.first,
            b: ev,
            offset: 0,
        },
        fail,
    );
}

/// The TimelineEvent fields this pass appends, in order.
struct Fields {
    unit: RefField,
    label: RefField,
    hud_w: RefField,
    list: list::ListFields,
    /// Last gate result traced (0 = portrait shown).
    why: RefField,
    /// Last `startedPlaying` traced.
    seen: RefField,
    /// The portrait tile set, to re-apply it when the game resets the bitmap.
    tile: RefField,
    /// `Sys.time()` of the last rate-limited report.
    rep_t: RefField,
}

const WHY_FIELD: &str = "mpHudWhy";
const SEEN_FIELD: &str = "mpHudSeen";
const TILE_FIELD: &str = "mpHudTile";
const REPORT_FIELD: &str = "mpHudRepT";

/// `timelineHudReport(ev, msg, exc)`: at most once a second per diamond,
/// `Sys.println(exc == null ? msg : msg + Std.string(exc))`; never throws.
/// The catch blocks of timelineHud / timelineHudList end here, so an error in
/// either reaches the shim log instead of vanishing.
fn add_report(code: &mut Bytecode, p: &Plan, fl: &Fields) -> Result<RefFun> {
    let one = float_const(code, 1.0);
    let mut r = Regs(vec![p.ev_t, p.str_t, p.dyn_t]);
    let (msg, exc) = (Reg(1), Reg(2));
    let (v, now, last, lim, txt, e2) = (
        r.r(p.void_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.str_t),
        r.r(p.dyn_t),
    );
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc: e2, offset: 0 }, "catch");
    a.op(Opcode::Call0 {
        dst: now,
        fun: p.sys_time,
    });
    a.op(Opcode::GetThis {
        dst: last,
        field: fl.rep_t,
    });
    a.op(Opcode::Sub {
        dst: last,
        a: now,
        b: last,
    });
    a.op(Opcode::Float { dst: lim, ptr: one });
    a.jmp(
        Opcode::JSLt {
            a: last,
            b: lim,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::SetThis {
        field: fl.rep_t,
        src: now,
    });
    a.jmp(
        Opcode::JNull {
            reg: exc,
            offset: 0,
        },
        "print",
    );
    a.op(Opcode::Call1 {
        dst: txt,
        fun: p.std_string,
        arg0: exc,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.label("print");
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc: e2 });
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::Ret { ret: v });
    push_fn(
        code,
        vec![p.ev_t, p.str_t, p.dyn_t],
        p.void_,
        r.0,
        a.finish(),
        p.dbg_file,
    )
}

/// `nameFont()`: the players panel's name font, built once and kept in a new
/// global; on any failure the HUD's former `BaseUI.loadFont("default")`.
///
/// ```text
/// if (G == null) {
///   try { G = Loader.currentInstance.load(NAME_FONT).to(BitmapFont).toSdfFont(16, 4 /*multi*/, 0.5, 0.45); }
///   catch (e) { Sys.println("mp: nameFont error: " + e); G = BaseUI.loadFont("default"); }
/// }
/// return G;
/// ```
fn add_name_font(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    code.globals.push(p.font_t);
    let g = RefGlobal(code.globals.len() - 1);
    let s_path = str_global(code, p.str_t, NAME_FONT);
    let s_err = str_global(code, p.str_t, "mp: nameFont error: ");
    let s_default = str_global(code, p.str_t, "default");
    let c_size = int_const(code, NAME_FONT_SIZE);
    let c_multi = int_const(code, 4);
    let c_cut = float_const(code, 0.5);
    let c_smooth = float_const(code, 0.45);
    let mut r = Regs(vec![]);
    let (font, exc, lc, ld, path, any, cls, res, bf) = (
        r.r(p.font_t),
        r.r(p.dyn_t),
        r.r(p.ldr_cls_t),
        r.r(p.loader_t),
        r.r(p.str_t),
        r.r(p.any_t),
        r.r(p.bf_cls_t),
        r.r(p.resource_t),
        r.r(p.bf_t),
    );
    let (i, size, ch, rch, cut, rcut, sm, rsm, msg, txt, v) = (
        r.r(p.i32_),
        r.r(p.nint_t),
        r.r(p.i32_),
        r.r(p.ref_i32_t),
        r.r(p.f64_),
        r.r(p.ref_f64_t),
        r.r(p.f64_),
        r.r(p.ref_f64_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.void_),
    );
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: font,
        global: g,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: font,
            offset: 0,
        },
        "ret",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.op(Opcode::GetGlobal {
        dst: lc,
        global: p.ldr_cls,
    });
    a.op(Opcode::Field {
        dst: ld,
        obj: lc,
        field: p.ldr_cur,
    });
    a.op(Opcode::GetGlobal {
        dst: path,
        global: s_path,
    });
    a.op(Opcode::Call2 {
        dst: any,
        fun: p.res_load,
        arg0: ld,
        arg1: path,
    });
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: p.bf_cls,
    });
    a.op(Opcode::Call2 {
        dst: res,
        fun: p.any_to,
        arg0: any,
        arg1: cls,
    });
    a.op(Opcode::SafeCast { dst: bf, src: res });
    a.op(Opcode::Int {
        dst: i,
        ptr: c_size,
    });
    a.op(Opcode::ToDyn { dst: size, src: i });
    a.op(Opcode::Int {
        dst: ch,
        ptr: c_multi,
    });
    a.op(Opcode::Ref { dst: rch, src: ch });
    a.op(Opcode::Float {
        dst: cut,
        ptr: c_cut,
    });
    a.op(Opcode::Ref {
        dst: rcut,
        src: cut,
    });
    a.op(Opcode::Float {
        dst: sm,
        ptr: c_smooth,
    });
    a.op(Opcode::Ref { dst: rsm, src: sm });
    a.op(Opcode::CallN {
        dst: font,
        fun: p.to_sdf,
        args: vec![bf, size, rch, rcut, rsm],
    });
    a.op(Opcode::EndTrap { exc });
    a.jmp(
        Opcode::JNotNull {
            reg: font,
            offset: 0,
        },
        "store",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "fallback");
    a.label("catch");
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: s_err,
    });
    a.op(Opcode::Call1 {
        dst: txt,
        fun: p.std_string,
        arg0: exc,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("fallback");
    a.op(Opcode::GetGlobal {
        dst: path,
        global: s_default,
    });
    a.op(Opcode::Call1 {
        dst: font,
        fun: p.load_font,
        arg0: path,
    });
    a.label("store");
    a.op(Opcode::SetGlobal {
        global: g,
        src: font,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: font });
    push_fn(code, vec![], p.font_t, r.0, a.finish(), p.dbg_file)
}

/// The font the game's players panel draws player names with (style.css:
/// `players-panel .players-list .player text`).
const NAME_FONT: &str = "ui/fonts/eb_garamond_medium.fnt";
const NAME_FONT_SIZE: i32 = 16;

/// `timelineHud(ev)` (see the header).
fn add_hud(
    code: &mut Bytecode,
    p: &Plan,
    fl: &Fields,
    report: RefFun,
    name_font: RefFun,
) -> Result<RefFun> {
    let (f_unit, f_label, f_w) = (fl.unit, fl.label, fl.hud_w);
    let (f_why, f_seen, f_tile) = (fl.why, fl.seen, fl.tile);
    let i0 = int_const(code, 0);
    let why_c: Vec<_> = (1..=6).map(|k| int_const(code, k)).collect();
    let s_trace = str_global(code, p.str_t, "mp: timelineHud: why=");
    let s_started = str_global(code, p.str_t, " started=");
    let s_shown = str_global(code, p.str_t, " shown=");
    let s_portrait = str_global(code, p.str_t, "portrait");
    let s_swords = str_global(code, p.str_t, "swords");
    let s_reset = str_global(
        code,
        p.str_t,
        "mp: timelineHud: portrait reset by the game, re-applied",
    );
    let s_err = str_global(code, p.str_t, "mp: timelineHud error: ");
    let one = float_const(code, 1.0);
    let shadow_alpha = float_const(code, 0.8);
    let half = float_const(code, 0.5);
    let s_icon = str_global(code, p.str_t, "TimelineIcon");
    let s_empty = str_global(code, p.str_t, "");

    let mut r = Regs(vec![p.ev_t]);
    let ev = Reg(0);
    let (v, b, zero, idx, elt, exc) = (
        r.r(p.void_),
        r.r(p.bool_),
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.elt_t),
        r.r(p.dyn_t),
    );
    let (lbl, bmp, filt, name, font, props, sh, fl, ci) = (
        r.r(p.html_t),
        r.r(p.bitmap_t),
        r.r(p.filter_t),
        r.r(p.str_t),
        r.r(p.font_t),
        r.r(p.fprops_t),
        r.r(p.shadow_t),
        r.r(p.f64_),
        r.r(p.i32_),
    );
    let (u, cand, cur, bat, st, owner, s1, s2) = (
        r.r(p.unit_t),
        r.r(p.unit_t),
        r.r(p.unit_t),
        r.r(p.battle_t),
        r.r(p.state_t),
        r.r(p.player_t),
        r.r(p.side_t),
        r.r(p.side_t),
    );
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
    let (icon, tile, api, sdata, bp, game, w, tw, th, hf) = (
        r.r(p.str_t),
        r.r(p.tile_t),
        r.r(p.api_t),
        r.r(p.sunit_t),
        r.r(p.bp_t),
        r.r(p.game_t),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
        r.r(p.f64_),
    );
    let (wi, wo) = (r.r(p.i32_), r.r(p.i32_));
    let (kb, uold, tls, line, hd) = (
        r.r(p.bool_),
        r.r(p.unit_t),
        r.r(p.arr_t),
        r.r(p.arr_t),
        r.r(p.elt_t),
    );
    let (why, oldw, seen, msg, txt, dd, t1, t2) = (
        r.r(p.i32_),
        r.r(p.i32_),
        r.r(p.unit_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.dyn_t),
        r.r(p.tile_t),
        r.r(p.tile_t),
    );
    let set_why = |a: &mut Asm, k: usize| {
        if k == 0 {
            a.op(Opcode::Int { dst: why, ptr: i0 });
        } else {
            a.op(Opcode::Int {
                dst: why,
                ptr: why_c[k - 1],
            });
        }
    };

    let mut a = Asm::new();
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
    a.op(Opcode::Int { dst: zero, ptr: i0 });
    a.jmp(
        Opcode::JNotEq {
            a: idx,
            b: zero,
            offset: 0,
        },
        "ret",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");

    // Once per diamond: swords unmasked as in vanilla, the label created empty.
    a.op(Opcode::GetThis {
        dst: lbl,
        field: f_label,
    });
    a.jmp(
        Opcode::JNotNull {
            reg: lbl,
            offset: 0,
        },
        "ready",
    );
    a.op(Opcode::GetThis {
        dst: bmp,
        field: p.ev_event,
    });
    a.jmp(
        Opcode::JNull {
            reg: bmp,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: filt,
        obj: bmp,
        field: p.o_filter,
    });
    a.jmp(
        Opcode::JNull {
            reg: filt,
            offset: 0,
        },
        "mk",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_enable,
        arg0: filt,
        arg1: b,
    });
    a.label("mk");
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
    a.op(Opcode::Float { dst: fl, ptr: one });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_dx,
        src: fl,
    });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_dy,
        src: fl,
    });
    a.op(Opcode::Int { dst: ci, ptr: i0 });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_color,
        src: ci,
    });
    a.op(Opcode::Float {
        dst: fl,
        ptr: shadow_alpha,
    });
    a.op(Opcode::SetField {
        obj: sh,
        field: p.sh_alpha,
        src: fl,
    });
    a.op(Opcode::SetField {
        obj: lbl,
        field: p.t_shadow,
        src: sh,
    });
    a.op(Opcode::SetThis {
        field: f_label,
        src: lbl,
    });

    // u = the acting player-side unit, on the Timeline's first diamond only.
    // why: 1 no battle / state, 2 nobody acting (startedPlaying null), 3 the
    // acting unit has no owner, 4 it is not on the player side, 5 this is not
    // the Timeline's first diamond, 0 shown.
    a.label("ready");
    a.op(Opcode::Null { dst: u });
    a.op(Opcode::Null { dst: cand });
    set_why(&mut a, 1);
    a.op(Opcode::GetThis {
        dst: bat,
        field: p.ev_battle,
    });
    a.jmp(
        Opcode::JNull {
            reg: bat,
            offset: 0,
        },
        "decide",
    );
    a.op(Opcode::Field {
        dst: st,
        obj: bat,
        field: p.b_state,
    });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "decide");
    a.op(Opcode::Field {
        dst: cand,
        obj: st,
        field: p.s_started,
    });
    set_why(&mut a, 2);
    a.op(Opcode::Bool {
        dst: kb,
        value: ValBool(false),
    });
    a.jmp(
        Opcode::JNotNull {
            reg: cand,
            offset: 0,
        },
        "acting",
    );
    // Nobody acting (end turn clears startedPlaying before the timeline
    // shifts): keep the portrait already on this diamond while its event is
    // still the live head of the turn order, timelines[0][0]. A shift, a
    // reused instance for another slot or a new round's timeline is another
    // enum value, so the swords come back then.
    a.op(Opcode::GetThis {
        dst: uold,
        field: f_unit,
    });
    a.jmp(
        Opcode::JNull {
            reg: uold,
            offset: 0,
        },
        "decide",
    );
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
        "decide",
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
        "decide",
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
    a.op(Opcode::UnsafeCast { dst: line, src: d });
    a.jmp(
        Opcode::JNull {
            reg: line,
            offset: 0,
        },
        "decide",
    );
    a.op(Opcode::Field {
        dst: n,
        obj: line,
        field: p.a_len,
    });
    a.jmp(
        Opcode::JSGte {
            a: zero,
            b: n,
            offset: 0,
        },
        "decide",
    );
    a.op(Opcode::Field {
        dst: raw,
        obj: line,
        field: p.a_raw,
    });
    a.op(Opcode::GetArray {
        dst: d,
        array: raw,
        index: zero,
    });
    a.op(Opcode::UnsafeCast { dst: hd, src: d });
    a.jmp(
        Opcode::JNotEq {
            a: hd,
            b: elt,
            offset: 0,
        },
        "decide",
    );
    a.op(Opcode::Bool {
        dst: kb,
        value: ValBool(true),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "chkfirst");
    a.label("acting");
    a.op(Opcode::Field {
        dst: owner,
        obj: cand,
        field: p.u_owner,
    });
    set_why(&mut a, 3);
    a.jmp(
        Opcode::JNull {
            reg: owner,
            offset: 0,
        },
        "decide",
    );
    set_why(&mut a, 4);
    a.op(Opcode::Field {
        dst: s1,
        obj: owner,
        field: p.p_side,
    });
    a.op(Opcode::Field {
        dst: s2,
        obj: st,
        field: p.s_side,
    });
    a.jmp(
        Opcode::JNotEq {
            a: s1,
            b: s2,
            offset: 0,
        },
        "decide",
    );
    set_why(&mut a, 5);
    a.label("chkfirst");
    emit_first_check(
        &mut a,
        p,
        FirstRegs {
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
        "decide",
    );
    a.jmp(
        Opcode::JTrue {
            cond: kb,
            offset: 0,
        },
        "keepit",
    );
    a.op(Opcode::Mov { dst: u, src: cand });
    set_why(&mut a, 0);
    a.jmp(Opcode::JAlways { offset: 0 }, "decide");
    a.label("keepit");
    a.op(Opcode::Mov { dst: u, src: uold });
    set_why(&mut a, 6);

    // Trace: one line whenever startedPlaying or the gate result changes,
    // "mp: timelineHud: why=<n> started=<unit name|null> shown=<portrait|swords>".
    a.label("decide");
    a.op(Opcode::GetThis {
        dst: seen,
        field: f_seen,
    });
    a.jmp(
        Opcode::JNotEq {
            a: seen,
            b: cand,
            offset: 0,
        },
        "trace",
    );
    a.op(Opcode::GetThis {
        dst: oldw,
        field: f_why,
    });
    a.jmp(
        Opcode::JEq {
            a: oldw,
            b: why,
            offset: 0,
        },
        "traced",
    );
    a.label("trace");
    a.op(Opcode::SetThis {
        field: f_seen,
        src: cand,
    });
    a.op(Opcode::SetThis {
        field: f_why,
        src: why,
    });
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: s_trace,
    });
    a.op(Opcode::ToDyn { dst: dd, src: why });
    a.op(Opcode::Call1 {
        dst: txt,
        fun: p.std_string,
        arg0: dd,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: s_started,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::Null { dst: txt });
    a.jmp(
        Opcode::JNull {
            reg: cand,
            offset: 0,
        },
        "named0",
    );
    a.op(Opcode::Field {
        dst: sdata,
        obj: cand,
        field: p.u_data,
    });
    a.jmp(
        Opcode::JNull {
            reg: sdata,
            offset: 0,
        },
        "named0",
    );
    a.op(Opcode::Field {
        dst: txt,
        obj: sdata,
        field: p.su_name,
    });
    a.label("named0");
    // Std.string(null) is "null"; a String goes to the Dyn argument as is.
    a.op(Opcode::Call1 {
        dst: txt,
        fun: p.std_string,
        arg0: txt,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: s_shown,
    });
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: s_swords,
    });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "shown");
    a.op(Opcode::GetGlobal {
        dst: txt,
        global: s_portrait,
    });
    a.label("shown");
    a.op(Opcode::Call2 {
        dst: msg,
        fun: p.str_add,
        arg0: msg,
        arg1: txt,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: msg,
    });
    a.label("traced");

    // Act on change; the same unit only re-centres after a rescale, and its
    // portrait is put back if something (the style system) reset the bitmap.
    a.op(Opcode::GetThis {
        dst: cur,
        field: f_unit,
    });
    a.jmp(
        Opcode::JNotEq {
            a: cur,
            b: u,
            offset: 0,
        },
        "changed",
    );
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "kept");
    a.op(Opcode::GetThis {
        dst: bmp,
        field: p.ev_event,
    });
    a.jmp(
        Opcode::JNull {
            reg: bmp,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: t1,
        obj: bmp,
        field: p.bm_tile,
    });
    a.op(Opcode::GetThis {
        dst: t2,
        field: f_tile,
    });
    a.jmp(
        Opcode::JEq {
            a: t1,
            b: t2,
            offset: 0,
        },
        "kept",
    );
    a.jmp(Opcode::JNull { reg: t2, offset: 0 }, "kept");
    a.op(Opcode::Call2 {
        dst: t2,
        fun: p.set_tile,
        arg0: bmp,
        arg1: t2,
    });
    a.op(Opcode::Field {
        dst: filt,
        obj: bmp,
        field: p.o_filter,
    });
    a.jmp(
        Opcode::JNull {
            reg: filt,
            offset: 0,
        },
        "reported",
    );
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_enable,
        arg0: filt,
        arg1: b,
    });
    a.label("reported");
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: s_reset,
    });
    a.op(Opcode::Null { dst: dd });
    a.op(Opcode::Call3 {
        dst: v,
        fun: report,
        arg0: ev,
        arg1: msg,
        arg2: dd,
    });
    a.label("kept");
    a.op(Opcode::Call1 {
        dst: wi,
        fun: p.outer_width,
        arg0: ev,
    });
    a.op(Opcode::GetThis {
        dst: wo,
        field: f_w,
    });
    a.jmp(
        Opcode::JEq {
            a: wi,
            b: wo,
            offset: 0,
        },
        "untrap",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "centre");
    // mpHudUnit is stored only once the swap has gone through, so a failed
    // attempt (an exception, reported by the catch) is retried next frame.
    a.label("changed");
    a.op(Opcode::GetThis {
        dst: bmp,
        field: p.ev_event,
    });
    a.jmp(
        Opcode::JNull {
            reg: bmp,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Field {
        dst: filt,
        obj: bmp,
        field: p.o_filter,
    });
    a.op(Opcode::GetGlobal {
        dst: name,
        global: s_empty,
    });
    a.jmp(Opcode::JNotNull { reg: u, offset: 0 }, "portrait");
    a.op(Opcode::Call0 {
        dst: api,
        fun: p.get_api,
    });
    a.op(Opcode::GetGlobal {
        dst: icon,
        global: s_icon,
    });
    a.op(Opcode::Call2 {
        dst: tile,
        fun: p.api_get_icon,
        arg0: api,
        arg1: icon,
    });
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "apply");
    a.label("portrait");
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
        "untrap",
    );
    a.op(Opcode::Call1 {
        dst: tile,
        fun: p.unit_get_icon,
        arg0: sdata,
    });
    a.op(Opcode::Field {
        dst: bp,
        obj: sdata,
        field: p.su_owner,
    });
    a.jmp(Opcode::JNull { reg: bp, offset: 0 }, "named");
    a.op(Opcode::GetThis {
        dst: game,
        field: p.ev_game,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "named",
    );
    a.op(Opcode::Call1 {
        dst: b,
        fun: p.is_multi,
        arg0: game,
    });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "named");
    a.op(Opcode::Call1 {
        dst: name,
        fun: p.bp_get_name,
        arg0: bp,
    });
    a.label("named");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(true),
    });

    // Swap the image, mask it for a portrait only, set and centre the label.
    a.label("apply");
    a.op(Opcode::SetThis {
        field: f_tile,
        src: tile,
    });
    a.op(Opcode::Call2 {
        dst: tile,
        fun: p.set_tile,
        arg0: bmp,
        arg1: tile,
    });
    a.jmp(
        Opcode::JNull {
            reg: filt,
            offset: 0,
        },
        "text",
    );
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.set_enable,
        arg0: filt,
        arg1: b,
    });
    a.label("text");
    a.op(Opcode::GetThis {
        dst: lbl,
        field: f_label,
    });
    a.jmp(
        Opcode::JNull {
            reg: lbl,
            offset: 0,
        },
        "untrap",
    );
    a.op(Opcode::Call2 {
        dst: name,
        fun: p.html_set_text,
        arg0: lbl,
        arg1: name,
    });
    a.op(Opcode::SetThis {
        field: f_unit,
        src: u,
    });
    a.label("centre");
    a.op(Opcode::Call1 {
        dst: wi,
        fun: p.outer_width,
        arg0: ev,
    });
    a.op(Opcode::SetThis {
        field: f_w,
        src: wi,
    });
    a.op(Opcode::ToSFloat { dst: w, src: wi });
    a.op(Opcode::Call1 {
        dst: tw,
        fun: p.text_width,
        arg0: lbl,
    });
    a.op(Opcode::Sub {
        dst: w,
        a: w,
        b: tw,
    });
    a.op(Opcode::Float { dst: hf, ptr: half });
    a.op(Opcode::Mul {
        dst: w,
        a: w,
        b: hf,
    });
    a.op(Opcode::Call2 {
        dst: w,
        fun: p.set_x,
        arg0: lbl,
        arg1: w,
    });
    a.op(Opcode::Call1 {
        dst: th,
        fun: p.text_height,
        arg0: lbl,
    });
    a.op(Opcode::Neg { dst: th, src: th });
    a.op(Opcode::Call2 {
        dst: th,
        fun: p.set_y,
        arg0: lbl,
        arg1: th,
    });
    a.label("untrap");
    a.op(Opcode::EndTrap { exc });
    a.label("ret");
    a.op(Opcode::Ret { ret: v });
    a.label("catch");
    a.op(Opcode::GetGlobal {
        dst: msg,
        global: s_err,
    });
    a.op(Opcode::Call3 {
        dst: v,
        fun: report,
        arg0: ev,
        arg1: msg,
        arg2: exc,
    });
    a.op(Opcode::Ret { ret: v });
    push_fn(code, vec![p.ev_t], p.void_, r.0, a.finish(), p.dbg_file)
}

/// Every appended field, in order: name and type.
fn new_fields(p: &Plan) -> Vec<(&'static str, RefType)> {
    let mut v = vec![
        (UNIT_FIELD, p.unit_t),
        (LABEL_FIELD, p.html_t),
        (WIDTH_FIELD, p.i32_),
    ];
    v.extend(list::FIELDS.iter().map(|(n, k)| (*n, k.of(p))));
    v.extend([
        (WHY_FIELD, p.i32_),
        (SEEN_FIELD, p.unit_t),
        (TILE_FIELD, p.tile_t),
        (REPORT_FIELD, p.f64_),
    ]);
    v
}

fn apply(code: &mut Bytecode, p: &Plan, lp: &list::ListPlan) -> Result<()> {
    // The fields go last on TimelineEvent: their indices follow every inherited one.
    let base = obj(code, p.ev_t)?.fields.len();
    let fl = Fields {
        unit: RefField(base),
        label: RefField(base + 1),
        hud_w: RefField(base + 2),
        list: list::ListFields::at(base + 3),
        why: RefField(base + 3 + list::FIELDS.len()),
        seen: RefField(base + 4 + list::FIELDS.len()),
        tile: RefField(base + 5 + list::FIELDS.len()),
        rep_t: RefField(base + 6 + list::FIELDS.len()),
    };
    let report = add_report(code, p, &fl)?;
    let name_font = add_name_font(code, p)?;
    let hud = add_hud(code, p, &fl, report, name_font)?;
    let hud_list = list::add_list(code, p, lp, &fl.list, report, name_font)?;
    let names: Vec<_> = new_fields(p)
        .into_iter()
        .map(|(n, t)| (string_ref(code, n), t))
        .collect();
    let Type::Obj(o) = &mut code.types[p.ev_t.0] else {
        unreachable!()
    };
    for (name, t) in names {
        o.own_fields.push(ObjField { name, t });
        o.fields.push(ObjField { name, t });
    }

    // Constructor: Player takes the masked branch (the AI case's target).
    let c = &mut code.functions[p.ctor_fi];
    let Opcode::Switch { offsets, .. } = &mut c.ops[p.mask_switch] else {
        unreachable!()
    };
    offsets[0] = offsets[1];
    let ctor = c.findex;

    let f = &mut code.functions[p.sync_fi];
    insert_ops(
        f,
        1,
        vec![
            Opcode::Call1 {
                dst: p.sync_void,
                fun: hud,
                arg0: Reg(0),
            },
            Opcode::Call1 {
                dst: p.sync_void,
                fun: hud_list,
                arg0: Reg(0),
            },
        ],
    );
    eprintln!(
        "patched timeline hud: TimelineEvent ctor fn@{} op {} masks Player, sync fn@{} calls timelineHud fn@{} and timelineHudList fn@{}, errors and changes printed (report fn@{})",
        ctor.0, p.mask_switch, f.findex.0, hud.0, hud_list.0, report.0
    );
    Ok(())
}

/// Shows the acting player unit's portrait and its player's nickname on the
/// battle timeline's first diamond, plus the co-op player status list, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_timeline_hud(code: &mut Bytecode) {
    let snap = crate::asm::Snap::take(code);
    let r = plan(code).and_then(|p| {
        let lp = list::plan(code, &p)?;
        apply(code, &p, &lp)
    });
    if let Err(e) = r {
        snap.restore(code);
        eprintln!("timeline hud skipped: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// The constructor changes one Switch offset, sync gains one call, two
    /// fields and one well-typed function are appended; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_timeline_hud(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 4);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != p.ctor_fi && i != p.sync_fi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }
        let report = &back.functions[orig.functions.len()];
        let name_font = &back.functions[orig.functions.len() + 1];
        let hud = &back.functions[orig.functions.len() + 2];
        let hud_list = &back.functions[orig.functions.len() + 3];

        // Constructor: only the Player offset of the mask switch.
        let (a, b) = (&orig.functions[p.ctor_fi], &back.functions[p.ctor_fi]);
        assert_eq!(a.regs, b.regs);
        for i in 0..a.ops.len() {
            if i == p.mask_switch {
                let (
                    Opcode::Switch {
                        offsets: oa,
                        end: ea,
                        ..
                    },
                    Opcode::Switch {
                        offsets: ob,
                        end: eb,
                        ..
                    },
                ) = (&a.ops[i], &b.ops[i])
                else {
                    panic!("not a switch");
                };
                assert_eq!(ea, eb);
                assert_eq!(ob[0], oa[1]);
                assert_eq!(ob[1..], oa[1..]);
            } else {
                assert_eq!(
                    format!("{:?}", b.ops[i]),
                    format!("{:?}", a.ops[i]),
                    "op {i}"
                );
            }
        }

        // sync: two calls inserted after the super sync.
        let (a, b) = (&orig.functions[p.sync_fi], &back.functions[p.sync_fi]);
        shifted(a, b, 1, 2);
        assert!(matches!(b.ops[1], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == hud.findex));
        assert!(
            matches!(b.ops[2], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == hud_list.findex)
        );
        check_types(&back, b, 1..3);
        check_flow(b);

        // Fields: appended, typed, used by the new functions only.
        let ev = back.types[p.ev_t.0].get_type_obj().expect("ev");
        let ov = orig.types[p.ev_t.0].get_type_obj().expect("ev");
        let want = new_fields(&p);
        assert_eq!(ev.own_fields.len(), ov.own_fields.len() + want.len());
        let names: Vec<&str> = ev.own_fields[ov.own_fields.len()..]
            .iter()
            .map(|f| back.strings[f.name.0].as_str())
            .collect();
        assert_eq!(
            names,
            want.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "field names"
        );
        for (n, t) in &want {
            assert_eq!(field(&back, p.ev_t, n).unwrap().1, *t, "{n}");
        }
        assert_eq!(names[..3], [UNIT_FIELD, LABEL_FIELD, WIDTH_FIELD]);

        for f in [report, name_font, hud, hud_list] {
            check_types(&back, f, 0..f.ops.len());
            check_flow(f);
        }
        // The list colours names with getName and is gated on isMulti.
        for want in [p.bp_get_name, p.is_multi] {
            assert!(hud_list
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == want)));
        }

        // Turn end: the portrait is kept only while this diamond's event is
        // still the live head (state.timelines[0][0], compared by identity).
        assert!(hud
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Field { field, .. } if *field == p.s_timelines)));
        assert!(hud
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::JNotEq { a, b, .. }
            if hud.regs[a.0 as usize] == p.elt_t && hud.regs[b.0 as usize] == p.elt_t)));
        // Names use the players panel's font, built by nameFont (SDF eb_garamond).
        let calls = |f: &Function, fun: RefFun| {
            f.ops.iter().any(|o| match o {
                Opcode::Call0 { fun: g, .. }
                | Opcode::Call1 { fun: g, .. }
                | Opcode::Call2 { fun: g, .. } => *g == fun,
                Opcode::CallN { fun: g, .. } => *g == fun,
                _ => false,
            })
        };
        for want in [p.res_load, p.any_to, p.to_sdf, p.load_font] {
            assert!(calls(name_font, want), "nameFont calls fn@{}", want.0);
        }
        for f in [hud, hud_list] {
            assert!(calls(f, name_font.findex));
            assert!(!calls(f, p.load_font));
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_timeline_hud(&mut again);
        assert!(write(&again) == patched);
    }

    /// An exception in timelineHud / timelineHudList is no longer swallowed:
    /// the Trap's handler passes the caught value to the report function,
    /// which prints through Sys.println (rate-limited by Sys.time).
    #[test]
    fn catch_prints() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_timeline_hud(&mut code);
        let back = read(&write(&code));
        let n = orig.functions.len();
        let report = &back.functions[n];
        let calls = |f: &Function, fun: RefFun| {
            f.ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun: g, .. } | Opcode::Call0 { fun: g, .. } if *g == fun))
        };
        assert!(calls(report, p.println), "report prints");
        assert!(calls(report, p.sys_time), "report is rate-limited");
        for f in [&back.functions[n + 2], &back.functions[n + 3]] {
            let (trap_at, exc, off) = f
                .ops
                .iter()
                .enumerate()
                .find_map(|(i, o)| match o {
                    Opcode::Trap { exc, offset } => Some((i, *exc, *offset)),
                    _ => None,
                })
                .expect("one Trap");
            let handler = (trap_at as i64 + 1 + off as i64) as usize;
            let tail = &f.ops[handler..];
            assert!(
                tail.iter().any(|o| matches!(o,
                    Opcode::Call3 { fun, arg0: Reg(0), arg2, .. } if *fun == report.findex && *arg2 == exc)),
                "fn@{}: the catch reports the exception",
                f.findex.0
            );
            assert!(matches!(tail.last(), Some(Opcode::Ret { .. })));
        }
    }

    /// Globals holding the String constant `value`.
    fn str_globals(code: &Bytecode, value: &str) -> Vec<RefGlobal> {
        code.constants
            .iter()
            .flatten()
            .filter(|c| matches!(c.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value)))
            .map(|c| c.global)
            .collect()
    }

    /// The hooked `sync` is the one the battle HUD builds and runs. Chain in the
    /// vanilla 1.0.48274 image (indices for reference, the asserts are structural):
    ///
    /// - `Battle.initUI` fn@9716 ops 202..204: `new battle.ui.win.Timeline`,
    ///   `Timeline.__constructor__` fn@10380, `this.timeline = it` (the only
    ///   reference to fn@10380).
    /// - `Timeline.init` fn@10357 (run by the ui.Window constructor) creates one
    ///   "timeline-event" domkit component per `getUnitsTimeline` entry.
    /// - `domkit.CompTimelineEvent.__constructor__` fn@10379 registers
    ///   "timeline-event" with the maker fn@45085, which does
    ///   `new TimelineEvent` and calls `TimelineEvent.__constructor__` fn@10378,
    ///   the only user of the "TimelineIcon" icon (the crossed swords).
    /// - `TimelineEvent`'s prototype `sync` (pindex 14) is fn@10369;
    ///   `h2d.Object.sync` fn@961 calls every child's sync each frame.
    ///
    /// The test patches a scratch copy of the installed image and asserts the
    /// timelineHud call lands in that very `sync`.
    #[test]
    fn battle_hud_constructs_hooked_sync() {
        let Some(image) = game() else { return };
        let scratch = image.clone();
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let battle_t = obj_type(&orig, "battle.Battle").expect("Battle");
        let tl_t = obj_type(&orig, "battle.ui.win.Timeline").expect("Timeline");

        // Battle.initUI: new Timeline, then its constructor on that register.
        let init_ui = method(&orig, battle_t, "initUI").expect("initUI");
        let tl_ctor = init_ui
            .ops
            .windows(2)
            .find_map(|w| match (&w[0], &w[1]) {
                (Opcode::New { dst }, Opcode::Call1 { fun, arg0, .. })
                    if arg0 == dst && init_ui.regs[dst.0 as usize] == tl_t =>
                {
                    Some(*fun)
                }
                _ => None,
            })
            .expect("initUI constructs the Timeline");
        assert_eq!(sig(&orig, tl_ctor).expect("ctor sig").0, [tl_t]);

        // Timeline.init creates "timeline-event" components.
        let comp_name = str_globals(&orig, "timeline-event");
        assert!(!comp_name.is_empty(), "timeline-event constant");
        let uses_name = |f: &Function| {
            f.ops.iter().any(
                |o| matches!(o, Opcode::GetGlobal { global, .. } if comp_name.contains(global)),
            )
        };
        assert!(uses_name(
            method(&orig, tl_t, "init").expect("Timeline.init")
        ));

        // The "timeline-event" maker builds a TimelineEvent with the patched constructor.
        let ctor = orig.functions[p.ctor_fi].findex;
        let makers: Vec<RefFun> = orig
            .functions
            .iter()
            .filter(|f| {
                f.t.as_fun(&orig).is_some_and(|t| t.ret == p.ev_t)
                    && f.ops.iter().any(
                        |o| matches!(o, Opcode::New { dst } if f.regs[dst.0 as usize] == p.ev_t),
                    )
                    && f.ops
                        .iter()
                        .any(|o| matches!(o, Opcode::Call3 { fun, .. } if *fun == ctor))
            })
            .map(|f| f.findex)
            .collect();
        assert_eq!(makers.len(), 1, "one TimelineEvent maker");
        let comp_t = obj_type(&orig, "domkit.CompTimelineEvent").expect("CompTimelineEvent");
        assert!(
            orig.functions.iter().any(|f| {
                sig(&orig, f.findex).is_ok_and(|(a, _)| a.first() == Some(&comp_t))
                    && uses_name(f)
                    && f.ops.iter().any(
                        |o| matches!(o, Opcode::StaticClosure { fun, .. } if *fun == makers[0]),
                    )
            }),
            "CompTimelineEvent registers the maker under timeline-event"
        );

        // The constructor is what draws the swords.
        let icon = str_globals(&orig, "TimelineIcon");
        assert!(orig.functions[p.ctor_fi]
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::GetGlobal { global, .. } if icon.contains(global))));

        // TimelineEvent's own prototype sync is the hooked function.
        let sync = proto(&orig, p.ev_t, "sync").expect("TimelineEvent.sync");
        assert_eq!(sync, orig.functions[p.sync_fi].findex);

        let mut code = read(&scratch);
        patch_timeline_hud(&mut code);
        let back = read(&write(&code));
        let hud = back.functions[orig.functions.len() + 2].findex;
        assert_eq!(proto(&back, p.ev_t, "sync").expect("sync"), sync);
        let f = &back.functions[p.sync_fi];
        assert!(matches!(f.ops[1], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == hud));
    }

    /// A constructor whose Player case is not the unmasked branch, or a sync
    /// that does not start with the super sync, is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let p = plan(&read(&image)).expect("plan");

        let mut code = read(&image);
        let Opcode::Switch { offsets, .. } = &mut code.functions[p.ctor_fi].ops[p.mask_switch]
        else {
            panic!("not a switch");
        };
        offsets[0] = offsets[1];
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_timeline_hud(&mut code);
        assert!(write(&code) == before);

        let mut code = read(&image);
        code.functions[p.sync_fi].ops[1] = Opcode::Nop;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_timeline_hud(&mut code);
        assert!(write(&code) == before);
    }
}
