// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// wartales-tips: patches Wartales' HashLink bytecode (hlboot.dat) so the item
// icons shown on the new-game "start choice" preview carry the game's own
// ItemTip tooltip on hover, and so enemy area attacks also hit the caster's
// allies (see friendly_fire.rs).

mod friendly_fire;

use anyhow::{bail, Context, Result};
use hlbc::opcodes::Opcode;
use hlbc::types::{Function, RefField, RefFun, RefType, Reg, Type, TypeFun, TypeObj};
use hlbc::Bytecode;
use std::io::Cursor;

/// Marker written nowhere in the image; used by callers to name this patch in logs.
pub const PATCH_NAME: &str = "wartales-tips start-choice item tooltips";

/// Applies every patch to a bytecode image held in memory and returns the new image.
pub fn patch_image(image: &[u8]) -> Result<Vec<u8>> {
    let mut code = Bytecode::deserialize(&mut Cursor::new(image)).context("read bytecode")?;
    patch_start_choice_item_tips(&mut code)?;
    patch_start_choice_unit_tips(&mut code)?;
    friendly_fire::patch_enemy_area_friendly_fire(&mut code).context("friendly fire")?;
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

/// New function `(ui.comp.ItemIcon) -> h2d.Object { return new ItemTip(this.item, null, null, null); }`.
/// Bound with InstanceClosure it becomes the `() -> h2d.Object` that `Element.getTipContent` expects.
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
    let findex = next_findex(code)?;
    code.types.push(Type::Fun(TypeFun {
        args: vec![icon_t],
        ret: ret_t,
    }));
    let fun_t = RefType(code.types.len() - 1);
    let ops = vec![
        Opcode::Field {
            dst: Reg(1),
            obj: Reg(0),
            field: item_f,
        },
        Opcode::New { dst: Reg(2) },
        Opcode::Null { dst: Reg(3) },
        Opcode::Null { dst: Reg(4) },
        Opcode::Null { dst: Reg(5) },
        Opcode::CallN {
            dst: Reg(6),
            fun: ctor_findex,
            args: vec![Reg(2), Reg(1), Reg(3), Reg(4), Reg(5)],
        },
        Opcode::Ret { ret: Reg(2) },
    ];
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
            | Opcode::JSGte { offset, .. }
            | Opcode::JEq { offset, .. }
            | Opcode::JNotEq { offset, .. }
            | Opcode::JFalse { offset, .. }
            | Opcode::JAlways { offset } => *offset = off,
            _ => unreachable!("not a jump"),
        }
    }
}

/// New function `(unitClass row) -> h2d.Object`:
/// a vertical h2d.Flow holding one `ui.comp.SkillTip(null, null, skill, 1, null, flow)` per
/// `baseSkills` entry, i.e. the class's own skill tooltips stacked into one card.
fn add_class_tip_function(code: &mut Bytecode, cls_t: RefType, dbg_file: usize) -> Result<RefFun> {
    let flow_t = obj_type(code, "h2d.Flow")?;
    let flow_ctor = method(code, flow_t, "__constructor__")?;
    let (flow_ctor_findex, flow_parent_t) = (flow_ctor.findex, fun_args(code, flow_ctor)[1]);
    let set_vertical = method(code, flow_t, "set_isVertical")?.findex;
    let bool_t = RefType(7);
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let (arr_f, arr_inner_t) = field(code, arr_t, "array")?;
    let (len_f, i32_t) = field(code, arr_t, "length")?;
    let (base_skills_f, base_skills_t) = field_of_virtual(code, cls_t, "baseSkills")?;
    if base_skills_t != arr_t {
        bail!("unitClass.baseSkills is not an ArrayObj");
    }
    let entry_t = virtual_type(code, &["learnLevel", "minLevel", "requires", "skill"])?;
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
    // r8 entry, r9 skill, r10 tip, r11 unit, r12 stats, r13 level, r14 vars, r15 parent, r16/r17 bool
    let regs = vec![
        cls_t,
        flow_t,
        RefType(0),
        arr_t,
        i32_t,
        i32_t,
        arr_inner_t,
        RefType(9),
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
        Opcode::Call1 {
            dst: r(9),
            fun: get_skill_findex,
            arg0: r(8),
        },
        Opcode::JNull {
            reg: r(9),
            offset: 0,
        }, // 16 -> LOOP
        Opcode::New { dst: r(10) },
        Opcode::Null { dst: r(11) },
        Opcode::Null { dst: r(12) },
        Opcode::Int {
            dst: r(13),
            ptr: one,
        },
        Opcode::Null { dst: r(14) },
        Opcode::CallN {
            dst: r(2),
            fun: tip_ctor_findex,
            args: vec![r(10), r(11), r(12), r(9), r(13), r(14), r(1)],
        },
        Opcode::JAlways { offset: 0 }, // 23 -> LOOP
        Opcode::Ret { ret: r(1) },     // 24 END
    ];
    resolve_jumps(&mut ops, &[(6, 24), (10, 24), (16, 8), (23, 8)]);
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
