// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// wartales-tips: patches Wartales' HashLink bytecode (hlboot.dat) in memory: start-choice
// tooltips (this file) plus one module per co-op fix / feature / diagnostic, applied in
// order by `patch_image`. Each pass validates the shapes it edits; on a mismatch it
// leaves the image untouched and reports it (`skipped`), and `patch_image` then refuses
// the whole image: a game build the co-op set does not fully match runs unpatched,
// never half-patched. Only the print-only diagnostics (diag, activity_diag,
// debrief_diag, camp_choice) are skipped alone. What each pass does: patcher/README.md.

mod activity_diag;
mod activity_injury;
mod all_inv;
mod alt_world;
mod asm;
mod barrier;
mod battle_camera;
mod camp_any_unit;
mod camp_choice;
mod camp_talk;
mod career_plan;
mod censer_tip;
mod chest_buttons;
mod coop_gates;
mod coop_spectate;
mod customize_slots;
mod debrief_cure;
mod debrief_diag;
mod debrief_enable;
mod diag;
mod dialog_recruit;
mod dlc_untouched;
mod drop_in;
mod follow;
mod force_leave;
mod forge_mirror;
mod friendly_fire;
mod hold_speed;
// A call to a patch-added function right after a NullCheck on its first argument makes the
// HashLink JIT hash a NULL function name and crash at startup; a Nop breaks that (see jit_names.rs).
mod jit_names;
mod job_confirm;
mod job_xp;
mod loot_all;
mod marker_names;
mod mirror;
mod mod_version;
mod net_guard;
mod npc_talk;
mod party_inventory;
mod ping_cell;
mod ready_start;
mod returning_units;
mod skill_cost;
mod style_guard;
mod take_all;
mod tavern_resume;
#[cfg(test)]
mod testsim;
mod timeline_hud;
mod tip_overflow;
mod title_version;
mod tooltip_input;
mod window_close;
mod window_drag;
mod work_mirror;

use anyhow::{bail, Context, Result};
use hlbc::opcodes::Opcode;
use hlbc::types::{
    Function, RefField, RefFun, RefGlobal, RefString, RefType, Reg, Type, TypeFun, TypeObj,
};
use hlbc::Bytecode;
use std::io::Cursor;

/// Marker written nowhere in the image; used by callers to name this patch in logs.
pub const PATCH_NAME: &str = "wartales-tips start-choice item tooltips";

thread_local! {
    /// What the running `patch_image` found not matching this game build.
    static SKIPPED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A pass, or a part of one, does not match this game build and left `code`
/// untouched: logged, and `patch_image` then refuses the whole image (no game
/// runs with half the co-op set). Print-only diagnostics log their own skip.
pub(crate) fn skipped(why: String) {
    eprintln!("{why}");
    SKIPPED.with(|s| s.borrow_mut().push(why));
}

/// Applies every patch to a bytecode image held in memory and returns the new
/// image; fails when any behaviour-changing pass does not match the image.
pub fn patch_image(image: &[u8]) -> Result<Vec<u8>> {
    SKIPPED.with(|s| s.borrow_mut().clear());
    let mut code = Bytecode::deserialize(&mut Cursor::new(image)).context("read bytecode")?;
    patch_start_choice_item_tips(&mut code)?;
    patch_start_choice_unit_tips(&mut code)?;
    friendly_fire::patch_enemy_area_friendly_fire(&mut code).context("friendly fire")?;
    career_plan::patch_career_plan(&mut code);
    customize_slots::patch_customize_slots(&mut code);
    coop_gates::patch_coop_gates(&mut code);
    force_leave::patch_force_leave(&mut code);
    npc_talk::patch_npc_talk(&mut code);
    dialog_recruit::patch_dialog_recruit(&mut code);
    camp_talk::patch_camp_talk(&mut code);
    camp_any_unit::patch_camp_any_unit(&mut code);
    camp_choice::patch_camp_choice(&mut code);
    hold_speed::patch_hold_speed(&mut code);
    job_xp::patch_job_xp(&mut code);
    job_confirm::patch_job_confirm(&mut code);
    barrier::patch_barrier(&mut code);
    ready_start::patch_ready_start(&mut code);
    drop_in::patch_drop_in(&mut code);
    returning_units::patch_returning_units(&mut code);
    diag::patch_diag(&mut code);
    activity_diag::patch_activity_diag(&mut code);
    follow::patch_follow(&mut code);
    marker_names::patch_marker_names(&mut code);
    skill_cost::patch_skill_cost(&mut code);
    chest_buttons::patch_chest_buttons(&mut code);
    party_inventory::patch_party_inventory(&mut code);
    party_inventory::patch_party_counts(&mut code);
    party_inventory::patch_party_lists(&mut code);
    party_inventory::patch_party_recipes(&mut code);
    party_inventory::patch_party_activities(&mut code);
    loot_all::patch_loot_all(&mut code);
    debrief_cure::patch_debrief_cure(&mut code);
    debrief_diag::patch(&mut code);
    debrief_enable::patch_debrief_enable(&mut code);
    window_close::patch_window_close(&mut code);
    window_drag::patch_window_drag(&mut code);
    take_all::patch_take_all(&mut code);
    tavern_resume::patch_tavern_resume(&mut code);
    mod_version::patch_mod_version(&mut code);
    net_guard::patch_net_guard(&mut code);
    tip_overflow::patch_tip_overflow(&mut code);
    tooltip_input::patch(&mut code);
    censer_tip::patch_censer_tip(&mut code);
    ping_cell::patch_ping_cell(&mut code);
    forge_mirror::patch_forge_mirror(&mut code);
    work_mirror::patch_work_mirror(&mut code);
    coop_spectate::patch_coop_spectate(&mut code);
    timeline_hud::patch_timeline_hud(&mut code);
    activity_injury::patch_activity_injury(&mut code);
    style_guard::patch_style_guard(&mut code);
    all_inv::patch_all_inv(&mut code);
    alt_world::patch_alt_world(&mut code);
    battle_camera::patch_battle_camera(&mut code);
    title_version::patch_title_version(&mut code);
    jit_names::patch_jit_names(&mut code);
    let skipped = SKIPPED.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !skipped.is_empty() {
        bail!(
            "{} pass(es) do not match this game build: {}",
            skipped.len(),
            skipped.join("; ")
        );
    }
    let mut out = Vec::with_capacity(image.len() + 4096);
    code.serialize(&mut out).context("write bytecode")?;
    Ok(out)
}

/// Prints the class hierarchy of `type_name` (fields, methods) for exploration.
pub fn inspect_type(image: &[u8], type_name: &str) -> Result<()> {
    let code = Bytecode::deserialize(&mut Cursor::new(image)).context("read bytecode")?;
    inspect(&code, type_name)
}

// ---------- C ABI, for linking into a proxy DLL ----------

/// Patches `image[0..len]`. On success returns 0 and sets `*out`/`*out_len` to a
/// buffer owned by this library (release with `wartales_tips_free`). On failure
/// returns 1 and leaves `*out` NULL; the reason is copied into `err` (NUL-terminated,
/// truncated to `err_len`). Never unwinds across the boundary.
///
/// # Safety
/// `image` must point to `len` readable bytes; `out`, `out_len` must be valid
/// pointers; `err` must point to `err_len` writable bytes (or be NULL with `err_len` 0).
#[no_mangle]
pub unsafe extern "C" fn wartales_tips_patch(
    image: *const u8,
    len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
    err: *mut u8,
    err_len: usize,
) -> i32 {
    if !out.is_null() {
        *out = std::ptr::null_mut();
    }
    if !out_len.is_null() {
        *out_len = 0;
    }
    let report = |msg: &str| {
        if err.is_null() || err_len == 0 {
            return;
        }
        let bytes = msg.as_bytes();
        let n = bytes.len().min(err_len - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err, n);
        *err.add(n) = 0;
    };
    if image.is_null() || out.is_null() || out_len.is_null() {
        report("null argument");
        return 1;
    }
    let input = std::slice::from_raw_parts(image, len);
    let result = std::panic::catch_unwind(|| patch_image(input));
    match result {
        Ok(Ok(v)) => {
            let mut v = v.into_boxed_slice();
            *out_len = v.len();
            *out = v.as_mut_ptr();
            std::mem::forget(v);
            0
        }
        Ok(Err(e)) => {
            report(&format!("{e:#}"));
            1
        }
        Err(_) => {
            report("panic inside the patcher");
            1
        }
    }
}

/// Releases a buffer returned by `wartales_tips_patch`.
///
/// # Safety
/// `p`/`len` must be exactly what `wartales_tips_patch` returned, or `p` NULL.
#[no_mangle]
pub unsafe extern "C" fn wartales_tips_free(p: *mut u8, len: usize) {
    if !p.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)));
    }
}

