// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Battle skill bar: no holes between skills, passives in their own order.
//
// A unit saves its bar per mode as pages of 10 skill ids (`st.Unit.skillsLayouts`,
// `SkillsLayout.skillBars`). Ids of skills the unit no longer has (other weapon
// set, items, statuses, replaced skills) are never removed and a new skill takes
// the first free slot or a new page, so the bar spreads a few skills over many
// pages, and the vanilla drag-reorder (`PREFS.editSkillbar`) only swaps inside
// one page. `battle.ui.win.SkillBar.init` (SkillBar.hx:187-298) rebuilds
// `this.skillsLayout` (pages of SkillIcon) from those saved positions.
//
// Display-only fix, no data write: right before init first reads the page count
// (`nbActiveSkillBar`, SkillBar.hx:491) a new `compact(bar)` repacks the icons
// in page order into pages of 10 (last one padded with null) and, when the
// current page is gone, switches to the last one. The saved layout changes only
// through the vanilla drag (`saveSkillsLayout` -> `saveOrderedSkills` RPC, host
// owned), which then stores the packed bar of the skills shown; a skill that
// comes back later (weapon swap) takes the first free slot, at the end. Nothing
// here writes synced data, so host and guests never fight over it. When the
// unit has no layout yet the positions come back after an RPC: no icon is
// placed at that point, `compact` keeps the bar, and vanilla builds a packed
// layout itself.
//
// Passives were each added at child 0 of `passiveSkillsContent`
// (SkillBar.hx:300), which reverses them; they now go at `children.length`.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};

const PAGE: i32 = 10;

struct Plan {
    fi: usize,
    bar_t: RefType,
    arr_t: RefType,
    icon_t: RefType,
    type_t: RefType,
    alloc_ret_t: RefType,
    wrap_arg_t: RefType,
    i32_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    alloc: RefFun,
    wrap: RefFun,
    push: RefFun,
    switch: RefFun,
    layout: RefField,
    cur: RefField,
    arr_raw: RefField,
    arr_len: RefField,
    children: RefField,
    /// init op index of the first `nbActiveSkillBar(this)` call.
    call_at: usize,
    /// init op index of the `Int k = 0` before `passiveSkillsContent.addChildAt(icon, k)`.
    passive_at: usize,
    dbg_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bar_t = obj_type(code, "battle.ui.win.SkillBar")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let icon_t = obj_type(code, "ui.comp.SkillIcon")?;
    let obj_t = obj_type(code, "h2d.Object")?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let layout = typed(code, bar_t, "skillsLayout", arr_t)?;
    let cur = typed(code, bar_t, "currentSkillBarIndex", i32_t)?;
    let passive = field(code, bar_t, "passiveSkillsContent")?.0;
    let arr_raw = field(code, arr_t, "array")?.0;
    let arr_len = typed(code, arr_t, "length", i32_t)?;
    let children = typed(code, obj_t, "children", arr_t)?;
    let push = method(code, arr_t, "push")?.findex;
    let nb = method(code, bar_t, "nbActiveSkillBar")?.findex;
    let switch = method(code, bar_t, "switchSkillBar")?;
    if fun_args(code, switch)[..] != [bar_t, i32_t] {
        bail!("switchSkillBar: unexpected signature");
    }
    let switch = switch.findex;
    let f = method(code, bar_t, "init")?;
    let fi = fun_index(code, f.findex)?;
    let ops = &f.ops;
    let zero = |p: hlbc::types::RefInt| code.ints.get(p.0) == Some(&0);

    // `this.skillsLayout = new ArrayObj(alloc_array(ArrayObj, 0))` (SkillBar.hx:187).
    let mut alloc_site = None;
    for w in ops.windows(6) {
        if let [Opcode::Int { dst: z, ptr }, Opcode::Type { dst: tr, ty }, Opcode::Call2 { dst: ar, fun: alloc, arg0, arg1 }, Opcode::UnsafeCast { dst: cr, src }, Opcode::Call1 { dst: lr, fun: wrap, arg0: c }, Opcode::SetThis { field, src: l }] = w
        {
            if *field == layout && *ty == arr_t && zero(*ptr) && arg0 == tr && arg1 == z
                && src == ar && c == cr && l == lr
            {
                alloc_site = Some((*alloc, *wrap, f.regs[tr.0 as usize], f.regs[ar.0 as usize], f.regs[cr.0 as usize]));
            }
        }
    }
    let Some((alloc, wrap, type_t, alloc_ret_t, wrap_arg_t)) = alloc_site else {
        bail!("init: no skillsLayout allocation");
    };

