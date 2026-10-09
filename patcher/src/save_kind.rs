// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The load / save list names each save with its kind: "Name (Autosave)",
// "Name (Quicksave)", "Name (Manual)".
//
// Vanilla (src/ui/win/LoadGame.hx) names every save of a campaign the same:
// LoadGame.getSaveName(g, escaped) is "[BETA] "? + header.gameName, and
// gameName is the campaign name typed at the start, written into every save
// (auto, quick and manual alike). The kind only shows in a second, smaller
// text, LoadGame.getFileName(g): "(Autosave)" / "(Quicksave)" from the file
// name (autosave.dat / quicksave.dat), nothing for a manual saveNNN.dat, and
// the raw file name for admin / beta builds. Not a mod bug: the mod never
// touches gameName or the save file names.
//
// Two edits, display only (nothing is written to a save, so a suffix never
// stacks across saves):
//   1. getSaveName: before its `Ret`, `name = saveKindName(name, g)`. The
//      appended saveKindName(name, g) = name + " (" + label + ")", label from
//      LoadGame.getSaveFile(g) (file, else localFile): "autosave.dat" ->
//      Texts.ui.autosave, "quicksave.dat" -> Texts.ui.quicksave (the game's own
//      localized words, read the way getFileName reads them), else "Manual"
//      (the game has no localized word for it). A null file adds nothing.
//      getSaveName feeds the list row title, the detail panel title and the
//      save-over / delete confirmations.
//   2. getFileName: its non-admin branch (the jump past the admin block) goes
//      straight to its `return ""`, so the row does not show the kind twice.
//      Admin / beta builds keep the raw file name there.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs, Snap};
use super::job_xp::str_global;
use super::*;

const MANUAL: &str = "Manual";

/// `Texts.DATA["ui"].<label>` as getFileName reads it.
#[derive(Clone, Copy, PartialEq, Debug)]
struct TextsRead {
    texts_g: RefGlobal,
    texts_t: RefType,
    data_f: RefField,
    data_t: RefType,
    ui_s: RefString,
    dyn_t: RefType,
    ui_t: RefType,
    label_f: RefField,
}

/// `idx = f.indexOf("<name>.dat", null); if idx == -1 ...` as getFileName tests it.
#[derive(Clone, Copy, PartialEq, Debug)]
struct IndexOf {
    name_g: RefGlobal,
    index_of: RefFun,
    null_t: RefType,
    i32_t: RefType,
    minus_one: hlbc::types::RefInt,
}

struct Plan {
    str_t: RefType,
    g_t: RefType,
    str_add: RefFun,
    get_save_file: RefFun,
    auto: (IndexOf, TextsRead),
    quick: (IndexOf, TextsRead),
    /// getSaveName: its function index and final `Ret` (op index, register).
    gs_fi: usize,
    gs_ret: usize,
    gs_name: Reg,
    /// getFileName: its function index, the non-admin jump and its `return ""`.
    gf_fi: usize,
    gf_jump: usize,
    gf_empty: usize,
    dbg_file: usize,
}

fn texts_read(code: &Bytecode, f: &Function, label: &str) -> Result<TextsRead> {
    let rt = |r: &Reg| f.regs[r.0 as usize];
    let hits: Vec<usize> = (0..f.ops.len())
        .filter(|&i| {
            matches!(&f.ops[i], Opcode::Field { obj, field, .. }
                if field_name(code, rt(obj), *field) == Some(label))
        })
        .collect();
    let [i] = hits[..] else {
        bail!("getFileName: expected one read of ui.{label}, found {}", hits.len());
    };
    if i < 6 {
        bail!("getFileName: ui.{label} read too early");
    }
    let o = &f.ops[i - 6..=i];
    let (
        Opcode::GetGlobal { dst: tx, global },
        Opcode::Field { dst: d, obj: tx2, field: data_f },
        Opcode::NullCheck { reg: d2 },
        Opcode::DynGet { dst: u, obj: d3, field: ui_s },
        Opcode::ToVirtual { dst: v, src: u2 },
        Opcode::NullCheck { reg: v2 },
        Opcode::Field { dst: l, obj: v3, field: label_f },
    ) = (&o[0], &o[1], &o[2], &o[3], &o[4], &o[5], &o[6])
    else {
        bail!("getFileName: unexpected Texts.DATA[\"ui\"].{label} shape");
    };
    if tx != tx2 || d != d2 || d != d3 || u != u2 || v != v2 || v != v3 {
        bail!("getFileName: Texts.DATA[\"ui\"].{label} registers do not chain");
    }
    if s(code, *ui_s) != "ui" || field_name(code, rt(tx), *data_f) != Some("DATA") {
        bail!("getFileName: not Texts.DATA[\"ui\"]");
    }
    if rt(l) != obj_type(code, "String")? {
        bail!("getFileName: ui.{label} is not a String");
    }
    Ok(TextsRead {
        texts_g: *global,
        texts_t: rt(tx),
        data_f: *data_f,
        data_t: rt(d),
        ui_s: *ui_s,
        dyn_t: rt(u),
        ui_t: rt(v),
        label_f: *label_f,
    })
}

