// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// wartales-tips: patches Wartales' HashLink bytecode (hlboot.dat) so the item
// icons shown on the new-game "start choice" preview carry the game's own
// ItemTip tooltip on hover.

use anyhow::{bail, Context, Result};
use hlbc::opcodes::Opcode;
use hlbc::types::{Function, RefField, RefFun, RefType, Reg, Type, TypeFun, TypeObj};
use hlbc::Bytecode;
use std::fs::File;
use std::io::BufWriter;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("patch") if args.len() == 4 => {
            let mut code = Bytecode::from_file(&args[2]).context("read bytecode")?;
            patch_start_choice_item_tips(&mut code)?;
            let mut w = BufWriter::new(File::create(&args[3]).context("create output")?);
            code.serialize(&mut w).context("write bytecode")?;
            Ok(())
        }
        Some("inspect") if args.len() == 4 => {
            let code = Bytecode::from_file(&args[2]).context("read bytecode")?;
            inspect(&code, &args[3])
        }
        _ => bail!("usage: wartales-tips patch <in.dat> <out.dat> | inspect <in.dat> <TypeName>"),
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
    if code.findex_max() != code.functions.len() + code.natives.len() {
        bail!("findex space is not dense; refusing to append a function");
    }
    let findex = RefFun(code.findex_max());
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
    let new_at = (0..oi)
        .rev()
        .find(|&i| matches!(f.ops[i], Opcode::New { dst } if dst == icon_reg))
        .context("New ItemIcon not found before its constructor call")?;
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
    insert_ops(
        f,
        new_at,
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
        f.findex.0, new_at, oi + 2, icon_reg.0, parent_reg.0, tip_fn.0
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