    let Some(call_at) = ops.iter().position(
        |o| matches!(o, Opcode::Call1 { fun, arg0: Reg(0), .. } if *fun == nb),
    ) else {
        bail!("init: no nbActiveSkillBar call");
    };
    let mut passive_sites = Vec::new();
    for (i, w) in ops.windows(4).enumerate() {
        if let [Opcode::GetThis { dst: a, field }, Opcode::NullCheck { reg: n }, Opcode::Int { dst: k, ptr }, Opcode::Call3 { arg0, arg2, .. }] = w
        {
            if *field == passive && n == a && arg0 == a && arg2 == k && zero(*ptr) {
                passive_sites.push(i + 2);
            }
        }
    }
    let passive_at = match passive_sites[..] {
        [i] => i,
        [] => bail!("init: passive addChildAt(icon, 0) not found (already applied or changed)"),
        _ => bail!("init: {} passive addChildAt sites, want 1", passive_sites.len()),
    };
    if passive_at > call_at {
        bail!("init: passives added after the page count is read");
    }
    for at in [call_at, passive_at + 1] {
        if (0..ops.len()).any(|i| jump_targets(f, i).contains(&at)) {
            bail!("init: op {at} is a jump target");
        }
    }
    if !matches!(code.types[f.regs[2].0], Type::Void) {
        bail!("init: reg2 is not void");
    }
    Ok(Plan {
        fi,
        bar_t,
        arr_t,
        icon_t,
        type_t,
        alloc_ret_t,
        wrap_arg_t,
        i32_t,
        dyn_t,
        void_t,
        alloc,
        wrap,
        push,
        switch,
        layout,
        cur,
        arr_raw,
        arr_len,
        children,
        call_at,
        passive_at,
        dbg_file: f.debug_info.as_ref().map_or(0, |d| d[call_at].0),
    })
}

/// `compact(bar)`: repacks `bar.skillsLayout` into full pages of 10, see the header.
fn compact_ops(code: &mut Bytecode, p: &Plan) -> (Vec<RefType>, Vec<Opcode>) {
    let zero_i = int_const(code, 0);
    let one_i = int_const(code, 1);
    let page_i = int_const(code, PAGE);
    let raw_t = code.types[p.arr_t.0]
        .get_type_obj()
        .map(|o| o.fields[p.arr_raw.0].t)
        .expect("ArrayObj.array");
    let mut r = Regs(vec![p.bar_t]);
    let bar = Reg(0);
    let lay = r.r(p.arr_t);
    let flat = r.r(p.arr_t);
    let out = r.r(p.arr_t);
    let page = r.r(p.arr_t);
    let raw = r.r(raw_t);
    let d = r.r(p.dyn_t);
    let ty = r.r(p.type_t);
    let ar = r.r(p.alloc_ret_t);
    let cr = r.r(p.wrap_arg_t);
    let [i, j, n, len, k, zero, one, ten, tmp] = [(); 9].map(|_| r.r(p.i32_t));
    let v = r.r(p.void_t);

    let mut a = Asm::new();
    let new_arr = |a: &mut Asm, elem: RefType, dst: Reg| {
        a.op(Opcode::Type { dst: ty, ty: elem });
        a.op(Opcode::Call2 { dst: ar, fun: p.alloc, arg0: ty, arg1: zero });
        a.op(Opcode::UnsafeCast { dst: cr, src: ar });
        a.op(Opcode::Call1 { dst, fun: p.wrap, arg0: cr });
    };
    a.op(Opcode::Int { dst: zero, ptr: zero_i });
    a.op(Opcode::Int { dst: one, ptr: one_i });
    a.op(Opcode::Int { dst: ten, ptr: page_i });
    a.op(Opcode::GetThis { dst: lay, field: p.layout });
    a.jmp(Opcode::JNull { reg: lay, offset: 0 }, "end");
    // flat = every icon, page by page
    new_arr(&mut a, p.icon_t, flat);
    a.op(Opcode::Int { dst: i, ptr: zero_i });
    a.loop_head("pages");
    a.op(Opcode::Field { dst: len, obj: lay, field: p.arr_len });
    a.jmp(Opcode::JSGte { a: i, b: len, offset: 0 }, "flat_done");
    a.op(Opcode::Field { dst: raw, obj: lay, field: p.arr_raw });
    a.op(Opcode::GetArray { dst: d, array: raw, index: i });
    a.op(Opcode::Incr { dst: i });
    a.op(Opcode::SafeCast { dst: page, src: d });
    a.jmp(Opcode::JNull { reg: page, offset: 0 }, "pages");
    a.op(Opcode::Int { dst: j, ptr: zero_i });
    a.loop_head("slots");
    a.op(Opcode::Field { dst: len, obj: page, field: p.arr_len });
    a.jmp(Opcode::JSGte { a: j, b: len, offset: 0 }, "pages");
    a.op(Opcode::Field { dst: raw, obj: page, field: p.arr_raw });
    a.op(Opcode::GetArray { dst: d, array: raw, index: j });
    a.op(Opcode::Incr { dst: j });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "slots");
    a.op(Opcode::Call2 { dst: tmp, fun: p.push, arg0: flat, arg1: d });
    a.jmp(Opcode::JAlways { offset: 0 }, "slots");
    a.label("flat_done");
    a.op(Opcode::Field { dst: n, obj: flat, field: p.arr_len });
    a.jmp(Opcode::JSLte { a: n, b: zero, offset: 0 }, "clamp");
    // out = pages of 10
    new_arr(&mut a, p.arr_t, out);
    a.op(Opcode::Int { dst: i, ptr: zero_i });
    a.loop_head("fill");
    a.jmp(Opcode::JSGte { a: i, b: n, offset: 0 }, "pad");
    a.op(Opcode::SMod { dst: k, a: i, b: ten });
    a.jmp(Opcode::JNotEq { a: k, b: zero, offset: 0 }, "put");
    new_arr(&mut a, p.icon_t, page);
    a.op(Opcode::Call2 { dst: tmp, fun: p.push, arg0: out, arg1: page });
    a.label("put");
    a.op(Opcode::Field { dst: raw, obj: flat, field: p.arr_raw });
    a.op(Opcode::GetArray { dst: d, array: raw, index: i });
    a.op(Opcode::Call2 { dst: tmp, fun: p.push, arg0: page, arg1: d });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JAlways { offset: 0 }, "fill");
    a.label("pad");
    a.op(Opcode::Null { dst: d });
    a.loop_head("pad_loop");
    a.op(Opcode::Field { dst: len, obj: page, field: p.arr_len });
    a.jmp(Opcode::JSGte { a: len, b: ten, offset: 0 }, "set");
    a.op(Opcode::Call2 { dst: tmp, fun: p.push, arg0: page, arg1: d });
    a.jmp(Opcode::JAlways { offset: 0 }, "pad_loop");
    a.label("set");
    a.op(Opcode::SetThis { field: p.layout, src: out });
    // the current page is gone (fewer pages than before): show the last one;
    // switchSkillBar refills the slots and the page number
    a.label("clamp");
    a.op(Opcode::GetThis { dst: lay, field: p.layout });
    a.op(Opcode::GetThis { dst: k, field: p.cur });
    a.op(Opcode::Field { dst: len, obj: lay, field: p.arr_len });
    a.jmp(Opcode::JSLt { a: k, b: zero, offset: 0 }, "fix");
    a.jmp(Opcode::JSLt { a: k, b: len, offset: 0 }, "end");
    a.label("fix");
    a.op(Opcode::Sub { dst: k, a: len, b: one });
    a.op(Opcode::Call2 { dst: v, fun: p.switch, arg0: bar, arg1: k });
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    (r.0, a.finish())
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let (regs, ops) = compact_ops(code, &p);
    let compact = push_fn(code, vec![p.bar_t], p.void_t, regs, ops, p.dbg_file)?;
    let f = &mut code.functions[p.fi];
    // later site first: the passive edit inserts one op before it
    insert_ops(
        f,
        p.call_at,
        vec![Opcode::Call1 { dst: Reg(2), fun: compact, arg0: Reg(0) }],
    );
    let Opcode::Int { dst: k, .. } = f.ops[p.passive_at] else { unreachable!() };
    let Opcode::GetThis { dst: content, .. } = f.ops[p.passive_at - 2] else { unreachable!() };
    let kids = new_reg(f, p.arr_t);
    f.ops[p.passive_at] = Opcode::Field { dst: kids, obj: content, field: p.children };
    insert_ops(
        f,
        p.passive_at + 1,
        vec![Opcode::Field { dst: k, obj: kids, field: p.arr_len }],
    );
    eprintln!(
        "patched skill bar fn@{}: pages packed by fn@{}, passives in order",
        f.findex.0, compact.0
    );
    Ok(())
}

