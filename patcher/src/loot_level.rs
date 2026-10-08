// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Enemies of a higher level can drop their worn equipment.
//
// `battle.Debrief.genLoot` picks the worn-gear drop from each dead enemy's
// active slots (weapon, secondaryWeapon, left, armor, helmet, trinket0..3)
// through one closure per slot (Debrief.hx:521-531, nine identical copies)
// that keeps an item only if:
//
//   if (!hasFeatureConst(item.props.feature) || item.props.disableLoot
//       || item.props.requireLevel != null
//          && state.getReferenceLevel(1.0) < item.props.requireLevel) return;
//   if (!item.isType(Armor)) { candidates.push(item); return; }
//   for (u in state.getUnits()) if (u.isPlayer() && u.canEquip(item)) { candidates.push(item); return; }
//
// `getReferenceLevel(1.0)` is the level of the highest unit of the army, and
// `Unit.canEquip` is `level >= item.requireLevel && !hasCantEquipReasons(item)`.
// So gear of a level above the squad never reaches the candidates. Two edits
// per closure, same op count, no new registers:
//
//   - the `JSGte refLevel >= requireLevel` becomes a `JAlways` to the same
//     target (the level is no reason to skip any more);
//   - for armor, `canEquip(item)` + `JFalse` becomes `hasCantEquipReasons(item)`
//     + `JTrue`: still "one of your units can wear it" (armor / helmet kind,
//     animal, item flag checks), without the unit's level.
//
// Everything else stays vanilla: feature / disableLoot, NoEquipDrop,
// ForceDropWeapon, drop chance and pity counter, quality, repair. The loot is
// generated on the host (the shared battle.Debrief.loot pool), so in co-op
// only the host's build matters; clients see the pool as usual.
//
// Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Site {
    fi: usize,
    /// The `JSGte refLevel >= requireLevel` after `getReferenceLevel`.
    level_at: usize,
    /// The `Call2 canEquip(unit, item)` (a `JFalse` on its result follows).
    equip_at: usize,
}

struct Plan {
    sites: Vec<Site>,
    cant_equip: RefFun,
}

fn site(code: &Bytecode, fi: usize, ref_level: RefFun, can_equip: RefFun) -> Result<Site> {
    let f = &code.functions[fi];
    let ops = &f.ops;
    let at = |want: RefFun| -> Result<usize> {
        let v: Vec<usize> = (0..ops.len())
            .filter(|&i| matches!(ops[i], Opcode::Call2 { fun, .. } if fun == want))
            .collect();
        match v[..] {
            [i] => Ok(i),
            _ => bail!("fn@{}: expected one call to fn@{}", f.findex.0, want.0),
        }
    };
    let r = at(ref_level)?;
    let Opcode::Call2 { dst: lvl, .. } = ops[r] else { unreachable!() };
    let level_at = r + 2;
    if !matches!(
        (ops.get(r + 1), ops.get(level_at)),
        (Some(Opcode::SafeCast { .. }), Some(Opcode::JSGte { a, .. })) if *a == lvl
    ) {
        bail!("fn@{}: no level compare after getReferenceLevel", f.findex.0);
    }
    let equip_at = at(can_equip)?;
    let Opcode::Call2 { dst, .. } = ops[equip_at] else { unreachable!() };
    if !matches!(ops.get(equip_at + 1), Some(Opcode::JFalse { cond, .. }) if *cond == dst) {
        bail!("fn@{}: canEquip result is not a JFalse", f.findex.0);
    }
    Ok(Site {
        fi,
        level_at,
        equip_at,
    })
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let unit_t = obj_type(code, "st.Unit")?;
    let can_equip = method(code, unit_t, "canEquip")?.findex;
    let cant_equip = method(code, unit_t, "hasCantEquipReasons")?.findex;
    if sig(code, can_equip)? != sig(code, cant_equip)? {
        bail!("canEquip / hasCantEquipReasons signatures differ");
    }
    let ref_level = method(code, obj_type(code, "st.GameState")?, "getReferenceLevel")?.findex;
    let debrief = debug_file(code, "src/battle/Debrief.hx")?;

    let in_debrief = |f: &Function| {
        matches!(f.debug_info.as_deref(), Some([(file, _), ..]) if *file == debrief)
            && calls(f, ref_level)
    };
    if code
        .functions
        .iter()
        .any(|f| in_debrief(f) && calls(f, cant_equip))
    {
        bail!("already applied");
    }
    let sites = (0..code.functions.len())
        .filter(|&i| in_debrief(&code.functions[i]) && calls(&code.functions[i], can_equip))
        .map(|i| site(code, i, ref_level, can_equip))
        .collect::<Result<Vec<_>>>()?;
    // One per worn slot: patching only some would leave a level filter behind.
    if sites.len() != 9 {
        bail!("expected 9 loot candidate closures, found {}", sites.len());
    }
    Ok(Plan { sites, cant_equip })
}

