// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Co-op post-battle screen: the repair / cure availability check never tears
// the window down.
//
// `ui.win.Debrief.update` (co-op only, every frame, Debrief.hx:618-638) derives
// whether the "repair all" and "cure all" buttons should be enabled and, when that
// differs from the button's `enable`, calls `rebuild()`:
//
//   if (repairBtn != null) {
//       var n = 0; for (u in getUnitsToRepair()) n += inventory.getArmorRepairCost(u);
//       if ((n > 0 && hasItemWithChest(me.inventory, "RawMaterials", n)) != repairBtn.enable) { rebuild(); return; }
//   }
//   if (cureBtn != null) { ...same with the injuries and the remedies...; rebuild(); return; }
//
// `rebuild()` is Window.rebuild: removeChildren + init, i.e. the unit cards, the
// loot grid, every item slot, Take all, Continue and both buttons are destroyed
// and created again. Only a squad with damaged armour or curable injuries gets
// past `n > 0`, so only then can this fire, and it fires on EVERY frame in which
// the derived value and the button disagree. The inputs are live co-op state that
// changes under the window while it is open: the party counts read every player's
// inventory, the camp chest and the units of all four players (debrief_cure.rs
// fixed one list mismatch that made it disagree permanently). Each rebuild under
// the cursor replaces the hovered slot (Scene re-hovers the new one: the item
// hover sound) and a press and its release land on different objects, so no item
// can be taken and Take all is dead, on whichever machine (host included) is
// rebuilding.
//
// Fix: the two `Call1 rebuild(this)` become `Call2 set_enable(button, desired)`
// (Element.set_enable, the call init itself ends with; `desired` is the register
// the comparison just tested, `button` the one it read `enable` from). The button
// follows the availability every frame with no rebuild at all; its cost label is
// refreshed by every other rebuild (loot change, and repairAll / cureAll end with
// netRebuild). One op per site, op count, registers and jumps unchanged.
//
// Runs after debrief_diag (whose trace call may sit between the comparison and
// the rebuild). Validated before editing; a mismatch skips the pass (logged).

use super::*;

struct Site {
    at: usize,
    button: Reg,
    desired: Reg,
}

struct Plan {
    fi: usize,
    set_enable: RefFun,
    sites: Vec<Site>,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let debrief_t = obj_type(code, "ui.win.Debrief")?;
    let element_t = obj_type(code, "ui.comp.Element")?;
    let update = method(code, debrief_t, "update")?;
    let fi = fun_index(code, update.findex)?;
    let rebuild = method(code, debrief_t, "rebuild")?.findex;
    let set_enable = method(code, element_t, "set_enable")?;
    let bool_t = set_enable
        .t
        .as_fun(code)
        .and_then(|ft| (ft.args.len() == 2 && ft.ret == ft.args[1]).then_some(ft.ret))
        .context("Element.set_enable is not (Element, Bool) -> Bool")?;
    let enable = field(code, element_t, "enable")?.0;
    let buttons = [
        field(code, debrief_t, "repairBtn")?.0,
        field(code, debrief_t, "cureBtn")?.0,
    ];

    let o = &update.ops;
    let mut sites = vec![];
    for (at, op) in o.iter().enumerate() {
        if !matches!(op, Opcode::Call1 { fun, arg0: Reg(0), .. } if *fun == rebuild) {
            continue;
        }
        // Comparison right before (debrief_diag may have put one trace call in between).
        let j = (at.saturating_sub(2)..at)
            .rev()
            .find(|&i| matches!(o[i], Opcode::JEq { .. }))
            .context("Debrief.update: rebuild is not guarded by a comparison")?;
        if j + 1 != at
            && !matches!(
                o[j + 1],
                Opcode::CallN { .. }
                    | Opcode::Call1 { .. }
                    | Opcode::Call2 { .. }
                    | Opcode::Call3 { .. }
            )
        {
            bail!("Debrief.update: unexpected op between the comparison and rebuild");
        }
        let Opcode::JEq {
            a: desired,
            b: current,
            offset,
        } = o[j]
        else {
            unreachable!()
        };
        if j as i32 + 1 + offset != at as i32 + 2 {
            bail!("Debrief.update: comparison does not skip exactly rebuild and return");
        }
        if !matches!(o.get(at + 1), Some(Opcode::Ret { .. })) {
            bail!("Debrief.update: rebuild is not followed by return");
        }
        let Some(Opcode::Field {
            dst,
            obj: button,
            field,
        }) = j.checked_sub(1).map(|i| &o[i])
        else {
            bail!("Debrief.update: comparison does not read button.enable");
        };
        if *dst != current || *field != enable {
            bail!("Debrief.update: comparison does not read button.enable");
        }
        let button = *button;
        // The button register was loaded from repairBtn / cureBtn just before.
        let loaded = o[..j - 1].iter().rev().take(3).any(|x| {
            matches!(x, Opcode::GetThis { dst, field } if *dst == button && buttons.contains(field))
        });
        if !loaded {
            bail!("Debrief.update: compared button is not repairBtn / cureBtn");
        }
        if update.regs[desired.0 as usize] != bool_t {
            bail!("Debrief.update: desired state is not Bool");
        }
        if !is_sub(code, update.regs[button.0 as usize], element_t) {
            bail!("Debrief.update: button register is not an Element");
        }
        sites.push(Site {
            at,
            button,
            desired,
        });
    }
    if sites.is_empty()
        && update
            .ops
            .iter()
            .any(|x| matches!(x, Opcode::Call2 { fun, .. } if *fun == set_enable.findex))
    {
        bail!("Debrief.update: already applied");
    }
    if sites.len() != 2 {
        bail!(
            "Debrief.update: {} availability rebuilds (want 2)",
            sites.len()
        );
    }
    Ok(Plan {
        fi,
        set_enable: set_enable.findex,
        sites,
    })
}

