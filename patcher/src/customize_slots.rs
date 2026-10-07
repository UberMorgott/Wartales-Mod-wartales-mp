// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// New-game customize screen: starting troops larger than the scene.
//
// `CustomizeScreen.initS3d` gives every unit of the troop its own spot in the
// 3D scene (TitleBackgroundCharaSelection.l3d): object "Model0<i+1>" and camera
// prefab "Camera0<i+1>". The scene has five (Camera01..05, vanilla max =
// Farmers: 3 humans + Swine + the Pony that `createTroop` always appends), and
// `Prefab.get` throws "Missing prefab Camera06" for a sixth unit. In co-op every
// unit must also be claimed by clicking it there (`LobbyState.allUnitsAssigned`,
// which canStart and the start button use), so a unit without a spot could
// never get an owner either.
//
// This pass lets the troop be larger than the screen:
//
//   slots  = CustomizeScreen.characterIndex.length   (5: the ctor's pad order [0,1,2,4,3])
//   excess = units.length - slots
//   for each unit, in troop order:
//       hidden = shown >= slots || (unit.isAnimal && hiddenAnimals++ < excess)
//
// so humans keep the on-screen slots and animals (first ones first: the troop's
// own animal before the auto-added Pony) are left off. With five units or
// fewer nothing is hidden and both functions behave exactly as before.
//
//   CustomizeScreen.initS3d          a hidden unit gets no Character (no model,
//                                    no camera); shown units take the next slot
//                                    number, so names stay Model01..05/Camera01..05.
//   LobbyState.initAssignments       (host, new game only) a hidden unit's lobby
//                                    entry is created with p = getUser().id,
//                                    i.e. owned by the host, so allUnitsAssigned
//                                    and makeGroups see an owner.
//
// Both run the same rule over the same order (the lobby's `units` proxy array
// is the createTroop order, and CustomizeScreen.units is copied from it), so the
// host and every client hide the same units. The rule never throws: every
// field read is null-guarded and falls back to "shown" (vanilla behaviour).
//
// Both functions are validated before anything is edited; on any mismatch the
// pass changes nothing and logs why, and the other patches still apply.

use super::*;

struct Plan {
    slots: i32,
    // CustomizeScreen.initS3d
    s3d_fi: usize,
    s3d_head: usize,
    s3d_at: usize,
    s3d_get: usize,
    s3d_len: Reg,
    s3d_slot: Reg,
    units_f: RefField,
    units_t: RefType,
    arr_f: RefField,
    arr_t: RefType,
    len_f: RefField,
    dyn_t: RefType,
    // LobbyState.initAssignments
    as_fi: usize,
    as_init: usize,
    as_at: usize,
    as_troop: Reg,
    as_unit: Reg,
    as_p: Reg,
    get_user: RefFun,
    user_t: RefType,
    user_id_f: RefField,
    // shared
    unit_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    is_animal: RefFun,
}

fn fun_name(code: &Bytecode, findex: RefFun) -> Option<&str> {
    code.functions
        .iter()
        .find(|f| f.findex == findex)
        .map(|f| s(code, f.name))
}

/// The string a `GetGlobal` of a constant String global loads.
fn global_string(code: &Bytecode, g: hlbc::types::RefGlobal) -> Option<&str> {
    let c = code.constants.as_ref()?.iter().find(|c| c.global == g)?;
    code.strings.get(*c.fields.first()?).map(|s| s.as_str())
}

/// True when an op of `f` with index below `from_before` jumps to `target`.
fn jumped_to(f: &Function, target: usize, from_before: usize) -> bool {
    (0..from_before.min(f.ops.len())).any(|i| jump_targets(f, i).contains(&target))
}

