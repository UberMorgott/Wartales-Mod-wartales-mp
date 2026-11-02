// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The load / save window opens fast, however many saves there are.
//
// Vanilla (src/ui/win/LoadGame.hx) LoadGame.loadGames builds the list with its
// processFiles closure: for every *.dat it reads the whole file and calls
// GameState.readSave on it, which inflates the full game state (a few MB to
// tens of MB per save) and SHA1s it (pure Haxe) only to get `corrupted`. The
// list keeps the header and that flag; the data is thrown away. So opening
// the window costs a full decode of every save.
//
// Two edits:
//   1. readSaveHeader: a copy of readSave without the inflate and the
//      signature check (corrupted stays false; header.compressed is still set
//      to false as vanilla does). processFiles' list read calls it; its catch
//      path (crash report) keeps the full readSave.
//   2. LoadGame.loadGame (every load / convert / co-op load click) first runs
//      checkSave(current): the full vanilla readSave of the chosen save, the
//      file Game.start loads (file, then app-data localFile if that one
//      fails), into current.corrupted; failing both counts as corrupted. So the vanilla
//      "save file corrupted" refusal still fires, for the one save loaded.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::{push_fn, Asm, Regs, Snap};
use super::*;

struct Plan {
    /// readSave: its function index, the inflate ops and the signature JNull.
    rs_fi: usize,
    inflate: [usize; 3],
    sign_jnull: usize,
    ret_new: usize,
    /// processFiles: its function index and the list read call.
    pf_fi: usize,
    pf_call: usize,
    /// What checkSave reuses from processFiles / loadGames.
    read: RefFun,
    loc_g: RefGlobal,
    add: RefFun,
    app_data: RefFun,
    header_s: hlbc::types::RefString,
    data_s: hlbc::types::RefString,
    dyn_t: RefType,
    arg_t: RefType,
    res_t: RefType,
    res_corrupt: RefField,
    info_t: RefType,
    info_file: RefField,
    info_local: RefField,
    info_corrupt: RefField,
    str_t: RefType,
    loc_t: RefType,
    bytes_t: RefType,
    bool_t: RefType,
    exc_t: RefType,
    /// loadGame: its function index, where the check goes, `current` and a void reg.
    lg_fi: usize,
    lg_at: usize,
    lg_cur: Reg,
    lg_void: Reg,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let rs = crate::diag::static_fn(code, "st.$GameState", "readSave")?;
    let rs_find = rs.findex;
    let o = &rs.ops;
    // `Null n; Call2 d = Uncompress.run(data, n); Mov data = d`, the only inflate.
    let runs: Vec<usize> = (1..o.len().saturating_sub(1))
        .filter(|&i| match (&o[i - 1], &o[i], &o[i + 1]) {
            (
                Opcode::Null { dst: n },
                Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                },
                Opcode::Mov { dst: m, src },
            ) => fname(code, *fun) == "run" && arg1 == n && src == dst && m == arg0,
            _ => false,
        })
        .collect();
    let [run] = runs[..] else {
        bail!("readSave: expected one inflate, found {}", runs.len());
    };
    if o.len() < 8 {
        bail!("readSave: too short");
    }
    let Opcode::Field { dst: sign, .. } = o[3] else {
        bail!("readSave: no sign read at op 3");
    };
    let ret_new = o.len() - 5;
    if !matches!(o[ret_new], Opcode::New { .. }) || !matches!(o[o.len() - 1], Opcode::Ret { .. }) {
        bail!("readSave: unexpected tail");
    }
    let signs: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::JNull { reg, .. } if reg == sign))
        .collect();
    let [sign_jnull] = signs[..] else {
        bail!("readSave: expected one sign test, found {}", signs.len());
    };
    if jump_targets(rs, sign_jnull) != [ret_new] || sign_jnull < run {
        bail!("readSave: sign test does not skip to the result");
    }
    if !matches!(o[sign_jnull + 1], Opcode::Call1 { fun, .. } if fname(code, fun) == "makeSignature")
    {
        bail!("readSave: no makeSignature after the sign test");
    }

    // processFiles: the one closure of loadGames that calls readSave.
    let lgs = crate::diag::static_fn(code, "ui.win.$LoadGame", "loadGames")?;
    let app: Vec<RefFun> = lgs
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call0 { fun, .. } if fname(code, *fun) == "getAppDataPath" => Some(*fun),
            _ => None,
        })
        .collect();
    let [app_data] = app[..] else {
        bail!(
            "loadGames: expected one getAppDataPath, found {}",
            app.len()
        );
    };
    let pfs: Vec<usize> = lgs
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::StaticClosure { fun, .. } | Opcode::InstanceClosure { fun, .. } => {
                fun_index(code, *fun).ok()
            }
            _ => None,
        })
        .filter(|&fi| {
            code.functions[fi]
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == rs_find))
        })
        .collect();
    let [pf_fi] = pfs[..] else {
        bail!(
            "loadGames: expected one readSave closure, found {}",
            pfs.len()
        );
    };
    let pf = &code.functions[pf_fi];
    let rt = |r: Reg| pf.regs[r.0 as usize];
    let calls: Vec<usize> = (0..pf.ops.len())
        .filter(|&i| matches!(pf.ops[i], Opcode::Call1 { fun, .. } if fun == rs_find))
        .collect();
    let [pf_call, _catch] = calls[..] else {
        bail!(
            "processFiles: expected two readSave calls, found {}",
            calls.len()
        );
    };
    if pf_call < 5 {
        bail!("processFiles: readSave call too early");
    }
    // `New o; Null n; DynSet o.header = n; DynSet o.data = bytes; ToVirtual a = o; Call1 r = readSave(a)`
    let (
        &Opcode::New { dst: obj },
        &Opcode::DynSet {
            field: header_s, ..
        },
        &Opcode::DynSet {
            field: data_s,
            src: bytes,
            ..
        },
        &Opcode::ToVirtual { dst: arg, .. },
        &Opcode::Call1 { dst: res, .. },
    ) = (
        &pf.ops[pf_call - 5],
        &pf.ops[pf_call - 3],
        &pf.ops[pf_call - 2],
        &pf.ops[pf_call - 1],
        &pf.ops[pf_call],
    )
    else {
        bail!("processFiles: unexpected readSave argument shape");
    };
    if s(code, header_s) != "header" || s(code, data_s) != "data" {
        bail!("processFiles: readSave argument fields");
    }
    // `GetGlobal loc; Call2 bytes = Storage.read(path, loc)`.
    let reads: Vec<(RefFun, RefGlobal, Reg, Reg)> = (1..pf_call)
        .filter_map(|i| match (&pf.ops[i - 1], &pf.ops[i]) {
            (
                &Opcode::GetGlobal { dst: l, global },
                &Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                },
            ) if dst == bytes && arg1 == l && fname(code, fun) == "read" => {
                Some((fun, global, arg0, l))
            }
            _ => None,
        })
        .collect();
    let [(read, loc_g, path, loc)] = reads[..] else {
        bail!(
            "processFiles: expected one Storage.read, found {}",
            reads.len()
        );
    };
    let adds: Vec<RefFun> = pf
        .ops
        .iter()
        .filter_map(|o| match o {
            Opcode::Call2 { dst, fun, .. } if *dst == path && fname(code, *fun) == "__add__" => {
                Some(*fun)
            }
            _ => None,
        })
        .collect();
    let [add] = adds[..] else {
        bail!("processFiles: path concat not found");
    };
    // saveInfo: `New info; ... SetField info.file/localFile/header/corrupted`.
    let field_of = |obj_t: RefType, name: &str| -> Result<RefField> {
        let Type::Virtual { fields } = &code.types[obj_t.0] else {
            bail!("not a virtual");
        };
        fields
            .iter()
            .position(|f| s(code, f.name) == name)
            .map(RefField)
            .with_context(|| format!("no field {name}"))
    };
    let info = pf
        .ops
        .iter()
        .skip(pf_call)
        .find_map(|o| match o {
            Opcode::New { dst } => Some(*dst),
            _ => None,
        })
        .context("processFiles: no saveInfo")?;
    let info_t = rt(info);
    let res_t = rt(res);
    let exc = pf
        .ops
        .iter()
        .find_map(|o| match o {
            Opcode::Trap { exc, .. } => Some(*exc),
            _ => None,
        })
        .context("processFiles: no trap")?;
    let bool_t = code
        .types
        .iter()
        .position(|t| matches!(t, Type::Bool))
        .map(RefType)
        .context("no bool type")?;

    // LoadGame.loadGame: `GetThis cur = this.current; NullCheck cur; Field b = cur.corrupted`.
    let lg = crate::diag::static_fn(code, "ui.win.LoadGame", "loadGame")?;
    if lg.ops.len() < 3 {
        bail!("loadGame: too short");
    }
    let (
        &Opcode::GetThis { dst: lg_cur, .. },
        &Opcode::NullCheck { reg },
        &Opcode::Field {
            obj: fobj, field, ..
        },
    ) = (&lg.ops[0], &lg.ops[1], &lg.ops[2])
    else {
        bail!("loadGame: unexpected head (already applied?)");
    };
    if reg != lg_cur || fobj != lg_cur || lg.regs[lg_cur.0 as usize] != info_t {
        bail!("loadGame: head does not read current.corrupted");
    }
    if field != field_of(info_t, "corrupted")? {
        bail!("loadGame: head reads another field");
    }
    let lg_void = Reg(lg
        .regs
        .iter()
        .position(|t| matches!(code.types[t.0], Type::Void))
        .context("loadGame: no void register")? as u32);
    if (0..lg.ops.len()).any(|i| jump_targets(lg, i).contains(&2)) {
        bail!("loadGame: op 2 is a jump target");
    }

    Ok(Plan {
        rs_fi: fun_index(code, rs_find)?,
        inflate: [run - 1, run, run + 1],
        sign_jnull,
        ret_new,
        pf_fi,
        pf_call,
        read,
        loc_g,
        add,
        app_data,
        header_s,
        data_s,
        dyn_t: rt(obj),
        arg_t: rt(arg),
        res_t,
        res_corrupt: field_of(res_t, "corrupted")?,
        info_t,
        info_file: field_of(info_t, "file")?,
        info_local: field_of(info_t, "localFile")?,
        info_corrupt: field_of(info_t, "corrupted")?,
        str_t: rt(path),
        loc_t: rt(loc),
        bytes_t: rt(bytes),
        bool_t,
        exc_t: rt(exc),
        lg_fi: fun_index(code, lg.findex)?,
        lg_at: 2,
        lg_cur,
        lg_void,
    })
}