// ---------- lookup helpers (by name, never by hard-coded index) ----------

fn s(code: &Bytecode, i: hlbc::types::RefString) -> &str {
    code.strings[i.0].as_str()
}

fn obj(code: &Bytecode, t: RefType) -> Result<&TypeObj> {
    code.types[t.0]
        .get_type_obj()
        .with_context(|| format!("type {} is not an object", t.0))
}

fn obj_type(code: &Bytecode, name: &str) -> Result<RefType> {
    code.types
        .iter()
        .position(|t| matches!(t, Type::Obj(o) if s(code, o.name) == name))
        .map(RefType)
        .with_context(|| format!("type {name} not found"))
}

fn field(code: &Bytecode, t: RefType, name: &str) -> Result<(RefField, RefType)> {
    let o = obj(code, t)?;
    o.fields
        .iter()
        .position(|f| s(code, f.name) == name)
        .map(|i| (RefField(i), o.fields[i].t))
        .with_context(|| format!("field {name} not found on {}", s(code, o.name)))
}

/// Function `name` whose first argument (this) has type `this_t`.
fn method<'a>(code: &'a Bytecode, this_t: RefType, name: &str) -> Result<&'a Function> {
    let mut hits = code.functions.iter().filter(|f| {
        s(code, f.name) == name
            && f.t.as_fun(code).and_then(|ft| ft.args.first().copied()) == Some(this_t)
    });
    let f = hits
        .next()
        .with_context(|| format!("method {name} on type {} not found", this_t.0))?;
    if hits.next().is_some() {
        bail!("method {name} on type {} is ambiguous", this_t.0);
    }
    Ok(f)
}

/// The function bound to method `name` in the prototype of class `t` itself.
fn proto(code: &Bytecode, t: RefType, name: &str) -> Result<RefFun> {
    obj(code, t)?
        .protos
        .iter()
        .find(|p| s(code, p.name) == name)
        .map(|p| p.findex)
        .with_context(|| format!("proto {name} not found on type {}", t.0))
}

fn fun_args(code: &Bytecode, f: &Function) -> Vec<RefType> {
    f.t.as_fun(code).map(|t| t.args.clone()).unwrap_or_default()
}

fn debug_file(code: &Bytecode, name: &str) -> Result<usize> {
    code.debug_files
        .as_ref()
        .and_then(|d| d.iter().position(|f| f.as_str() == name))
        .with_context(|| format!("debug file {name} not found"))
}

/// The next free function index: findexes are dense over functions + natives.
fn next_findex(code: &Bytecode) -> Result<RefFun> {
    let n = code.functions.len() + code.natives.len();
    let max = code
        .functions
        .iter()
        .map(|f| f.findex.0)
        .chain(code.natives.iter().map(|f| f.findex.0))
        .max()
        .unwrap_or(0);
    if max + 1 != n {
        bail!("findex space is not dense; refusing to append a function");
    }
    Ok(RefFun(n))
}

/// Position of function `findex` in `code.functions`.
fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

/// Argument and return types of a function or native.
fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let t = match code.natives.iter().find(|n| n.findex == f) {
        Some(n) => n.t,
        None => code.functions[fun_index(code, f)?].t,
    };
    let t = t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

/// Name of function `f` ("" when it is not a bytecode function).
fn fname(code: &Bytecode, f: RefFun) -> &str {
    code.functions
        .iter()
        .find(|g| g.findex == f)
        .map(|g| s(code, g.name))
        .unwrap_or("")
}

