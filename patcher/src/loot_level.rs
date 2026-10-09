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

// The battle's guaranteed worn-gear drop goes to a random enemy, stronger ones
// more likely.
//
// The worn-gear loop of genLoot (Debrief.hx:500-544) walks
// `state.allUnits.array` in array order, and the first dead enemy with an
// eligible item takes the guaranteed drop (`firstEquip`). The loop now walks a
// copy of that array with the dead enemies in a weighted random order:
//
//   units = units.copy(); lootPick(this, units);   // inserted after the cast
//
//   lootPick:   for s = 0, 1, ...: total = sum of lootWeight(u) over units[s..];
//               stop if total <= 0; r = game.state.random(total); the unit
//               where the running sum passes r swaps with units[s]
//   lootWeight: 0 for a unit without data / owner, of the player side, or alive;
//               else min(max(level, 1), 100)^2, x2 for a champion / boss class
//               (flags & (IsChampion | IsBoss))
//
// A weighted draw without replacement: the first unit of the order that has
// an eligible item (not every dead enemy has one: captured animals,
// NoEquipDrop creatures, all gear disableLoot) is drawn with odds
// proportional to its weight among the eligible ones. So with levels 10 and
// 12 the odds are 100 : 144, a champion of the same level doubles its share.
// The draws use the host's game RNG (GameState.random, the call genLoot
// already makes for the item, Debrief.hx:535; one call per dead enemy): no new
// seed, and the loot is still generated only on the host. The item inside the
// enemy is vanilla: a uniform `random(candidates.length)` over its worn items
// that pass the candidate closures (feature, disableLoot data flag). The
// level cap keeps the sum far below GameState.random's 2^30 range.
//
// A ForceDropWeapon class (named bosses) drops its weapon and vanilla then
// clears `firstEquip`, so the forced weapon used up the guarantee whenever the
// boss came first. That `Mov firstEquip = false` becomes a Nop: the forced
// weapon no longer counts as the guaranteed worn-gear drop. The guaranteed
// drop is still at most one per battle.
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
    debrief_t: RefType,
    arr_t: RefType,
    /// ArrayObj.length / ArrayObj.array.
    len: RefField,
    raw: (RefField, RefType),
    dyn_t: RefType,
    i32_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    unit_t: RefType,
    data: (RefField, RefType),
    level: RefField,
    get_class: RefFun,
    class_t: RefType,
    props: (RefField, RefType),
    flags: (RefField, RefType),
    is_alive: RefFun,
    owner: (RefField, RefType),
    side: (RefField, RefType),
    /// Debrief.state (battle.State) and its playerSide.
    bstate: (RefField, RefType),
    player_side: RefField,
    /// Debrief.game, Game.state (st.GameState) and GameState.random.
    game: (RefField, RefType),
    gstate: (RefField, RefType),
    random: RefFun,
    dbg: (usize, usize),
    /// The Mov firstEquip = false after a ForceDropWeapon drop.
    forced: usize,
}

/// genLoot's worn-gear loop: the op after `SafeCast units = cast state.allUnits.array`, and `units`.
fn loop_site(code: &Bytecode, gen: &Function) -> Result<(usize, Reg)> {
    let (all_units, _) = field(code, obj_type(code, "battle.State")?, "allUnits")?;
    let is_alive = method(code, obj_type(code, "battle.Unit")?, "isAlive")?.findex;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
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
        if ops[i + 7..(i + 60).min(ops.len())]
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == is_alive))
        {
            found.push((i + 7, units));
        }
    }
    match found[..] {
        [s] => Ok(s),
        _ => bail!("genLoot: expected one worn-gear unit loop, found {}", found.len()),
    }
}