/// Packs the battle skill bar pages and keeps passives in order, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_skillbar_compact(code: &mut Bytecode) {
    // apply fails only in push_fn, before init is edited
    let snap = Snap::take(code);
    if let Err(e) = plan(code).and_then(|p| apply(code, p)) {
        snap.restore(code);
        crate::skipped(format!("skill bar compact skipped: {e:#}"));
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
        let (fi, call_at, passive_at) = (p.fi, p.call_at, p.passive_at);
        let mut code = read(&image);
        patch_skillbar_compact(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len() + 1);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let new = back.functions.last().unwrap();
        check_flow(new);
        check_types(&back, new, 0..new.ops.len());

        // init: two ops more, every original op and jump kept around them
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        check_flow(b);
        assert_eq!(b.ops.len(), a.ops.len() + 2);
        let map = |t: usize| t + (t > passive_at) as usize + (t >= call_at) as usize;
        for i in 0..a.ops.len() {
            let ta: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), ta, "op {i}");
            if ta.is_empty() && i != passive_at {
                assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]), "op {i}");
            }
        }
        assert!(matches!(b.ops[call_at + 1], Opcode::Call1 { fun, arg0: Reg(0), .. } if fun == new.findex));
        assert!(matches!(b.ops[passive_at], Opcode::Field { .. }));
        assert!(matches!(b.ops[passive_at + 1], Opcode::Field { .. }));
        check_types(&back, b, passive_at..passive_at + 2);
        check_types(&back, b, call_at + 1..call_at + 2);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_skillbar_compact(&mut again);
        assert!(write(&again) == patched);
    }
}
