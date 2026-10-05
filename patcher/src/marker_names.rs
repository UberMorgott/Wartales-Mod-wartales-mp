// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op player markers show the player's nickname instead of "P1" / "И1".
//
// The world-map screen-edge locator (`ui.comp.PlayerMarker`, one per co-op
// player, built by GameUI.update) and the minimap arrow (`ui.comp.PlayerArrow`,
// MiniMap.initPlayerMarkers, and the child arrow inside each PlayerMarker) set
// their `playerText` label in the constructor from the vanilla lang array
// `Texts.multiplayer.playersArrow[player.getColor() - 1]`:
//
//   playerText.set_text(playersArrow[getColor() - 1]);   // CallMethod set_text
//
// Right after that call (it is a jump target, the next op is not), each
// constructor gets
//
//   s = markerName(player); if (s != null) playerText.set_text(s);
//
// markerName(bp): bp == null -> null; n = bp.getUserName() (the plain,
// profanity-filtered nickname, the text getName() wraps in a colour font tag;
// h2d.Text is not HTML, so the tagged form would print literally); null or
// empty -> null (the vanilla "P1" stays); longer than MAX chars -> its first
// MAX - 1 chars + "...". The colour comes from the vanilla `player-<color>` dom
// class both constructors already add to playerText.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs, Snap};
use super::job_xp::str_global;
use super::*;

/// Longest nickname shown whole; a longer one keeps `MAX - 1` chars + "...".
pub(crate) const MAX: i32 = 12;
const CUT: &str = "...";

struct Site {
    fi: usize,
    /// The vanilla `playerText.set_text(...)` CallMethod.
    at: usize,
    text: Reg,
    name: &'static str,
}