/// The number of slots: the CustomizeScreen constructor builds the gamepad order
/// `characterIndex` with `allocI32(bytes, N)`, one entry per scene slot.
fn slot_count(code: &Bytecode, cs_t: RefType) -> Result<i32> {
    let ctor = method(code, cs_t, "__constructor__")?;
    let (ci_f, _) = field(code, cs_t, "characterIndex")?;
    let ops = &ctor.ops;
    let sites: Vec<i32> = (2..ops.len())
        .filter_map(|i| match (&ops[i - 2], &ops[i - 1], &ops[i]) {
            (
                Opcode::Int { dst: n, ptr },
                Opcode::Call2 { dst: arr, arg1, .. },
                Opcode::SetThis { field, src },
            ) if field == &ci_f && src == arr && arg1 == n => Some(code.ints[ptr.0]),
            _ => None,
        })
        .collect();
    let [n] = sites[..] else {
        bail!(
            "ctor: expected one characterIndex allocation, found {}",
            sites.len()
        );
    };
    if !(1..=9).contains(&n) {
        bail!("ctor: characterIndex length {n} is not a single-digit slot count");
    }
    Ok(n)
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let cs_t = obj_type(code, "ui.win.CustomizeScreen")?;
    let lobby_t = obj_type(code, "LobbyState")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let is_animal_f = method(code, unit_t, "get_isAnimal")?;
    let is_animal = is_animal_f.findex;
    let bool_t = is_animal_f.t.as_fun(code).context("get_isAnimal type")?.ret;
    if !matches!(code.types[bool_t.0], Type::Bool) {
        bail!("get_isAnimal does not return bool");
    }
    let (units_f, units_t) = field(code, cs_t, "units")?;
    let (arr_f, arr_t) = field(code, units_t, "array")?;
    let (len_f, i32_t) = field(code, units_t, "length")?;
    if !matches!(code.types[i32_t.0], Type::I32) {
        bail!("ArrayObj.length is not i32");
    }
    let slots = slot_count(code, cs_t)?;

    // ---- CustomizeScreen.initS3d: `for (i in 0...units.length) { ... Camera0(i+1) ... units[i] ... }`
    let s3d = method(code, cs_t, "initS3d")?;
    let s3d_fi = fun_index(code, s3d.findex)?;
    let ops = &s3d.ops;
    let loads = |name: &str| -> Vec<usize> {
        (0..ops.len())
            .filter(|&i| matches!(ops[i], Opcode::GetGlobal { global, .. } if global_string(code, global) == Some(name)))
            .collect()
    };
    let [cam] = loads("Camera0")[..] else {
        bail!("initS3d: expected one \"Camera0\" load");
    };
    let [model] = loads("Model0")[..] else {
        bail!("initS3d: expected one \"Model0\" load");
    };
    if model > cam {
        bail!("initS3d: \"Model0\" is not looked up before the camera");
    }
    // The loop head: `GetThis units; NullCheck; Field len = units.length; Label; JSGte idx >= len; Mov slot = idx; Incr idx`.
    let heads: Vec<usize> = (3..model)
        .filter(|&i| {
            matches!(
                (&ops[i - 3], &ops[i - 1], &ops[i], &ops[i + 1], &ops[i + 2], &ops[i + 3]),
                (
                    Opcode::GetThis { field: uf, dst: u },
                    Opcode::Field { obj, field: lf, dst: l },
                    Opcode::Label,
                    Opcode::JSGte { a: idx, b, .. },
                    Opcode::Mov { src, .. },
                    Opcode::Incr { dst: inc },
                ) if *uf == units_f && obj == u && *lf == len_f && b == l && src == idx && inc == idx
            )
        })
        .collect();
    let [head] = heads[..] else {
        bail!(
            "initS3d: expected one `for (i in 0...units.length)` loop head, found {}",
            heads.len()
        );
    };
    let Opcode::Field { dst: s3d_len, .. } = ops[head - 1] else {
        unreachable!()
    };
    let Opcode::Mov { dst: s3d_slot, .. } = ops[head + 2] else {
        unreachable!()
    };
    let s3d_at = head + 4;
    if !matches!(ops[s3d_at], Opcode::Call1 { .. }) {
        bail!("initS3d: loop body does not start with the get_s3d call");
    }
    // The name suffix is `slot + 1` for both lookups.
    let adds = (head..cam + 3)
        .filter(|&i| matches!(ops[i], Opcode::Add { a, .. } if a == s3d_slot))
        .count();
    if adds != 2 {
        bail!("initS3d: expected Model0/Camera0 suffixes `slot + 1`, found {adds}");
    }
    // `units[slot]` after the camera: `Field raw = units.array; GetArray d = raw[slot]; UnsafeCast u = d`.
    let gets: Vec<usize> = (cam + 2..ops.len() - 1)
        .filter(|&i| {
            matches!(
                (&ops[i - 1], &ops[i], &ops[i + 1]),
                (
                    Opcode::Field { field: af, dst: raw, .. },
                    Opcode::GetArray { array, index, dst: d },
                    Opcode::UnsafeCast { src, dst: u },
                ) if *af == arr_f && array == raw && *index == s3d_slot && src == d && s3d.regs[u.0 as usize] == unit_t
            )
        })
        .collect();
    let [s3d_get] = gets[..] else {
        bail!(
            "initS3d: expected one `units[slot]` read, found {}",
            gets.len()
        );
    };
    let Some(back) = (0..ops.len())
        .rev()
        .find(|&i| matches!(ops[i], Opcode::JAlways { .. }))
    else {
        bail!("initS3d: no loop back-edge");
    };
    if jump_targets(s3d, back) != vec![head] || s3d_get > back {
        bail!("initS3d: loop back-edge does not return to the head");
    }
    if jumped_to(s3d, head, head) || jumped_to(s3d, s3d_at, ops.len()) {
        bail!("initS3d: unexpected jump into the loop head or body start");
    }
    let Opcode::GetArray { dst: d, .. } = ops[s3d_get] else {
        unreachable!()
    };
    let dyn_t = s3d.regs[d.0 as usize];
    if s3d.regs[s3d_slot.0 as usize] != i32_t || s3d.regs[s3d_len.0 as usize] != i32_t {
        bail!("initS3d: slot/length registers are not i32");
    }

    // ---- LobbyState.initAssignments: `for (u in createTroop(..)) { ...; units.push(new Proxy(null, null /*p*/, null, u)) }`
    let asg = method(code, lobby_t, "initAssignments")?;
    let as_fi = fun_index(code, asg.findex)?;
    let ops = &asg.ops;
    let troops: Vec<usize> = (0..ops.len())
        .filter(|&i| matches!(ops[i], Opcode::Call1 { fun, .. } if fun_name(code, fun) == Some("createTroop")))
        .collect();
    let [troop_call] = troops[..] else {
        bail!(
            "initAssignments: expected one createTroop call, found {}",
            troops.len()
        );
    };
    let Opcode::Call1 { dst: as_troop, .. } = ops[troop_call] else {
        unreachable!()
    };
    let as_init = troop_call + 1;
    let ctors: Vec<(usize, Reg, Reg)> = (as_init..ops.len())
        .filter_map(|i| match &ops[i] {
            Opcode::CallN { fun, args, .. } if args.len() == 5 => {
                let f = &code.functions[fun_index(code, *fun).ok()?];
                let a = fun_args(code, f);
                (s(code, f.name) == "__constructor__"
                    && a.len() == 5
                    && a[4] == unit_t
                    && matches!(&code.types[a[2].0], Type::Obj(o) if s(code, o.name) == "String"))
                .then_some((i, args[2], args[4]))
            }
            _ => None,
        })
        .collect();
    let [(ctor_at, as_p, as_unit)] = ctors[..] else {
        bail!(
            "initAssignments: expected one unit proxy constructor, found {}",
            ctors.len()
        );
    };
    let nulls: Vec<usize> = (as_init..ctor_at)
        .filter(|&i| matches!(ops[i], Opcode::Null { dst } if dst == as_p))
        .collect();
    let [p_null] = nulls[..] else {
        bail!(
            "initAssignments: expected one `p = null` before the proxy constructor, found {}",
            nulls.len()
        );
    };
    let as_at = p_null + 1;
    if (as_at..ctor_at).any(|i| match &ops[i] {
        Opcode::Mov { dst, .. } | Opcode::Null { dst } | Opcode::Field { dst, .. } => {
            *dst == as_p || *dst == as_unit
        }
        _ => false,
    }) {
        bail!("initAssignments: p or unit is rewritten between `p = null` and the constructor");
    }
    if jumped_to(asg, as_init, ops.len()) || jumped_to(asg, as_at, ops.len()) {
        bail!("initAssignments: unexpected jump into an insertion point");
    }
    if asg.regs[as_unit.0 as usize] != unit_t || asg.regs[as_troop.0 as usize] != units_t {
        bail!("initAssignments: unit/troop register types differ");
    }
    // `getUser().id`, as hasAssigned uses it for "me".
    let has = method(code, lobby_t, "hasAssigned")?;
    let users: Vec<(RefFun, RefField, RefType)> = (1..has.ops.len())
        .filter_map(|i| match (&has.ops[i - 1], &has.ops[i]) {
            (Opcode::Call0 { dst, fun }, Opcode::NullCheck { reg }) if dst == reg => {
                let t = has.regs[dst.0 as usize];
                let (id_f, id_t) = field(code, t, "id").ok()?;
                (id_t == asg.regs[as_p.0 as usize]).then_some((*fun, id_f, t))
            }
            _ => None,
        })
        .collect();
    let [(get_user, user_id_f, user_t)] = users[..] else {
        bail!(
            "hasAssigned: expected one `getUser().id`, found {}",
            users.len()
        );
    };
    if fun_name(code, get_user) != Some("getUser") {
        bail!("hasAssigned: the user lookup is not getUser");
    }

    Ok(Plan {
        slots,
        s3d_fi,
        s3d_head: head,
        s3d_at,
        s3d_get,
        s3d_len,
        s3d_slot,
        units_f,
        units_t,
        arr_f,
        arr_t,
        len_f,
        dyn_t,
        as_fi,
        as_init,
        as_at,
        as_troop,
        as_unit,
        as_p,
        get_user,
        user_t,
        user_id_f,
        unit_t,
        i32_t,
        bool_t,
        is_animal,
    })
}