/// `t` is `of` or one of its subclasses.
fn is_sub(code: &Bytecode, t: RefType, of: RefType) -> bool {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if c == of {
            return true;
        }
        cur = code.types[c.0].get_type_obj().and_then(|o| o.super_);
    }
    false
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
    let want = if pkg.is_empty() {
        format!("${cls}")
    } else {
        format!("{pkg}.${cls}")
    };
    if obj(code, t).ok().map(|o| s(code, o.name)) != Some(want.as_str()) {
        bail!("{name}: class global is not {want}");
    }
    Ok((g, t))
}

/// Field `name` of `t`, which must have type `want`.
fn typed(code: &Bytecode, t: RefType, name: &str, want: RefType) -> Result<RefField> {
    let (f, ft) = field(code, t, name)?;
    if ft != want {
        bail!("field {name} has an unexpected type");
    }
    Ok(f)
}

/// Name of field `f` of an object or virtual type.
fn field_name(code: &Bytecode, t: RefType, f: RefField) -> Option<&str> {
    match &code.types[t.0] {
        Type::Virtual { fields } => fields.get(f.0).map(|x| s(code, x.name)),
        _ => obj(code, t).ok()?.fields.get(f.0).map(|x| s(code, x.name)),
    }
}

/// The one native `name` with this signature.
fn native(code: &Bytecode, name: &str, args: &[RefType], ret: RefType) -> Result<RefFun> {
    let hits: Vec<RefFun> = code
        .natives
        .iter()
        .filter(|n| {
            s(code, n.name) == name
                && n.t
                    .as_fun(code)
                    .is_some_and(|t| t.args == args && t.ret == ret)
        })
        .map(|n| n.findex)
        .collect();
    match hits[..] {
        [f] => Ok(f),
        _ => bail!("expected one native {name}, found {}", hits.len()),
    }
}

/// `f` calls `want` directly.
fn calls(f: &Function, want: RefFun) -> bool {
    f.ops.iter().any(|o| {
        matches!(o,
            Opcode::Call0 { fun, .. } | Opcode::Call1 { fun, .. } | Opcode::Call2 { fun, .. }
            | Opcode::Call3 { fun, .. } | Opcode::Call4 { fun, .. } | Opcode::CallN { fun, .. }
            if *fun == want)
    })
}

/// Global of an existing String constant `value` (built by the game, not appended).
fn existing_str(code: &Bytecode, str_t: RefType, value: &str) -> Result<RefGlobal> {
    code.constants
        .iter()
        .flatten()
        .find(|c| {
            code.globals.get(c.global.0) == Some(&str_t)
                && matches!(c.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value))
        })
        .map(|c| c.global)
        .with_context(|| format!("string global {value:?} not found"))
}

fn string_index(code: &Bytecode, value: &str) -> Result<RefString> {
    code.strings
        .iter()
        .position(|v| v.as_str() == value)
        .map(RefString)
        .with_context(|| format!("string {value:?} not in the pool"))
}

fn add_global(code: &mut Bytecode, t: RefType) -> RefGlobal {
    code.globals.push(t);
    RefGlobal(code.globals.len() - 1)
}

fn new_reg(f: &mut Function, t: RefType) -> Reg {
    f.regs.push(t);
    Reg((f.regs.len() - 1) as u32)
}

// ---------- bytecode editing ----------

/// Insert `new_ops` before original op index `at`, keeping every jump target,
/// debug line and variable-name assignment in `f` consistent.
fn insert_ops(f: &mut Function, at: usize, new_ops: Vec<Opcode>) {
    let n = new_ops.len() as i64;
    let map = |i: i64| if i < at as i64 { i } else { i + n };
    for (i, op) in f.ops.iter_mut().enumerate() {
        let i = i as i64;
        let fix = |off: &mut i32| {
            let target = i + 1 + *off as i64;
            *off = (map(target) - map(i) - 1) as i32;
        };
        match op {
            Opcode::JTrue { offset, .. }
            | Opcode::JFalse { offset, .. }
            | Opcode::JNull { offset, .. }
            | Opcode::JNotNull { offset, .. }
            | Opcode::JSLt { offset, .. }
            | Opcode::JSGte { offset, .. }
            | Opcode::JSGt { offset, .. }
            | Opcode::JSLte { offset, .. }
            | Opcode::JULt { offset, .. }
            | Opcode::JUGte { offset, .. }
            | Opcode::JNotLt { offset, .. }
            | Opcode::JNotGte { offset, .. }
            | Opcode::JEq { offset, .. }
            | Opcode::JNotEq { offset, .. }
            | Opcode::JAlways { offset }
            | Opcode::Trap { offset, .. } => fix(offset),
            Opcode::Switch { offsets, end, .. } => {
                for o in offsets.iter_mut() {
                    fix(o);
                }
                fix(end);
            }
            _ => {}
        }
    }
    if let Some(dbg) = &mut f.debug_info {
        let line = dbg[at.saturating_sub(1)];
        for _ in 0..n {
            dbg.insert(at, line);
        }
    }
    if let Some(assigns) = &mut f.assigns {
        let len = f.ops.len();
        for (_, pos) in assigns.iter_mut() {
            if *pos < len && *pos >= at {
                *pos += n as usize;
            }
        }
    }
    for (k, op) in new_ops.into_iter().enumerate() {
        f.ops.insert(at + k, op);
    }
}