fn index_of(code: &Bytecode, f: &Function, file: Reg, name: &str) -> Result<IndexOf> {
    let str_t = obj_type(code, "String")?;
    let name_g = existing_str(code, str_t, name)?;
    let rt = |r: &Reg| f.regs[r.0 as usize];
    let hits: Vec<usize> = (0..f.ops.len().saturating_sub(4))
        .filter(|&i| matches!(&f.ops[i], Opcode::GetGlobal { global, .. } if *global == name_g))
        .collect();
    let [i] = hits[..] else {
        bail!("getFileName: expected one {name:?}, found {}", hits.len());
    };
    let (
        Opcode::GetGlobal { dst: t, .. },
        Opcode::Null { dst: n },
        Opcode::Call3 {
            dst: idx,
            fun,
            arg0,
            arg1,
            arg2,
        },
        Opcode::Int { dst: m, ptr },
        Opcode::JEq { a, b, .. },
    ) = (&f.ops[i], &f.ops[i + 1], &f.ops[i + 2], &f.ops[i + 3], &f.ops[i + 4])
    else {
        bail!("getFileName: unexpected indexOf({name:?}) shape");
    };
    if *arg0 != file || arg1 != t || arg2 != n || a != idx || b != m {
        bail!("getFileName: indexOf({name:?}) registers do not chain");
    }
    if code.ints.get(ptr.0) != Some(&-1) {
        bail!("getFileName: indexOf({name:?}) not compared with -1");
    }
    if sig(code, *fun)? != (vec![str_t, str_t, rt(n)], rt(idx)) || fname(code, *fun) != "indexOf" {
        bail!("getFileName: unexpected indexOf signature");
    }
    Ok(IndexOf {
        name_g,
        index_of: *fun,
        null_t: rt(n),
        i32_t: rt(idx),
        minus_one: *ptr,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let str_t = obj_type(code, "String")?;
    let lg_t = obj_type(code, "ui.win.LoadGame")?;
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t) {
        bail!("unexpected String.__add__ signature");
    }

    // getSaveName(g: {file, localFile, header, ...}, escaped: ref<bool>) -> String
    let gss: Vec<&Function> = code
        .functions
        .iter()
        .filter(|f| {
            s(code, f.name) == "getSaveName"
                && f.t.as_fun(code).is_some_and(|t| {
                    t.args.len() == 2
                        && t.ret == str_t
                        && matches!(code.types[t.args[0].0], Type::Virtual { .. })
                })
        })
        .collect();
    let [gs] = gss[..] else {
        bail!("expected one LoadGame.getSaveName, found {}", gss.len());
    };
    let g_t = fun_args(code, gs)[0];
    let n = gs.ops.len();
    let (Some(Opcode::Call2 { dst, fun, .. }), Some(Opcode::Ret { ret })) =
        (gs.ops.get(n.wrapping_sub(2)), gs.ops.last())
    else {
        bail!("getSaveName: does not end in a call + Ret");
    };
    if fname(code, *fun) != "formatSaveName" {
        bail!("getSaveName: does not end in formatSaveName (already applied?)");
    }
    if dst != ret {
        bail!("getSaveName: Ret is not the formatSaveName result");
    }
    let gs_name = *ret;

    // getFileName(this, g) -> String
    let gf = method(code, lg_t, "getFileName")?;
    if fun_args(code, gf) != [lg_t, g_t] {
        bail!("getFileName: unexpected signature");
    }
    let calls: Vec<(usize, Reg, RefFun)> = gf
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call1 { dst, fun, arg0 }
                if fname(code, *fun) == "getSaveFile" && *arg0 == Reg(1) =>
            {
                Some((i, *dst, *fun))
            }
            _ => None,
        })
        .collect();
    let [(k, file, get_save_file)] = calls[..] else {
        bail!("getFileName: expected one getSaveFile(g), found {}", calls.len());
    };
    if sig(code, get_save_file)? != (vec![g_t], str_t) {
        bail!("getSaveFile: unexpected signature");
    }
    let jumps: Vec<usize> = (0..gf.ops.len())
        .filter(|&i| jump_targets(gf, i).contains(&k))
        .collect();
    let [gf_jump] = jumps[..] else {
        bail!("getFileName: expected one jump to the non-admin branch, found {}", jumps.len());
    };
    if !matches!(gf.ops[gf_jump], Opcode::JFalse { .. }) {
        bail!("getFileName: the non-admin branch is not a JFalse target");
    }
    let m = gf.ops.len();
    let gf_empty = m - 2;
    match (&gf.ops[gf_empty], &gf.ops[m - 1]) {
        (Opcode::GetGlobal { dst, global }, Opcode::Ret { ret })
            if dst == ret && crate::job_xp::const_str(code, *global) == Some("") => {}
        _ => bail!("getFileName: does not end in `return \"\"`"),
    }
    let auto = (
        index_of(code, gf, file, "autosave.dat")?,
        texts_read(code, gf, "autosave")?,
    );
    let quick = (
        index_of(code, gf, file, "quicksave.dat")?,
        texts_read(code, gf, "quicksave")?,
    );
    if auto.0.index_of != quick.0.index_of || auto.1.ui_t != quick.1.ui_t {
        bail!("getFileName: autosave and quicksave tests differ");
    }
    Ok(Plan {
        str_t,
        g_t,
        str_add,
        get_save_file,
        auto,
        quick,
        gs_fi: fun_index(code, gs.findex)?,
        gs_ret: n - 1,
        gs_name,
        gf_fi: fun_index(code, gf.findex)?,
        gf_jump,
        gf_empty,
        dbg_file: debug_file(code, "src/ui/win/LoadGame.hx")?,
    })
}