fn order_plan(code: &Bytecode) -> Result<OrderPlan> {
    let debrief_t = obj_type(code, "battle.Debrief")?;
    let gen = method(code, debrief_t, "genLoot")?;
    let fi = fun_index(code, gen.findex)?;
    if gen.regs.first() != Some(&debrief_t) {
        bail!("genLoot: reg0 is not battle.Debrief");
    }
    let unit_t = obj_type(code, "battle.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let copy = method(code, arr_t, "copy")?.findex;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    if sig(code, copy)?.1 != arr_t {
        bail!("ArrayObj.copy does not return ArrayObj");
    }
    let (len, len_t) = field(code, arr_t, "length")?;
    let raw = field(code, arr_t, "array")?;
    if len_t != i32_t || !matches!(code.types[raw.1.0], Type::Array) {
        bail!("ArrayObj.length / array have unexpected types");
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
    let get_class = method(code, su_t, "getClass")?.findex;
    let class_t = sig(code, get_class)?.1;
    let props = field_of_virtual(code, class_t, "props")?;
    let flags = field_of_virtual(code, props.1, "flags")?;
    if !matches!(code.types[flags.1.0], Type::Null(t) if t == i32_t) {
        bail!("class props.flags is not null<i32>");
    }
    let is_alive = method(code, unit_t, "isAlive")?.findex;
    if sig(code, is_alive)? != (vec![unit_t], bool_t) {
        bail!("battle.Unit.isAlive is not (Unit) -> bool");
    }
    let owner = field(code, unit_t, "owner")?;
    let side = field(code, owner.1, "side")?;
    let bstate = field(code, debrief_t, "state")?;
    let (player_side, ps_t) = field(code, bstate.1, "playerSide")?;
    if ps_t != side.1 {
        bail!("battle.State.playerSide and Player.side differ in type");
    }
    let game = field(code, debrief_t, "game")?;
    let gstate = field(code, game.1, "state")?;
    let random = method(code, gstate.1, "random")?.findex;
    if sig(code, random)? != (vec![gstate.1, i32_t], i32_t) {
        bail!("GameState.random is not (GameState, i32) -> i32");
    }
    // genLoot itself reads every one of these fields with these types.
    let reads = |f: RefField, t: RefType| {
        gen.ops.iter().any(|o| match *o {
            Opcode::Field { dst, field, .. } | Opcode::GetThis { dst, field } => {
                field == f && gen.regs[dst.0 as usize] == t
            }
            _ => false,
        })
    };
    for (name, f, t) in [
        ("owner", owner.0, owner.1),
        ("side", side.0, side.1),
        ("state", bstate.0, bstate.1),
        ("playerSide", player_side, side.1),
        ("game", game.0, game.1),
        ("game.state", gstate.0, gstate.1),
        ("data", data.0, data.1),
    ] {
        if !reads(f, t) {
            bail!("genLoot does not read field {name} as expected");
        }
    }

    let (at, units) = loop_site(code, gen)?;
    let ops = &gen.ops;
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
        debrief_t,
        arr_t,
        len,
        raw,
        dyn_t,
        i32_t,
        void_t,
        bool_t,
        unit_t,
        data,
        level,
        get_class,
        class_t,
        props,
        flags,
        is_alive,
        owner,
        side,
        bstate,
        player_side,
        game,
        gstate,
        random,
        dbg,
        forced,
    })
}