/// Absolute op indices that op `i` of `f` can jump to (empty for non-jumps).
fn jump_targets(f: &Function, i: usize) -> Vec<usize> {
    let offsets: Vec<i32> = match &f.ops[i] {
        Opcode::JTrue { offset, .. }
        | Opcode::JFalse { offset, .. }
        | Opcode::JNull { offset, .. }
        | Opcode::JNotNull { offset, .. }
        | Opcode::JSLt { offset, .. }
        | Opcode::JSGte { offset, .. }
        | Opcode::JSGt { offset, .. }
        | Opcode::JSLte { offset, .. }
        | Opcode::JULt { offset, .. }
        | Opcode::JUGte { offset, .. }
        | Opcode::JNotLt { offset, .. }
        | Opcode::JNotGte { offset, .. }
        | Opcode::JEq { offset, .. }
        | Opcode::JNotEq { offset, .. }
        | Opcode::JAlways { offset }
        | Opcode::Trap { offset, .. } => vec![*offset],
        Opcode::Switch { offsets, end, .. } => offsets.iter().chain([end]).copied().collect(),
        _ => vec![],
    };
    offsets
        .into_iter()
        .map(|o| (i as i64 + 1 + o as i64) as usize)
        .collect()
}

/// The first type in the pool matching `pred` (primitive types such as Dyn or Bool).
fn prim_type(code: &Bytecode, what: &str, pred: impl Fn(&Type) -> bool) -> Result<RefType> {
    code.types
        .iter()
        .position(pred)
        .map(RefType)
        .with_context(|| format!("{what} type not found"))
}

/// New function `(ui.comp.ItemIcon) -> h2d.Object`:
/// `try { return new ItemTip(this.item, null, null, null); } catch (_) { return new h2d.Flow(null); }`.
/// A tooltip that throws while building is replaced by an empty one instead of
/// reaching the game's uncaught-exception screen. Bound with InstanceClosure it
/// becomes the `() -> h2d.Object` that `Element.getTipContent` expects.
fn add_icon_tip_function(code: &mut Bytecode, dbg_file: usize) -> Result<RefFun> {
    let icon_t = obj_type(code, "ui.comp.ItemIcon")?;
    let tip_t = obj_type(code, "ui.comp.ItemTip")?;
    let (item_f, item_t) = field(code, icon_t, "item")?;
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let (_, tip_content_t) = field(code, elem_t, "getTipContent")?;
    let ret_t = tip_content_t
        .as_fun(code)
        .context("getTipContent is not a function type")?
        .ret;
    let ctor = method(code, tip_t, "__constructor__")?;
    let ctor_args = fun_args(code, ctor);
    let ctor_findex = ctor.findex;
    if ctor_args.len() != 5 || ctor_args[1] != item_t {
        bail!("unexpected ItemTip constructor signature");
    }
    let flow_t = obj_type(code, "h2d.Flow")?;
    let flow_ctor = method(code, flow_t, "__constructor__")?;
    let (flow_ctor_findex, flow_parent_t) = (flow_ctor.findex, fun_args(code, flow_ctor)[1]);
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let findex = next_findex(code)?;
    code.types.push(Type::Fun(TypeFun {
        args: vec![icon_t],
        ret: ret_t,
    }));
    let fun_t = RefType(code.types.len() - 1);
    let r = Reg;
    // r0 icon, r1 item, r2 tip, r3..r5 null args, r6 void, r7 exc, r8 fallback flow, r9 null parent
    let mut ops = vec![
        Opcode::Field {
            dst: r(1),
            obj: r(0),
            field: item_f,
        },
        Opcode::Trap {
            exc: r(7),
            offset: 0,
        }, // 1 -> CATCH
        Opcode::New { dst: r(2) },
        Opcode::Null { dst: r(3) },
        Opcode::Null { dst: r(4) },
        Opcode::Null { dst: r(5) },
        Opcode::CallN {
            dst: r(6),
            fun: ctor_findex,
            args: vec![r(2), r(1), r(3), r(4), r(5)],
        },
        Opcode::EndTrap { exc: r(7) },
        Opcode::Ret { ret: r(2) },
        Opcode::New { dst: r(8) }, // 9 CATCH
        Opcode::Null { dst: r(9) },
        Opcode::Call2 {
            dst: r(6),
            fun: flow_ctor_findex,
            arg0: r(8),
            arg1: r(9),
        },
        Opcode::Ret { ret: r(8) },
    ];
    resolve_jumps(&mut ops, &[(1, 9)]);
    let nops = ops.len();
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t: fun_t,
        findex,
        regs: vec![
            icon_t,
            item_t,
            tip_t,
            ctor_args[2],
            ctor_args[3],
            ctor_args[4],
            RefType(0),
            dyn_t,
            flow_t,
            flow_parent_t,
        ],
        ops,
        debug_info: Some(vec![(dbg_file, 1); nops]),
        assigns: Some(vec![]),
        parent: None,
    });
    Ok(findex)
}

/// ItemIcon is a plain h2d.Flow with no hover handling, so the preview's
/// `new ItemIcon(item, null, flow)` becomes
/// `var e = new Element(flow); new ItemIcon(item, null, e); e.getTipContent = <tip fn bound to icon>`.
/// ui.comp.Element creates its own interactive and shows getTipContent() on hover.
fn patch_start_choice_item_tips(code: &mut Bytecode) -> Result<()> {
    let start_t = obj_type(code, "ui.win.StartChoice")?;
    let icon_t = obj_type(code, "ui.comp.ItemIcon")?;
    let icon_ctor = method(code, icon_t, "__constructor__")?.findex;
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let elem_ctor = method(code, elem_t, "__constructor__")?;
    if fun_args(code, elem_ctor).len() != 2 {
        bail!("unexpected Element constructor signature");
    }
    let elem_ctor = elem_ctor.findex;
    let (tip_field, tip_field_t) = field(code, elem_t, "getTipContent")?;
    let dbg_file = debug_file(code, "src/ui/win/StartChoice.hx")?;

    // The preview closure: the only StartChoice function that constructs an ItemIcon.
    let mut sites = vec![];
    for (fi, f) in code.functions.iter().enumerate() {
        if f.regs.first() != Some(&start_t) {
            continue;
        }
        for (oi, op) in f.ops.iter().enumerate() {
            if let Opcode::Call4 {
                fun, arg0, arg3, ..
            } = op
            {
                if *fun == icon_ctor {
                    sites.push((fi, oi, *arg0, *arg3));
                }
            }
        }
    }
    let [(fi, oi, icon_reg, parent_reg)] = sites[..] else {
        bail!(
            "expected exactly one ItemIcon construction in StartChoice, found {}",
            sites.len()
        );
    };

    let tip_fn = add_icon_tip_function(code, dbg_file)?;
    let f = &mut code.functions[fi];
    let void_reg = Reg(f
        .regs
        .iter()
        .position(|t| t.is_void())
        .context("no void register")? as u32);
    f.regs.push(elem_t);
    let elem_reg = Reg((f.regs.len() - 1) as u32);
    f.regs.push(tip_field_t);
    let closure_reg = Reg((f.regs.len() - 1) as u32);

    // Later edits first so earlier indices stay valid.
    insert_ops(
        f,
        oi + 1,
        vec![
            Opcode::InstanceClosure {
                dst: closure_reg,
                fun: tip_fn,
                obj: icon_reg,
            },
            Opcode::SetField {
                obj: elem_reg,
                field: tip_field,
                src: closure_reg,
            },
        ],
    );
    if let Opcode::Call4 { arg3, .. } = &mut f.ops[oi] {
        *arg3 = elem_reg;
    }
    // Right before the ItemIcon constructor call, where the parent register is final.
    insert_ops(
        f,
        oi,
        vec![
            Opcode::New { dst: elem_reg },
            Opcode::Call2 {
                dst: void_reg,
                fun: elem_ctor,
                arg0: elem_reg,
                arg1: parent_reg,
            },
        ],
    );
    eprintln!(
        "patched StartChoice preview fn@{}: Element wrapper at op {}, ItemIcon ctor at op {} (icon reg {}, parent reg {}), tip fn@{}",
        f.findex.0, oi, oi + 2, icon_reg.0, parent_reg.0, tip_fn.0
    );
    Ok(())
}

