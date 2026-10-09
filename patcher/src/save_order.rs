// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The load / save list is newest first, with no exception.
//
// Vanilla (src/ui/win/LoadGame.hx) LoadGame.loadGames(mode) is the only list
// builder (LoadGame.init for every mode: load, co-op load, save, convert;
// TitlescreenWindow.checkContinue). It sorts the saves by header.irlTime
// (Date.now() at save time), newest first, with a stable merge sort
// (games.sort(cmp), cmp = sign(b.irlTime - a.irlTime)). But an autosave.dat
// with a playerId never enters that list: it goes to a separate `autosaves`
// list, prepended after the sort (`games = autosaves.concat(games)`, every
// mode but one). So the autosave sits on top even when it is older than the
// saves below it.
//
// One op: right after that concat, `sort(result, cmp)`, the same vanilla
// sort with the same vanilla comparator closure. The mode without the concat
// jumps past it (it is already sorted). Nothing else changes.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::coop_spectate::dst_of;
use super::*;

struct Plan {
    /// loadGames: its function index and where the sort goes.
    fi: usize,
    at: usize,
    sort: Opcode,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let f = crate::diag::static_fn(code, "ui.win.$LoadGame", "loadGames")?;
    let sorts: Vec<(usize, Reg, RefFun, Reg, Reg)> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call2 {
                dst,
                fun,
                arg0,
                arg1,
            } if fname(code, *fun) == "sort" => Some((i, *dst, *fun, *arg0, *arg1)),
            _ => None,
        })
        .collect();
    let [(i_sort, void, sort, list, cmp)] = sorts[..] else {
        bail!(
            "loadGames: expected one sort, found {} (already applied?)",
            sorts.len()
        );
    };
    let concats: Vec<(usize, Reg)> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::Call2 { dst, fun, .. } if fname(code, *fun) == "concat" => Some((i, *dst)),
            _ => None,
        })
        .collect();
    let [(i_concat, out)] = concats[..] else {
        bail!("loadGames: expected one concat, found {}", concats.len());
    };
    if i_concat < i_sort {
        bail!("loadGames: concat before the sort");
    }
    if f.regs[out.0 as usize] != f.regs[list.0 as usize] {
        bail!("loadGames: concat result is not the sorted list type");
    }
    let (args, _) = sig(code, sort)?;
    if args.first() != Some(&f.regs[list.0 as usize]) {
        bail!("loadGames: unexpected sort signature");
    }
    // Last write of the comparator: before the sort, never after it.
    if (i_sort + 1..f.ops.len()).any(|i| dst_of(&f.ops[i]) == Some(cmp)) {
        bail!("loadGames: comparator register rewritten after the sort");
    }
    let at = i_concat + 1;
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&at)) {
        bail!("loadGames: the op after concat is a jump target");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        at,
        sort: Opcode::Call2 {
            dst: void,
            fun: sort,
            arg0: out,
            arg1: cmp,
        },
    })
}