/// `checkSave(g)`, the file order of Game.start (saveFile, then app-data localSaveFile):
/// `if (g.file != null) try { g.corrupted = check(g.file); return; } catch (_) {}
/// try { g.corrupted = check(getAppDataPath() + g.localFile); } catch (_) { g.corrupted = true; }`
/// with `check(path) = readSave({header: null, data: Storage.read(path, loc)}).corrupted`.
fn add_check(code: &mut Bytecode, p: &Plan, void_t: RefType) -> Result<RefFun> {
    let mut r = Regs(vec![p.info_t]);
    let g = Reg(0);
    let (v, path, dir, loc, bytes, obj, nul, arg, res, b, exc) = (
        r.r(void_t),
        r.r(p.str_t),
        r.r(p.str_t),
        r.r(p.loc_t),
        r.r(p.bytes_t),
        r.r(p.dyn_t),
        r.r(p.bytes_t),
        r.r(p.arg_t),
        r.r(p.res_t),
        r.r(p.bool_t),
        r.r(p.exc_t),
    );
    let read_save = code.functions[p.rs_fi].findex;
    // b = check(path)
    let check = |a: &mut Asm| {
        a.op(Opcode::GetGlobal {
            dst: loc,
            global: p.loc_g,
        });
        a.op(Opcode::Call2 {
            dst: bytes,
            fun: p.read,
            arg0: path,
            arg1: loc,
        });
        a.op(Opcode::New { dst: obj });
        a.op(Opcode::Null { dst: nul });
        a.op(Opcode::DynSet {
            obj,
            field: p.header_s,
            src: nul,
        });
        a.op(Opcode::DynSet {
            obj,
            field: p.data_s,
            src: bytes,
        });
        a.op(Opcode::ToVirtual { dst: arg, src: obj });
        a.op(Opcode::Call1 {
            dst: res,
            fun: read_save,
            arg0: arg,
        });
        a.op(Opcode::NullCheck { reg: res });
        a.op(Opcode::Field {
            dst: b,
            obj: res,
            field: p.res_corrupt,
        });
    };
    let mut a = Asm::new();
    a.op(Opcode::Field {
        dst: path,
        obj: g,
        field: p.info_file,
    });
    a.jmp(
        Opcode::JNull {
            reg: path,
            offset: 0,
        },
        "local",
    );
    a.jmp(Opcode::Trap { exc, offset: 0 }, "local");
    check(&mut a);
    a.op(Opcode::EndTrap { exc });
    a.jmp(Opcode::JAlways { offset: 0 }, "set");
    a.label("local");
    a.jmp(Opcode::Trap { exc, offset: 0 }, "bad");
    a.op(Opcode::Field {
        dst: path,
        obj: g,
        field: p.info_local,
    });
    a.op(Opcode::Call0 {
        dst: dir,
        fun: p.app_data,
    });
    a.op(Opcode::Call2 {
        dst: path,
        fun: p.add,
        arg0: dir,
        arg1: path,
    });
    check(&mut a);
    a.op(Opcode::EndTrap { exc });
    a.jmp(Opcode::JAlways { offset: 0 }, "set");
    a.label("bad");
    a.op(Opcode::Bool {
        dst: b,
        value: hlbc::types::ValBool(true),
    });
    a.label("set");
    a.op(Opcode::SetField {
        obj: g,
        field: p.info_corrupt,
        src: b,
    });
    a.op(Opcode::Ret { ret: v });
    let dbg = code.functions[p.lg_fi]
        .debug_info
        .as_ref()
        .and_then(|d| d.first().map(|x| x.0))
        .unwrap_or(0);
    push_fn(code, vec![p.info_t], void_t, r.0, a.finish(), dbg)
}

