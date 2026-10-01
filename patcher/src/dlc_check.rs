// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The in-game DLC ownership check never reports a player as missing a DLC; it
// prints a warning instead.
//
// `ent.BasePlayer.hasDlc(dlc)` (Player.hx:2021-2035) is the one in-game check:
// it answers `checkSignature(dlc)` (the player's synced `pwtdc` list holds `dlc`
// and its `pwtdcSignature` matches) and caches the answer in `cachedDLC` for 10
// units of `GameState.time`. Its users:
//   - `GameState.hasFeature(dlc)` (GameState.hx:1253): true only when EVERY
//     player has the DLC. It gates DLC content for the whole party: entering
//     DLC places (`Place.doEnter`), discovering DLC regions, the world bar's DLC
//     buttons, the Beast / Sea Lords / Sanity systems, script `hasFeature`
//     (dialog branches such as the `NoDlc` password scenes) and
//     `BasePlayer.recalPlayer`, which pushes the party out of a DLC region.
//   - `Game.getPlayersWithoutDlc` -> `Dialog.displayChoices` (Dialog.hx:1668):
//     "Some players do not own this additional content: <names>".
//
// How a player who owns the DLC still reads as "without" it (most likely the
// one whose list lands last, i.e. the last to join; inferred from the bytecode,
// not traced in a live game): a player's list reaches the
// others only through the `BasePlayer.setDlcs` RPC that its own machine sends
// from `globalUpdate` once it runs, and a `false` computed before that is
// cached. Only `setDlcs__impl` clears the cache, and it runs on the host alone;
// a client keeps the stale `false` until `GameState.time` moves 10 units on,
// and the host only advances it while the world is not soft-paused
// (GameState.hx:728-729). So on the other clients, and on that player's own
// machine, the last player stays "without" the DLC, and with it the whole
// party loses the DLC content.
//
// The pass edits only `hasDlc`. After each of its two `checkSignature` calls:
//
//   if (!have) {
//       if (cachedDLC.get("!" + dlc) == null) {     // once per player and DLC
//           cachedDLC.set("!" + dlc, entry);
//           Sys.println("mp: dlc: player " + user + " does not own DLC " + dlc + ...);
//       }
//       have = true;
//   }
//
// The `"!"` key never collides with a DLC id (hasDlc reads `cachedDLC` by the
// id); the host's `setDlcs__impl` clears the map, so a changed list warns again.
// `Sys.println` lands in shim.log as a "game:" line (see diag.rs).
//
// Risk: a player who really lacks a DLC now gets that DLC's places, regions and
// dialog branches like everyone else; whatever the game needs from content that
// player does not have installed may fail there.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::ValBool;

const MARK: &str = "!";
const MSG_A: &str = "mp: dlc: player ";
const MSG_B: &str = " does not own DLC ";
const MSG_C: &str =
    " (or its DLC list has not arrived yet); the DLC check is ignored, treated as owned";

/// One `have = checkSignature(this, dlc)` site of hasDlc.
struct Site {
    /// Index of the op right after the call (where the block goes).
    at: usize,
    /// The Bool register holding the answer.
    have: Reg,
}

struct Plan {
    fi: usize,
    sites: Vec<Site>,
    /// The cache entry register (the value hasDlc stores in cachedDLC).
    entry: Reg,
    cached: RefField,
    cached_t: RefType,
    user: RefField,
    get: RefFun,
    get_t: RefType,
    set: RefFun,
    set_t: RefType,
    println: RefFun,
    str_add: RefFun,
    str_t: RefType,
    void_t: RefType,
}