fn apply(code: &mut Bytecode, p: Plan) {
    for s in &p.sites {
        let f = &mut code.functions[s.fi];
        let Opcode::JSGte { offset, .. } = f.ops[s.level_at] else { unreachable!() };
        f.ops[s.level_at] = Opcode::JAlways { offset };
        let Opcode::Call2 { dst, arg0, arg1, .. } = f.ops[s.equip_at] else { unreachable!() };
        f.ops[s.equip_at] = Opcode::Call2 {
            dst,
            fun: p.cant_equip,
            arg0,
            arg1,
        };
        let Opcode::JFalse { cond, offset } = f.ops[s.equip_at + 1] else { unreachable!() };
        f.ops[s.equip_at + 1] = Opcode::JTrue { cond, offset };
    }
    let first = code.functions[p.sites[0].fi].findex.0;
    eprintln!(
        "patched loot level fn@{first} (+{} more): enemies of a higher level drop their worn gear",
        p.sites.len() - 1
    );
}

/// Lets enemy gear above the squad's level drop, or leaves `code` untouched and logs why.
pub(crate) fn patch_loot_level(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => crate::skipped(format!("loot level skipped: {e:#}")),
    }
}

// Armor drops even when none of your units' classes can wear it.
//
// In each candidate closure an armor item takes a separate branch (the
// `for (u in units) if (u.isPlayer() && canEquip)` loop above):
//
//   Call2 b = item.isType(Armor); JTrue b -> armor branch; push(item); Ret
//
// The `JTrue` becomes a `Nop`, so armor is pushed like weapons and trinkets.
// disableLoot / feature, NoEquipDrop, ForceDropWeapon and the chances are
// unchanged. Gear nobody in the squad can wear is still an ordinary item:
// stored, sold, dismantled, or worn by a later recruit.

/// Index of the armor-branch `JTrue` in each of the nine candidate closures.
fn armor_sites(code: &Bytecode) -> Result<Vec<(usize, usize)>> {
    let unit_t = obj_type(code, "st.Unit")?;
    let can_equip = method(code, unit_t, "canEquip")?.findex;
    let cant_equip = method(code, unit_t, "hasCantEquipReasons")?.findex;
    let ref_level = method(code, obj_type(code, "st.GameState")?, "getReferenceLevel")?.findex;
    let debrief = debug_file(code, "src/battle/Debrief.hx")?;
    let equips = |o: &Opcode| {
        matches!(o, Opcode::Call2 { fun, .. } if *fun == can_equip || *fun == cant_equip)
    };
    let mut sites = Vec::new();
    for (fi, f) in code.functions.iter().enumerate() {
        if !matches!(f.debug_info.as_deref(), Some([(file, _), ..]) if *file == debrief)
            || !calls(f, ref_level)
            || !f.ops.iter().any(equips)
        {
            continue;
        }
        let ops = &f.ops;
        let is_type = |j: usize| {
            matches!(ops[j - 1], Opcode::Call2 { fun, .. } if fname(code, fun) == "isType")
        };
        let found: Vec<usize> = (1..ops.len())
            .filter(|&j| match ops[j] {
                Opcode::JTrue { cond, offset } => {
                    let t = (j as i32 + 1 + offset) as usize;
                    is_type(j)
                        && matches!(ops[j - 1], Opcode::Call2 { dst, .. } if dst == cond)
                        && ops[t..(t + 40).min(ops.len())].iter().any(equips)
                }
                _ => false,
            })
            .collect();
        match found[..] {
            [j] => sites.push((fi, j)),
            [] if (1..ops.len()).any(|j| is_type(j) && matches!(ops[j], Opcode::Nop)) => {
                bail!("already applied")
            }
            _ => bail!("fn@{}: expected one armor branch, found {}", f.findex.0, found.len()),
        }
    }
    if sites.len() != 9 {
        bail!("expected 9 loot candidate closures, found {}", sites.len());
    }
    Ok(sites)
}