fn order_apply(code: &mut Bytecode, p: OrderPlan) -> Result<()> {
    let zero = int_const(code, 0);
    let one = int_const(code, 1);
    let mask = int_const(code, RANK_MASK);
    let cap = int_const(code, 100);

    // lootWeight(bs: battle.State, u: battle.Unit) -> i32
    let mut r = asm::Regs(vec![p.bstate.1, p.unit_t]);
    let (bs, u) = (Reg(0), Reg(1));
    let (d, o, s1, s2, b, k, m, c, pr, fl, t) = (
        r.r(p.data.1),
        r.r(p.owner.1),
        r.r(p.side.1),
        r.r(p.side.1),
        r.r(p.bool_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.class_t),
        r.r(p.props.1),
        r.r(p.flags.1),
        r.r(p.i32_t),
    );
    let mut a = asm::Asm::new();
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "none");
    a.op(Opcode::Field { dst: d, obj: u, field: p.data.0 });
    a.jmp(Opcode::JNull { reg: d, offset: 0 }, "none");
    a.op(Opcode::Field { dst: o, obj: u, field: p.owner.0 });
    a.jmp(Opcode::JNull { reg: o, offset: 0 }, "none");
    a.op(Opcode::Field { dst: s1, obj: o, field: p.side.0 });
    a.op(Opcode::Field { dst: s2, obj: bs, field: p.player_side });
    a.jmp(Opcode::JEq { a: s1, b: s2, offset: 0 }, "none");
    a.op(Opcode::Call1 { dst: b, fun: p.is_alive, arg0: u });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "none");
    // k = min(max(level, 1), 100)^2
    a.op(Opcode::Field { dst: k, obj: d, field: p.level });
    a.op(Opcode::Int { dst: m, ptr: one });
    a.jmp(Opcode::JSGte { a: k, b: m, offset: 0 }, "cap");
    a.op(Opcode::Mov { dst: k, src: m });
    a.label("cap");
    a.op(Opcode::Int { dst: m, ptr: cap });
    a.jmp(Opcode::JSLte { a: k, b: m, offset: 0 }, "sq");
    a.op(Opcode::Mov { dst: k, src: m });
    a.label("sq");
    a.op(Opcode::Mul { dst: k, a: k, b: k });
    // x2 for a champion / boss class
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
    a.op(Opcode::Int { dst: m, ptr: one });
    a.op(Opcode::Shl { dst: k, a: k, b: m });
    a.label("ret");
    a.op(Opcode::Ret { ret: k });
    a.label("none");
    a.op(Opcode::Int { dst: k, ptr: zero });
    a.op(Opcode::Ret { ret: k });
    let weight_fn = asm::push_fn(code, vec![p.bstate.1, p.unit_t], p.i32_t, r.0, a.finish(), p.dbg.0)?;

    // lootPick(this: battle.Debrief, units: ArrayObj) -> void
    let mut r = asm::Regs(vec![p.debrief_t, p.arr_t]);
    let (this, units) = (Reg(0), Reg(1));
    let (bs, g, gs, raw, n, i, total, w, x, z, dv, u, first, v, st) = (
        r.r(p.bstate.1),
        r.r(p.game.1),
        r.r(p.gstate.1),
        r.r(p.raw.1),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.i32_t),
        r.r(p.dyn_t),
        r.r(p.unit_t),
        r.r(p.dyn_t),
        r.r(p.void_t),
        r.r(p.i32_t),
    );
    let mut a = asm::Asm::new();
    a.jmp(Opcode::JNull { reg: units, offset: 0 }, "end");
    a.op(Opcode::Field { dst: bs, obj: this, field: p.bstate.0 });
    a.jmp(Opcode::JNull { reg: bs, offset: 0 }, "end");
    a.op(Opcode::Field { dst: g, obj: this, field: p.game.0 });
    a.jmp(Opcode::JNull { reg: g, offset: 0 }, "end");
    a.op(Opcode::Field { dst: gs, obj: g, field: p.gstate.0 });
    a.jmp(Opcode::JNull { reg: gs, offset: 0 }, "end");
    a.op(Opcode::Field { dst: n, obj: units, field: p.len });
    a.op(Opcode::Field { dst: raw, obj: units, field: p.raw.0 });
    a.op(Opcode::Int { dst: z, ptr: zero });
    a.op(Opcode::Int { dst: st, ptr: zero });
    // slot st: a weighted draw from units[st..]
    a.loop_head("next");
    a.op(Opcode::Int { dst: total, ptr: zero });
    a.op(Opcode::Mov { dst: i, src: st });
    a.loop_head("sum");
    a.jmp(Opcode::JSGte { a: i, b: n, offset: 0 }, "pick");
    a.op(Opcode::GetArray { dst: dv, array: raw, index: i });
    a.op(Opcode::UnsafeCast { dst: u, src: dv });
    a.op(Opcode::Call2 { dst: w, fun: weight_fn, arg0: bs, arg1: u });
    a.op(Opcode::Add { dst: total, a: total, b: w });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JAlways { offset: 0 }, "sum");
    a.label("pick");
    a.jmp(Opcode::JSLte { a: total, b: z, offset: 0 }, "end");
    a.op(Opcode::Call2 { dst: x, fun: p.random, arg0: gs, arg1: total });
    a.op(Opcode::Mov { dst: i, src: st });
    a.loop_head("find");
    a.jmp(Opcode::JSGte { a: i, b: n, offset: 0 }, "end");
    a.op(Opcode::GetArray { dst: dv, array: raw, index: i });
    a.op(Opcode::UnsafeCast { dst: u, src: dv });
    a.op(Opcode::Call2 { dst: w, fun: weight_fn, arg0: bs, arg1: u });
    a.op(Opcode::Sub { dst: x, a: x, b: w });
    a.jmp(Opcode::JSLt { a: x, b: z, offset: 0 }, "swap");
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JAlways { offset: 0 }, "find");
    a.label("swap");
    a.op(Opcode::GetArray { dst: first, array: raw, index: st });
    a.op(Opcode::SetArray { array: raw, index: i, src: first });
    a.op(Opcode::SetArray { array: raw, index: st, src: dv });
    a.op(Opcode::Incr { dst: st });
    a.jmp(Opcode::JAlways { offset: 0 }, "next");
    a.label("end");
    a.op(Opcode::Ret { ret: v });
    let pick_fn = asm::push_fn(code, vec![p.debrief_t, p.arr_t], p.void_t, r.0, a.finish(), p.dbg.0)?;

    let f = &mut code.functions[p.fi];
    f.ops[p.forced + 1] = Opcode::Nop;
    let rv = new_reg(f, p.void_t);
    insert_ops(
        f,
        p.at,
        vec![
            Opcode::Call1 { dst: p.units, fun: p.copy, arg0: p.units },
            Opcode::Call2 { dst: rv, fun: pick_fn, arg0: Reg(0), arg1: p.units },
        ],
    );
    eprintln!(
        "patched loot order fn@{}: the guaranteed worn-gear drop goes to a random enemy, stronger ones more likely",
        f.findex.0
    );
    Ok(())
}