fn apply(code: &mut Bytecode, p: &Plan) -> Result<(RefFun, RefFun)> {
    // 1. readSaveHeader: readSave minus inflate and signature.
    let header_fn = next_findex(code)?;
    let mut f = code.functions[p.rs_fi].clone();
    f.findex = header_fn;
    f.name = hlbc::types::RefString(0);
    f.parent = None;
    for i in p.inflate {
        f.ops[i] = Opcode::Nop;
    }
    f.ops[p.sign_jnull] = Opcode::JAlways {
        offset: (p.ret_new - p.sign_jnull - 1) as i32,
    };
    code.functions.push(f);
    match &mut code.functions[p.pf_fi].ops[p.pf_call] {
        Opcode::Call1 { fun, .. } => *fun = header_fn,
        _ => unreachable!(),
    }
    // 2. loadGame: checkSave(current) before the corrupted test.
    let void_t = code.functions[p.lg_fi].regs[p.lg_void.0 as usize];
    let check = add_check(code, p, void_t)?;
    insert_ops(
        &mut code.functions[p.lg_fi],
        p.lg_at,
        vec![Opcode::Call1 {
            dst: p.lg_void,
            fun: check,
            arg0: p.lg_cur,
        }],
    );
    Ok((header_fn, check))
}

/// Lists saves from their headers only and checks the chosen save on load, or
/// leaves `code` untouched and logs why.
pub(crate) fn patch_save_list(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("save list skipped: {e:#}"));
            return;
        }
    };
    let snap = Snap::take(code);
    let saved = [
        code.functions[p.pf_fi].clone(),
        code.functions[p.lg_fi].clone(),
    ];
    match apply(code, &p) {
        Ok((h, c)) => eprintln!(
            "patched save list: processFiles fn@{} reads headers via fn@{}, loadGame fn@{} checks the chosen save via fn@{}",
            code.functions[p.pf_fi].findex.0, h.0, code.functions[p.lg_fi].findex.0, c.0
        ),
        Err(e) => {
            snap.restore(code);
            let [a, b] = saved;
            code.functions[p.pf_fi] = a;
            code.functions[p.lg_fi] = b;
            crate::skipped(format!("save list skipped: {e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::*;
    use super::*;

    /// readSave copy differs by the inflate Nops and the sign jump; processFiles
    /// changes one call; loadGame gains one op; the rest is untouched; a second
    /// pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_save_list(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 2);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == p.lg_fi {
                shifted(a, b, p.lg_at, 1);
                check_flow(b);
                check_types(&back, b, p.lg_at..p.lg_at + 1);
            } else if i == p.pf_fi {
                for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                    assert_eq!(
                        k == p.pf_call,
                        format!("{x:?}") != format!("{y:?}"),
                        "op {k}"
                    );
                }
                check_types(&back, b, p.pf_call..p.pf_call + 1);
            } else {
                assert!(same(a, b), "function #{i} (fn@{}) changed", a.findex.0);
            }
        }
        let rs = &orig.functions[p.rs_fi];
        let hdr = &back.functions[n];
        assert_eq!(hdr.t, rs.t);
        assert_eq!(hdr.regs, rs.regs);
        for (k, (x, y)) in rs.ops.iter().zip(&hdr.ops).enumerate() {
            let changed = p.inflate.contains(&k) || k == p.sign_jnull;
            assert_eq!(changed, format!("{x:?}") != format!("{y:?}"), "op {k}");
        }
        assert_eq!(jump_targets(hdr, p.sign_jnull), [p.ret_new]);
        check_flow(hdr);
        let chk = &back.functions[n + 1];
        check_flow(chk);
        check_types(&back, chk, 0..chk.ops.len());
        assert_eq!(traps_ok(chk), 2);
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_save_list(&mut again);
        assert!(write(&again) == patched);
    }
}