/// Lets armor drop whatever classes the squad has, or leaves `code` untouched and logs why.
pub(crate) fn patch_armor_any(code: &mut Bytecode) {
    match armor_sites(code) {
        Ok(sites) => {
            for &(fi, j) in &sites {
                code.functions[fi].ops[j] = Opcode::Nop;
            }
            eprintln!("patched armor drop x{}: armor drops whatever the squad can wear", sites.len());
        }
        Err(e) => crate::skipped(format!("armor drop skipped: {e:#}")),
    }
}

// Champions and bosses can drop their worn gear too.
//
// genLoot skips the whole worn-gear roll for a class with the NoEquipDrop flag
// (Debrief.hx:518):
//
//   Int a = 1; Int b = 4; Shl a = a << b; And x = x & a; Int a = 0; JNotEq x != a -> skip
//
// cdb unitClass flags: IsChampion bit 3, NoEquipDrop bit 4, ArenaChampion
// bit 19. The NoEquipDrop classes are the named champions and bosses
// (humanoids with legendary gear), the arena champions, and creatures:
// ghosts, The Beast, sea snake, rats / molerats / rat nests and Remastered's
// workmen. The roll now runs for a NoEquipDrop class that is a champion but
// not an arena champion. With x = flags & (bit3 | bit4 | bit19) the possible
// values (every ArenaChampion class is also IsChampion + NoEquipDrop, vanilla
// and Remastered cdb) map through `x ^ 8` as:
//
//   0 (plain) -> 8, 8 (champion) -> 0, 24 (boss) -> 16: roll;
//   16 (creature) -> 24, arena champion -> 16 + bit19:     skip.
//
// So the same six ops become
//
//   Int a = MASK; And x = x & a; Int a = 8; Xor x = x ^ a; Int a = 16; JSGt x > a -> skip
//
// Arena champions keep the flag: vanilla arena masters give their gear as
// rewards (A1Arena, E1Arena, IR1Arena prefab dialog gains), so dropping it
// would duplicate a reward. The IsChampion creatures (H2TheBeast, Ecila,
// Erymanthe, CrawlerChampion) have no class equipment and roll for nothing,
// as ordinary animals already do. A boss's unique items are kept out by the
// item data the candidate closure reads (`disableLoot`). ForceDropWeapon, the
// drop chance and the pity counter stay vanilla (see the loot order below for
// the forced weapon and the guaranteed drop).

const CHAMPION_MASK: i32 = (1 << 3) | (1 << 4) | (1 << 19);

/// Function index and index of the `Int a = 1` that starts the NoEquipDrop test in genLoot.
fn champion_site(code: &Bytecode) -> Result<(usize, usize)> {
    let gen = method(code, obj_type(code, "battle.Debrief")?, "genLoot")?;
    let fi = fun_index(code, gen.findex)?;
    let int = |o: &Opcode| match o {
        Opcode::Int { dst, ptr } => Some((*dst, code.ints[ptr.0])),
        _ => None,
    };
    let ops = &gen.ops;
    let mut found = Vec::new();
    for k in 0..ops.len().saturating_sub(5) {
        let (Some((a, 1)), Some((b, 4))) = (int(&ops[k]), int(&ops[k + 1])) else {
            continue;
        };
        let Opcode::Shl { dst, a: sa, b: sb } = ops[k + 2] else { continue };
        let Opcode::And { dst: x, a: xa, b: xb } = ops[k + 3] else { continue };
        if (dst, sa, sb) != (a, a, b) || xa != x || xb != a {
            continue;
        }
        if int(&ops[k + 4]) == Some((a, 0))
            && matches!(ops[k + 5], Opcode::JNotEq { a: ja, b: jb, .. } if ja == x && jb == a)
        {
            found.push(k);
        }
    }
    match found[..] {
        [k] => Ok((fi, k)),
        [] if ops.iter().any(|o| int(o).is_some_and(|(_, v)| v == CHAMPION_MASK)) => {
            bail!("already applied")
        }
        _ => bail!("genLoot: expected one NoEquipDrop test, found {}", found.len()),
    }
}