/// Puts a strength-weighted random dead enemy first in genLoot's worn-gear loop, or leaves `code` untouched and logs why.
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

// The extra worn-gear chance scales with the number of dead enemies.
//
// After the guaranteed drop, each next dead enemy rolls (genLoot ops 654-700)
//
//   p = LootEquipDropProba (0.08) + bonuses + equipLootProba * LootEquipDropProbaProgression (0.03)
//   if (rand() < p) drop        // equipLootProba++ each roll, -1 on a drop
//
// so the expected extra drops grow faster than the enemy count (pity builds
// up within a battle). Each roll's chance is now multiplied by
//
//   lootScale = sqrt(NREF / N),  N = dead enemies of the battle (at least 1)
//
// computed once before the worn-gear loop by an appended function that counts
// the units the loop itself treats as dead enemies (data, not on the player
// side, not alive, not a captured animal). With NREF 8 the expected items per
// battle are 1.10 / 1.32 / 1.57 / 1.81 / 2.04 / 2.26 at N = 2 / 4 / 6 / 8 / 10
// / 12 (vanilla 1.05 / 1.23 / 1.50 / 1.81 / 2.13 / 2.45): about 0.1 extra
// item per extra enemy at every battle size. The guaranteed drop, the counter
// and its reset are unchanged.

const NREF: f64 = 8.0;

struct PityPlan {
    fi: usize,
    /// Loop entry (lootScale call goes here).
    at: usize,
    /// The roll compare `JNotLt rand !< p` (the Mul goes before it).
    roll_at: usize,
    p: Reg,
    debrief_t: RefType,
    state: (RefField, RefType),
    all_units: (RefField, RefType),
    proxy_array: (RefField, RefType),
    arr_t: RefType,
    arr_len: RefField,
    arr_array: (RefField, RefType),
    unit_t: RefType,
    data: (RefField, RefType),
    owner: (RefField, RefType),
    side: (RefField, RefType),
    player_side: RefField,
    is_alive: RefFun,
    is_animal: RefFun,
    is_captured: RefFun,
    sqrt: RefFun,
    f64_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    dyn_t: RefType,
    dbg: (usize, usize),
}