fn fun_index(code: &Bytecode, findex: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == findex)
        .with_context(|| format!("function @{} not found", findex.0))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let bp_t = obj_type(code, "ent.BasePlayer")?;
    let f = method(code, bp_t, "hasDlc")?;
    let fi = fun_index(code, f.findex)?;
    let check = method(code, bp_t, "checkSignature")?.findex;
    let println = static_fn(code, "$Sys", "println")?.findex;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    let str_t = obj_type(code, "String")?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let (cached, cached_t) = field(code, bp_t, "cachedDLC")?;
    let (user, user_t) = field(code, bp_t, "user")?;
    if user_t != str_t {
        bail!("hasDlc: BasePlayer.user is not a String");
    }
    if fun_args(code, f).get(1) != Some(&str_t) {
        bail!("hasDlc: the DLC argument is not a String");
    }
    if f.ops
        .iter()
        .any(|op| matches!(op, Opcode::Call1 { fun, .. } if *fun == println))
    {
        bail!("hasDlc: already applied");
    }

    // The cache map: `cachedDLC.get(dlc)` and `cachedDLC.set(dlc, entry)`.
    let map_regs: Vec<Reg> = f
        .ops
        .iter()
        .filter_map(|op| match op {
            Opcode::GetThis { dst, field } if *field == cached => Some(*dst),
            _ => None,
        })
        .collect();
    if map_regs.is_empty() {
        bail!("hasDlc: does not read cachedDLC");
    }
    let gets: Vec<(RefFun, Reg)> = f
        .ops
        .iter()
        .filter_map(|op| match op {
            Opcode::Call2 {
                dst,
                fun,
                arg0,
                arg1,
            } if map_regs.contains(arg0) && *arg1 == Reg(1) => Some((*fun, *dst)),
            _ => None,
        })
        .collect();
    let get = match gets[..] {
        [(g, _), ..] if gets.iter().all(|(x, _)| *x == g) => g,
        _ => bail!("hasDlc: no single cachedDLC.get(dlc)"),
    };
    let get_t = f.regs[gets[0].1 .0 as usize];
    let sets: Vec<(RefFun, Reg, Reg)> = f
        .ops
        .iter()
        .filter_map(|op| match op {
            Opcode::Call3 {
                dst,
                fun,
                arg0,
                arg1,
                arg2,
            } if map_regs.contains(arg0) && *arg1 == Reg(1) => Some((*fun, *dst, *arg2)),
            _ => None,
        })
        .collect();
    let [(set, set_dst, entry)] = sets[..] else {
        bail!("hasDlc: no single cachedDLC.set(dlc, entry)");
    };
    let set_t = f.regs[set_dst.0 as usize];

    // Both answers: `have = checkSignature(this, dlc); entry.have = have`.
    let mut sites = Vec::new();
    for (i, op) in f.ops.iter().enumerate() {
        let Opcode::Call2 {
            dst,
            fun,
            arg0,
            arg1,
        } = op
        else {
            continue;
        };
        if *fun != check {
            continue;
        }
        if *arg0 != Reg(0) || *arg1 != Reg(1) || f.regs[dst.0 as usize] != bool_t {
            bail!("hasDlc: unexpected checkSignature call at op {i}");
        }
        match f.ops.get(i + 1) {
            Some(Opcode::SetField { obj, src, .. }) if *obj == entry && src == dst => {}
            _ => bail!("hasDlc: the answer at op {i} is not stored in the cache entry"),
        }
        sites.push(Site {
            at: i + 1,
            have: *dst,
        });
    }
    if sites.len() != 2 {
        bail!("hasDlc: {} checkSignature calls, want 2", sites.len());
    }
    // Nothing may jump straight to the store: the block must run on every path.
    for s in &sites {
        if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&s.at)) {
            bail!("hasDlc: a jump lands on op {}", s.at);
        }
    }
    Ok(Plan {
        fi,
        sites,
        entry,
        cached,
        cached_t,
        user,
        get,
        get_t,
        set,
        set_t,
        println,
        str_add,
        str_t,
        void_t,
    })
}

/// Offset for a jump at `from` to `to` inside one block.
fn off(from: usize, to: usize) -> i32 {
    to as i32 - from as i32 - 1
}