/// Index of `value` in the i32 constant pool, appended when missing.
fn int_const(code: &mut Bytecode, value: i32) -> hlbc::types::RefInt {
    if let Some(i) = code.ints.iter().position(|&v| v == value) {
        return hlbc::types::RefInt(i);
    }
    code.ints.push(value);
    hlbc::types::RefInt(code.ints.len() - 1)
}

fn float_const(code: &mut Bytecode, value: f64) -> hlbc::types::RefFloat {
    if let Some(i) = code.floats.iter().position(|&v| v == value) {
        return hlbc::types::RefFloat(i);
    }
    code.floats.push(value);
    hlbc::types::RefFloat(code.floats.len() - 1)
}

/// The virtual type whose field names are exactly `names` (sorted, as HL stores them).
fn virtual_type(code: &Bytecode, names: &[&str]) -> Result<RefType> {
    code.types
        .iter()
        .position(|t| match t {
            Type::Virtual { fields } => {
                fields.len() == names.len()
                    && fields.iter().zip(names).all(|(f, n)| s(code, f.name) == *n)
            }
            _ => false,
        })
        .map(RefType)
        .with_context(|| format!("virtual type {names:?} not found"))
}

/// Resolves jump offsets in a hand-written op list: `(target_index, op)` where the
/// op's offset is a placeholder replaced with `target - i - 1`.
fn resolve_jumps(ops: &mut [Opcode], targets: &[(usize, usize)]) {
    for &(i, target) in targets {
        let off = target as i32 - i as i32 - 1;
        match &mut ops[i] {
            Opcode::JNull { offset, .. }
            | Opcode::JNotNull { offset, .. }
            | Opcode::JSGte { offset, .. }
            | Opcode::JSGt { offset, .. }
            | Opcode::JEq { offset, .. }
            | Opcode::JNotEq { offset, .. }
            | Opcode::JFalse { offset, .. }
            | Opcode::JAlways { offset }
            | Opcode::Trap { offset, .. } => *offset = off,
            _ => unreachable!("not a jump"),
        }
    }
}