struct Plan {
    bp_t: RefType,
    str_t: RefType,
    i32_t: RefType,
    ni32_t: RefType,
    get_user_name: RefFun,
    str_add: RefFun,
    str_substr: RefFun,
    str_len: RefField,
    set_text: RefField,
    sites: Vec<Site>,
    dbg_file: usize,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn sig(code: &Bytecode, f: RefFun) -> Result<(Vec<RefType>, RefType)> {
    let fun = &code.functions[fun_index(code, f)?];
    let t = fun.t.as_fun(code).context("not a function type")?;
    Ok((t.args.clone(), t.ret))
}

/// Proto `name` of class `t` or its nearest ancestor: (function, vtable index).
fn vproto(code: &Bytecode, t: RefType, name: &str) -> Result<(RefFun, RefField)> {
    let mut cur = Some(t);
    while let Some(c) = cur {
        let o = obj(code, c)?;
        if let Some(p) = o.protos.iter().find(|p| s(code, p.name) == name) {
            let i = usize::try_from(p.pindex).context("negative proto index")?;
            return Ok((p.findex, RefField(i)));
        }
        cur = o.super_;
    }
    bail!("proto {name} not found on type {} or its ancestors", t.0)
}

/// Reads virtual field `name` somewhere in `f`.
fn reads_virtual_field(code: &Bytecode, f: &Function, name: &str) -> bool {
    f.ops.iter().any(|op| match op {
        Opcode::Field { obj, field, .. } => match &code.types[f.regs[obj.0 as usize].0] {
            Type::Virtual { fields } => fields
                .get(field.0)
                .is_some_and(|x| s(code, x.name) == name),
            _ => false,
        },
        _ => false,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let str_t = obj_type(code, "String")?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let text_t = obj_type(code, "h2d.Text")?;
    let obj_t = obj_type(code, "h2d.Object")?;

    let gun = method(code, bp_t, "getUserName")?.findex;
    if sig(code, gun)? != (vec![bp_t], str_t) {
        bail!("unexpected BasePlayer.getUserName signature");
    }
    let str_add = crate::diag::static_fn(code, "$String", "__add__")?.findex;
    if sig(code, str_add)? != (vec![str_t, str_t], str_t) {
        bail!("unexpected String.__add__ signature");
    }
    let str_substr = proto(code, str_t, "substr")?;
    let (sa, sr) = sig(code, str_substr)?;
    let ni32_t = *sa.get(2).context("String.substr arity")?;
    if sa != [str_t, i32_t, ni32_t]
        || sr != str_t
        || !matches!(code.types[ni32_t.0], Type::Null(t) if t == i32_t)
    {
        bail!("unexpected String.substr signature");
    }
    let (sl, slt) = field(code, str_t, "length")?;
    if slt != i32_t {
        bail!("String.length is not an i32");
    }
    let (st_fn, set_text) = vproto(code, text_t, "set_text")?;
    if sig(code, st_fn)? != (vec![text_t, str_t], str_t) {
        bail!("unexpected h2d.Text.set_text signature");
    }

    let mut sites = vec![];
    for (cls, name) in [
        ("ui.comp.PlayerArrow", "PlayerArrow"),
        ("ui.comp.PlayerMarker", "PlayerMarker"),
    ] {
        let cls_t = obj_type(code, cls)?;
        let ctor = method(code, cls_t, "__constructor__")?;
        if fun_args(code, ctor) != [cls_t, bp_t, obj_t] {
            bail!("{name}: unexpected constructor signature");
        }
        if !reads_virtual_field(code, ctor, "playersArrow") {
            bail!("{name}: constructor does not read Texts.multiplayer.playersArrow");
        }
        let calls: Vec<usize> = (0..ctor.ops.len())
            .filter(|&i| {
                matches!(&ctor.ops[i], Opcode::CallMethod { field, args, .. }
                    if *field == set_text && args.len() == 2
                        && ctor.regs[args[0].0 as usize] == text_t)
            })
            .collect();
        let [at] = calls[..] else {
            bail!(
                "{name}: expected one playerText.set_text call, found {}",
                calls.len()
            );
        };
        let Opcode::CallMethod { args, .. } = &ctor.ops[at] else {
            unreachable!()
        };
        let text = args[0];
        let next = at + 1;
        if next >= ctor.ops.len() {
            bail!("{name}: set_text is the last op");
        }
        if (0..ctor.ops.len()).any(|i| jump_targets(ctor, i).contains(&next)) {
            bail!("{name}: a jump targets the op after set_text");
        }
        match &ctor.ops[next] {
            Opcode::Call1 { arg0: Reg(1), .. } => bail!("{name}: already applied"),
            Opcode::GetGlobal { .. } => {}
            o => bail!("{name}: unexpected op after set_text: {o:?}"),
        }
        sites.push(Site {
            fi: fun_index(code, ctor.findex)?,
            at,
            text,
            name,
        });
    }
    let dbg_file = debug_file(code, "src/ui/comp/PlayerMarker.hx")?;
    Ok(Plan {
        bp_t,
        str_t,
        i32_t,
        ni32_t,
        get_user_name: gun,
        str_add,
        str_substr,
        str_len: sl,
        set_text,
        sites,
        dbg_file,
    })
}

/// `markerName(bp) -> String` (see the module comment).
fn add_marker_name(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let c_max = int_const(code, MAX);
    let c_cut = int_const(code, MAX - 1);
    let c0 = int_const(code, 0);
    let s_cut = str_global(code, p.str_t, CUT);
    let mut r = Regs(vec![p.bp_t]);
    let (n, len, k, zero, ni, tmp) = (
        r.r(p.str_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.ni32_t),
        r.r(p.str_t),
    );
    let mut a = Asm::new();
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "none",
    );
    a.op(Opcode::Call1 {
        dst: n,
        fun: p.get_user_name,
        arg0: Reg(0),
    });
    a.jmp(Opcode::JNull { reg: n, offset: 0 }, "none");
    a.op(Opcode::Field {
        dst: len,
        obj: n,
        field: p.str_len,
    });
    a.op(Opcode::Int { dst: zero, ptr: c0 });
    a.jmp(
        Opcode::JSLte {
            a: len,
            b: zero,
            offset: 0,
        },
        "none",
    );
    a.op(Opcode::Int { dst: k, ptr: c_max });
    a.jmp(
        Opcode::JSGte {
            a: k,
            b: len,
            offset: 0,
        },
        "ret",
    );
    a.op(Opcode::Int { dst: k, ptr: c_cut });
    a.op(Opcode::ToDyn { dst: ni, src: k });
    a.op(Opcode::Call3 {
        dst: tmp,
        fun: p.str_substr,
        arg0: n,
        arg1: zero,
        arg2: ni,
    });
    a.op(Opcode::GetGlobal {
        dst: n,
        global: s_cut,
    });
    a.op(Opcode::Call2 {
        dst: n,
        fun: p.str_add,
        arg0: tmp,
        arg1: n,
    });
    a.label("ret");
    a.op(Opcode::Ret { ret: n });
    a.label("none");
    a.op(Opcode::Null { dst: n });
    a.op(Opcode::Ret { ret: n });
    push_fn(code, vec![p.bp_t], p.str_t, r.0, a.finish(), p.dbg_file)
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let helper = add_marker_name(code, &p)?;
    for site in &p.sites {
        let f = &mut code.functions[site.fi];
        f.regs.push(p.str_t);
        let sr = Reg((f.regs.len() - 1) as u32);
        insert_ops(
            f,
            site.at + 1,
            vec![
                Opcode::Call1 {
                    dst: sr,
                    fun: helper,
                    arg0: Reg(1),
                },
                Opcode::JNull { reg: sr, offset: 1 }, // over the second set_text
                Opcode::CallMethod {
                    dst: sr,
                    field: p.set_text,
                    args: vec![site.text, sr],
                },
            ],
        );
        eprintln!(
            "patched marker names fn@{}: {} label = markerName fn@{} (max {MAX}) after set_text at op {}",
            f.findex.0, site.name, helper.0, site.at
        );
    }
    Ok(())
}

/// Shows nicknames on the co-op player markers, or leaves `code` untouched and logs why.
pub(crate) fn patch_marker_names(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("marker names skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    if let Err(e) = apply(code, p) {
        eprintln!("marker names skipped: {e:#}");
        snap.restore(code);
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::*;
    use super::*;

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// Both constructors get the 3-op relabel right after the vanilla set_text;
    /// one well-typed helper is appended; nothing else changes; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(p.sites.len(), 2);
        let mut code = read(&image);
        patch_marker_names(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        let nf = orig.functions.len();
        assert_eq!(back.functions.len(), nf + 1);
        assert_eq!(back.types[..orig.types.len()], orig.types[..]);
        let helper = &back.functions[nf];
        check_flow(helper);
        check_types(&back, helper, 0..helper.ops.len());
        // getUserName (plain), never getName (HTML).
        let gn = method(&back, p.bp_t, "getName").unwrap().findex;
        assert!(helper
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == p.get_user_name)));
        assert!(!helper
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == gn)));
        assert!(helper
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call3 { fun, .. } if *fun == p.str_substr)));

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            match p.sites.iter().find(|s| s.fi == i) {
                Some(site) => {
                    shifted(a, b, site.at + 1, 3);
                    check_flow(b);
                    check_types(&back, b, site.at + 1..site.at + 4);
                    assert!(matches!(&b.ops[site.at + 1],
                        Opcode::Call1 { fun, arg0: Reg(1), .. } if *fun == helper.findex));
                    assert_eq!(jump_targets(b, site.at + 2), vec![site.at + 4]);
                    let (Opcode::CallMethod { field: f0, args: a0, .. },
                         Opcode::CallMethod { field: f1, args: a1, .. }) =
                        (&b.ops[site.at], &b.ops[site.at + 3])
                    else {
                        panic!("{}: no set_text pair", site.name)
                    };
                    assert_eq!(f0, f1);
                    assert_eq!(a0[0], a1[0]);
                    assert_eq!(b.regs[a1[1].0 as usize], p.str_t);
                }
                None => assert!(
                    ops(&a.ops) == ops(&b.ops) && a.regs == b.regs,
                    "function #{i} (fn@{}) changed",
                    a.findex.0
                ),
            }
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_marker_names(&mut again);
        assert!(write(&again) == patched);
    }

    /// A jump onto the insertion point, a second set_text, or no set_text: skipped, untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let base = write(&orig);
        for site in 0..2 {
            let edits: [fn(&mut Function, usize); 3] = [
                // op after set_text becomes a jump target
                |f, at| {
                    let j = (0..at)
                        .rev()
                        .find(|&i| matches!(f.ops[i], Opcode::JAlways { .. }))
                        .unwrap();
                    f.ops[j] = Opcode::JAlways {
                        offset: (at + 1 - j - 1) as i32,
                    };
                },
                |f, at| f.ops[at] = Opcode::Nop,
                |f, at| {
                    let c = f.ops[at].clone();
                    f.ops[at + 1] = c;
                },
            ];
            for (k, edit) in edits.iter().enumerate() {
                let mut code = read(&image);
                let s = &p.sites[site];
                edit(&mut code.functions[s.fi], s.at);
                let before = write(&code);
                assert!(before != base, "site {site} edit {k} was a no-op");
                assert!(plan(&code).is_err(), "site {site} edit {k} still planned");
                patch_marker_names(&mut code);
                assert!(write(&code) == before, "site {site} edit {k} changed the code");
            }
        }
    }
}
