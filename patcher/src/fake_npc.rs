// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op: a guest joining (or rejoining) while a confession scene runs no longer
// loads forever.
//
// Every confession NPC is a FakeNpc built by Controller.playConfessionSetup
// from a deep copy of `game.allNpcs.get("ConfessionSetup").inf`, so its `id`
// starts as "ConfessionSetup". Npc.confession__impl (closure fn@35765, Npc.hx:458)
// then runs `makeDialogFromConfessionData(conf); id = conf.confID`: the id
// becomes the confession's id ("ConfessionGoalCaptureAnimal", ...), which is
// no key of allNpcs, and the inf now holds the confession dialog built in
// memory. `inf` itself is never synced; the id and `dialogData.lastConf` are.
//
// Vanilla FakeNpc.initData (Npc.hx:1129) on a client:
//
//   inf = Json.parse(Json.stringify(game.allNpcs.get(id).inf));
//
// A guest present when the confession started got the FakeNpc while its id was
// still "ConfessionSetup" and ran confession__impl itself. A guest that gets it
// in a full sync (join, rejoin after a drop) sees the confession id: `.get` is
// null, `Null access .inf` throws out of Game.start (Game.hx:807, before
// state.init), and the client is left with a half-built world.
//
// The fix, in FakeNpc.initData, client path only, when the id is no allNpcs key:
//
//   var conf = dialogData?.lastConf;   // synced, set by confession__impl
//   if (conf == null) return;          // keep Element.initData's inf
//   e = allNpcs.get("ConfessionSetup"); if (e == null) return;
//   inf = Json.parse(Json.stringify(e.inf));          // vanilla line
//   makeDialogFromConfessionData(conf);               // as confession__impl did
//
// i.e. the joining guest rebuilds the inf exactly the way a guest present at
// the start has it. A found id runs the vanilla line alone. One log line on the
// miss path. Validated before editing; a mismatch skips the pass (logged).

use super::*;
use crate::diag::static_fn;
use crate::job_xp::str_global;

const LOG_A: &str = "mp: fakenpc: id not in allNpcs, rebuilt from confession data: ";
const SETUP: &str = "ConfessionSetup";

struct Plan {
    fi: usize,
    /// The `NullCheck v` after `v = cast allNpcs.get(id)`.
    nc: usize,
    /// The final `Ret`, right after `SetThis inf`.
    ret: usize,
    map: Reg,
    got: Reg,
    v: Reg,
    get: RefFun,
    str_t: RefType,
    void_t: RefType,
    conf_t: RefType,
    proxy_t: RefType,
    f_dialog: RefField,
    f_last: RefField,
    f_id: RefField,
    make: RefFun,
    println: RefFun,
    str_add: RefFun,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let str_t = obj_type(code, "String")?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let elt_t = obj_type(code, "ent.p.Element")?;
    let npc_t = obj_type(code, "ent.p.Npc")?;
    let fake_t = obj_type(code, "ent.p.FakeNpc")?;
    let conf_t = obj_type(code, "st.player.ConfessionLogData")?;
    let (f_dialog, proxy_t) = field(code, elt_t, "dialogData")?;
    let f_last = typed(code, proxy_t, "lastConf", conf_t)?;
    let f_id = typed(code, elt_t, "id", str_t)?;
    let (f_inf, _) = field(code, elt_t, "inf")?;
    let make = method(code, npc_t, "makeDialogFromConfessionData")?;
    if fun_args(code, make) != [npc_t, conf_t] {
        bail!("makeDialogFromConfessionData: unexpected signature");
    }
    let make = make.findex;
    let println = static_fn(code, "$Sys", "println")?.findex;
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    // The allNpcs key every confession FakeNpc starts from.
    existing_str(code, str_t, SETUP)?;

    let f = method(code, fake_t, "initData")?;
    if fun_args(code, f) != [fake_t] {
        bail!("FakeNpc.initData: unexpected signature");
    }
    // v = cast allNpcs.get(this.id); NullCheck v; x = v.inf
    let hits: Vec<usize> = (1..f.ops.len().saturating_sub(3))
        .filter(|&i| {
            matches!(
                (&f.ops[i], &f.ops[i + 1], &f.ops[i + 2], &f.ops[i + 3]),
                (
                    Opcode::Call2 { dst, .. },
                    Opcode::ToVirtual { dst: v, src },
                    Opcode::NullCheck { reg },
                    Opcode::Field { obj, .. },
                ) if src == dst && reg == v && obj == v
            )
        })
        .collect();
    let [i] = hits[..] else {
        bail!("FakeNpc.initData: {} allNpcs lookups, want 1", hits.len());
    };
    let Opcode::Call2 {
        dst: got,
        fun: get,
        arg0: map,
        arg1: key,
    } = f.ops[i]
    else {
        unreachable!()
    };
    let Opcode::ToVirtual { dst: v, .. } = f.ops[i + 1] else {
        unreachable!()
    };
    if !matches!(f.ops[i - 1], Opcode::GetThis { dst, field } if dst == key && field == f_id) {
        bail!("FakeNpc.initData: the lookup key is not this.id");
    }
    let n = f.ops.len();
    if !matches!(f.ops[n - 2], Opcode::SetThis { field, .. } if field == f_inf)
        || !matches!(f.ops[n - 1], Opcode::Ret { .. })
        || (0..n).any(|j| jump_targets(f, j).contains(&(i + 2)))
        || (0..n).any(|j| matches!(f.ops[j], Opcode::Switch { .. } | Opcode::Trap { .. }))
    {
        bail!("FakeNpc.initData: unexpected shape");
    }
    Ok(Plan {
        fi: fun_index(code, f.findex)?,
        nc: i + 2,
        ret: n - 1,
        map,
        got,
        v,
        get,
        str_t,
        void_t,
        conf_t,
        proxy_t,
        f_dialog,
        f_last,
        f_id,
        make,
        println,
        str_add,
    })
}