/// New function `(unitClass row) -> h2d.Object`: a vertical h2d.Flow holding the
/// class's starting skills as stacked `ui.comp.SkillTip` cards.
///
/// ```text
/// var flow = new h2d.Flow(null); flow.isVertical = true;
/// for (e in cls.baseSkills) {
///     if (e == null) continue;
///     if (e.minLevel != null && e.minLevel != 0) continue;   // tree skill, not a starting one
///     if (e.learnLevel != null && e.learnLevel > 1) continue; // learnt later (animals, pit recruits)
///     var skill = e.skill; if (skill == null) continue;
///     try { var tip = new SkillTip(null, null, skill, 1, null, null); flow.addChild(tip); }
///     catch (_) {}
/// }
/// return flow;
/// ```
///
/// `minLevel == null` marks the class's innate skills and `minLevel == 0` its
/// starting utility pool (Unit.setClassLevel draws one of those at random); every
/// other entry is unlocked on the skill tree. The tip is built without a parent and
/// attached only once its constructor returned, so a skill whose tooltip code
/// dereferences the (absent) unit — Purge/Inhalation read its equipped weapon —
/// is dropped whole instead of throwing to the game's uncaught-exception screen.
fn add_class_tip_function(code: &mut Bytecode, cls_t: RefType, dbg_file: usize) -> Result<RefFun> {
    let obj_t = obj_type(code, "h2d.Object")?;
    let add_child = proto(code, obj_t, "addChild")?;
    let flow_t = obj_type(code, "h2d.Flow")?;
    let flow_ctor = method(code, flow_t, "__constructor__")?;
    let (flow_ctor_findex, flow_parent_t) = (flow_ctor.findex, fun_args(code, flow_ctor)[1]);
    let set_vertical = method(code, flow_t, "set_isVertical")?.findex;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let (arr_f, arr_inner_t) = field(code, arr_t, "array")?;
    let (len_f, i32_t) = field(code, arr_t, "length")?;
    let (base_skills_f, base_skills_t) = field_of_virtual(code, cls_t, "baseSkills")?;
    if base_skills_t != arr_t {
        bail!("unitClass.baseSkills is not an ArrayObj");
    }
    let entry_t = virtual_type(code, &["learnLevel", "minLevel", "requires", "skill"])?;
    let (min_level_f, level_t) = field_of_virtual(code, entry_t, "minLevel")?;
    let (learn_level_f, learn_level_t) = field_of_virtual(code, entry_t, "learnLevel")?;
    if learn_level_t != level_t || !matches!(&code.types[level_t.0], Type::Null(t) if *t == i32_t) {
        bail!("baseSkills minLevel/learnLevel are not null<i32>");
    }
    let get_skill = method(code, entry_t, "get_skill")?;
    let (get_skill_findex, skill_t) = (get_skill.findex, get_skill.t.as_fun(code).unwrap().ret);
    let tip_t = obj_type(code, "ui.comp.SkillTip")?;
    let tip_ctor = method(code, tip_t, "__constructor__")?;
    let tip_args = fun_args(code, tip_ctor);
    let tip_ctor_findex = tip_ctor.findex;
    if tip_args.len() != 7 || tip_args[3] != skill_t || tip_args[4] != i32_t {
        bail!("unexpected SkillTip constructor signature");
    }
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let ret_t = field(code, elem_t, "getTipContent")?
        .1
        .as_fun(code)
        .unwrap()
        .ret;
    let zero = int_const(code, 0);
    let one = int_const(code, 1);
    let findex = next_findex(code)?;
    code.types.push(Type::Fun(TypeFun {
        args: vec![cls_t],
        ret: ret_t,
    }));
    let fun_t = RefType(code.types.len() - 1);

    // r0 cls, r1 flow, r2 void, r3 baseSkills, r4 i, r5 len, r6 raw array, r7 dyn,
    // r8 entry, r9 skill, r10 tip, r11 unit, r12 stats, r13 level, r14 vars, r15 parent,
    // r16/r17 bool, r18 null<i32> level field, r19 its value, r20 bound, r21 exc
    let regs = vec![
        cls_t,
        flow_t,
        void_t,
        arr_t,
        i32_t,
        i32_t,
        arr_inner_t,
        dyn_t,
        entry_t,
        skill_t,
        tip_t,
        tip_args[1],
        tip_args[2],
        i32_t,
        tip_args[5],
        flow_parent_t,
        bool_t,
        bool_t,
        level_t,
        i32_t,
        i32_t,
        dyn_t,
    ];
    let r = Reg;
    let mut ops = vec![
        Opcode::New { dst: r(1) },
        Opcode::Null { dst: r(15) },
        Opcode::Call2 {
            dst: r(2),
            fun: flow_ctor_findex,
            arg0: r(1),
            arg1: r(15),
        },
        Opcode::Bool {
            dst: r(16),
            value: hlbc::types::ValBool(true),
        },
        Opcode::Call2 {
            dst: r(17),
            fun: set_vertical,
            arg0: r(1),
            arg1: r(16),
        },
        Opcode::Field {
            dst: r(3),
            obj: r(0),
            field: base_skills_f,
        },
        Opcode::JNull {
            reg: r(3),
            offset: 0,
        }, // 6 -> END
        Opcode::Int {
            dst: r(4),
            ptr: zero,
        },
        Opcode::Label, // 8 LOOP
        Opcode::Field {
            dst: r(5),
            obj: r(3),
            field: len_f,
        },
        Opcode::JSGte {
            a: r(4),
            b: r(5),
            offset: 0,
        }, // 10 -> END
        Opcode::Field {
            dst: r(6),
            obj: r(3),
            field: arr_f,
        },
        Opcode::GetArray {
            dst: r(7),
            array: r(6),
            index: r(4),
        },
        Opcode::ToVirtual {
            dst: r(8),
            src: r(7),
        },
        Opcode::Incr { dst: r(4) },
        Opcode::JNull {
            reg: r(8),
            offset: 0,
        }, // 15 -> LOOP
        Opcode::Field {
            dst: r(18),
            obj: r(8),
            field: min_level_f,
        },
        Opcode::JNull {
            reg: r(18),
            offset: 0,
        }, // 17 -> LEARN
        Opcode::SafeCast {
            dst: r(19),
            src: r(18),
        },
        Opcode::Int {
            dst: r(20),
            ptr: zero,
        },
        Opcode::JNotEq {
            a: r(19),
            b: r(20),
            offset: 0,
        }, // 20 -> LOOP
        Opcode::Field {
            dst: r(18),
            obj: r(8),
            field: learn_level_f,
        }, // 21 LEARN
        Opcode::JNull {
            reg: r(18),
            offset: 0,
        }, // 22 -> SKILL
        Opcode::SafeCast {
            dst: r(19),
            src: r(18),
        },
        Opcode::Int {
            dst: r(20),
            ptr: one,
        },
        Opcode::JSGt {
            a: r(19),
            b: r(20),
            offset: 0,
        }, // 25 -> LOOP
        Opcode::Call1 {
            dst: r(9),
            fun: get_skill_findex,
            arg0: r(8),
        }, // 26 SKILL
        Opcode::JNull {
            reg: r(9),
            offset: 0,
        }, // 27 -> LOOP
        Opcode::Trap {
            exc: r(21),
            offset: 0,
        }, // 28 -> CATCH
        Opcode::New { dst: r(10) },
        Opcode::Null { dst: r(11) },
        Opcode::Null { dst: r(12) },
        Opcode::Int {
            dst: r(13),
            ptr: one,
        },
        Opcode::Null { dst: r(14) },
        Opcode::Null { dst: r(15) },
        Opcode::CallN {
            dst: r(2),
            fun: tip_ctor_findex,
            args: vec![r(10), r(11), r(12), r(9), r(13), r(14), r(15)],
        },
        Opcode::Call2 {
            dst: r(2),
            fun: add_child,
            arg0: r(1),
            arg1: r(10),
        },
        Opcode::EndTrap { exc: r(21) },
        Opcode::JAlways { offset: 0 }, // 38 -> LOOP
        Opcode::JAlways { offset: 0 }, // 39 CATCH -> LOOP
        Opcode::Ret { ret: r(1) },     // 40 END
    ];
    resolve_jumps(
        &mut ops,
        &[
            (6, 40),
            (10, 40),
            (15, 8),
            (17, 21),
            (20, 8),
            (22, 26),
            (25, 8),
            (27, 8),
            (28, 39),
            (38, 8),
            (39, 8),
        ],
    );
    let nops = ops.len();
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t: fun_t,
        findex,
        regs,
        ops,
        debug_info: Some(vec![(dbg_file, 2); nops]),
        assigns: Some(vec![]),
        parent: None,
    });
    Ok(findex)
}

