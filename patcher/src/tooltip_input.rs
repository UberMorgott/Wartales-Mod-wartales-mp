// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
// A normal ItemTip can overlap its anchor after TipContent's screen clamps.
// Its weight/star Icons inherit Element's blocking Interactive. SceneEvents
// then outs the anchor, removes that tip, rejects the removed Icon candidate,
// and repeats the anchor's over on the next stationary-pointer check.
// Skip transient passive tip descendants during Scene's hit test. Sticky tips,
// gamepad, other tooltip classes and ordinary controls retain their handlers.

use super::*;
use crate::asm::{push_fn, Asm, Regs, Snap};
use hlbc::types::{RefGlobal, ValBool};

const MARKER: &str = "mpPassiveTooltipInput";
const CONTENTS: [&str; 3] = ["ui.comp.ItemTip", "ui.comp.SkillTip", "ui.comp.TipHelper"];

struct Plan {
    fi: usize,
    at: usize,
    interactive: Reg,
    event: Reg,
    event_t: RefType,
    kind: RefField,
    kind_t: RefType,
    pointer_kinds: Vec<i32>,
    cancel: RefField,
    object: RefType,
    inter_t: RefType,
    tip: RefType,
    bool_: RefType,
    int: RefType,
    dyn_: RefType,
    parent: RefField,
    content: RefField,
    check: RefFun,
    active: RefFun,
    prefs_global: RefGlobal,
    prefs: RefField,
    prefs_t: RefType,
    keep: RefField,
    classes: [(RefGlobal, RefType); 4],
    dbg: usize,
}

fn index(code: &Bytecode, fun: RefFun) -> Result<usize> {
    code.functions
        .iter()
        .position(|f| f.findex == fun)
        .context("function missing")
}

fn class(code: &Bytecode, name: &str) -> Result<(RefGlobal, RefType)> {
    let g = obj(code, obj_type(code, name)?)?
        .global
        .0
        .checked_sub(1)
        .context("class global missing")?;
    Ok((
        RefGlobal(g),
        *code.globals.get(g).context("class global invalid")?,
    ))
}

fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|v| v.as_str() == MARKER) {
        bail!("already applied");
    }
    let object = obj_type(code, "h2d.Object")?;
    let inter_t = obj_type(code, "h2d.Interactive")?;
    let tip = obj_type(code, "ui.comp.TipContent")?;
    let primitive = |pred: fn(&Type) -> bool| {
        code.types
            .iter()
            .position(pred)
            .map(RefType)
            .context("primitive missing")
    };
    let bool_ = primitive(|t| matches!(t, Type::Bool))?;
    let int = primitive(|t| matches!(t, Type::I32))?;
    let dyn_ = primitive(|t| matches!(t, Type::Dyn))?;
    let f = method(code, obj_type(code, "h2d.Scene")?, "handleEvent")?;
    let dispatch = obj(code, inter_t)?
        .protos
        .iter()
        .find(|p| s(code, p.name) == "handleEvent")
        .context("interactive dispatch missing")?;
    let slot = RefField(usize::try_from(dispatch.pindex).context("dispatch not virtual")?);
    let hits: Vec<_> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Opcode::CallMethod { field, args, .. }
                if *field == slot && args.len() == 2 && f.regs[args[0].0 as usize] == inter_t =>
            {
                Some((i, args[0], args[1]))
            }
            _ => None,
        })
        .collect();
    let [(at, interactive, event)] = hits[..] else {
        bail!("expected one interactive dispatch");
    };
    let event_t = f.regs[event.0 as usize];
    let (kind, kind_t) = field(code, event_t, "kind")?;
    let Type::Enum { constructs, .. } = &code.types[kind_t.0] else {
        bail!("event kind is not an enum");
    };
    let pointer_names = [
        "EPush",
        "ERelease",
        "EMove",
        "EOver",
        "EOut",
        "EWheel",
        "EReleaseOutside",
        "ECheck",
    ];
    let pointer_kinds: Vec<i32> = constructs
        .iter()
        .enumerate()
        .filter_map(|(i, c)| pointer_names.contains(&s(code, c.name)).then_some(i as i32))
        .collect();
    if pointer_kinds.len() != pointer_names.len() {
        bail!(
            "unrecognized pointer event constructors: {:?}",
            constructs
                .iter()
                .map(|c| s(code, c.name))
                .collect::<Vec<_>>()
        );
    }
    let cancel = field(code, f.regs[event.0 as usize], "cancel")?.0;
    let propagate = field(code, f.regs[event.0 as usize], "propagate")?.0;
    // The original cancellation branch clears cancel/propagate and continues
    // the candidate loop. Never add/remove/reorder SceneEvents.overList here.
    match f
        .ops
        .get(at + 1..at + 8)
        .context("truncated Scene cancellation continuation")?
    {
        [Opcode::Field {
            dst: b,
            obj: e,
            field: c,
        }, Opcode::JFalse { cond, offset: 5 }, Opcode::Bool {
            dst: z,
            value: ValBool(false),
        }, Opcode::SetField {
            obj: e2,
            field: c2,
            src: z2,
        }, Opcode::Bool {
            dst: z3,
            value: ValBool(false),
        }, Opcode::SetField {
            obj: e3,
            field: p,
            src: z4,
        }, Opcode::JAlways { .. }]
            if *e == event
                && *e2 == event
                && *e3 == event
                && *c == cancel
                && *c2 == cancel
                && *p == propagate
                && b == cond
                && z == z2
                && z3 == z4 => {}
        _ => bail!("unrecognized Scene cancellation continuation"),
    }
    let remove = method(code, obj_type(code, "ui.GlobalUI")?, "removeTip")?;
    let (prefs_global, prefs, prefs_t, keep) = remove
        .ops
        .windows(5)
        .find_map(|ops| match ops {
            [Opcode::GetGlobal { dst: g, global }, Opcode::Field {
                dst: p,
                obj: g2,
                field,
            }, Opcode::NullCheck { reg: p2 }, Opcode::Field {
                obj: p3,
                field: keep,
                ..
            }, Opcode::JTrue { .. }]
                if g == g2 && p == p2 && p == p3 =>
            {
                Some((*global, *field, remove.regs[p.0 as usize], *keep))
            }
            _ => None,
        })
        .context("removeTip keepTips gate missing")?;
    if field_of_virtual(code, prefs_t, "keepTips")?.0 != keep {
        bail!("unexpected sticky-tip flag");
    }
    let sync = method(code, tip, "sync")?;
    let mut actives: Vec<_> = sync
        .ops
        .iter()
        .filter_map(|op| match op {
            Opcode::Call0 { dst, fun }
                if sync.regs[dst.0 as usize] == bool_
                    && s(code, code.functions[index(code, *fun).ok()?].name) == "get_active" =>
            {
                Some(*fun)
            }
            _ => None,
        })
        .collect();
    actives.sort_by_key(|f| f.0);
    actives.dedup();
    let [active] = actives[..] else {
        bail!("tooltip gamepad predicate ambiguous");
    };
    let check = method(code, obj_type(code, "hl.BaseType")?, "check")?.findex;
    let sig = code.functions[index(code, check)?]
        .t
        .as_fun(code)
        .context("check signature missing")?;
    if sig.args != [obj_type(code, "hl.BaseType")?, dyn_] || sig.ret != bool_ {
        bail!("unexpected BaseType.check signature");
    }
    if field(code, tip, "tipContent")?.1 != object || field(code, object, "parent")?.1 != object {
        bail!("unexpected tooltip hierarchy fields");
    }
    Ok(Plan {
        fi: index(code, f.findex)?,
        at,
        interactive,
        event,
        event_t,
        kind,
        kind_t,
        pointer_kinds,
        cancel,
        object,
        inter_t,
        tip,
        bool_,
        int,
        dyn_,
        parent: field(code, object, "parent")?.0,
        content: field(code, tip, "tipContent")?.0,
        check,
        active,
        prefs_global,
        prefs,
        prefs_t,
        keep,
        classes: [
            class(code, "ui.comp.TipContent")?,
            class(code, CONTENTS[0])?,
            class(code, CONTENTS[1])?,
            class(code, CONTENTS[2])?,
        ],
        dbg: debug_file(code, "h2d/Scene.hx")?,
    })
}