fn pity_plan(code: &Bytecode) -> Result<PityPlan> {
    let debrief_t = obj_type(code, "battle.Debrief")?;
    let gen = method(code, debrief_t, "genLoot")?;
    let fi = fun_index(code, gen.findex)?;
    let (at, _) = loop_site(code, gen)?;
    let state_t = obj_type(code, "battle.State")?;
    let unit_t = obj_type(code, "battle.Unit")?;
    let su_t = obj_type(code, "st.Unit")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let f64_t = prim_type(code, "f64", |t| matches!(t, Type::F64))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let state = field(code, debrief_t, "state")?;
    if state.1 != state_t {
        bail!("Debrief.state is not battle.State");
    }
    let all_units = field(code, state_t, "allUnits")?;
    let proxy_array = field(code, all_units.1, "array")?;
    let (arr_len, len_t) = field(code, arr_t, "length")?;
    let arr_array = field(code, arr_t, "array")?;
    if len_t != i32_t || !matches!(code.types[arr_array.1.0], Type::Array) {
        bail!("ArrayObj layout");
    }
    let data = field(code, unit_t, "data")?;
    let owner = field(code, unit_t, "owner")?;
    let side = field(code, owner.1, "side")?;
    let (player_side, ps_t) = field(code, state_t, "playerSide")?;
    if data.1 != su_t || ps_t != side.1 {
        bail!("battle.Unit data / owner.side types");
    }
    let is_alive = method(code, unit_t, "isAlive")?.findex;
    let is_captured = method(code, unit_t, "isCaptured")?.findex;
    let is_animal = method(code, su_t, "get_isAnimal")?.findex;
    let sqrt = native(code, "math_sqrt", &[f64_t], f64_t)?;

    // The loop itself skips exactly these units; keep the count in step with it.
    let loop_ops = &gen.ops[at..(at + 60).min(gen.ops.len())];
    for f in [is_alive, is_captured, is_animal] {
        if !loop_ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == f))
        {
            bail!("genLoot loop: dead-enemy checks changed");
        }
    }

    let ops = &gen.ops;
    let ty = |r: Reg| gen.regs[r.0 as usize];
    let rolls: Vec<usize> = (1..ops.len())
        .filter(|&j| {
            let Opcode::JNotLt { b, .. } = ops[j] else { return false };
            matches!(ops[j - 1], Opcode::Add { dst, .. } if dst == b && ty(b) == f64_t)
                || matches!(ops[j - 1], Opcode::Mul { dst, a, .. } if dst == b && a == b)
        })
        .collect();
    let [roll_at] = rolls[..] else {
        bail!("genLoot: expected one worn-gear roll, found {}", rolls.len());
    };
    let Opcode::JNotLt { b: p, .. } = ops[roll_at] else { unreachable!() };
    if matches!(ops[roll_at - 1], Opcode::Mul { .. }) {
        bail!("already applied");
    }
    if (0..ops.len()).any(|j| jump_targets(gen, j).contains(&roll_at)) {
        bail!("genLoot: a jump lands on the roll compare");
    }
    let dbg = gen.debug_info.as_ref().map_or((0, 0), |d| d[at]);
    Ok(PityPlan {
        fi,
        at,
        roll_at,
        p,
        debrief_t,
        state,
        all_units,
        proxy_array,
        arr_t,
        arr_len,
        arr_array,
        unit_t,
        data,
        owner,
        side,
        player_side,
        is_alive,
        is_animal,
        is_captured,
        sqrt,
        f64_t,
        i32_t,
        bool_t,
        dyn_t,
        dbg,
    })
}