fn field_of_virtual(code: &Bytecode, t: RefType, name: &str) -> Result<(RefField, RefType)> {
    match &code.types[t.0] {
        Type::Virtual { fields } => fields
            .iter()
            .position(|f| s(code, f.name) == name)
            .map(|i| (RefField(i), fields[i].t))
            .with_context(|| format!("virtual field {name} not found")),
        _ => bail!("type {} is not virtual", t.0),
    }
}

/// The preview lists one `TextFixed` per unit of the troop pattern ("class + trait").
/// TextFixed is a plain h2d.Text, so as with the item icons it gets an
/// `ui.comp.Element` wrapper: `new TextFixed(troopList)` becomes
/// `var e = new Element(troopList); new TextFixed(e)` and after
/// `cls = get_unitClass(entry)` we bind `e.getTipContent = <class tip fn bound to cls>`.
fn patch_start_choice_unit_tips(code: &mut Bytecode) -> Result<()> {
    let start_t = obj_type(code, "ui.win.StartChoice")?;
    let text_t = obj_type(code, "ui.comp.TextFixed")?;
    let text_ctor = method(code, text_t, "__constructor__")?.findex;
    let elem_t = obj_type(code, "ui.comp.Element")?;
    let elem_ctor = method(code, elem_t, "__constructor__")?.findex;
    let (tip_field, tip_field_t) = field(code, elem_t, "getTipContent")?;
    let dbg_file = debug_file(code, "src/ui/win/StartChoice.hx")?;

    let mut sites = vec![];
    for (fi, f) in code.functions.iter().enumerate() {
        if f.regs.first() != Some(&start_t) {
            continue;
        }
        for (oi, op) in f.ops.iter().enumerate() {
            if let Opcode::Call1 { dst, fun, .. } = op {
                let callee = code.functions.iter().find(|g| g.findex == *fun);
                if callee.is_some_and(|g| s(code, g.name) == "get_unitClass") {
                    sites.push((fi, oi, *dst));
                }
            }
        }
    }
    let [(fi, oi, cls_reg)] = sites[..] else {
        bail!(
            "expected exactly one get_unitClass call in StartChoice, found {}",
            sites.len()
        );
    };
    let cls_t = code.functions[fi].regs[cls_reg.0 as usize];
    let (new_at, txt_reg) = (0..oi)
        .rev()
        .find_map(|i| match code.functions[fi].ops[i] {
            Opcode::New { dst } if code.functions[fi].regs[dst.0 as usize] == text_t => {
                Some((i, dst))
            }
            _ => None,
        })
        .context("New TextFixed not found before get_unitClass")?;
    let ctor_at = (new_at..oi)
        .find(|&i| {
            matches!(code.functions[fi].ops[i],
                Opcode::Call2 { fun, arg0, .. } if fun == text_ctor && arg0 == txt_reg)
        })
        .context("TextFixed constructor call not found")?;

    let tip_fn = add_class_tip_function(code, cls_t, dbg_file)?;
    let f = &mut code.functions[fi];
    let void_reg = Reg(f
        .regs
        .iter()
        .position(|t| t.is_void())
        .context("no void register")? as u32);
    let Opcode::Call2 {
        arg1: parent_reg, ..
    } = f.ops[ctor_at]
    else {
        unreachable!()
    };
    f.regs.push(elem_t);
    let elem_reg = Reg((f.regs.len() - 1) as u32);
    f.regs.push(tip_field_t);
    let closure_reg = Reg((f.regs.len() - 1) as u32);

    // Later edits first so earlier indices stay valid.
    insert_ops(
        f,
        oi + 1,
        vec![
            Opcode::InstanceClosure {
                dst: closure_reg,
                fun: tip_fn,
                obj: cls_reg,
            },
            Opcode::SetField {
                obj: elem_reg,
                field: tip_field,
                src: closure_reg,
            },
        ],
    );
    if let Opcode::Call2 { arg1, .. } = &mut f.ops[ctor_at] {
        *arg1 = elem_reg;
    }
    // Right before the TextFixed constructor call: the parent register is loaded
    // between `New TextFixed` and the call, so any earlier point would see a stale value.
    insert_ops(
        f,
        ctor_at,
        vec![
            Opcode::New { dst: elem_reg },
            Opcode::Call2 {
                dst: void_reg,
                fun: elem_ctor,
                arg0: elem_reg,
                arg1: parent_reg,
            },
        ],
    );
    eprintln!(
        "patched StartChoice preview fn@{}: unit line Element wrapper at op {}, class tip after op {} (class reg {}, text reg {}, parent reg {}), tip fn@{}",
        f.findex.0, ctor_at, oi + 2, cls_reg.0, txt_reg.0, parent_reg.0, tip_fn.0
    );
    Ok(())
}

// ---------- inspection ----------