/// Lets champions / bosses (not arena champions) drop worn gear, or leaves `code` untouched and logs why.
pub(crate) fn patch_champion_gear(code: &mut Bytecode) {
    let (fi, k) = match champion_site(code) {
        Ok(s) => s,
        Err(e) => return crate::skipped(format!("champion gear skipped: {e:#}")),
    };
    let mask = int_const(code, CHAMPION_MASK);
    let eight = int_const(code, 8);
    let sixteen = int_const(code, 16);
    let f = &mut code.functions[fi];
    let Opcode::Int { dst: a, .. } = f.ops[k] else { unreachable!() };
    let Opcode::And { dst: x, .. } = f.ops[k + 3] else { unreachable!() };
    let Opcode::JNotEq { offset, .. } = f.ops[k + 5] else { unreachable!() };
    f.ops.splice(k..k + 6, [
        Opcode::Int { dst: a, ptr: mask },
        Opcode::And { dst: x, a: x, b: a },
        Opcode::Int { dst: a, ptr: eight },
        Opcode::Xor { dst: x, a: x, b: a },
        Opcode::Int { dst: a, ptr: sixteen },
        Opcode::JSGt { a: x, b: a, offset },
    ]);
    eprintln!(
        "patched champion gear fn@{}: champions and bosses can drop worn gear",
        f.findex.0
    );
}

// The battle's guaranteed worn-gear drop goes to the strongest enemy.
//
// The worn-gear loop of genLoot (Debrief.hx:500-544) walks
// `state.allUnits.array` in array order, and the first dead enemy with an
// eligible item takes the guaranteed drop (`firstEquip`). The loop now walks a
// copy of that array sorted strongest first:
//
//   units = units.copy(); units.sort(lootCmp);   // inserted after the cast
//
//   lootCmp(a, b) = lootKey(b) - lootKey(a)
//   lootKey(u)    = u.data.level * 2 + (class flags & (IsChampion | IsBoss) != 0 ? 1 : 0)
//                   (-1 for a unit without data)
//
// haxe.ds.ArraySort is a stable merge sort, so equal keys keep array order.
//
// A ForceDropWeapon class (named bosses) drops its weapon and vanilla then
// clears `firstEquip`, so the forced weapon used up the guarantee whenever the
// boss came first. Sorted, the boss nearly always comes first, so that `Mov
// firstEquip = false` becomes a Nop: the forced weapon no longer counts as the
// guaranteed worn-gear drop, which goes to the strongest enemy with an
// eligible item (the boss itself or the next one); the others roll after it
// strongest first. The guaranteed drop is still at most one per battle.
//
// The rest of the loop body per dead enemy (its loot-table rolls, Outlaws
// gold) is unchanged; it runs in the new order, so the RNG calls interleave
// differently, but each enemy's table rolls are independent of the others,
// so their odds stay the same. state.allUnits itself is not reordered (the
// loop walks a copy).

const RANK_MASK: i32 = (1 << 3) | (1 << 23);

struct OrderPlan {
    fi: usize,
    /// Op after `SafeCast units = cast allUnits.array`.
    at: usize,
    units: Reg,
    copy: RefFun,
    sort: RefFun,
    cmp_t: RefType,
    dyn_t: RefType,
    i32_t: RefType,
    void_t: RefType,
    unit_t: RefType,
    data: (RefField, RefType),
    level: RefField,
    get_class: RefFun,
    class_t: RefType,
    props: (RefField, RefType),
    flags: (RefField, RefType),
    dbg: (usize, usize),
    /// The Mov firstEquip = false after a ForceDropWeapon drop.
    forced: usize,
}