fn pity_apply(code: &mut Bytecode, p: PityPlan) -> Result<()> {
    // lootScale(d: battle.Debrief) -> f64
    let mut r = asm::Regs(vec![p.debrief_t]);
    let st = r.r(p.state.1);
    let list = r.r(p.all_units.1);
    let raw = r.r(p.proxy_array.1);
    let arr = r.r(p.arr_t);
    let (i, len, n, one) = (r.r(p.i32_t), r.r(p.i32_t), r.r(p.i32_t), r.r(p.i32_t));
    let na = r.r(p.arr_array.1);
    let dv = r.r(p.dyn_t);
    let u = r.r(p.unit_t);
    let su = r.r(p.data.1);
    let pl = r.r(p.owner.1);
    let (s1, s2) = (r.r(p.side.1), r.r(p.side.1));
    let b = r.r(p.bool_t);
    let (nf, k) = (r.r(p.f64_t), r.r(p.f64_t));
    let zero_c = int_const(code, 0);
    let one_c = int_const(code, 1);
    let nref_c = float_const(code, NREF);
    let mut a = asm::Asm::new();
    a.op(Opcode::Int { dst: n, ptr: zero_c });
    a.op(Opcode::Int { dst: i, ptr: zero_c });
    a.op(Opcode::Field { dst: st, obj: Reg(0), field: p.state.0 });
    a.jmp(Opcode::JNull { reg: st, offset: 0 }, "done");
    a.op(Opcode::Field { dst: list, obj: st, field: p.all_units.0 });
    a.jmp(Opcode::JNull { reg: list, offset: 0 }, "done");
    a.op(Opcode::Field { dst: raw, obj: list, field: p.proxy_array.0 });
    a.op(Opcode::SafeCast { dst: arr, src: raw });
    a.jmp(Opcode::JNull { reg: arr, offset: 0 }, "done");
    a.loop_head("loop");
    a.op(Opcode::Field { dst: len, obj: arr, field: p.arr_len });
    a.jmp(Opcode::JSGte { a: i, b: len, offset: 0 }, "done");
    a.op(Opcode::Field { dst: na, obj: arr, field: p.arr_array.0 });
    a.op(Opcode::GetArray { dst: dv, array: na, index: i });
    a.op(Opcode::UnsafeCast { dst: u, src: dv });
    a.op(Opcode::Incr { dst: i });
    a.jmp(Opcode::JNull { reg: u, offset: 0 }, "loop");
    a.op(Opcode::Field { dst: su, obj: u, field: p.data.0 });
    a.jmp(Opcode::JNull { reg: su, offset: 0 }, "loop");
    a.op(Opcode::Field { dst: pl, obj: u, field: p.owner.0 });
    a.jmp(Opcode::JNull { reg: pl, offset: 0 }, "loop");
    a.op(Opcode::Field { dst: s1, obj: pl, field: p.side.0 });
    a.op(Opcode::Field { dst: s2, obj: st, field: p.player_side });
    a.jmp(Opcode::JEq { a: s1, b: s2, offset: 0 }, "loop");
    a.op(Opcode::Call1 { dst: b, fun: p.is_alive, arg0: u });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "loop");
    a.op(Opcode::Call1 { dst: b, fun: p.is_animal, arg0: su });
    a.jmp(Opcode::JFalse { cond: b, offset: 0 }, "count");
    a.op(Opcode::Call1 { dst: b, fun: p.is_captured, arg0: u });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "loop");
    a.label("count");
    a.op(Opcode::Incr { dst: n });
    a.jmp(Opcode::JAlways { offset: 0 }, "loop");
    a.label("done");
    a.op(Opcode::Int { dst: one, ptr: one_c });
    a.jmp(Opcode::JSGte { a: n, b: one, offset: 0 }, "calc");
    a.op(Opcode::Mov { dst: n, src: one });
    a.label("calc");
    a.op(Opcode::ToSFloat { dst: nf, src: n });
    a.op(Opcode::Float { dst: k, ptr: nref_c });
    a.op(Opcode::SDiv { dst: k, a: k, b: nf });
    a.op(Opcode::Call1 { dst: k, fun: p.sqrt, arg0: k });
    a.op(Opcode::Ret { ret: k });
    let scale_fn = asm::push_fn(code, vec![p.debrief_t], p.f64_t, r.0, a.finish(), p.dbg.0)?;

    let f = &mut code.functions[p.fi];
    let rs = new_reg(f, p.f64_t);
    // Roll first (higher index), then the loop entry, so `roll_at` stays valid.
    insert_ops(f, p.roll_at, vec![Opcode::Mul { dst: p.p, a: p.p, b: rs }]);
    insert_ops(f, p.at, vec![Opcode::Call1 { dst: rs, fun: scale_fn, arg0: Reg(0) }]);
    eprintln!(
        "patched loot pity fn@{}: worn-gear chance x sqrt({NREF} / dead enemies)",
        f.findex.0
    );
    Ok(())
}