fn inspect(code: &Bytecode, name: &str) -> Result<()> {
    let t = obj_type(code, name)?;
    let mut cur = Some(t);
    while let Some(ct) = cur {
        let o = obj(code, ct)?;
        println!(
            "type@{} {} ({} own fields, {} protos, {} bindings)",
            ct.0,
            s(code, o.name),
            o.own_fields.len(),
            o.protos.len(),
            o.bindings.len()
        );
        for f in &o.own_fields {
            println!("  field {}: type@{}", s(code, f.name), f.t.0);
        }
        for p in &o.protos {
            println!("  proto {} -> fn@{}", s(code, p.name), p.findex.0);
        }
        for (fi, fun) in &o.bindings {
            println!("  binding field#{} -> fn@{}", fi.0, fun.0);
        }
        cur = o.super_;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    fn same(a: &Function, b: &Function) -> bool {
        format!("{:?}", a.ops) == format!("{:?}", b.ops)
            && a.regs == b.regs
            && a.debug_info == b.debug_info
    }

    /// Every jump stays in range, and each Trap is closed by an EndTrap on the same
    /// dynamic register with no jump leaving the protected block, the handler right
    /// after it and unreachable by fallthrough.
    fn check_structure(code: &Bytecode, f: &Function) -> usize {
        let n = f.ops.len();
        for i in 0..n {
            for t in jump_targets(f, i) {
                assert!(t < n, "fn@{} op {i} jumps out of range", f.findex.0);
            }
        }
        let mut traps = 0;
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Trap { exc, .. } = *op else {
                continue;
            };
            traps += 1;
            assert!(matches!(code.types[f.regs[exc.0 as usize].0], Type::Dyn));
            let [handler] = jump_targets(f, i)[..] else {
                unreachable!()
            };
            let end = (i + 1..n)
                .find(|&j| matches!(f.ops[j], Opcode::EndTrap { exc: e } if e == exc))
                .expect("EndTrap");
            assert!(handler > end, "handler inside the protected block");
            assert!(matches!(
                f.ops[handler - 1],
                Opcode::JAlways { .. } | Opcode::Ret { .. }
            ));
            for j in i + 1..end {
                assert!(!matches!(
                    f.ops[j],
                    Opcode::Trap { .. } | Opcode::Ret { .. }
                ));
                for t in jump_targets(f, j) {
                    assert!(t > i && t <= end, "op {j} leaves the trap block");
                }
            }
        }
        traps
    }

    /// A bytecode format other than 4 or 5 is refused before anything is parsed.
    #[test]
    fn unsupported_bytecode_version_is_refused() {
        for v in [3u8, 6] {
            let err = patch_image(&[b'H', b'L', b'B', v, 0, 0, 0, 0]).unwrap_err();
            assert!(format!("{err:#}").contains("nsupported"), "version {v}: {err:#}");
        }
    }

    /// The installed game is patched in full; the same image with one co-op
    /// function changed (Controller.waitForClients no longer reads waitLocks,
    /// so the barrier pass mismatches) is refused as a whole, naming that pass.
    #[test]
    fn a_mismatching_pass_refuses_the_image() {
        let Some(image) = game() else { return };
        patch_image(&image).expect("installed game: every pass applies");
        let mut code = read(&image);
        let ctrl = obj_type(&code, "st.Controller").unwrap();
        let fi = fun_index(&code, method(&code, ctrl, "waitForClients").unwrap().findex).unwrap();
        for op in &mut code.functions[fi].ops {
            if matches!(op, Opcode::GetThis { .. }) {
                *op = Opcode::Nop;
            }
        }
        let mut broken = Vec::new();
        code.serialize(&mut broken).expect("write");
        let err = format!("{:#}", patch_image(&broken).unwrap_err());
        assert!(err.contains("barrier skipped"), "{err}");
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent): the
    /// image round-trips, only the StartChoice preview changes, the two tooltip
    /// functions are appended, and every tooltip constructor runs under a trap.
    #[test]
    fn start_choice_tips_on_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut out = Vec::new();
        orig.serialize(&mut out).expect("write");
        assert!(out == image, "unpatched round-trip is not byte-identical");

        let mut code = read(&image);
        patch_start_choice_item_tips(&mut code).expect("item tips");
        patch_start_choice_unit_tips(&mut code).expect("unit tips");
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 2);
        assert_eq!(back.types.len(), orig.types.len() + 2);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        assert_eq!(back.strings, orig.strings);
        assert_eq!(back.ints[..orig.ints.len()], orig.ints[..]);
        let start_t = obj_type(&orig, "ui.win.StartChoice").unwrap();
        let changed: Vec<usize> = (0..orig.functions.len())
            .filter(|&i| !same(&orig.functions[i], &back.functions[i]))
            .collect();
        let [pi] = changed[..] else {
            panic!("expected one changed function, got {changed:?}")
        };
        assert_eq!(orig.functions[pi].regs[0], start_t);
        let p = &back.functions[pi];
        assert_eq!(p.ops.len(), orig.functions[pi].ops.len() + 8);
        assert_eq!(check_structure(&back, p), 0);

        let icon_fn = &back.functions[orig.functions.len()];
        let class_fn = &back.functions[orig.functions.len() + 1];
        assert_eq!(check_structure(&back, icon_fn), 1);
        assert_eq!(check_structure(&back, class_fn), 1);

        // The SkillTip is built parentless inside the trap and attached right after.
        let tip_t = obj_type(&back, "ui.comp.SkillTip").unwrap();
        let tip_ctor = method(&back, tip_t, "__constructor__").unwrap().findex;
        let add_child = proto(&back, obj_type(&back, "h2d.Object").unwrap(), "addChild").unwrap();
        let at = class_fn
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::CallN { fun, .. } if *fun == tip_ctor))
            .expect("SkillTip ctor call");
        let Opcode::CallN { args, .. } = &class_fn.ops[at] else {
            unreachable!()
        };
        assert!(class_fn.ops[..at]
            .iter()
            .rev()
            .take(6)
            .any(|o| matches!(o, Opcode::Null { dst } if *dst == args[6])));
        assert!(matches!(class_fn.ops[at + 1],
            Opcode::Call2 { fun, arg1, .. } if fun == add_child && arg1 == args[0]));
        assert!(matches!(class_fn.ops[at + 2], Opcode::EndTrap { .. }));
        assert!(matches!(class_fn.ops[at - 7], Opcode::Trap { .. }));

        // The starting-skill filter reads minLevel and learnLevel before get_skill.
        let entry_t =
            virtual_type(&back, &["learnLevel", "minLevel", "requires", "skill"]).unwrap();
        let (min_f, _) = field_of_virtual(&back, entry_t, "minLevel").unwrap();
        let (learn_f, _) = field_of_virtual(&back, entry_t, "learnLevel").unwrap();
        let reads = |fld: RefField| {
            class_fn.ops[..at]
                .iter()
                .any(|o| matches!(o, Opcode::Field { field, obj, .. } if *field == fld && class_fn.regs[obj.0 as usize] == entry_t))
        };
        assert!(reads(min_f) && reads(learn_f));
    }
}