fn order_plan(code: &Bytecode) -> Result<OrderPlan> {
    let gen = method(code, obj_type(code, "battle.Debrief")?, "genLoot")?;
    let fi = fun_index(code, gen.findex)?;
    let state_t = obj_type(code, "battle.State")?;
    let (all_units, _) = field(code, state_t, "allUnits")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let is_alive = method(code, unit_t, "isAlive")?.findex;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let copy = method(code, arr_t, "copy")?.findex;
    let sort = method(code, arr_t, "sort")?.findex;
    let (sort_args, _) = sig(code, sort)?;
    let [_, cmp_t] = sort_args[..] else { bail!("ArrayObj.sort: unexpected arguments") };
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    match &code.types[cmp_t.0] {
        Type::Fun(TypeFun { args, ret }) if args[..] == [dyn_t, dyn_t] && *ret == i32_t => {}
        _ => bail!("ArrayObj.sort: comparator is not (dynamic, dynamic) -> i32"),
    }
    if sig(code, copy)?.1 != arr_t {
        bail!("ArrayObj.copy does not return ArrayObj");
    }
    let su_t = obj_type(code, "st.Unit")?;
    let data = field(code, unit_t, "data")?;
    if data.1 != su_t {
        bail!("battle.Unit.data is not st.Unit");
    }
    let (level, level_t) = field(code, su_t, "level")?;
    if level_t != i32_t {
        bail!("st.Unit.level is not i32");
    }
    let gc = method(code, su_t, "getClass")?;
    let get_class = gc.findex;
    let class_t = sig(code, get_class)?.1;
    let props = field_of_virtual(code, class_t, "props")?;
    let flags = field_of_virtual(code, props.1, "flags")?;
    if !matches!(code.types[flags.1.0], Type::Null(t) if t == i32_t) {
        bail!("class props.flags is not null<i32>");
    }

    let ops = &gen.ops;
    let ty = |r: Reg| gen.regs[r.0 as usize];
    let mut found = Vec::new();
    for i in 0..ops.len().saturating_sub(8) {
        let Opcode::Field { dst: list, field: f, .. } = ops[i] else { continue };
        if f != all_units {
            continue;
        }
        let Opcode::Field { dst: raw, obj, .. } = ops[i + 5] else { continue };
        let Opcode::SafeCast { dst: units, src } = ops[i + 6] else { continue };
        if obj != list || src != raw || ty(units) != arr_t {
            continue;
        }
        if ops[i + 7..(i + 40).min(ops.len())]
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == is_alive))
        {
            found.push((i + 7, units));
        }
    }
    let [(at, units)] = found[..] else {
        bail!("genLoot: expected one worn-gear unit loop, found {}", found.len());
    };
    if matches!(ops[at], Opcode::Call1 { fun, .. } if fun == copy) {
        bail!("already applied");
    }
    // ForceDropWeapon: `loot.addItem(weapon); firstEquip = false;` in the loop.
    let forced: Vec<usize> = (at..(at + 400).min(ops.len() - 1))
        .filter(|&j| {
            let (Opcode::Bool { dst: t, value: hlbc::types::ValBool(false) }, Opcode::Mov { dst: fe, src }) =
                (&ops[j], &ops[j + 1])
            else {
                return false;
            };
            src == t
                && matches!(ops[j - 1], Opcode::Call4 { fun, .. } if fname(code, fun) == "addItem")
                && ops[j + 2..(j + 80).min(ops.len())]
                    .iter()
                    .any(|o| matches!(o, Opcode::JTrue { cond, .. } if cond == fe))
        })
        .collect();
    let [forced] = forced[..] else {
        bail!("genLoot: expected one ForceDropWeapon firstEquip reset, found {}", forced.len());
    };
    let dbg = gen.debug_info.as_ref().map_or((0, 0), |d| d[at]);
    Ok(OrderPlan {
        fi,
        at,
        units,
        copy,
        sort,
        cmp_t,
        dyn_t,
        i32_t,
        void_t,
        unit_t,
        data,
        level,
        get_class,
        class_t,
        props,
        flags,
        dbg,
        forced,
    })
}