fn apply(code: &mut Bytecode, p: Plan) {
    let f = &mut code.functions[p.fi];
    for s in &p.sites {
        f.ops[s.at] = Opcode::Call2 {
            dst: s.desired,
            fun: p.set_enable,
            arg0: s.button,
            arg1: s.desired,
        };
    }
    eprintln!(
        "patched debrief enable fn@{} ops {:?}: co-op repair/cure availability sets the button, no rebuild",
        f.findex.0,
        p.sites.iter().map(|s| s.at).collect::<Vec<_>>()
    );
}

/// The co-op Debrief.update availability check updates the buttons in place
/// instead of rebuilding the window, or leaves `code` untouched and logs why.
pub(crate) fn patch_debrief_enable(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("debrief enable skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    fn write(code: &Bytecode) -> Vec<u8> {
        let mut v = Vec::new();
        code.serialize(&mut v).expect("write");
        v
    }

    /// On the vanilla image and after cure + diag: only the two rebuild calls of
    /// Debrief.update change, into set_enable(button, desired); idempotent.
    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        for chain in [false, true] {
            let mut orig = read(&image);
            if chain {
                debrief_cure::patch_debrief_cure(&mut orig);
                debrief_diag::patch(&mut orig);
            }
            let p = plan(&orig).expect("plan");
            let fi = p.fi;
            let sites: Vec<(usize, Reg, Reg)> = p
                .sites
                .iter()
                .map(|s| (s.at, s.button, s.desired))
                .collect();
            let set_enable = p.set_enable;
            let mut code = read(&write(&orig));
            patch_debrief_enable(&mut code);
            let patched = write(&code);
            let back = read(&patched);
            assert_eq!(back.types, orig.types);
            for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
                let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
                assert_eq!(same, i != fi, "function #{i}");
            }
            let (a, b) = (&orig.functions[fi], &back.functions[fi]);
            assert_eq!(a.regs, b.regs);
            assert_eq!(a.ops.len(), b.ops.len());
            for i in 0..a.ops.len() {
                match sites.iter().find(|s| s.0 == i) {
                    Some(&(_, button, desired)) => assert_eq!(
                        format!("{:?}", b.ops[i]),
                        format!(
                            "{:?}",
                            Opcode::Call2 {
                                dst: desired,
                                fun: set_enable,
                                arg0: button,
                                arg1: desired
                            }
                        )
                    ),
                    None => assert_eq!(
                        format!("{:?}", b.ops[i]),
                        format!("{:?}", a.ops[i]),
                        "op {i}"
                    ),
                }
            }
            // No rebuild is reachable from update any more.
            let rebuild = method(&back, obj_type(&back, "ui.win.Debrief").unwrap(), "rebuild")
                .unwrap()
                .findex;
            assert!(!b
                .ops
                .iter()
                .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == rebuild)));
            let mut again = read(&patched);
            assert!(plan(&again).is_err());
            patch_debrief_enable(&mut again);
            assert!(write(&again) == patched);
        }
    }

    // ---- replay of the real Debrief.update / Element.set_enable bytecode ----
    //
    // Only the calls update makes are stubbed (by callee name): the unit lists,
    // the counts and rebuild (recorded, it changes nothing: a disagreement that
    // persists frame after frame). set_enable runs its real bytecode.

    use std::collections::HashMap;

    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        Bool(bool),
        Int(i32),
        Obj(usize),
        Raw(Vec<V>),
        Ref(Box<V>),
    }

    #[derive(Default)]
    struct World {
        objs: Vec<HashMap<String, V>>,
        rebuilds: usize,
        set_enables: usize,
        materials: i32,
        remedies: i32,
        repair_units: Vec<V>,
        heal_units: Vec<V>,
    }

    impl World {
        fn new_obj(&mut self, fields: &[(&str, V)]) -> V {
            self.objs.push(
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            );
            V::Obj(self.objs.len() - 1)
        }
        fn list(&mut self, items: Vec<V>) -> V {
            let n = items.len() as i32;
            self.new_obj(&[("length", V::Int(n)), ("array", V::Raw(items))])
        }
        fn get(&self, o: &V, name: &str) -> V {
            let V::Obj(i) = o else {
                panic!("field {name} on {o:?}")
            };
            self.objs[*i].get(name).cloned().unwrap_or(V::Null)
        }
        fn set(&mut self, o: &V, name: &str, v: V) {
            let V::Obj(i) = o else {
                panic!("store {name} on {o:?}")
            };
            self.objs[*i].insert(name.to_string(), v);
        }
    }

    fn field_name(code: &Bytecode, t: RefType, f: RefField) -> String {
        match &code.types[t.0] {
            Type::Virtual { fields } => s(code, fields[f.0].name).to_string(),
            _ => s(code, obj(code, t).unwrap().fields[f.0].name).to_string(),
        }
    }

    fn int(v: &V) -> i32 {
        match v {
            V::Int(i) => *i,
            V::Ref(b) => int(b),
            o => panic!("int {o:?}"),
        }
    }

    fn run(code: &Bytecode, w: &mut World, fun: RefFun, args: &[V]) -> V {
        let f = &code.functions[fun_index(code, fun).unwrap()];
        let mut r = vec![V::Null; f.regs.len()];
        r[..args.len()].clone_from_slice(args);
        let mut pc = 0usize;
        for _ in 0..100_000 {
            let i = pc;
            pc += 1;
            let at = |o: i32| (i as i32 + 1 + o) as usize;
            let ints =
                |a: &Reg, b: &Reg, r: &Vec<V>| (int(&r[a.0 as usize]), int(&r[b.0 as usize]));
            match &f.ops[i] {
                Opcode::Label => {}
                Opcode::NullCheck { reg } => {
                    assert_ne!(r[reg.0 as usize], V::Null, "null access op{i}")
                }
                Opcode::Null { dst } => r[dst.0 as usize] = V::Null,
                Opcode::Bool { dst, value } => r[dst.0 as usize] = V::Bool(value.0),
                Opcode::Int { dst, ptr } => r[dst.0 as usize] = V::Int(code.ints[ptr.0]),
                Opcode::GetGlobal { dst, .. } => r[dst.0 as usize] = V::Null,
                Opcode::Mov { dst, src }
                | Opcode::UnsafeCast { dst, src }
                | Opcode::SafeCast { dst, src }
                | Opcode::ToDyn { dst, src } => r[dst.0 as usize] = r[src.0 as usize].clone(),
                Opcode::Ref { dst, src } => {
                    r[dst.0 as usize] = V::Ref(Box::new(r[src.0 as usize].clone()))
                }
                Opcode::Not { dst, src } => {
                    r[dst.0 as usize] = V::Bool(r[src.0 as usize] != V::Bool(true))
                }
                Opcode::Incr { dst } => r[dst.0 as usize] = V::Int(int(&r[dst.0 as usize]) + 1),
                Opcode::Add { dst, a, b } => {
                    let (x, y) = ints(a, b, &r);
                    r[dst.0 as usize] = V::Int(x + y)
                }
                Opcode::Field { dst, obj: o, field } => {
                    let name = field_name(code, f.regs[o.0 as usize], *field);
                    r[dst.0 as usize] = w.get(&r[o.0 as usize], &name)
                }
                Opcode::GetThis { dst, field } => {
                    let name = field_name(code, f.regs[0], *field);
                    r[dst.0 as usize] = w.get(&r[0], &name)
                }
                Opcode::SetThis { field, src } => {
                    let name = field_name(code, f.regs[0], *field);
                    let v = r[src.0 as usize].clone();
                    w.set(&r[0].clone(), &name, v)
                }
                Opcode::GetArray { dst, array, index } => {
                    let V::Raw(items) = &r[array.0 as usize] else {
                        panic!("array")
                    };
                    r[dst.0 as usize] = items[int(&r[index.0 as usize]) as usize].clone()
                }
                Opcode::JAlways { offset } => pc = at(*offset),
                Opcode::JTrue { cond, offset } => {
                    if r[cond.0 as usize] == V::Bool(true) {
                        pc = at(*offset)
                    }
                }
                Opcode::JFalse { cond, offset } => {
                    if r[cond.0 as usize] == V::Bool(false) {
                        pc = at(*offset)
                    }
                }
                Opcode::JNull { reg, offset } => {
                    if r[reg.0 as usize] == V::Null {
                        pc = at(*offset)
                    }
                }
                Opcode::JNotNull { reg, offset } => {
                    if r[reg.0 as usize] != V::Null {
                        pc = at(*offset)
                    }
                }
                Opcode::JEq { a, b, offset } => {
                    if r[a.0 as usize] == r[b.0 as usize] {
                        pc = at(*offset)
                    }
                }
                Opcode::JNotEq { a, b, offset } => {
                    if r[a.0 as usize] != r[b.0 as usize] {
                        pc = at(*offset)
                    }
                }
                Opcode::JSGte { a, b, offset } => {
                    let (x, y) = ints(a, b, &r);
                    if x >= y {
                        pc = at(*offset)
                    }
                }
                Opcode::JSLt { a, b, offset } => {
                    let (x, y) = ints(a, b, &r);
                    if x < y {
                        pc = at(*offset)
                    }
                }
                Opcode::JULt { a, b, offset } => {
                    let (x, y) = ints(a, b, &r);
                    if (x as u32) < (y as u32) {
                        pc = at(*offset)
                    }
                }
                Opcode::Ret { ret } => return r[ret.0 as usize].clone(),
                // debrief_diag's trace call (prints only).
                Opcode::CallN { dst, .. } => r[dst.0 as usize] = V::Null,
                op @ (Opcode::Call1 { .. }
                | Opcode::Call2 { .. }
                | Opcode::Call3 { .. }
                | Opcode::Call4 { .. }) => {
                    let (dst, callee, a): (Reg, RefFun, Vec<V>) = match op {
                        Opcode::Call1 { dst, fun, arg0 } => {
                            (*dst, *fun, vec![r[arg0.0 as usize].clone()])
                        }
                        Opcode::Call2 {
                            dst,
                            fun,
                            arg0,
                            arg1,
                        } => (
                            *dst,
                            *fun,
                            vec![r[arg0.0 as usize].clone(), r[arg1.0 as usize].clone()],
                        ),
                        Opcode::Call3 {
                            dst,
                            fun,
                            arg0,
                            arg1,
                            arg2,
                        } => (
                            *dst,
                            *fun,
                            vec![
                                r[arg0.0 as usize].clone(),
                                r[arg1.0 as usize].clone(),
                                r[arg2.0 as usize].clone(),
                            ],
                        ),
                        Opcode::Call4 {
                            dst,
                            fun,
                            arg0,
                            arg1,
                            arg2,
                            arg3,
                        } => (
                            *dst,
                            *fun,
                            vec![
                                r[arg0.0 as usize].clone(),
                                r[arg1.0 as usize].clone(),
                                r[arg2.0 as usize].clone(),
                                r[arg3.0 as usize].clone(),
                            ],
                        ),
                        _ => unreachable!(),
                    };
                    let name = match fun_index(code, callee) {
                        Ok(fi) => s(code, code.functions[fi].name).to_string(),
                        Err(_) => s(
                            code,
                            code.natives
                                .iter()
                                .find(|n| n.findex == callee)
                                .expect("callee")
                                .name,
                        )
                        .to_string(),
                    };
                    let v = match name.as_str() {
                        "update" | "push" | "alloc_bytes" | "allocI32" | "toggleClass"
                        | "set_hover" | "set_active" => V::Int(0),
                        "get_isMulti" => V::Bool(true),
                        "getUnitsToRepair" => {
                            let u = w.repair_units.clone();
                            w.list(u)
                        }
                        "getUnitsToHeal" => {
                            let u = w.heal_units.clone();
                            w.list(u)
                        }
                        "get_inventory" => V::Int(0),
                        "getArmorRepairCost" => w.get(&a[1], "cost"),
                        "hasItemWithChest" => V::Bool(w.materials >= int(&a[2])),
                        "countWithChest" => {
                            // "Remedy" then "EstantRemedy": all remedies are plain ones.
                            V::Int(std::mem::take(&mut w.remedies))
                        }
                        "getInjuries" => w.get(&a[0], "injuries"),
                        "get_inf" => w.get(&a[0], "inf"),
                        "rebuild" => {
                            w.rebuilds += 1;
                            V::Int(0)
                        }
                        "set_enable" => {
                            w.set_enables += 1;
                            run(code, w, callee, &a)
                        }
                        other => panic!("unexpected call {other} in replay"),
                    };
                    r[dst.0 as usize] = v;
                }
                o => panic!("replay fn@{} op{i}: {o:?}", fun.0),
            }
        }
        panic!("unbounded replay");
    }

    /// One frame of Debrief.update; returns the buttons' enable afterwards.
    fn frame(code: &Bytecode, w: &mut World, this: &V, materials: i32, remedies: i32) -> (V, V) {
        let update = method(code, obj_type(code, "ui.win.Debrief").unwrap(), "update")
            .unwrap()
            .findex;
        w.materials = materials;
        w.remedies = remedies;
        run(code, w, update, &[this.clone(), V::Null]);
        let (rb, cb) = (w.get(this, "repairBtn"), w.get(this, "cureBtn"));
        (w.get(&rb, "enable"), w.get(&cb, "enable"))
    }

    fn scene(w: &mut World) -> V {
        let inv = w.new_obj(&[]);
        let me = w.new_obj(&[("inventory", inv)]);
        let state = w.new_obj(&[]);
        let game = w.new_obj(&[("me", me), ("state", state)]);
        // A unit whose armour needs 5 raw materials and that has one curable injury.
        let props = w.new_obj(&[("injury", V::Int(1))]);
        let inf = w.new_obj(&[("props", props)]);
        let injury = w.new_obj(&[("inf", inf)]);
        let injuries = w.list(vec![injury]);
        let unit = w.new_obj(&[("cost", V::Int(5)), ("injuries", injuries)]);
        w.repair_units = vec![unit.clone()];
        w.heal_units = vec![unit];
        // init enabled both buttons; the party then lacks the materials / remedies.
        let rb = w.new_obj(&[("enable", V::Bool(true))]);
        let cb = w.new_obj(&[("enable", V::Bool(true))]);
        w.new_obj(&[("game", game), ("repairBtn", rb), ("cureBtn", cb)])
    }

    /// The real update bytecode: vanilla (with the cure fix) rebuilds on every
    /// frame of a lasting disagreement; patched, the first frame sets the buttons
    /// and no frame ever rebuilds, and the buttons follow later changes.
    #[test]
    fn replayed_update_never_rebuilds_and_follows_availability() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut vanilla = read(&image);
        debrief_cure::patch_debrief_cure(&mut vanilla);
        let mut w = World::default();
        let this = scene(&mut w);
        for _ in 0..5 {
            frame(&vanilla, &mut w, &this, 0, 0);
        }
        assert_eq!(w.rebuilds, 5, "vanilla: one full window rebuild per frame");

        let mut patched = read(&image);
        debrief_cure::patch_debrief_cure(&mut patched);
        debrief_diag::patch(&mut patched);
        patch_debrief_enable(&mut patched);
        let patched = read(&write(&patched));
        let mut w = World::default();
        let this = scene(&mut w);
        // Frame 1: repair disagrees -> set in place (update returns, as vanilla did).
        assert_eq!(
            frame(&patched, &mut w, &this, 0, 0),
            (V::Bool(false), V::Bool(true))
        );
        // Frame 2: cure disagrees -> set in place.
        assert_eq!(
            frame(&patched, &mut w, &this, 0, 0),
            (V::Bool(false), V::Bool(false))
        );
        let settled = w.set_enables;
        for _ in 0..10 {
            assert_eq!(
                frame(&patched, &mut w, &this, 0, 0),
                (V::Bool(false), V::Bool(false))
            );
        }
        assert_eq!(w.set_enables, settled, "stable state touches nothing");
        // Materials and remedies arrive (another player took them from the loot).
        frame(&patched, &mut w, &this, 5, 1);
        assert_eq!(
            frame(&patched, &mut w, &this, 5, 1),
            (V::Bool(true), V::Bool(true))
        );
        assert_eq!(w.rebuilds, 0, "patched: update never rebuilds the window");
    }

    /// A comparison that does not read button.enable is refused, image untouched.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let p = plan(&read(&image)).expect("plan");
        let (fi, at) = (p.fi, p.sites[0].at);
        let mut code = read(&image);
        code.functions[fi].ops[at - 2] = Opcode::Label;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_debrief_enable(&mut code);
        assert!(write(&code) == before);
    }
}