/// Sets the offset of jump `ops[i]` so it lands on block index `target`
/// (block op j sits at `at + j`; targets before the block are negative).
fn set_jump(ops: &mut [Opcode], i: usize, target: i64) {
    let off = (target - i as i64 - 1) as i32;
    match &mut ops[i] {
        Opcode::JNull { offset, .. }
        | Opcode::JSGte { offset, .. }
        | Opcode::JULt { offset, .. }
        | Opcode::JFalse { offset, .. }
        | Opcode::JAlways { offset } => *offset = off,
        _ => unreachable!("not a jump"),
    }
}

fn apply(code: &mut Bytecode, p: Plan) {
    let slots_c = int_const(code, p.slots);
    let zero_c = int_const(code, 0);

    // ---- initS3d
    let f = &mut code.functions[p.s3d_fi];
    let k = new_reg(f, p.i32_t);
    let shown = new_reg(f, p.i32_t);
    let hid = new_reg(f, p.i32_t);
    let excess = new_reg(f, p.i32_t);
    let idx = new_reg(f, p.i32_t);
    let arr = new_reg(f, p.units_t);
    let len = new_reg(f, p.i32_t);
    let raw = new_reg(f, p.arr_t);
    let dynv = new_reg(f, p.dyn_t);
    let unit = new_reg(f, p.unit_t);
    let anim = new_reg(f, p.bool_t);
    let mut body = vec![
        Opcode::Mov {
            dst: idx,
            src: p.s3d_slot,
        },
        Opcode::JSGte {
            a: shown,
            b: k,
            offset: 0,
        }, // 1 -> HIDE
        Opcode::GetThis {
            dst: arr,
            field: p.units_f,
        },
        Opcode::JNull {
            reg: arr,
            offset: 0,
        }, // 3 -> SHOW
        Opcode::Field {
            dst: len,
            obj: arr,
            field: p.len_f,
        },
        Opcode::JULt {
            a: idx,
            b: len,
            offset: 0,
        }, // 5 -> 7
        Opcode::JAlways { offset: 0 }, // 6 -> SHOW
        Opcode::Field {
            dst: raw,
            obj: arr,
            field: p.arr_f,
        },
        Opcode::GetArray {
            dst: dynv,
            array: raw,
            index: idx,
        },
        Opcode::UnsafeCast {
            dst: unit,
            src: dynv,
        },
        Opcode::JNull {
            reg: unit,
            offset: 0,
        }, // 10 -> SHOW
        Opcode::Call1 {
            dst: anim,
            fun: p.is_animal,
            arg0: unit,
        },
        Opcode::JFalse {
            cond: anim,
            offset: 0,
        }, // 12 -> SHOW
        Opcode::JSGte {
            a: hid,
            b: excess,
            offset: 0,
        }, // 13 -> SHOW
        Opcode::Incr { dst: hid },
        Opcode::JAlways { offset: 0 }, // 15 HIDE -> loop head
        Opcode::Mov {
            dst: p.s3d_slot,
            src: shown,
        }, // 16 SHOW
        Opcode::Incr { dst: shown },
    ];
    let n = body.len();
    let head = p.s3d_head as i64 - p.s3d_at as i64; // before the block
    for (i, t) in [
        (1, 15),
        (3, 16),
        (5, 7),
        (6, 16),
        (10, 16),
        (12, 16),
        (13, 16),
        (15, head),
    ] {
        set_jump(&mut body, i, t);
    }
    // Later edits first so earlier indices stay valid.
    if let Opcode::GetArray { index, .. } = &mut f.ops[p.s3d_get] {
        *index = idx;
    }
    insert_ops(f, p.s3d_at, body);
    let pre = vec![
        Opcode::Int {
            dst: k,
            ptr: slots_c,
        },
        Opcode::Int {
            dst: shown,
            ptr: zero_c,
        },
        Opcode::Int {
            dst: hid,
            ptr: zero_c,
        },
        Opcode::Sub {
            dst: excess,
            a: p.s3d_len,
            b: k,
        },
    ];
    let pre_n = pre.len();
    // Inserted before the loop Label: the back-edge still lands on the Label.
    insert_ops(f, p.s3d_head, pre);
    eprintln!(
        "patched customize-slots initS3d fn@{}: {} slots, {pre_n} ops before the loop at {}, {n} ops at {} (hide extra animals), units[] index reg{} at op {}",
        f.findex.0,
        p.slots,
        p.s3d_head,
        p.s3d_at + pre_n,
        idx.0,
        p.s3d_get + pre_n + n
    );

    // ---- initAssignments
    let f = &mut code.functions[p.as_fi];
    let k = new_reg(f, p.i32_t);
    let shown = new_reg(f, p.i32_t);
    let hid = new_reg(f, p.i32_t);
    let excess = new_reg(f, p.i32_t);
    let len = new_reg(f, p.i32_t);
    let anim = new_reg(f, p.bool_t);
    let user = new_reg(f, p.user_t);
    let mut body = vec![
        Opcode::JSGte {
            a: shown,
            b: k,
            offset: 0,
        }, // 0 -> HIDE
        Opcode::JNull {
            reg: p.as_unit,
            offset: 0,
        }, // 1 -> SHOW
        Opcode::Call1 {
            dst: anim,
            fun: p.is_animal,
            arg0: p.as_unit,
        },
        Opcode::JFalse {
            cond: anim,
            offset: 0,
        }, // 3 -> SHOW
        Opcode::JSGte {
            a: hid,
            b: excess,
            offset: 0,
        }, // 4 -> SHOW
        Opcode::Incr { dst: hid },
        Opcode::JAlways { offset: 0 }, // 6 -> HIDE
        Opcode::Incr { dst: shown },   // 7 SHOW
        Opcode::JAlways { offset: 0 }, // 8 -> END
        Opcode::Call0 {
            dst: user,
            fun: p.get_user,
        }, // 9 HIDE
        Opcode::JNull {
            reg: user,
            offset: 0,
        }, // 10 -> END
        Opcode::Field {
            dst: p.as_p,
            obj: user,
            field: p.user_id_f,
        },
    ];
    let bn = body.len() as i64; // END
    for (i, t) in [(0, 9), (1, 7), (3, 7), (4, 7), (6, 9), (8, bn), (10, bn)] {
        set_jump(&mut body, i, t);
    }
    insert_ops(f, p.as_at, body);
    let mut pre = vec![
        Opcode::Int {
            dst: k,
            ptr: slots_c,
        },
        Opcode::Int {
            dst: shown,
            ptr: zero_c,
        },
        Opcode::Int {
            dst: hid,
            ptr: zero_c,
        },
        Opcode::Mov {
            dst: excess,
            src: hid,
        },
        Opcode::JNull {
            reg: p.as_troop,
            offset: 0,
        }, // 4 -> END
        Opcode::Field {
            dst: len,
            obj: p.as_troop,
            field: p.len_f,
        },
        Opcode::Sub {
            dst: excess,
            a: len,
            b: k,
        },
    ];
    let pn = pre.len() as i64;
    set_jump(&mut pre, 4, pn);
    insert_ops(f, p.as_init, pre);
    eprintln!(
        "patched customize-slots initAssignments fn@{}: {pn} ops at {}, {bn} ops at {} (hidden unit reg{} -> p reg{} = getUser().id)",
        f.findex.0,
        p.as_init,
        p.as_at + pn as usize,
        p.as_unit.0,
        p.as_p.0
    );
}