/// Ops of the miss path inserted before the NullCheck (`to_ret`: offset base
/// of the final Ret from the block's start).
const MISS_LEN: usize = 14;

fn apply(code: &mut Bytecode, p: &Plan) {
    let setup = str_global(code, p.str_t, SETUP);
    let log_a = str_global(code, p.str_t, LOG_A);
    let f = &mut code.functions[p.fi];
    let conf = new_reg(f, p.conf_t);
    let dd = new_reg(f, p.proxy_t);
    let key = new_reg(f, p.str_t);
    let msg = new_reg(f, p.str_t);
    let v0 = new_reg(f, p.void_t);

    // After `SetThis inf`: the confession dialog, when this was a miss.
    insert_ops(
        f,
        p.ret,
        vec![
            Opcode::JNull { reg: conf, offset: 1 },
            Opcode::Call2 { dst: v0, fun: p.make, arg0: Reg(0), arg1: conf },
        ],
    );
    // The Ret is now at p.ret + 2 + MISS_LEN; op k of the block is at p.nc + k.
    let to_ret = |k: usize| (p.ret + 2 + MISS_LEN - (p.nc + k) - 1) as i32;
    let ops = vec![
        Opcode::Null { dst: conf },
        Opcode::JNotNull { reg: p.v, offset: (MISS_LEN - 2) as i32 },
        Opcode::GetThis { dst: dd, field: p.f_dialog },
        Opcode::JNull { reg: dd, offset: to_ret(3) },
        Opcode::Field { dst: conf, obj: dd, field: p.f_last },
        Opcode::JNull { reg: conf, offset: to_ret(5) },
        Opcode::GetGlobal { dst: msg, global: log_a },
        Opcode::GetThis { dst: key, field: p.f_id },
        Opcode::Call2 { dst: msg, fun: p.str_add, arg0: msg, arg1: key },
        Opcode::Call1 { dst: v0, fun: p.println, arg0: msg },
        Opcode::GetGlobal { dst: key, global: setup },
        Opcode::Call2 { dst: p.got, fun: p.get, arg0: p.map, arg1: key },
        Opcode::ToVirtual { dst: p.v, src: p.got },
        Opcode::JNull { reg: p.v, offset: to_ret(13) },
    ];
    assert_eq!(ops.len(), MISS_LEN);
    insert_ops(f, p.nc, ops);
    eprintln!(
        "patched fake npc fn@{} op {}: a joining guest rebuilds a running confession NPC instead of a null access",
        f.findex.0, p.nc
    );
}