fn add_guard(code: &mut Bytecode, p: &Plan) -> Result<RefFun> {
    let mut r = Regs(vec![p.inter_t, p.event_t]);
    let b = r.r(p.bool_);
    let exc = r.r(p.dyn_);
    let remaining = r.r(p.int);
    let zero = r.r(p.int);
    let kind = r.r(p.kind_t);
    let kind_index = r.r(p.int);
    let pointer_kind = r.r(p.int);
    let parent = r.r(p.object);
    let tip = r.r(p.tip);
    let game = r.r(code.globals[p.prefs_global.0]);
    let prefs = r.r(p.prefs_t);
    let classes = p.classes.map(|(_, t)| r.r(t));
    let mut a = Asm::new();
    a.jmp(Opcode::Trap { exc, offset: 0 }, "catch");
    a.jmp(
        Opcode::JNull {
            reg: Reg(1),
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::Field {
        dst: kind,
        obj: Reg(1),
        field: p.kind,
    });
    a.jmp(
        Opcode::JNull {
            reg: kind,
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::EnumIndex {
        dst: kind_index,
        value: kind,
    });
    for value in &p.pointer_kinds {
        a.op(Opcode::Int {
            dst: pointer_kind,
            ptr: int_const(code, *value),
        });
        a.jmp(
            Opcode::JEq {
                a: kind_index,
                b: pointer_kind,
                offset: 0,
            },
            "pointer",
        );
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "allow");
    a.label("pointer");
    a.op(Opcode::Call0 {
        dst: b,
        fun: p.active,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "allow");
    a.op(Opcode::GetGlobal {
        dst: game,
        global: p.prefs_global,
    });
    a.jmp(
        Opcode::JNull {
            reg: game,
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::Field {
        dst: prefs,
        obj: game,
        field: p.prefs,
    });
    a.jmp(
        Opcode::JNull {
            reg: prefs,
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::Field {
        dst: b,
        obj: prefs,
        field: p.keep,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "allow");
    a.jmp(
        Opcode::JNull {
            reg: Reg(0),
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::Field {
        dst: parent,
        obj: Reg(0),
        field: p.parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: parent,
            offset: 0,
        },
        "allow",
    );
    for (reg, (global, _)) in classes.iter().zip(p.classes) {
        a.op(Opcode::GetGlobal { dst: *reg, global });
    }
    a.op(Opcode::Int {
        dst: remaining,
        ptr: int_const(code, 128),
    });
    a.op(Opcode::Int {
        dst: zero,
        ptr: int_const(code, 0),
    });
    a.loop_head("parent");
    a.jmp(
        Opcode::JSLte {
            a: remaining,
            b: zero,
            offset: 0,
        },
        "allow",
    );
    a.op(Opcode::Decr { dst: remaining });
    a.op(Opcode::Call2 {
        dst: b,
        fun: p.check,
        arg0: classes[0],
        arg1: parent,
    });
    a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "tip");
    a.op(Opcode::Field {
        dst: parent,
        obj: parent,
        field: p.parent,
    });
    a.jmp(
        Opcode::JNull {
            reg: parent,
            offset: 0,
        },
        "allow",
    );
    a.jmp(Opcode::JAlways { offset: 0 }, "parent");
    a.label("tip");
    a.op(Opcode::UnsafeCast {
        dst: tip,
        src: parent,
    });
    a.op(Opcode::Field {
        dst: parent,
        obj: tip,
        field: p.content,
    });
    a.jmp(
        Opcode::JNull {
            reg: parent,
            offset: 0,
        },
        "allow",
    );
    for reg in &classes[1..] {
        a.op(Opcode::Call2 {
            dst: b,
            fun: p.check,
            arg0: *reg,
            arg1: parent,
        });
        a.jmp(Opcode::JTrue { cond: b, offset: 0 }, "done");
    }
    a.label("allow");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.label("done");
    a.op(Opcode::EndTrap { exc });
    a.op(Opcode::Ret { ret: b });
    a.label("catch");
    a.op(Opcode::Bool {
        dst: b,
        value: ValBool(false),
    });
    a.op(Opcode::Ret { ret: b });
    let fun = push_fn(
        code,
        vec![p.inter_t, p.event_t],
        p.bool_,
        r.0,
        a.finish(),
        p.dbg,
    )?;
    crate::asm::string_ref(code, MARKER);
    Ok(fun)
}

fn block(p: &Plan, helper: RefFun, b: Reg) -> Vec<Opcode> {
    vec![
        Opcode::Call2 {
            dst: b,
            fun: helper,
            arg0: p.interactive,
            arg1: p.event,
        },
        Opcode::JFalse { cond: b, offset: 2 },
        Opcode::SetField {
            obj: p.event,
            field: p.cancel,
            src: b,
        },
        Opcode::JAlways { offset: 1 },
    ]
}

pub(crate) fn patch(code: &mut Bytecode) {
    let p = match plan(code) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tooltip input skipped: {e:#}");
            return;
        }
    };
    let snap = Snap::take(code);
    match add_guard(code, &p) {
        Ok(helper) => {
            let f = &mut code.functions[p.fi];
            let b = Reg(f.regs.len() as u32);
            f.regs.push(p.bool_);
            insert_ops(f, p.at, block(&p, helper, b));
            eprintln!("patched tooltip input fn@{}: passive item decorations skip mouse hit-test (guard fn@{})", f.findex.0, helper.0);
        }
        Err(e) => {
            snap.restore(code);
            eprintln!("tooltip input skipped: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, read, write, HLBOOT};

    fn fixture() -> Bytecode {
        read(&std::fs::read(HLBOOT).expect("installed bytecode fixture"))
    }

    fn installed_css() -> String {
        use std::io::{Read, Seek, SeekFrom};
        fn num(c: &mut Cursor<Vec<u8>>, n: usize) -> Vec<u8> {
            let mut b = vec![0; n];
            c.read_exact(&mut b).unwrap();
            b
        }
        fn entry(c: &mut Cursor<Vec<u8>>, prefix: &str) -> Option<(u64, usize)> {
            let n = num(c, 1)[0] as usize;
            let name = String::from_utf8(num(c, n)).unwrap();
            let flags = num(c, 1)[0];
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if flags & 1 != 0 {
                let count = i32::from_le_bytes(num(c, 4).try_into().unwrap());
                let mut found = None;
                for _ in 0..count {
                    if let Some(e) = entry(c, &path) {
                        found = Some(e);
                    }
                }
                found
            } else {
                let pos = if flags & 2 != 0 {
                    f64::from_le_bytes(num(c, 8).try_into().unwrap()) as u64
                } else {
                    u32::from_le_bytes(num(c, 4).try_into().unwrap()) as u64
                };
                let size = u32::from_le_bytes(num(c, 4).try_into().unwrap()) as usize;
                num(c, 4);
                (path == "ui/style.css").then_some((pos, size))
            }
        }
        let path = std::path::Path::new(HLBOOT)
            .parent()
            .unwrap()
            .join("assets.pak");
        let mut f = std::fs::File::open(path).unwrap();
        let mut head = [0; 12];
        f.read_exact(&mut head).unwrap();
        assert_eq!(&head[..3], b"PAK");
        let size = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
        let mut header = vec![0; size - 12];
        f.read_exact(&mut header).unwrap();
        let (pos, len) = entry(&mut Cursor::new(header), "").expect("actual UI stylesheet");
        f.seek(SeekFrom::Start(size as u64 + pos)).unwrap();
        let mut css = vec![0; len];
        f.read_exact(&mut css).unwrap();
        String::from_utf8(css).unwrap()
    }
    fn css_value(css: &str, selector: &str, name: &str) -> Vec<f64> {
        let body = css
            .split('}')
            .find_map(|block| {
                let (selectors, body) = block.split_once('{')?;
                selectors
                    .split(',')
                    .any(|s| s.trim() == selector)
                    .then_some(body)
            })
            .unwrap_or_else(|| panic!("CSS selector {selector}"));
        body.split(';')
            .find_map(|line| {
                let (property, value) = line.split_once(':')?;
                (property.trim() == name).then(|| {
                    value
                        .split_whitespace()
                        .map(|n| n.trim_end_matches("px").parse().unwrap())
                        .collect()
                })
            })
            .unwrap_or_else(|| panic!("CSS property {selector} {name}"))
    }

    #[test]
    fn actual_template_clamps_reach_hover_cycle_and_scope_has_no_control_subclasses() {
        let mut code = fixture();
        let css = installed_css();
        let padding = css_value(&css, "tip-content .with-title", "padding")[0];
        let main_w = css_value(&css, "item-tip .with-title", "width")[0] + padding * 2.;
        let min_h = css_value(&css, "tip-content .with-title", "min-height")[0] + padding * 2.;
        let helper_pad = css_value(&css, "tip-helper", "padding");
        let helper_w = css_value(&css, "tip-helper", "width")[0] + helper_pad[1] + helper_pad[3];
        let slot_w = css_value(&css, "item-slot", "width")[0];
        let slot_h = css_value(&css, "item-slot", "height")[0];
        let icon_w = css_value(
            &css,
            "item-tip .with-title .head .title icon.qualitystar bitmap",
            "width",
        )[0];
        let head_h = css_value(&css, "tip-content .with-title .head", "height")[0];
        let head_top = css_value(&css, "tip-content .with-title .head", "margin-top")[0];
        let icon_offset = css_value(
            &css,
            "item-tip .with-title .head .title icon.qualitystar",
            "offset-y",
        )[0];
        let tc = obj_type(&code, "ui.comp.TipContent").unwrap();
        let sync = method(&code, tc, "sync").unwrap();
        let float_on_line = |line: usize, value: f64| {
            sync.ops
                .iter()
                .enumerate()
                .find_map(|(i, op)| match op {
                    Opcode::Float { ptr, .. }
                        if sync.debug_info.as_ref().unwrap()[i].1 == line
                            && code.floats[ptr.0] == value =>
                    {
                        Some(code.floats[ptr.0])
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("source coefficient TipContent.hx:{line}"))
        };
        let half = float_on_line(53, 0.5);
        let side = float_on_line(147, 0.65);
        let border = float_on_line(137, 0.01);
        // Source minimum y is the Int40 selected at TipContent.hx:138.
        assert!(sync.ops.iter().enumerate().any(|(i, op)| matches!(op, Opcode::Int { ptr, .. } if sync.debug_info.as_ref().unwrap()[i].1 == 138 && code.ints[ptr.0] == 40)));
        let y = 40.;
        let pointer_y = y + padding + head_top + head_h / 2. + icon_offset;
        assert!(pointer_y >= y && pointer_y < y + slot_h);
        assert!(y - min_h - 5. < y && y - min_h * half < y);
        // Two real 300px helper panels + their CSS horizontal padding and the
        // custom 5px gap. Main/helper width conventions don't affect reachability:
        // test both content-width and outer-width interpretations explicitly.
        for (t, w) in [
            (main_w, helper_w * 2. + 5.),
            (
                main_w - 2. * padding,
                helper_w * 2. + 5. - 2. * (helper_pad[1] + helper_pad[3]),
            ),
        ] {
            let screen = t + w + 20.;
            // b is the *actual* header icon center within its tip; demonstrate
            // the whole possible header range rather than invent a text width.
            for b in (icon_w as usize..(t - icon_w) as usize).map(|b| b as f64) {
                let anchor = w + b - slot_w * half;
                let source_x = anchor + slot_w * half - t * half - t * side;
                assert!(source_x >= 0. && source_x < w);
                assert!(
                    source_x + t + w + (screen * border).ceil() > screen,
                    "source helper-left predicate"
                );
                assert!(
                    w + t + (screen * border).ceil() <= screen,
                    "final right clamp preserves custom x"
                );
                assert!(anchor >= 0. && anchor + slot_w <= screen);
                let pointer_x = w + b;
                assert!(anchor <= pointer_x && pointer_x < anchor + slot_w);
                // Custom left clamp moves the tip to w; its icon center is now
                // exactly inside the original slot under the stationary pointer.
                assert_eq!(pointer_x, w + b);
            }
        }
        let ctor = method(
            &code,
            obj_type(&code, "ui.comp.ItemTip").unwrap(),
            "__constructor__",
        )
        .unwrap();
        for icon in ["QualityStar", "QualityStar_10", "Weight"] {
            let global = code
                .constants
                .iter()
                .flatten()
                .find(|c| matches!(c.fields[..], [s0, _] if code.strings[s0].as_str() == icon))
                .unwrap()
                .global;
            assert!(
                ctor.ops
                    .iter()
                    .any(|op| matches!(op, Opcode::GetGlobal { global: g, .. } if *g == global)),
                "actual template icon {icon}"
            );
        }
        let icon_ctor = method(
            &code,
            obj_type(&code, "ui.comp.Icon").unwrap(),
            "__constructor__",
        )
        .unwrap();
        let element = obj_type(&code, "ui.comp.Element").unwrap();
        let element_ctor = method(&code, element, "__constructor__").unwrap();
        assert!(icon_ctor
            .ops
            .iter()
            .any(|op| matches!(op, Opcode::CallN { fun, .. } if *fun == element_ctor.findex)));
        let make = method(&code, element, "makeInteractive").unwrap().findex;
        assert!(element_ctor
            .ops
            .iter()
            .any(|op| matches!(op, Opcode::Call1 { fun, .. } if *fun == make)));
        let events = obj_type(&code, "hxd.SceneEvents").unwrap();
        let emit = method(&code, events, "emitEvent").unwrap();
        let on_line = |line| {
            emit.ops
                .iter()
                .enumerate()
                .filter(|(i, op)| {
                    emit.debug_info.as_ref().unwrap()[*i].1 == line
                        && matches!(op, Opcode::CallMethod { .. })
                })
                .map(|(i, _)| i)
                .next()
                .unwrap()
        };
        assert!(
            on_line(254) < on_line(265) && on_line(265) < on_line(266),
            "out removes tip before pending over visibility test/callback"
        );
        let check = method(&code, events, "checkEvents").unwrap();
        assert!(
            check
                .ops
                .iter()
                .enumerate()
                .any(|(i, op)| check.debug_info.as_ref().unwrap()[i].1 == 381
                    && matches!(op, Opcode::Call2 { fun, .. } if *fun == emit.findex)),
            "stationary frame rechecks pointer"
        );
        // All current subclasses in the protected scope inherit these passive
        // templates; reject an unexpected mouse callback override in the fixture.
        let p = plan(&code).unwrap();
        let vm = Replay::new(&code, &p, RefFun(0));
        for root in CONTENTS {
            let root_t = obj_type(&code, root).unwrap();
            for (i, t) in code
                .types
                .iter()
                .enumerate()
                .filter(|(_, t)| t.get_type_obj().is_some())
            {
                if !vm.subclass(RefType(i), root_t) {
                    continue;
                }
                let o = t.get_type_obj().unwrap();
                assert!(
                    !o.protos
                        .iter()
                        .any(|p| ["onClick", "onPush", "onLocalClick"].contains(&s(&code, p.name))),
                    "unexpected tooltip callback subclass {}",
                    s(&code, o.name)
                );
            }
        }
        crate::tip_overflow::patch_tip_overflow(&mut code);
        let sync = method(&code, tc, "sync").unwrap();
        let x = field(&code, tc, "x").unwrap().0;
        assert!(sync.ops.windows(8).any(|ops| matches!(ops, [Opcode::GetThis { field, .. }, Opcode::Add { .. }, Opcode::Float { .. }, Opcode::JNotLt { .. }, Opcode::Neg { .. }, Opcode::Bool { .. }, Opcode::SetThis { .. }, Opcode::SetThis { field: x2, .. }] if *field == x && *x2 == x)), "actual custom left clamp");
    }

    // Execute the emitted guard and the game's actual dispatch/click opcodes.
    // Only engine boundaries (BaseType.check, pad mode and callbacks) are stubs.
    // This is intentionally limited to these real bytecode paths, not a second
    // implementation of the intended filter or of SceneEvents.
    #[derive(Clone, Debug, PartialEq)]
    enum V {
        Null,
        Bool(bool),
        Int(i32),
        Object(usize),
        Class(RefType),
        Callback(usize, &'static str),
    }
    struct Object {
        t: RefType,
        fields: std::collections::HashMap<usize, V>,
    }
    struct Replay<'a> {
        code: &'a Bytecode,
        p: &'a Plan,
        objects: Vec<Object>,
        globals: std::collections::HashMap<usize, V>,
        pad: bool,
        throw_pad: bool,
        clicks: Vec<usize>,
        callbacks: Vec<(usize, &'static str)>,
        guard: RefFun,
        dispatch: RefFun,
    }
    impl<'a> Replay<'a> {
        fn new(code: &'a Bytecode, p: &'a Plan, guard: RefFun) -> Self {
            let dispatch = method(code, p.inter_t, "handleEvent").unwrap().findex;
            Self {
                code,
                p,
                objects: vec![],
                globals: Default::default(),
                pad: false,
                throw_pad: false,
                clicks: vec![],
                callbacks: vec![],
                guard,
                dispatch,
            }
        }
        fn object(&mut self, t: RefType, values: &[(&str, V)]) -> V {
            let fields = values
                .iter()
                .map(|(name, v)| {
                    let f = match &self.code.types[t.0] {
                        Type::Virtual { fields } => fields
                            .iter()
                            .position(|f| s(self.code, f.name) == *name)
                            .unwrap(),
                        _ => field(self.code, t, name).unwrap().0 .0,
                    };
                    (f, v.clone())
                })
                .collect();
            let id = self.objects.len();
            self.objects.push(Object { t, fields });
            V::Object(id)
        }
        fn get(&self, o: &V, f: RefField) -> V {
            let V::Object(i) = o else {
                panic!("field on {o:?}");
            };
            self.objects[*i]
                .fields
                .get(&f.0)
                .cloned()
                .unwrap_or(V::Null)
        }
        fn set(&mut self, o: &V, f: RefField, v: V) {
            let V::Object(i) = o else {
                panic!("store on {o:?}");
            };
            self.objects[*i].fields.insert(f.0, v);
        }
        fn subclass(&self, mut t: RefType, of: RefType) -> bool {
            loop {
                if t == of {
                    return true;
                }
                let Some(parent) = obj(self.code, t).unwrap().super_ else {
                    return false;
                };
                t = parent;
            }
        }
        fn global(&self, g: RefGlobal) -> V {
            if let Some(v) = self.globals.get(&g.0) {
                return v.clone();
            }
            for (i, t) in self.code.types.iter().enumerate() {
                if t.get_type_obj().is_some_and(|o| o.global.0 == g.0 + 1) {
                    return V::Class(RefType(i));
                }
            }
            V::Null
        }
        fn run(&mut self, fun: RefFun, args: &[V], start: usize, stop: Option<usize>) -> V {
            let f = self.code.functions[index(self.code, fun).unwrap()].clone();
            let mut r = vec![V::Null; f.regs.len()];
            r[..args.len()].clone_from_slice(args);
            let mut pc = start;
            let mut catch = None;
            for _ in 0..5000 {
                if Some(pc) == stop {
                    return V::Null;
                }
                let i = pc;
                pc += 1;
                let at = |o: i32| (i as i32 + 1 + o) as usize;
                match &f.ops[i] {
                    Opcode::Trap { offset, .. } => catch = Some(at(*offset)),
                    Opcode::EndTrap { .. } => catch = None,
                    Opcode::Label => {}
                    Opcode::NullCheck { reg } => assert_ne!(r[reg.0 as usize], V::Null),
                    Opcode::Null { dst } => r[dst.0 as usize] = V::Null,
                    Opcode::Bool { dst, value } => r[dst.0 as usize] = V::Bool(value.0),
                    Opcode::Int { dst, ptr } => r[dst.0 as usize] = V::Int(self.code.ints[ptr.0]),
                    Opcode::GetGlobal { dst, global } => r[dst.0 as usize] = self.global(*global),
                    Opcode::Field { dst, obj, field } => {
                        r[dst.0 as usize] = self.get(&r[obj.0 as usize], *field)
                    }
                    Opcode::GetThis { dst, field } => r[dst.0 as usize] = self.get(&r[0], *field),
                    Opcode::SetField { obj, field, src } => {
                        self.set(&r[obj.0 as usize], *field, r[src.0 as usize].clone())
                    }
                    Opcode::SetThis { field, src } => {
                        self.set(&r[0], *field, r[src.0 as usize].clone())
                    }
                    Opcode::UnsafeCast { dst, src }
                    | Opcode::SafeCast { dst, src }
                    | Opcode::ToVirtual { dst, src }
                    | Opcode::Mov { dst, src } => r[dst.0 as usize] = r[src.0 as usize].clone(),
                    Opcode::EnumIndex { dst, value } => {
                        r[dst.0 as usize] = r[value.0 as usize].clone()
                    }
                    Opcode::Decr { dst } => {
                        let V::Int(v) = &mut r[dst.0 as usize] else {
                            panic!("counter");
                        };
                        *v -= 1;
                    }
                    Opcode::JAlways { offset } => pc = at(*offset),
                    Opcode::JNull { reg, offset } if r[reg.0 as usize] == V::Null => {
                        pc = at(*offset)
                    }
                    Opcode::JNotNull { reg, offset } if r[reg.0 as usize] != V::Null => {
                        pc = at(*offset)
                    }
                    Opcode::JTrue { cond, offset } if r[cond.0 as usize] == V::Bool(true) => {
                        pc = at(*offset)
                    }
                    Opcode::JFalse { cond, offset } if r[cond.0 as usize] == V::Bool(false) => {
                        pc = at(*offset)
                    }
                    Opcode::JEq { a, b, offset } if r[a.0 as usize] == r[b.0 as usize] => {
                        pc = at(*offset)
                    }
                    Opcode::JNotEq { a, b, offset } if r[a.0 as usize] != r[b.0 as usize] => {
                        pc = at(*offset)
                    }
                    Opcode::JSLte { a, b, offset } if matches!((&r[a.0 as usize], &r[b.0 as usize]), (V::Int(x), V::Int(y)) if x <= y) => {
                        pc = at(*offset)
                    }
                    Opcode::JNull { .. }
                    | Opcode::JNotNull { .. }
                    | Opcode::JTrue { .. }
                    | Opcode::JFalse { .. }
                    | Opcode::JEq { .. }
                    | Opcode::JNotEq { .. }
                    | Opcode::JSLte { .. } => {}
                    Opcode::Switch { reg, offsets, end } => {
                        let V::Int(k) = r[reg.0 as usize] else {
                            panic!("kind");
                        };
                        pc = at(offsets.get(k as usize).copied().unwrap_or(*end));
                    }
                    Opcode::Call0 { dst, fun } if *fun == self.p.active => {
                        if self.throw_pad {
                            pc = catch.expect("guard must fail open");
                        } else {
                            r[dst.0 as usize] = V::Bool(self.pad);
                        }
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } if *fun == self.p.check => {
                        let V::Class(t) = r[arg0.0 as usize] else {
                            panic!("check class");
                        };
                        r[dst.0 as usize] = V::Bool(match r[arg1.0 as usize] {
                            V::Object(o) => self.subclass(self.objects[o].t, t),
                            _ => false,
                        });
                    }
                    Opcode::Call2 {
                        dst,
                        fun,
                        arg0,
                        arg1,
                    } if *fun == self.guard => {
                        r[dst.0 as usize] = self.run(
                            *fun,
                            &[r[arg0.0 as usize].clone(), r[arg1.0 as usize].clone()],
                            0,
                            None,
                        );
                    }
                    Opcode::CallMethod { dst, args, .. } => {
                        self.run(
                            self.dispatch,
                            &[r[args[0].0 as usize].clone(), r[args[1].0 as usize].clone()],
                            0,
                            None,
                        );
                        r[dst.0 as usize] = V::Null;
                    }
                    Opcode::CallClosure { dst, fun, .. } => {
                        let V::Callback(o, name) = r[fun.0 as usize] else {
                            panic!("callback {:?}", r[fun.0 as usize]);
                        };
                        self.callbacks.push((o, name));
                        if name == "click" {
                            self.clicks.push(o);
                        }
                        r[dst.0 as usize] = V::Null;
                    }
                    Opcode::Ret { ret } => return r[ret.0 as usize].clone(),
                    o => panic!("replay fn@{} op{i}: {o:?}", fun.0),
                }
            }
            panic!("unbounded replay");
        }
        fn inter(&mut self, parent: V) -> V {
            let o = self.object(
                self.p.inter_t,
                &[
                    ("parent", parent),
                    ("isEllipse", V::Bool(false)),
                    ("propagateEvents", V::Bool(false)),
                    ("cancelEvents", V::Bool(false)),
                    ("enableRightButton", V::Bool(false)),
                    ("mouseDownButton", V::Int(-1)),
                    ("lastClickFrame", V::Int(-1)),
                    ("allowMultiClick", V::Bool(false)),
                ],
            );
            let V::Object(i) = o else {
                unreachable!();
            };
            for (field_name, callback) in [
                ("onPush", "push"),
                ("onRelease", "release"),
                ("onClick", "click"),
                ("onMove", "move"),
                ("onCheck", "check"),
                ("onOver", "over"),
                ("onOut", "out"),
                ("onWheel", "wheel"),
                ("onReleaseOutside", "outside"),
            ] {
                self.set(
                    &o,
                    field(self.code, self.p.inter_t, field_name).unwrap().0,
                    V::Callback(i, callback),
                );
            }
            o
        }
        fn event(&mut self, kind: i32) -> V {
            self.object(
                self.p.event_t,
                &[
                    ("kind", V::Int(kind)),
                    ("button", V::Int(0)),
                    ("cancel", V::Bool(false)),
                    ("propagate", V::Bool(false)),
                ],
            )
        }
        fn candidate(
            &mut self,
            original: &'a Bytecode,
            patched: bool,
            candidate: V,
            event: V,
        ) -> V {
            let current = self.code;
            if !patched {
                self.code = original;
            }
            let f = &self.code.functions[self.p.fi];
            let fun = f.findex;
            let mut args = vec![V::Null; f.regs.len()];
            args[self.p.event.0 as usize] = event;
            args[self.p.interactive.0 as usize] = candidate;
            let loop_at = jump_targets(&original.functions[self.p.fi], self.p.at + 7)[0];
            let result = self.run(fun, &args, self.p.at, Some(loop_at));
            self.code = current;
            result
        }
    }

    #[test]
    fn replays_native_candidate_and_click_paths_preserving_sticky_pad_and_controls() {
        let before = fixture();
        let p = plan(&before).unwrap();
        let mut code = fixture();
        patch(&mut code);
        let guard = code.functions.last().unwrap().findex;
        let mut vm = Replay::new(&code, &p, guard);
        let prefs = vm.object(p.prefs_t, &[("keepTips", V::Bool(false))]);
        let game = vm.object(code.globals[p.prefs_global.0], &[("PREFS", prefs.clone())]);
        vm.globals.insert(p.prefs_global.0, game);
        let timer = obj_type(&code, "hxd.$Timer").unwrap();
        let timer_object = vm.object(timer, &[("frameCount", V::Int(1))]);
        let dispatch = method(&code, p.inter_t, "handleEvent").unwrap();
        let timer_global = dispatch
            .ops
            .iter()
            .find_map(|op| match op {
                Opcode::GetGlobal { dst, global } if dispatch.regs[dst.0 as usize] == timer => {
                    Some(*global)
                }
                _ => None,
            })
            .unwrap();
        vm.globals.insert(timer_global.0, timer_object);
        let content = vm.object(obj_type(&code, CONTENTS[0]).unwrap(), &[]);
        let tip = vm.object(p.tip, &[("tipContent", content)]);
        let icon = vm.object(
            obj_type(&code, "ui.comp.Icon").unwrap(),
            &[("parent", tip.clone())],
        );
        let decoration = vm.inter(icon);
        let slot = vm.object(obj_type(&code, "ui.comp.Slot").unwrap(), &[]);
        let slot = vm.inter(slot);
        // Before: icon consumes push, is removed by anchor-out before release;
        // the new slot receives release without its own mouseDownButton.
        let push = vm.event(0);
        assert_ne!(
            vm.candidate(&before, false, decoration.clone(), push),
            V::Null
        );
        let release = vm.event(1);
        vm.candidate(&before, false, slot.clone(), release);
        assert!(
            vm.clicks.is_empty(),
            "native push/release identity regression"
        );
        // After: the real cancellation branch rejects the icon; native slot
        // handles both events and its original click predicate succeeds once.
        let push = vm.event(0);
        assert_eq!(
            vm.candidate(&before, true, decoration.clone(), push.clone()),
            V::Null
        );
        vm.candidate(&before, true, slot.clone(), push);
        let release = vm.event(1);
        assert_eq!(
            vm.candidate(&before, true, decoration.clone(), release.clone()),
            V::Null
        );
        vm.candidate(&before, true, slot.clone(), release);
        assert_eq!(vm.clicks.len(), 1);
        for k in &p.pointer_kinds {
            let event = vm.event(*k);
            assert_eq!(
                vm.run(guard, &[decoration.clone(), event], 0, None),
                V::Bool(true)
            );
        }
        for k in [6, 7, 8, 9, 11] {
            let event = vm.event(k);
            assert_eq!(
                vm.run(guard, &[decoration.clone(), event], 0, None),
                V::Bool(false),
                "keyboard/focus/text event{k}"
            );
        }
        for content_name in CONTENTS {
            let content = vm.object(obj_type(&code, content_name).unwrap(), &[]);
            vm.set(&tip, p.content, content);
            let event = vm.event(2);
            assert_eq!(
                vm.run(guard, &[decoration.clone(), event], 0, None),
                V::Bool(true)
            );
        }
        let other = vm.object(obj_type(&code, "ui.comp.Button").unwrap(), &[]);
        vm.set(&tip, p.content, other);
        let event = vm.event(2);
        assert_eq!(
            vm.run(guard, &[decoration.clone(), event.clone()], 0, None),
            V::Bool(false),
            "custom control content"
        );
        let content = vm.object(obj_type(&code, CONTENTS[0]).unwrap(), &[]);
        vm.set(&tip, p.content, content);
        vm.set(&prefs, p.keep, V::Bool(true));
        assert_eq!(
            vm.run(guard, &[decoration.clone(), event.clone()], 0, None),
            V::Bool(false),
            "keepTips"
        );
        vm.set(&prefs, p.keep, V::Bool(false));
        vm.pad = true;
        assert_eq!(
            vm.run(guard, &[decoration.clone(), event.clone()], 0, None),
            V::Bool(false),
            "gamepad"
        );
        vm.pad = false;
        vm.throw_pad = true;
        assert_eq!(
            vm.run(guard, &[decoration.clone(), event.clone()], 0, None),
            V::Bool(false),
            "fail open on error"
        );
        vm.throw_pad = false;
        vm.globals.insert(p.prefs_global.0, V::Null);
        assert_eq!(
            vm.run(guard, &[decoration.clone(), event.clone()], 0, None),
            V::Bool(false),
            "startup"
        );
        // A malformed cyclic hierarchy cannot trap the pointer loop forever.
        let cycle = vm.object(p.object, &[]);
        vm.set(&cycle, p.parent, cycle.clone());
        let cyclic_inter = vm.inter(cycle);
        let prefs = vm.object(p.prefs_t, &[("keepTips", V::Bool(false))]);
        let game = vm.object(code.globals[p.prefs_global.0], &[("PREFS", prefs)]);
        vm.globals.insert(p.prefs_global.0, game);
        assert_eq!(
            vm.run(guard, &[cyclic_inter, event], 0, None),
            V::Bool(false),
            "bounded ancestor walk"
        );
    }

    #[test]
    fn preserves_original_scene_ops_and_validates_full_patch_chain() {
        let mut code = fixture();
        let p = plan(&code).expect("grounded scene plan");
        let before = read(&write(&code));
        patch(&mut code);
        let back = read(&write(&code));
        let helper = back.functions.last().unwrap();
        assert!(back.strings.iter().any(|s| s.as_str() == MARKER));
        check_types(&back, helper, 0..helper.ops.len());
        check_flow(helper);
        for (i, original) in before.functions.iter().enumerate() {
            let mut expected = original.clone();
            if i == p.fi {
                let b = Reg(expected.regs.len() as u32);
                expected.regs.push(p.bool_);
                insert_ops(&mut expected, p.at, block(&p, helper.findex, b));
                check_types(&back, &back.functions[i], p.at..p.at + 4);
                check_flow(&back.functions[i]);
            }
            assert_eq!(
                format!("{:?}", expected),
                format!("{:?}", back.functions[i]),
                "fn@{}",
                original.findex.0
            );
        }
        let bytes = write(&back);
        let mut again = read(&bytes);
        patch(&mut again);
        assert_eq!(write(&again), bytes);
        let full = crate::patch_image(&std::fs::read(HLBOOT).unwrap()).expect("full chain");
        let full = read(&full);
        assert!(full.strings.iter().any(|s| s.as_str() == MARKER));
    }
}