/// Sorts the whole save list (autosave included) newest first, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_save_order(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            crate::skipped(format!("save order skipped: {e:#}"));
            return;
        }
    };
    let f = &mut code.functions[p.fi];
    insert_ops(f, p.at, vec![p.sort]);
    eprintln!(
        "patched save order: loadGames fn@{} sorts after autosaves.concat (op {})",
        f.findex.0, p.at
    );
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::*;
    use super::*;
    use crate::testsim::{Core, Sim, V};

    /// One op inserted in loadGames; nothing else changes; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_save_order(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(back.types[..], orig.types[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            if i == p.fi {
                shifted(a, b, p.at, 1);
                check_flow(b);
                check_types(&back, b, p.at..p.at + 1);
                assert_eq!(format!("{:?}", b.ops[p.at]), format!("{:?}", p.sort));
            } else {
                assert!(
                    format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs,
                    "function #{i} (fn@{}) changed",
                    a.findex.0
                );
            }
        }
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_save_order(&mut again);
        assert!(write(&again) == patched);
    }

    /// The patched tail of loadGames (after the files are read): an older
    /// autosave lands at its place by date, not on top; the save mode (no
    /// autosave concat) stays sorted. The vanilla comparator orders newest first.
    #[test]
    fn newest_first_with_older_autosave() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_save_order(&mut code);
        let f = &code.functions[p.fi];
        let findex = f.findex;
        let Opcode::Call2 { fun: sort, .. } = p.sort else {
            unreachable!()
        };
        // ArrayObj length / array fields, as loadGames reads them.
        let fields: Vec<RefField> = f
            .ops
            .iter()
            .filter_map(|o| match o {
                Opcode::Field { obj, field, .. } if *obj == Reg(1) => Some(*field),
                _ => None,
            })
            .collect();
        let (a_len, a_raw) = (fields[0], fields[1]);
        assert_ne!(a_len, a_raw);
        // The comparator: closure wrapper of a static fn reading header.irlTime.
        let cmp_fn = f
            .ops
            .iter()
            .find_map(|o| match o {
                Opcode::StaticClosure { fun, .. } => Some(*fun),
                _ => None,
            })
            .unwrap();
        let cf = &code.functions[fun_index(&code, cmp_fn).unwrap()];
        let Opcode::Field {
            field: f_header, ..
        } = cf.ops[1]
        else {
            panic!("comparator shape")
        };
        let Opcode::Field { field: f_irl, .. } = cf.ops[3] else {
            panic!("comparator shape")
        };
        // The save-mode global the concat is skipped for.
        let (i_mode, mode_g) = f
            .ops
            .iter()
            .enumerate()
            .filter_map(|(i, o)| match o {
                Opcode::GetGlobal { global, .. } => Some((i, *global)),
                _ => None,
            })
            .filter(|(i, _)| *i > p.at - 20 && *i < p.at)
            .last()
            .unwrap();
        assert!(matches!(f.ops[i_mode + 1], Opcode::JEq { .. }));
        let flags: Vec<RefFun> = code
            .functions
            .iter()
            .filter(|g| ["get_isAdmin", "get_isBeta"].contains(&s(&code, g.name)))
            .map(|g| g.findex)
            .collect();
        let start = f
            .ops
            .iter()
            .position(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == sort))
            .unwrap()
            - 15;
        let (concat, autos_r) = match f.ops[p.at - 1] {
            Opcode::Call2 { fun, arg0, .. } => (fun, arg0),
            _ => panic!("no concat before the sort"),
        };
        let ret = f.ops.len() - 1;
        let Opcode::Ret { ret: ret_r } = f.ops[ret] else {
            panic!("no final Ret")
        };

        let save = |c: &mut Core, name: &str, t: f64| {
            let h = c.obj(&[(f_irl, V::F(t))]);
            let g = c.obj(&[(f_header, h)]);
            c.key_set(&g, "name".into(), V::S(name.into()));
            g
        };
        let items = |c: &Core, a: &V| -> Vec<V> {
            let V::I(n) = c.get(a, a_len) else {
                panic!("len")
            };
            let raw = c.get(a, a_raw);
            (0..n).map(|k| c.key_get(&raw, &format!("i{k}"))).collect()
        };
        let names = |c: &Core, a: &V| -> Vec<String> {
            items(c, a)
                .iter()
                .map(|g| match c.key_get(g, "name") {
                    V::S(x) => x,
                    o => panic!("{o:?}"),
                })
                .collect()
        };

        for save_mode in [false, true] {
            let flags = flags.clone();
            let mut sim = Sim::new(
                &code,
                code.functions.len(),
                move |c, g, a| {
                    if flags.contains(&g) {
                        Some(V::B(true))
                    } else if g == sort {
                        // Stable sort with the vanilla comparator's rule.
                        assert!(matches!(&a[1], V::Clo(_, b) if **b == V::Fun(cmp_fn)));
                        let mut v = items(c, &a[0]);
                        let irl = |c: &Core, g: &V| match c.get(&c.get(g, f_header), f_irl) {
                            V::F(x) => x,
                            o => panic!("{o:?}"),
                        };
                        let keys: Vec<f64> = v.iter().map(|g| irl(c, g)).collect();
                        let mut idx: Vec<usize> = (0..v.len()).collect();
                        idx.sort_by(|&x, &y| keys[y].partial_cmp(&keys[x]).unwrap());
                        v = idx.into_iter().map(|k| v[k].clone()).collect();
                        let raw = c.get(&a[0], a_raw);
                        for (k, g) in v.into_iter().enumerate() {
                            c.key_set(&raw, format!("i{k}"), g);
                        }
                        c.log.push(("sort", vec![]));
                        Some(V::Null)
                    } else if g == concat {
                        let mut v = items(c, &a[0]);
                        v.extend(items(c, &a[1]));
                        Some(c.arr(a_len, a_raw, v))
                    } else {
                        None
                    }
                },
                |_, _, _| panic!("no virtual call expected"),
            );
            let c = &mut sim.c;
            let auto = save(c, "autosave", 100.0);
            let s1 = save(c, "save1", 300.0);
            let s2 = save(c, "save2", 50.0);
            let s3 = save(c, "save3", 200.0);
            let inner = c.arr(a_len, a_raw, vec![s2, s1, s3]);
            let games = c.arr(a_len, a_raw, vec![inner]);
            let autos = c.arr(a_len, a_raw, vec![auto]);
            let save_mode_v = c.enm(2, vec![]);
            let load_mode_v = c.enm(0, vec![]);
            c.globals.insert(mode_g.0, save_mode_v.clone());
            let mut r = vec![V::Null; f.regs.len()];
            r[0] = if save_mode { save_mode_v } else { load_mode_v };
            r[1] = games;
            r[autos_r.0 as usize] = autos;
            assert_eq!(sim.span(findex, &mut r, start, &[ret]), ret);
            let want: &[&str] = if save_mode {
                &["save1", "save3", "save2"]
            } else {
                &["save1", "save3", "autosave", "save2"]
            };
            assert_eq!(
                names(&sim.c, &r[ret_r.0 as usize]),
                want,
                "save mode {save_mode}"
            );
        }

        // The vanilla comparator: newer b -> 1 (a after b), older b -> -1, equal 0.
        let mut sim = Sim::new(&code, 0, |_, _, _| None, |_, _, _| panic!());
        let c = &mut sim.c;
        let (old, new) = (save(c, "old", 1.0), save(c, "new", 2.0));
        assert_eq!(sim.run(cmp_fn, vec![old.clone(), new.clone()]), V::I(1));
        assert_eq!(sim.run(cmp_fn, vec![new.clone(), old.clone()]), V::I(-1));
        assert_eq!(sim.run(cmp_fn, vec![old.clone(), old]), V::I(0));
    }
}