/// Lets a co-op guest join during a confession scene, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_fake_npc(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => crate::skipped(format!("fake npc skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;
    use crate::testsim::{Sim, V};

    /// Only FakeNpc.initData changes: the two blocks, every vanilla op kept,
    /// the early returns still skip both; a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_fake_npc(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.functions.len(), orig.functions.len());
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            assert_eq!(same(a, b), i != p.fi, "fn #{i}");
        }
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        assert_eq!(b.ops.len(), a.ops.len() + MISS_LEN + 2);
        let map = |t: usize| {
            t + if t >= p.nc { MISS_LEN } else { 0 } + if t >= p.ret { 2 } else { 0 }
        };
        for i in 0..a.ops.len() {
            let want: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), want, "op {i}");
            if want.is_empty() {
                assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]));
            }
        }
        let ret = map(p.ret);
        for k in [3, 5, 13] {
            assert_eq!(jump_targets(b, p.nc + k), [ret], "miss op {k}");
        }
        assert_eq!(jump_targets(b, p.nc + 1), [p.nc + MISS_LEN]);
        assert_eq!(jump_targets(b, ret - 2), [ret]);
        check_types(&back, b, p.nc..p.nc + MISS_LEN);
        check_types(&back, b, ret - 2..ret);
        check_flow(b);

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_fake_npc(&mut again);
        assert!(write(&again) == patched);
    }

    /// Runs the patched initData on a client: a known id copies its own entry;
    /// a confession id rebuilds from ConfessionSetup and the synced lastConf;
    /// no lastConf, or no ConfessionSetup entry, keeps the inf and never throws.
    #[test]
    fn client_rebuilds_confession_npc() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_fake_npc(&mut code);
        let fun = code.functions[p.fi].findex;
        let f0 = &orig.functions[p.fi];
        // Vanilla prologue: the super call, game, isAuth; JsonParser New.
        let super_init = match f0.ops[0] {
            Opcode::Call1 { fun, .. } => fun,
            ref o => panic!("{o:?}"),
        };
        let fld = |op: usize| match f0.ops[op] {
            Opcode::GetThis { field, .. } | Opcode::Field { field, .. } => field,
            ref o => panic!("op {op}: {o:?}"),
        };
        let (f_game, f_auth, f_all) = (fld(1), fld(5), fld(10));
        let (f_inf, _) = field(&code, obj_type(&code, "ent.p.Element").unwrap(), "inf").unwrap();
        let v_inf = match f0.ops[p.nc + 1] {
            Opcode::Field { field, .. } => field,
            ref o => panic!("{o:?}"),
        };
        let (get, make) = (p.get, p.make);
        let print_fn = match f0.ops[p.nc + 4] {
            Opcode::Call3 { fun, .. } => fun,
            ref o => panic!("{o:?}"),
        };
        let parser_ctor = match f0.ops[p.nc + 5] {
            Opcode::Call2 { fun, .. } => fun,
            ref o => panic!("{o:?}"),
        };
        let parse = match f0.ops[p.nc + 6] {
            Opcode::Call1 { fun, .. } => fun,
            ref o => panic!("{o:?}"),
        };
        let (println, str_add) = (p.println, p.str_add);

        struct Case {
            id: &'static str,
            last: bool,
            setup: bool,
            want_inf: &'static str,
            made: bool,
        }
        let cases = [
            Case { id: "Npc1", last: false, setup: true, want_inf: "copy:Npc1", made: false },
            Case { id: "ConfX", last: true, setup: true, want_inf: "copy:ConfessionSetup", made: true },
            Case { id: "ConfX", last: false, setup: true, want_inf: "placeholder", made: false },
            Case { id: "ConfX", last: true, setup: false, want_inf: "placeholder", made: false },
        ];
        for c in cases {
            let mut sim = Sim::new(
                &code,
                code.functions.len(),
                move |core, f, args| {
                    if f == super_init {
                        return Some(V::Null);
                    }
                    if f == get {
                        let V::S(k) = &args[1] else { panic!("key {:?}", args[1]) };
                        return Some(core.map("all", k));
                    }
                    if f == print_fn {
                        // Json.stringify(inf): the inf's tag
                        let V::S(t) = core.key_get(&args[0], "tag") else { panic!() };
                        return Some(V::S(t));
                    }
                    if f == parser_ctor {
                        core.key_set(&args[0], "src".into(), args[1].clone());
                        return Some(V::Null);
                    }
                    if f == parse {
                        let V::S(t) = core.key_get(&args[0], "src") else { panic!() };
                        return Some(V::S(format!("copy:{t}")));
                    }
                    if f == make {
                        core.log.push(("make", args.to_vec()));
                        return Some(V::Null);
                    }
                    if f == str_add {
                        let s = |v: &V| match v {
                            V::S(x) => x.clone(),
                            o => format!("{o:?}"),
                        };
                        return Some(V::S(s(&args[0]) + &s(&args[1])));
                    }
                    if f == println {
                        core.log.push(("println", args.to_vec()));
                        return Some(V::Null);
                    }
                    None
                },
                |_, _, _| V::Null,
            );
            for (k, present) in [("Npc1", true), (SETUP, c.setup)] {
                if present {
                    let inf = sim.c.obj(&[]);
                    sim.c.key_set(&inf, "tag".into(), V::S(k.to_string()));
                    let e = sim.c.obj(&[(v_inf, inf)]);
                    sim.c.put("all", k, e);
                }
            }
            let all = sim.c.obj(&[]);
            let game = sim.c.obj(&[(f_auth, V::B(false)), (f_all, all)]);
            let conf = if c.last { sim.c.obj(&[]) } else { V::Null };
            let dd = sim.c.obj(&[(p.f_last, conf.clone())]);
            let this = sim.c.obj(&[
                (f_game, game),
                (p.f_id, V::S(c.id.into())),
                (p.f_dialog, dd),
                (f_inf, V::S("placeholder".into())),
            ]);
            sim.run(fun, vec![this.clone()]);
            assert_eq!(sim.c.get(&this, f_inf), V::S(c.want_inf.into()), "{}", c.id);
            let made = sim.c.take("make");
            if c.made {
                assert_eq!(made, [vec![this.clone(), conf.clone()]]);
            } else {
                assert!(made.is_empty());
            }
            let logs = sim.c.take("println");
            assert_eq!(logs.len(), usize::from(c.last), "{}", c.id);
        }
    }
}
