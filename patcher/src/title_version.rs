// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The title screen shows the mod version on its own line above the game
// version ("v.1.0.48274", bottom right).
//
// TitlescreenWindow.init (src/ui/win/TitleScreen.hx:46) fills the version label,
// a ui.comp.FmtText (an h2d.HtmlText), with
//
//   s = "v." + Const.VERSION;  label.set_text(s);   // Field, Call2 __add__, CallMethod
//
// Right before that set_text the pass adds one op, `s = titleVersion(s)`:
// titleVersion(s) = LABEL + s, and logs the result to shim.log
// ("mp: title version label: ..."). LABEL is "Co-op Fix v<VERSION><br/>": the
// same label, font, colour and alignment, one line above the game version.
// <VERSION> is the repo's VERSION file, compiled in, so the label and the
// release cannot drift apart.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs, Snap};
use super::job_xp::str_global;
use super::*;
use crate::marker_names::vproto;

/// The repo's VERSION file (single source of the release version).
const VERSION_FILE: &str = include_str!("../../VERSION");
const LOG: &str = "mp: title version label: ";

/// The mod version, e.g. "0.2.5".
pub(crate) fn mod_version() -> &'static str {
    VERSION_FILE.trim()
}

/// The prefix put in front of the game version: label + line break.
fn label() -> &'static str {
    static L: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    L.get_or_init(|| format!("Co-op Fix v{}<br/>", mod_version()))
}

struct Plan {
    str_t: RefType,
    void_t: RefType,
    str_add: RefFun,
    println: RefFun,
    fi: usize,
    /// The vanilla `label.set_text(s)` CallMethod.
    at: usize,
    /// `s`, the "v." + VERSION string.
    text: Reg,
    dbg_file: usize,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    if mod_version().is_empty() || mod_version().contains(['<', '>', '&', '\n']) {
        bail!("bad VERSION {:?}", mod_version());
    }
    let str_t = obj_type(code, "String")?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let tw_t = obj_type(code, "ui.win.TitlescreenWindow")?;
    let fmt_t = obj_type(code, "ui.comp.FmtText")?;
    let const_t = obj_type(code, "$Const")?;
    let (ver_f, ver_t) = field(code, const_t, "VERSION")?;
    if ver_t != str_t {
        bail!("Const.VERSION is not a String");
    }
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t) {
        bail!("unexpected String.__add__ signature");
    }
    let (st_fn, set_text) = vproto(code, fmt_t, "set_text")?;
    if sig(code, st_fn)? != (vec![fmt_t, str_t], str_t) {
        bail!("unexpected FmtText.set_text signature");
    }
    let println = crate::diag::static_fn(code, "$Sys", "println")?;
    if fun_args(code, println) != [dyn_t] {
        bail!("Sys.println does not take one Dyn");
    }

    let init = method(code, tw_t, "init")?;
    let reg_t = |r: &Reg| init.regs[r.0 as usize];
    let sites: Vec<(usize, Reg)> = (0..init.ops.len().saturating_sub(2))
        .filter_map(|i| {
            let Opcode::Field {
                dst: v,
                obj: c,
                field,
            } = &init.ops[i]
            else {
                return None;
            };
            if *field != ver_f || reg_t(c) != const_t {
                return None;
            }
            let Opcode::Call2 {
                dst: sd, fun, arg1, ..
            } = &init.ops[i + 1]
            else {
                return None;
            };
            if *fun != str_add || arg1 != v {
                return None;
            }
            match &init.ops[i + 2] {
                Opcode::CallMethod { field, args, .. }
                    if *field == set_text
                        && args.len() == 2
                        && reg_t(&args[0]) == fmt_t
                        && args[1] == *sd =>
                {
                    Some((i + 2, *sd))
                }
                _ => None,
            }
        })
        .collect();
    let [(at, text)] = sites[..] else {
        let applied = init.ops.windows(2).any(|w| {
            matches!(&w[0], Opcode::Call2 { fun, .. } if *fun == str_add)
                && matches!(&w[1], Opcode::Call1 { arg0, .. } if reg_t(arg0) == str_t)
        });
        if applied {
            bail!("already applied");
        }
        bail!(
            "TitlescreenWindow.init: expected one \"v.\" + Const.VERSION -> set_text, found {}",
            sites.len()
        );
    };
    if (0..init.ops.len()).any(|i| jump_targets(init, i).contains(&at)) {
        bail!("a jump targets the version set_text");
    }
    let dbg_file = debug_file(code, "src/ui/win/TitleScreen.hx")?;
    Ok(Plan {
        str_t,
        void_t,
        str_add,
        println: println.findex,
        fi: fun_index(code, init.findex)?,
        at,
        text,
        dbg_file,
    })
}