fn order_apply(code: &mut Bytecode, p: OrderPlan) -> Result<()> {
    // lootKey(u: dynamic) -> i32
    let mut r = asm::Regs(vec![p.dyn_t]);
    let (u, d, k, c, pr, fl, t, m) = (
        r.r(p.unit_t),
        r.r(p.data.1),
        r.r(p.i32_t),
        r.r(p.class_t),
        r.r(p.props.1),
        r.r(p.flags.1),
        r.r(p.i32_t),
        r.r(p.i32_t),
    );
    let minus1 = int_const(code, -1);
    let one = int_const(code, 1);
    let mask = int_const(code, RANK_MASK);
    let zero = int_const(code, 0);
    let mut a = asm::Asm::new();
    a.op(Opcode::SafeCast { dst: u, src: Reg(0) });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "none");
    a.op(Opcode::Field { dst: d, obj: u, field: p.data.0 });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "none");
    // k = level * 2
    a.op(Opcode::Field { dst: k, obj: d, field: p.level });
    a.op(Opcode::Int { dst: m, ptr: one });
    a.op(Opcode::Shl { dst: k, a: k, b: m });
    // + 1 for a champion / boss class
    a.op(Opcode::Call1 { dst: c, fun: p.get_class, arg0: d });
    a.jmp(Opcode::JNull { reg: c, offset: 0 }, "ret");
    a.op(Opcode::Field { dst: pr, obj: c, field: p.props.0 });
    a.jmp(Opcode::JNull { reg: pr, offset: 0 }, "ret");
    a.op(Opcode::Field { dst: fl, obj: pr, field: p.flags.0 });
    a.jmp(Opcode::JNull { reg: fl, offset: 0 }, "ret");
    a.op(Opcode::SafeCast { dst: t, src: fl });
    a.op(Opcode::Int { dst: m, ptr: mask });
    a.op(Opcode::And { dst: t, a: t, b: m });
    a.op(Opcode::Int { dst: m, ptr: zero });
    a.jmp(Opcode::JEq { a: t, b: m, offset: 0 }, "ret");
    a.op(Opcode::Incr { dst: k });
    a.label("ret");
    a.op(Opcode::Ret { ret: k });
    a.label("none");
    a.op(Opcode::Int { dst: k, ptr: minus1 });
    a.op(Opcode::Ret { ret: k });
    let key_fn = asm::push_fn(code, vec![p.dyn_t], p.i32_t, r.0, a.finish(), p.dbg.0)?;

    // lootCmp(a: dynamic, b: dynamic) -> i32 = lootKey(b) - lootKey(a), typed as
    // ArrayObj.sort's comparator.
    let cmp_fn = next_findex(code)?;
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t: p.cmp_t,
        findex: cmp_fn,
        regs: vec![p.dyn_t, p.dyn_t, p.i32_t, p.i32_t],
        ops: vec![
            Opcode::Call1 { dst: Reg(2), fun: key_fn, arg0: Reg(0) },
            Opcode::Call1 { dst: Reg(3), fun: key_fn, arg0: Reg(1) },
            Opcode::Sub { dst: Reg(2), a: Reg(3), b: Reg(2) },
            Opcode::Ret { ret: Reg(2) },
        ],
        debug_info: Some(vec![p.dbg; 4]),
        assigns: Some(vec![]),
        parent: None,
    });

    let f = &mut code.functions[p.fi];
    f.ops[p.forced + 1] = Opcode::Nop;
    let rc = new_reg(f, p.cmp_t);
    let rv = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::Call1 { dst: p.units, fun: p.copy, arg0: p.units },
            Opcode::StaticClosure { dst: rc, fun: cmp_fn },
            Opcode::Call2 { dst: rv, fun: p.sort, arg0: p.units, arg1: rc },
        ],
    );
    eprintln!(
        "patched loot order fn@{}: the guaranteed worn-gear drop goes to the strongest enemy",
        f.findex.0
    );
    Ok(())
}