/// `saveKindName(name, g) -> String`: name + " (" + kind label + ")".
fn add_save_kind_name(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let g_manual = str_global(code, p.str_t, MANUAL);
    let g_open = str_global(code, p.str_t, " (");
    let g_close = str_global(code, p.str_t, ")");
    let (io, tr) = p.auto;
    let mut r = Regs(vec![p.str_t, p.g_t]);
    let (name, g) = (Reg(0), Reg(1));
    let file = r.r(p.str_t);
    let tmp = r.r(p.str_t);
    let null = r.r(io.null_t);
    let idx = r.r(io.i32_t);
    let m1 = r.r(io.i32_t);
    let tx = r.r(tr.texts_t);
    let data = r.r(tr.data_t);
    let ui = r.r(tr.dyn_t);
    let v = r.r(tr.ui_t);
    let label = r.r(p.str_t);

    let mut a = Asm::new();
    a.op(Opcode::Call1 {
        dst: file,
        fun: p.get_save_file,
        arg0: g,
    });
    a.jmp(Opcode::JNull { reg: file, offset: 0 }, "ret");
    a.op(Opcode::Int {
        dst: m1,
        ptr: io.minus_one,
    });
    for ((io, _), to) in [(p.auto, "auto"), (p.quick, "quick")] {
        a.op(Opcode::GetGlobal {
            dst: tmp,
            global: io.name_g,
        });
        a.op(Opcode::Null { dst: null });
        a.op(Opcode::Call3 {
            dst: idx,
            fun: io.index_of,
            arg0: file,
            arg1: tmp,
            arg2: null,
        });
        a.jmp(
            Opcode::JNotEq {
                a: idx,
                b: m1,
                offset: 0,
            },
            to,
        );
    }
    a.op(Opcode::GetGlobal {
        dst: label,
        global: g_manual,
    });
    a.jmp(Opcode::JAlways { offset: 0 }, "join");
    for ((_, tr), at) in [(p.auto, "auto"), (p.quick, "quick")] {
        a.label(at);
        a.op(Opcode::GetGlobal {
            dst: tx,
            global: tr.texts_g,
        });
        a.op(Opcode::Field {
            dst: data,
            obj: tx,
            field: tr.data_f,
        });
        a.op(Opcode::NullCheck { reg: data });
        a.op(Opcode::DynGet {
            dst: ui,
            obj: data,
            field: tr.ui_s,
        });
        a.op(Opcode::ToVirtual { dst: v, src: ui });
        a.op(Opcode::NullCheck { reg: v });
        a.op(Opcode::Field {
            dst: label,
            obj: v,
            field: tr.label_f,
        });
        a.jmp(Opcode::JAlways { offset: 0 }, "join");
    }
    a.label("join");
    a.jmp(Opcode::JNull { reg: label, offset: 0 }, "ret");
    a.op(Opcode::GetGlobal {
        dst: tmp,
        global: g_open,
    });
    a.op(Opcode::Call2 {
        dst: name,
        fun: p.str_add,
        arg0: name,
        arg1: tmp,
    });
    a.op(Opcode::Call2 {
        dst: name,
        fun: p.str_add,
        arg0: name,
        arg1: label,
    });
    a.op(Opcode::GetGlobal {
        dst: tmp,
        global: g_close,
    });
    a.op(Opcode::Call2 {
        dst: name,
        fun: p.str_add,
        arg0: name,
        arg1: tmp,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: name });
    push_fn(code, vec![p.str_t, p.g_t], p.str_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let helper = add_save_kind_name(code, p)?;
    let gs = &mut code.functions[p.gs_fi];
    insert_ops(
        gs,
        p.gs_ret,
        vec![Opcode::Call2 {
            dst: p.gs_name,
            fun: helper,
            arg0: p.gs_name,
            arg1: Reg(0),
        }],
    );
    let gs_findex = gs.findex.0;
    let gf = &mut code.functions[p.gf_fi];
    let Opcode::JFalse { offset, .. } = &mut gf.ops[p.gf_jump] else {
        bail!("getFileName: jump moved");
    };
    *offset = (p.gf_empty as i64 - p.gf_jump as i64 - 1) as i32;
    eprintln!(
        "patched save kind: getSaveName fn@{gs_findex} + saveKindName fn@{}, getFileName fn@{} non-admin -> \"\"",
        helper.0, gf.findex.0
    );
    Ok(())
}

/// Appends the save kind to save names in the load / save list, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_save_kind(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("save kind skipped: {e:#}"));
            return;
        }
    };
    let snap = Snap::take(code);
    if let Err(e) = apply(code, &p) {
        crate::skipped(format!("save kind skipped: {e:#}"));
        snap.restore(code);
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::*;
    use super::*;
    use crate::testsim::{Sim, V};

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// One op in getSaveName, one jump retargeted in getFileName, one
    /// well-typed helper appended; nothing else changes; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_save_kind(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        let helper = &back.functions[nf];
        check_flow(helper);
        check_types(&back, helper, 0..helper.ops.len());

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == p.gs_fi {
                shifted(a, b, p.gs_ret, 1);
                check_flow(b);
                check_types(&back, b, p.gs_ret..p.gs_ret + 1);
                assert!(matches!(&b.ops[p.gs_ret],
                    Opcode::Call2 { fun, .. } if *fun == helper.findex));
            } else if i == p.gf_fi {
                check_flow(b);
                for k in 0..a.ops.len() {
                    if k == p.gf_jump {
                        assert_eq!(jump_targets(b, k), vec![p.gf_empty]);
                    } else {
                        assert_eq!(ops(&a.ops[k..=k]), ops(&b.ops[k..=k]), "op {k}");
                    }
                }
            } else {
                assert!(
                    ops(&a.ops) == ops(&b.ops) && a.regs == b.regs,
                    "function #{i} (fn@{}) changed",
                    a.findex.0
                );
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_save_kind(&mut again);
        assert!(write(&again) == patched);
    }

    /// The whole patched getSaveName: campaign name + localized kind, per file;
    /// getFileName then shows nothing for a player (non-admin, non-beta).
    #[test]
    fn names_each_kind() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_save_kind(&mut code);
        let nf = orig.functions.len();
        let gs = code.functions[p.gs_fi].findex;
        let gf = code.functions[p.gf_fi].findex;
        let str_add = p.str_add;
        let gsf = p.get_save_file;
        let index_of = p.auto.0.index_of;
        let vfield = |t: RefType, name: &str| -> RefField {
            let Type::Virtual { fields } = &code.types[t.0] else {
                panic!("not virtual")
            };
            RefField(
                fields
                    .iter()
                    .position(|f| s(&code, f.name) == name)
                    .unwrap(),
            )
        };
        let (f_file, f_header) = (vfield(p.g_t, "file"), vfield(p.g_t, "header"));
        let Type::Virtual { fields } = &code.types[p.g_t.0] else {
            unreachable!()
        };
        let h_t = fields[f_header.0].t;
        let (f_tags, f_name) = (vfield(h_t, "tags"), vfield(h_t, "gameName"));
        let tr = p.auto.1;
        let by_name = |n: &str| -> Vec<RefFun> {
            code.functions
                .iter()
                .filter(|f| s(&code, f.name) == n)
                .map(|f| f.findex)
                .collect()
        };
        let fmt = by_name("formatSaveName");
        let flags: Vec<RefFun> = [by_name("get_isBeta"), by_name("get_isAdmin")].concat();

        let cases = [
            (Some("save/autosave.dat"), "Camp (Autosave)"),
            (Some("save/quicksave.dat"), "Camp (Quicksave)"),
            (Some("save/save003.dat"), "Camp (Manual)"),
            (None, "Camp"),
        ];
        for (file, want) in cases {
            let (fmt, flags) = (fmt.clone(), flags.clone());
            let mut sim = Sim::new(
                &code,
                nf,
                move |c, f, a| {
                    if f == str_add {
                        let (V::S(x), V::S(y)) = (&a[0], &a[1]) else {
                            panic!("__add__ {a:?}")
                        };
                        Some(V::S(format!("{x}{y}")))
                    } else if f == gsf {
                        Some(c.get(&a[0], f_file))
                    } else if f == index_of {
                        let (V::S(x), V::S(y)) = (&a[0], &a[1]) else {
                            panic!("indexOf {a:?}")
                        };
                        Some(V::I(x.find(y.as_str()).map_or(-1, |i| i as i32)))
                    } else if fmt.contains(&f) {
                        Some(a[0].clone())
                    } else if flags.contains(&f) {
                        Some(V::B(false))
                    } else {
                        None
                    }
                },
                |_, _, _| panic!("no virtual call expected"),
            );
            let ui = sim.c.obj(&[(tr.label_f, V::S("Autosave".into()))]);
            let qf = p.quick.1.label_f;
            sim.c.set(&ui, qf, V::S("Quicksave".into()));
            let data = sim.c.obj(&[]);
            sim.c.key_set(&data, "dui".into(), ui);
            let texts = sim.c.obj(&[(tr.data_f, data)]);
            sim.c.globals.insert(tr.texts_g.0, texts);
            let header = sim
                .c
                .obj(&[(f_tags, V::Null), (f_name, V::S("Camp".into()))]);
            let g = sim.c.obj(&[
                (f_file, file.map_or(V::Null, |x| V::S(x.into()))),
                (f_header, header),
            ]);
            let out = sim.run(gs, vec![g.clone(), V::Null]);
            assert_eq!(out, V::S(want.into()), "{file:?}");
            if file.is_some() {
                let out = sim.run(gf, vec![V::Null, g]);
                assert_eq!(out, V::S(String::new()), "getFileName {file:?}");
            }
        }
    }

    /// Unexpected shapes: skipped, untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let base = write(&orig);
        let edits: [fn(&mut Bytecode, &Plan); 3] = [
            |c, p| c.functions[p.gs_fi].ops[p.gs_ret - 1] = Opcode::Nop,
            |c, p| c.functions[p.gf_fi].ops[p.gf_empty] = Opcode::Nop,
            |c, p| c.functions[p.gf_fi].ops[p.gf_jump] = Opcode::Nop,
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code, &p);
            let before = write(&code);
            assert!(before != base, "edit {k} was a no-op");
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_save_kind(&mut code);
            assert!(write(&code) == before, "edit {k} changed the code");
        }
    }
}