/// `titleVersion(s) -> String`: `r = LABEL + s; Sys.println(LOG + r); return r`.
fn add_title_version(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let leaked: &'static str = label();
    let g_label = str_global(code, p.str_t, leaked);
    let g_log = str_global(code, p.str_t, LOG);
    let mut r = Regs(vec![p.str_t]);
    let (res, tmp, v) = (r.r(p.str_t), r.r(p.str_t), r.r(p.void_t));
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: tmp,
        global: g_label,
    });
    a.op(Opcode::Call2 {
        dst: res,
        fun: p.str_add,
        arg0: tmp,
        arg1: Reg(0),
    });
    a.op(Opcode::GetGlobal {
        dst: tmp,
        global: g_log,
    });
    a.op(Opcode::Call2 {
        dst: tmp,
        fun: p.str_add,
        arg0: tmp,
        arg1: res,
    });
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: tmp,
    });
    a.op(Opcode::Ret { ret: res });
    push_fn(code, vec![p.str_t], p.str_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<()> {
    let helper = add_title_version(code, p)?;
    let f = &mut code.functions[p.fi];
    insert_ops(
        f,
        p.at,
        vec![Opcode::Call1 {
            dst: p.text,
            fun: helper,
            arg0: p.text,
        }],
    );
    eprintln!(
        "patched title version fn@{}: \"{}\" above the game version (titleVersion fn@{}, op {})",
        f.findex.0,
        label(),
        helper.0,
        p.at
    );
    Ok(())
}

/// Puts the mod version above the game version on the title screen, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_title_version(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("title version skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    if let Err(e) = apply(code, &p) {
        eprintln!("title version skipped: {e:#}");
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

    #[test]
    fn version_file_is_clean() {
        let v = mod_version();
        assert!(!v.is_empty());
        assert!(v.split('.').all(|n| n.parse::<u32>().is_ok()), "{v:?}");
        assert_eq!(label(), format!("Co-op Fix v{v}<br/>"));
    }

    /// One op before the vanilla set_text; one well-typed helper appended;
    /// nothing else changes; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_title_version(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        let helper = &back.functions[nf];
        check_flow(helper);
        check_types(&back, helper, 0..helper.ops.len());

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == p.fi {
                shifted(a, b, p.at, 1);
                check_flow(b);
                check_types(&back, b, p.at..p.at + 1);
                assert!(matches!(&b.ops[p.at],
                    Opcode::Call1 { fun, dst, arg0 } if *fun == helper.findex
                        && *dst == p.text && *arg0 == p.text));
                assert!(matches!(&b.ops[p.at + 1], Opcode::CallMethod { args, .. }
                    if args[1] == p.text));
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
        patch_title_version(&mut again);
        assert!(write(&again) == patched);
    }

    /// The helper prepends the label and logs the whole text once.
    #[test]
    fn helper_builds_and_logs_label() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_title_version(&mut code);
        let nf = orig.functions.len();
        let helper = code.functions[nf].findex;
        let (add, println) = (p.str_add, p.println);
        let mut s = Sim::new(
            &code,
            nf,
            move |c, f, a| {
                if f == add {
                    let (V::S(x), V::S(y)) = (&a[0], &a[1]) else {
                        panic!("__add__ {a:?}")
                    };
                    Some(V::S(format!("{x}{y}")))
                } else if f == println {
                    c.log.push(("println", a.to_vec()));
                    Some(V::Null)
                } else {
                    None
                }
            },
            |_, _, _| panic!("no virtual call expected"),
        );
        let out = s.run(helper, vec![V::S("v.1.0.48274".into())]);
        let want = format!("Co-op Fix v{}<br/>v.1.0.48274", mod_version());
        assert_eq!(out, V::S(want.clone()));
        assert_eq!(
            s.c.take("println"),
            vec![vec![V::S(format!("{LOG}{want}"))]]
        );
    }

    /// No version site, two sites, or a jump onto set_text: skipped, untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let base = write(&orig);
        let edits: [fn(&mut Function, usize); 3] = [
            |f, at| f.ops[at] = Opcode::Nop,
            |f, at| f.ops[at - 2] = Opcode::Nop,
            |f, at| {
                let j = (0..at)
                    .rev()
                    .find(|&i| matches!(f.ops[i], Opcode::JAlways { .. }))
                    .unwrap();
                f.ops[j] = Opcode::JAlways {
                    offset: (at - j - 1) as i32,
                };
            },
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut code = read(&image);
            edit(&mut code.functions[p.fi], p.at);
            let before = write(&code);
            assert!(before != base, "edit {k} was a no-op");
            assert!(plan(&code).is_err(), "edit {k} still planned");
            patch_title_version(&mut code);
            assert!(write(&code) == before, "edit {k} changed the code");
        }
    }
}