/// Walks genLoot's worn-gear loop strongest enemy first, or leaves `code` untouched and logs why.
pub(crate) fn patch_loot_order(code: &mut Bytecode) {
    let p = match order_plan(code) {
        Ok(p) => p,
        Err(e) => return crate::skipped(format!("loot order skipped: {e:#}")),
    };
    let snap = asm::Snap::take(code);
    if let Err(e) = order_apply(code, p) {
        snap.restore(code);
        crate::skipped(format!("loot order skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    #[test]
    fn armor_any_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let sites = armor_sites(&orig).expect("sites");
        // Also after the level pass (canEquip already swapped).
        let mut lvl = read(&image);
        patch_loot_level(&mut lvl);
        assert_eq!(armor_sites(&lvl).expect("sites after level"), sites);

        let mut code = read(&image);
        patch_armor_any(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let site = sites.iter().find(|s| s.0 == i);
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                let changed = format!("{x:?}") != format!("{y:?}");
                assert_eq!(changed, site.is_some_and(|s| s.1 == k), "fn #{i} op {k}");
            }
            assert_eq!(a.ops.len(), b.ops.len());
            if let Some(s) = site {
                assert!(matches!(a.ops[s.1], Opcode::JTrue { .. }));
                assert!(matches!(b.ops[s.1], Opcode::Nop));
            }
        }

        let mut again = read(&patched);
        assert!(armor_sites(&again).is_err());
        patch_armor_any(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    #[test]
    fn loot_order_installed_game() {
        use crate::asm::testutil::{check_flow, check_types, shifted};
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = order_plan(&orig).expect("plan");
        let (fi, at, copy, sort) = (p.fi, p.at, p.copy, p.sort);
        let mut code = read(&image);
        patch_loot_order(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        let n = orig.functions.len();
        assert_eq!(back.functions.len(), n + 2);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        // The forced-weapon Mov became a Nop (index past the 3 inserted ops).
        let mut b = b.clone();
        assert!(matches!(b.ops[p.forced + 1 + 3], Opcode::Nop));
        b.ops[p.forced + 1 + 3] = a.ops[p.forced + 1].clone();
        let b = &b;
        shifted(a, b, at, 3);
        check_flow(b);
        check_types(&back, b, at..at + 3);
        assert!(matches!(b.ops[at], Opcode::Call1 { fun, .. } if fun == copy));
        assert!(matches!(b.ops[at + 2], Opcode::Call2 { fun, .. } if fun == sort));
        let Opcode::StaticClosure { fun: cmp, .. } = b.ops[at + 1] else { panic!("closure") };
        for f in &back.functions[n..] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }
        let cmp_f = &back.functions[n + 1];
        assert_eq!(cmp_f.findex, cmp);

        let mut again = read(&patched);
        assert!(order_plan(&again).is_err());
        patch_loot_order(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    #[test]
    fn champion_gear_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let (fi, k) = champion_site(&orig).expect("site");
        let mut code = read(&image);
        patch_champion_gear(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[fi], &back.functions[fi]);
        assert_eq!(a.ops.len(), b.ops.len());
        assert_eq!(a.regs, b.regs);
        for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
            let changed = format!("{x:?}") != format!("{y:?}");
            assert_eq!(changed, (k..k + 6).contains(&i), "op {i}");
        }
        let v = |o: &Opcode| match o {
            Opcode::Int { ptr, .. } => back.ints[ptr.0],
            _ => panic!("not Int: {o:?}"),
        };
        assert_eq!((v(&b.ops[k]), v(&b.ops[k + 2]), v(&b.ops[k + 4])), (CHAMPION_MASK, 8, 16));
        let (Opcode::JNotEq { offset: o1, .. }, Opcode::JSGt { offset: o2, .. }) =
            (&a.ops[k + 5], &b.ops[k + 5])
        else {
            panic!("skip jump");
        };
        assert_eq!(o1, o2);
        // The skip decision for every flag combination the cdb has.
        let skip = |flags: i32| ((flags & CHAMPION_MASK) ^ 8) > 16;
        let (champ, ned, arena) = (1 << 3, 1 << 4, 1 << 19);
        assert!(!skip(0) && !skip(champ) && !skip(champ | ned) && !skip(1 << 7));
        assert!(skip(ned) && skip(ned | champ | arena) && skip(ned | 1 << 13));

        let mut again = read(&patched);
        assert!(champion_site(&again).is_err());
        patch_champion_gear(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        assert_eq!(p.sites.len(), 9, "one closure per worn slot");
        let cant = p.cant_equip;
        let mut code = read(&image);
        patch_loot_level(&mut code);
        let mut patched = Vec::new();
        code.serialize(&mut patched).expect("write");
        let back = read(&patched);

        assert_eq!(back.types, orig.types);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            let site = p.sites.iter().find(|s| s.fi == i);
            assert_eq!(same, site.is_none(), "function #{i} (fn@{})", a.findex.0);
            let Some(s) = site else { continue };
            assert_eq!(a.ops.len(), b.ops.len());
            assert_eq!(a.regs, b.regs);
            for (k, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
                let changed = format!("{x:?}") != format!("{y:?}");
                assert_eq!(changed, [s.level_at, s.equip_at, s.equip_at + 1].contains(&k), "op {k}");
            }
            let (Opcode::JSGte { offset: o1, .. }, Opcode::JAlways { offset: o2 }) =
                (&a.ops[s.level_at], &b.ops[s.level_at])
            else {
                panic!("level jump");
            };
            assert_eq!(o1, o2);
            assert!(matches!(b.ops[s.equip_at], Opcode::Call2 { fun, .. } if fun == cant));
            let (Opcode::JFalse { cond: c1, offset: o1 }, Opcode::JTrue { cond: c2, offset: o2 }) =
                (&a.ops[s.equip_at + 1], &b.ops[s.equip_at + 1])
            else {
                panic!("equip jump");
            };
            assert_eq!((c1, o1), (c2, o2));
        }

        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_loot_level(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }
}