/// Applies the customize-screen slot fix, or leaves `code` untouched and logs why.
pub(crate) fn patch_customize_slots(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("customize-slots fix skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    fn ops(o: &[Opcode]) -> String {
        format!("{o:?}")
    }

    /// Every original op `i` (except `skip`) sits at `m(i)` in `p`, unchanged,
    /// with jumps landing on the mapped targets.
    fn same_mapped(o: &Function, p: &Function, m: impl Fn(usize) -> usize, skip: &[usize]) {
        for i in 0..o.ops.len() {
            if skip.contains(&i) {
                continue;
            }
            let t = jump_targets(o, i);
            if t.is_empty() {
                assert_eq!(ops(&o.ops[i..=i]), ops(&p.ops[m(i)..=m(i)]), "op {i}");
            } else {
                assert_eq!(
                    std::mem::discriminant(&o.ops[i]),
                    std::mem::discriminant(&p.ops[m(i)]),
                    "op {i}"
                );
                let mapped: Vec<usize> = t.into_iter().map(&m).collect();
                assert_eq!(jump_targets(p, m(i)), mapped, "op {i} target");
            }
        }
    }

    /// Patches a copy of the installed game's bytecode (skipped when absent):
    /// only the two target functions change, the image round-trips, the jumps
    /// land where intended, and a second pass is a no-op.
    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let mut out = Vec::new();
        orig.serialize(&mut out).expect("write");
        assert!(out == image, "unpatched round-trip is not byte-identical");

        let p = plan(&orig).expect("plan");
        assert_eq!(p.slots, 5);
        let (sfi, afi) = (p.s3d_fi, p.as_fi);
        let (head, at, get) = (p.s3d_head, p.s3d_at, p.s3d_get);
        let (init, aat) = (p.as_init, p.as_at);
        let mut code = read(&image);
        patch_customize_slots(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write patched");
        if let Ok(path) = std::env::var("CUSTOMIZE_SLOTS_OUT") {
            std::fs::write(path, &patched).expect("save patched copy");
        }
        let back = read(&patched);

        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(back.types, orig.types);
        assert_eq!(back.strings, orig.strings);
        assert_eq!(&back.ints[..orig.ints.len()], &orig.ints[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same =
                ops(&a.ops) == ops(&b.ops) && a.regs == b.regs && a.debug_info == b.debug_info;
            assert_eq!(
                same,
                i != sfi && i != afi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // initS3d: 4 ops before the loop Label, 18 at the body start, units[] by the real index.
        let o = &orig.functions[sfi];
        let s = &back.functions[sfi];
        assert_eq!(s.ops.len(), o.ops.len() + 22);
        assert_eq!(s.regs.len(), o.regs.len() + 11);
        let m = |i: usize| i + if i >= head { 4 } else { 0 } + if i >= at { 18 } else { 0 };
        same_mapped(o, s, m, &[get]);
        assert!(matches!(s.ops[head + 4], Opcode::Label));
        assert!(
            matches!(s.ops[m(get)], Opcode::GetArray { index, .. } if index.0 as usize == o.regs.len() + 4)
        );
        // HIDE returns to the Label, like the loop's own back-edge.
        assert_eq!(jump_targets(s, at + 4 + 15), vec![head + 4]);
        assert_eq!(jump_targets(s, at + 4 + 1), vec![at + 4 + 15]);
        for i in 0..s.ops.len() {
            for t in jump_targets(s, i) {
                assert!(t < s.ops.len(), "initS3d op {i} jumps out of range");
                assert!(
                    !(head..head + 4).contains(&t),
                    "initS3d op {i} jumps into the pre-loop init"
                );
            }
        }

        // initAssignments: 7 ops after createTroop, 12 after `p = null`.
        let o = &orig.functions[afi];
        let a = &back.functions[afi];
        assert_eq!(a.ops.len(), o.ops.len() + 19);
        assert_eq!(a.regs.len(), o.regs.len() + 7);
        let m = |i: usize| i + if i >= init { 7 } else { 0 } + if i >= aat { 12 } else { 0 };
        same_mapped(o, a, m, &[]);
        for i in 0..a.ops.len() {
            for t in jump_targets(a, i) {
                assert!(t < a.ops.len(), "initAssignments op {i} jumps out of range");
            }
        }
        assert_eq!(jump_targets(a, init + 4), vec![init + 7]);
        assert_eq!(jump_targets(a, aat + 7 + 10), vec![aat + 7 + 12]);

        // A second pass finds nothing to patch and leaves the image alone.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_customize_slots(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