fn apply(code: &mut Bytecode, p: Plan) {
    let mark = str_global(code, p.str_t, MARK);
    let msg_a = str_global(code, p.str_t, MSG_A);
    let msg_b = str_global(code, p.str_t, MSG_B);
    let msg_c = str_global(code, p.str_t, MSG_C);
    let f = &mut code.functions[p.fi];
    let mut reg = |t: RefType| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let (map, key, got, set_out, msg, tmp, out) = (
        reg(p.cached_t),
        reg(p.str_t),
        reg(p.get_t),
        reg(p.set_t),
        reg(p.str_t),
        reg(p.str_t),
        reg(p.void_t),
    );
    let dlc = Reg(1);
    // Later site first, so the earlier index stays valid.
    for s in p.sites.iter().rev() {
        let have = s.have;
        let ops = vec![
            /* 0 */
            Opcode::JTrue {
                cond: have,
                offset: off(0, 17),
            },
            /* 1 */
            Opcode::GetThis {
                dst: map,
                field: p.cached,
            },
            /* 2 */ Opcode::NullCheck { reg: map },
            /* 3 */
            Opcode::GetGlobal {
                dst: tmp,
                global: mark,
            },
            /* 4 */
            Opcode::Call2 {
                dst: key,
                fun: p.str_add,
                arg0: tmp,
                arg1: dlc,
            },
            /* 5 */
            Opcode::Call2 {
                dst: got,
                fun: p.get,
                arg0: map,
                arg1: key,
            },
            /* 6 */
            Opcode::JNotNull {
                reg: got,
                offset: off(6, 17),
            },
            /* 7 */
            Opcode::Call3 {
                dst: set_out,
                fun: p.set,
                arg0: map,
                arg1: key,
                arg2: p.entry,
            },
            /* 8 */
            Opcode::GetGlobal {
                dst: msg,
                global: msg_a,
            },
            /* 9 */
            Opcode::GetThis {
                dst: tmp,
                field: p.user,
            },
            /* 10 */
            Opcode::Call2 {
                dst: msg,
                fun: p.str_add,
                arg0: msg,
                arg1: tmp,
            },
            /* 11 */
            Opcode::GetGlobal {
                dst: tmp,
                global: msg_b,
            },
            /* 12 */
            Opcode::Call2 {
                dst: msg,
                fun: p.str_add,
                arg0: msg,
                arg1: tmp,
            },
            /* 13 */
            Opcode::Call2 {
                dst: msg,
                fun: p.str_add,
                arg0: msg,
                arg1: dlc,
            },
            /* 14 */
            Opcode::GetGlobal {
                dst: tmp,
                global: msg_c,
            },
            /* 15 */
            Opcode::Call2 {
                dst: msg,
                fun: p.str_add,
                arg0: msg,
                arg1: tmp,
            },
            /* 16 */
            Opcode::Call1 {
                dst: out,
                fun: p.println,
                arg0: msg,
            },
            /* 17 */
            Opcode::Bool {
                dst: have,
                value: ValBool(true),
            },
        ];
        insert_ops(f, s.at, ops);
    }
    eprintln!(
        "patched dlc check fn@{}: hasDlc never reports a missing DLC, it prints a warning",
        f.findex.0
    );
}

/// Makes `BasePlayer.hasDlc` always true with a one-time warning per player and
/// DLC, or leaves `code` untouched and logs why.
pub(crate) fn patch_dlc_check(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("dlc check skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";
    const BLOCK: usize = 18;

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let fi = p.fi;
        let ats: Vec<usize> = p.sites.iter().map(|s| s.at).collect();
        let haves: Vec<Reg> = p.sites.iter().map(|s| s.have).collect();
        let (println, check) = (
            p.println,
            method(
                &orig,
                obj_type(&orig, "ent.BasePlayer").unwrap(),
                "checkSignature",
            )
            .unwrap()
            .findex,
        );
        let mut code = read(&image);
        patch_dlc_check(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(b.ops.len(), a.ops.len() + 2 * BLOCK);
        assert_eq!(b.regs.len(), a.regs.len() + 7);

        // Original op i sits at map(i); every original jump keeps its target.
        let map = |i: usize| i + BLOCK * ats.iter().filter(|&&at| i >= at).count();
        for i in 0..a.ops.len() {
            let ta: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), ta, "op {i}");
            if ta.is_empty() {
                assert_eq!(format!("{:?}", a.ops[i]), format!("{:?}", b.ops[map(i)]));
            }
        }
        for (k, (&at, &have)) in ats.iter().zip(&haves).enumerate() {
            let start = at + k * BLOCK;
            assert!(
                matches!(b.ops[start - 1], Opcode::Call2 { fun, .. } if fun == check),
                "site {k}: block not right after checkSignature"
            );
            // An owned DLC, or one already warned about, skips to `have = true`.
            assert_eq!(jump_targets(b, start), [start + 17]);
            assert_eq!(jump_targets(b, start + 6), [start + 17]);
            assert!(
                matches!(b.ops[start + 16], Opcode::Call1 { fun, arg0, .. } if fun == println && arg0 == Reg(a.regs.len() as u32 + 4))
            );
            assert!(matches!(
                b.ops[start + 17],
                Opcode::Bool { dst, value: ValBool(true) } if dst == have
            ));
            // The answer stored in the cache entry is the forced one.
            assert!(matches!(b.ops[start + BLOCK], Opcode::SetField { src, .. } if src == have));
        }
        for v in [MARK, MSG_A, MSG_B, MSG_C] {
            assert!(back.strings.iter().any(|x| x.as_str() == v), "string {v:?}");
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_dlc_check(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