/// Scales the extra worn-gear chance by the battle size, or leaves `code` untouched and logs why.
pub(crate) fn patch_loot_pity(code: &mut Bytecode) {
    let p = match pity_plan(code) {
        Ok(p) => p,
        Err(e) => return crate::skipped(format!("loot pity skipped: {e:#}")),
    };
    let snap = asm::Snap::take(code);
    if let Err(e) = pity_apply(code, p) {
        snap.restore(code);
        crate::skipped(format!("loot pity skipped: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read};

    #[test]
    fn loot_pity_installed_game() {
        use crate::asm::testutil::{check_flow, check_types};
        let Some(image) = game() else { return };
        // Alone, and after the order pass (both insert at the loop entry).
        for with_order in [false, true] {
            let mut code = read(&image);
            if with_order {
                patch_loot_order(&mut code);
            }
            let p = pity_plan(&code).expect("plan");
            let (fi, at, roll_at, pr) = (p.fi, p.at, p.roll_at, p.p);
            let a = code.functions[fi].clone();
            let n = code.functions.len();
            patch_loot_pity(&mut code);
            let mut patched = Vec::new();
            code.serialize(&mut patched).expect("write");
            let back = read(&patched);
            assert_eq!(back.functions.len(), n + 1);
            let b = &back.functions[fi];
            assert_eq!(b.ops.len(), a.ops.len() + 2);
            assert_eq!(b.regs.len(), a.regs.len() + 1);
            let rs = Reg(a.regs.len() as u32);
            let Opcode::Call1 { dst, fun, arg0: Reg(0) } = b.ops[at] else { panic!("scale call") };
            assert_eq!((dst, fun), (rs, back.functions[n].findex));
            assert_eq!(format!("{:?}", b.ops[roll_at + 1]), format!("{:?}", Opcode::Mul { dst: pr, a: pr, b: rs }));
            assert!(matches!(b.ops[roll_at + 2], Opcode::JNotLt { b, .. } if b == pr));
            check_flow(b);
            check_types(&back, b, at..at + 1);
            check_types(&back, b, roll_at + 1..roll_at + 2);
            let s = &back.functions[n];
            check_flow(s);
            check_types(&back, s, 0..s.ops.len());

            let mut again = read(&patched);
            assert!(pity_plan(&again).is_err());
            patch_loot_pity(&mut again);
            let mut twice = Vec::new();
            again.serialize(&mut twice).expect("write");
            assert!(twice == patched);
        }
        // The scale itself.
        let scale = |n: f64| (NREF / n.max(1.0)).sqrt();
        assert!((scale(8.0) - 1.0).abs() < 1e-12 && (scale(2.0) - 2.0).abs() < 1e-12);
        assert_eq!(scale(0.0), scale(1.0));
    }

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
        let (fi, at, copy) = (p.fi, p.at, p.copy);
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
        // The forced-weapon Mov became a Nop (index past the 2 inserted ops).
        let mut b = b.clone();
        assert!(matches!(b.ops[p.forced + 1 + 2], Opcode::Nop));
        b.ops[p.forced + 1 + 2] = a.ops[p.forced + 1].clone();
        let b = &b;
        shifted(a, b, at, 2);
        check_flow(b);
        check_types(&back, b, at..at + 2);
        assert!(matches!(b.ops[at], Opcode::Call1 { fun, .. } if fun == copy));
        let Opcode::Call2 { fun: pick, arg0: Reg(0), arg1, .. } = b.ops[at + 1] else { panic!("pick call") };
        assert_eq!(arg1, p.units);
        for f in &back.functions[n..] {
            check_flow(f);
            check_types(&back, f, 0..f.ops.len());
        }
        assert_eq!(back.functions[n + 1].findex, pick);

        let mut again = read(&patched);
        assert!(order_plan(&again).is_err());
        patch_loot_order(&mut again);
        let mut twice = Vec::new();
        again.serialize(&mut twice).expect("write");
        assert!(twice == patched);
    }

    /// lootPick run in the interpreter for every value the first RNG draw can
    /// return (later draws return 0): the order matches a weighted draw without
    /// replacement, the first slot has odds = level^2 (x2 champion / boss),
    /// and units of weight 0 (the player's, alive) stay behind, untouched.
    #[test]
    fn loot_pick_sim() {
        use crate::testsim::{Core, Sim, V};
        let Some(image) = game() else { return };
        let mut code = read(&image);
        let p = order_plan(&code).expect("plan");
        let n = code.functions.len();
        patch_loot_order(&mut code);
        let pick = code.functions[n + 1].findex;
        let (is_alive, get_class, random) = (p.is_alive, p.get_class, p.random);
        let mut sim = Sim::new(
            &code,
            n,
            move |c, f, a| {
                if f == is_alive {
                    Some(V::B(c.key_get(&a[0], "alive") == V::B(true)))
                } else if f == get_class {
                    Some(c.key_get(&a[0], "cls"))
                } else if f == random {
                    c.log.push(("random", a.to_vec()));
                    let r = c.map("rng", "r");
                    c.put("rng", "r", V::I(0));
                    Some(r)
                } else {
                    None
                }
            },
            |_, _, _| panic!("no virtual calls"),
        );
        let c = &mut sim.c;
        let (ps, es) = (c.obj(&[]), c.obj(&[]));
        let bstate = c.obj(&[(p.player_side, ps.clone())]);
        let gstate = c.obj(&[]);
        let gm = c.obj(&[(p.gstate.0, gstate.clone())]);
        let debrief = c.obj(&[(p.bstate.0, bstate), (p.game.0, gm)]);
        let unit = |c: &mut Core, side: &V, level: i32, flags: i32, alive: bool| {
            let props = c.obj(&[(p.flags.0, V::I(flags))]);
            let cls = c.obj(&[(p.props.0, props)]);
            let data = c.obj(&[(p.level, V::I(level))]);
            c.key_set(&data, "cls".into(), cls);
            let owner = c.obj(&[(p.side.0, side.clone())]);
            let u = c.obj(&[(p.data.0, data), (p.owner.0, owner)]);
            c.key_set(&u, "alive".into(), V::B(alive));
            u
        };
        let units = vec![
            unit(c, &ps, 30, 0, false),       // the player's: 0
            unit(c, &es, 10, 0, false),       // 100
            unit(c, &es, 12, 0, false),       // 144
            unit(c, &es, 20, 1 << 3, true),   // alive: 0
            unit(c, &es, 10, 1 << 23, false), // boss: 200
            unit(c, &es, 0, 0, false),        // level 0 counts as 1: 1
            unit(c, &es, 9, 1 << 3, false),   // champion: 162
        ];
        let w = [0, 100, 144, 0, 200, 1, 162];
        // The same draw in Rust: (order, totals passed to random).
        let model = |mut r: i32| {
            let mut idx: Vec<usize> = (0..w.len()).collect();
            let mut totals = vec![];
            for s in 0..w.len() {
                let total: i32 = idx[s..].iter().map(|&k| w[k]).sum();
                if total <= 0 {
                    break;
                }
                totals.push(total);
                let mut i = s;
                loop {
                    r -= w[idx[i]];
                    if r < 0 {
                        break;
                    }
                    i += 1;
                }
                idx.swap(s, i);
                r = 0;
            }
            (idx, totals)
        };
        let total: i32 = w.iter().sum();
        let mut hits = vec![0; units.len()];
        for r in 0..total {
            sim.c.put("rng", "r", V::I(r));
            let arr = sim.c.arr(p.len, p.raw.0, units.clone());
            sim.call(pick, vec![debrief.clone(), arr.clone()]);
            let (order, totals) = model(r);
            let calls: Vec<Vec<V>> = totals.iter().map(|&t| vec![gstate.clone(), V::I(t)]).collect();
            assert_eq!(sim.c.take("random"), calls, "r = {r}");
            assert_eq!(totals.len(), 5, "one draw per dead enemy");
            let raw = sim.c.get(&arr, p.raw.0);
            let got: Vec<V> = (0..units.len()).map(|k| sim.c.key_get(&raw, &format!("i{k}"))).collect();
            let want: Vec<V> = order.iter().map(|&k| units[k].clone()).collect();
            assert_eq!(got, want, "r = {r}");
            assert!(got[5..].iter().all(|u| *u == units[0] || *u == units[3]), "r = {r}");
            hits[order[0]] += 1;
        }
        assert_eq!(hits, w.to_vec());
        // Level capped at 100 (no i32 overflow), and no dead enemy: no draw.
        let big = unit(&mut sim.c, &es, 1_000_000, 1 << 3, false);
        sim.c.put("rng", "r", V::I(0));
        let arr = sim.c.arr(p.len, p.raw.0, vec![units[0].clone(), big]);
        sim.call(pick, vec![debrief.clone(), arr]);
        assert_eq!(sim.c.take("random"), vec![vec![gstate.clone(), V::I(20_000)]]);
        let arr = sim.c.arr(p.len, p.raw.0, vec![units[0].clone(), units[3].clone()]);
        sim.call(pick, vec![debrief.clone(), arr]);
        assert!(sim.c.take("random").is_empty());
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
