// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Trace camp choice delivery and aptitude awards without changing gameplay.
// FriendlyFire/Aider rewards Target (the attacker), not Self (the injured
// speaker). giveGains appends a Strength/Dexterity preselection, then calls
// addAptitudePoints(recipient, 1). The logs distinguish a missing click RPC,
// an unresolved/old button ID, the selected choice, and the actual recipient
// and aptitude count on either side of that grant.

use super::*;
use crate::diag::{index_of, static_fn};
use crate::job_xp::str_global;
use hlbc::types::{RefGlobal, RefInt, ValBool};

const REQUEST: &str = "mp: dialog choice request";
const RESOLVED: &str = "mp: dialog choice resolved";
const GAINS: &str = "mp: confession choice gains";
const BEFORE: &str = "mp: confession aptitude before";
const AFTER: &str = "mp: confession aptitude after";

struct Part {
    label: &'static str,
    root: Reg,
    path: Vec<(RefField, RefType)>,
    /// None prints whether the root is non-null.
    value: Option<RefType>,
}

struct Probe {
    fi: usize,
    at: usize,
    tag: &'static str,
    parts: Vec<Part>,
}

struct Plan {
    probes: Vec<Probe>,
    println: RefFun,
    string: RefFun,
    add: RefFun,
    str_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    bool_t: RefType,
    minus_one: RefInt,
}

fn plan(code: &Bytecode) -> Result<Plan> {
    if code.strings.iter().any(|s| s.as_str() == BEFORE) {
        bail!("already applied");
    }
    let str_t = obj_type(code, "String")?;
    let bool_t = prim_type(code, "bool", |t| matches!(t, Type::Bool))?;
    let i32_t = prim_type(code, "i32", |t| matches!(t, Type::I32))?;
    let dyn_t = prim_type(code, "dynamic", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "void", |t| matches!(t, Type::Void))?;
    let minus_one = RefInt(
        code.ints
            .iter()
            .position(|&v| v == -1)
            .context("no -1 constant")?,
    );
    let dialog_t = obj_type(code, "ui.win.Dialog")?;
    let conf_t = obj_type(code, "st.player.Confession")?;
    let unit_t = obj_type(code, "st.Unit")?;
    let game_t = obj_type(code, "Game")?;
    let current = field(code, dialog_t, "currentDialog")?;
    let dg = field(code, dialog_t, "game")?;
    let cg = field(code, conf_t, "game")?;
    let auth = field(code, game_t, "isAuth")?;
    let uid = field(code, unit_t, "__uid")?;
    let aptitude = field(code, unit_t, "aptitudePoints")?;
    if current.1 != str_t
        || dg.1 != game_t
        || cg.1 != game_t
        || auth.1 != bool_t
        || uid.1 != i32_t
        || aptitude.1 != i32_t
    {
        bail!("unexpected dialog/game/unit field types");
    }
    let part = |label, root, path: Vec<_>, value| Part {
        label,
        root,
        path,
        value: Some(value),
    };
    let request = method(code, dialog_t, "setClick")?;
    let resolved = method(code, dialog_t, "setClick__impl")?;
    if fun_args(code, request) != [dialog_t, str_t] || fun_args(code, resolved) != [dialog_t, str_t]
    {
        bail!("unexpected setClick signature");
    }
    let lookup = method(code, obj_type(code, "ui.Window")?, "getElementByNetID")?.findex;
    let [(lookup_at, found)] = resolved
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Call2 {
                dst,
                fun,
                arg0: Reg(0),
                arg1: Reg(1),
            } if *fun == lookup => Some((i, *dst)),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        bail!("setClick: no unique ID lookup");
    };
    if !matches!(resolved.ops.get(lookup_at + 1), Some(Opcode::JNull { reg, .. }) if *reg == found)
        || !resolved
            .ops
            .iter()
            .any(|op| matches!(op, Opcode::CallClosure { args, .. } if args.is_empty()))
    {
        bail!("setClick: unexpected lookup/callback gate");
    }
    let click_parts = || {
        vec![
            part(" requested=", Reg(1), vec![], str_t),
            part(" current=", Reg(0), vec![current], str_t),
            part(" auth=", Reg(0), vec![dg, auth], bool_t),
        ]
    };
    let mut resolve_parts = click_parts();
    resolve_parts.push(Part {
        label: " found=",
        root: found,
        path: vec![],
        value: None,
    });
    let gain = method(code, conf_t, "giveGains")?;
    let ga = fun_args(code, gain);
    if ga.len() != 5 || ga[0] != conf_t || ga[2] != unit_t {
        bail!("giveGains: unexpected signature");
    }
    let Type::Virtual { fields } = &code.types[ga[1].0] else {
        bail!("choice is not virtual");
    };
    let verb = fields
        .iter()
        .position(|f| s(code, f.name) == "verb")
        .context("choice verb missing")?;
    if fields[verb].t != str_t {
        bail!("verb not String");
    }
    let add_aptitude = method(code, unit_t, "addAptitudePoints")?.findex;
    let [(award_at, recipient)] = gain
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Call2 { fun, arg0, .. } if *fun == add_aptitude => Some((i, *arg0)),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        bail!("giveGains: no unique aptitude award");
    };
    if gain.regs[recipient.0 as usize] != unit_t
        || !matches!(gain.ops.get(award_at - 1), Some(Opcode::Int { ptr, .. }) if code.ints[ptr.0] == 1)
    {
        bail!("giveGains: unexpected aptitude recipient/amount");
    }
    let award_parts = || {
        vec![
            part(" verb=", Reg(1), vec![(RefField(verb), str_t)], str_t),
            part(" recipient_uid=", recipient, vec![uid], i32_t),
            part(" aptitude=", recipient, vec![aptitude], i32_t),
            part(" auth=", Reg(0), vec![cg, auth], bool_t),
        ]
    };
    Ok(Plan {
        probes: vec![
            Probe {
                fi: index_of(code, request.findex)?,
                at: 0,
                tag: REQUEST,
                parts: click_parts(),
            },
            Probe {
                fi: index_of(code, resolved.findex)?,
                at: lookup_at + 1,
                tag: RESOLVED,
                parts: resolve_parts,
            },
            Probe {
                fi: index_of(code, gain.findex)?,
                at: 0,
                tag: GAINS,
                parts: vec![
                    part(" verb=", Reg(1), vec![(RefField(verb), str_t)], str_t),
                    part(" speaker_uid=", Reg(2), vec![uid], i32_t),
                    part(" auth=", Reg(0), vec![cg, auth], bool_t),
                ],
            },
            Probe {
                fi: index_of(code, gain.findex)?,
                at: award_at,
                tag: BEFORE,
                parts: award_parts(),
            },
            Probe {
                fi: index_of(code, gain.findex)?,
                at: award_at + 1,
                tag: AFTER,
                parts: award_parts(),
            },
        ],
        println: static_fn(code, "$Sys", "println")?.findex,
        string: static_fn(code, "$Std", "string")?.findex,
        add: method(code, str_t, "__add__")?.findex,
        str_t,
        dyn_t,
        void_t,
        bool_t,
        minus_one,
    })
}

fn probe_ops(
    p: &Plan,
    f: &mut Function,
    tag: RefGlobal,
    labels: &[RefGlobal],
    null: RefGlobal,
    parts: &[Part],
) -> Vec<Opcode> {
    let mut reg = |t| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let acc = reg(p.str_t);
    let label = reg(p.str_t);
    let d = reg(p.dyn_t);
    let text = reg(p.str_t);
    let v = reg(p.void_t);
    let mut ops = vec![Opcode::GetGlobal {
        dst: acc,
        global: tag,
    }];
    for (part, &global) in parts.iter().zip(labels) {
        ops.extend([
            Opcode::GetGlobal { dst: label, global },
            Opcode::Call2 {
                dst: acc,
                fun: p.add,
                arg0: acc,
                arg1: label,
            },
        ]);
        let t = part.value.unwrap_or(p.bool_t);
        let value = reg(t);
        ops.push(if t == p.str_t {
            Opcode::GetGlobal {
                dst: value,
                global: null,
            }
        } else if t == p.bool_t {
            Opcode::Bool {
                dst: value,
                value: ValBool(false),
            }
        } else {
            Opcode::Int {
                dst: value,
                ptr: p.minus_one,
            }
        });
        let mut cur = part.root;
        let mut skips = vec![];
        // Every pointer is checked before reading it. A missing numeric link
        // prints -1, a missing String prints "null", and presence prints false.
        for &(field, ft) in &part.path {
            skips.push(ops.len());
            ops.push(Opcode::JNull {
                reg: cur,
                offset: 0,
            });
            let next = reg(ft);
            ops.push(Opcode::Field {
                dst: next,
                obj: cur,
                field,
            });
            cur = next;
        }
        if part.value.is_none() || t == p.str_t {
            skips.push(ops.len());
            ops.push(Opcode::JNull {
                reg: cur,
                offset: 0,
            });
        }
        ops.push(if part.value.is_none() {
            Opcode::Bool {
                dst: value,
                value: ValBool(true),
            }
        } else {
            Opcode::Mov {
                dst: value,
                src: cur,
            }
        });
        let end = ops.len();
        for i in skips {
            let Opcode::JNull { offset, .. } = &mut ops[i] else {
                unreachable!()
            };
            *offset = (end - i - 1) as i32;
        }
        if t == p.str_t {
            ops.push(Opcode::Mov {
                dst: text,
                src: value,
            });
        } else {
            ops.extend([
                Opcode::ToDyn { dst: d, src: value },
                Opcode::Call1 {
                    dst: text,
                    fun: p.string,
                    arg0: d,
                },
            ]);
        }
        ops.push(Opcode::Call2 {
            dst: acc,
            fun: p.add,
            arg0: acc,
            arg1: text,
        });
    }
    ops.push(Opcode::Call1 {
        dst: v,
        fun: p.println,
        arg0: acc,
    });
    ops
}

fn apply(code: &mut Bytecode, p: &Plan) {
    let null = str_global(code, p.str_t, "null");
    let mut probes: Vec<_> = p.probes.iter().collect();
    // Descending sites keep the original bytecode offsets valid when several
    // probes share giveGains. insert_ops adjusts original jumps and debug rows.
    probes.sort_by_key(|pr| (pr.fi, std::cmp::Reverse(pr.at)));
    for pr in probes {
        let tag = str_global(code, p.str_t, pr.tag);
        let labels: Vec<_> = pr
            .parts
            .iter()
            .map(|q| str_global(code, p.str_t, q.label))
            .collect();
        let f = &mut code.functions[pr.fi];
        let ops = probe_ops(p, f, tag, &labels, null, &pr.parts);
        insert_ops(f, pr.at, ops);
    }
    eprintln!("patched camp choice diagnostics: outbound click, resolved host ID, chosen gains and aptitude recipient before/after");
}

pub(crate) fn patch_camp_choice(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, &p),
        Err(e) => eprintln!("camp choice diagnostics skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    fn logger_ranges(code: &Bytecode, f: &Function) -> Vec<(usize, usize)> {
        let println = static_fn(code, "$Sys", "println").unwrap().findex;
        let mut ranges = vec![];
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::GetGlobal { global, .. } = op else {
                continue;
            };
            let tagged = code.globals_initializers.get(global).is_some_and(|&k| {
                code.constants.as_ref().is_some_and(|cs| {
                    let str_index = cs[k].fields.first().copied();
                    str_index.is_some_and(|k| {
                        [REQUEST, RESOLVED, GAINS, BEFORE, AFTER]
                            .contains(&code.strings[k].as_str())
                    })
                })
            });
            if tagged {
                let end = (i..f.ops.len())
                    .find(|&j| matches!(f.ops[j], Opcode::Call1 { fun, .. } if fun == println))
                    .expect("logger println")
                    + 1;
                ranges.push((i, end));
            }
        }
        ranges
    }

    #[test]
    fn traces_actual_click_and_aptitude_grant() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            panic!("{HLBOOT} missing");
        };
        let orig = read(&image);
        let p = plan(&orig).expect("actual game plan");
        let mut code = read(&image);
        patch_camp_choice(&mut code);
        let bytes = write(&code);
        let back = read(&bytes);
        assert_eq!(orig.functions.len(), back.functions.len());
        for tag in [REQUEST, RESOLVED, GAINS, BEFORE, AFTER] {
            assert!(
                back.strings.iter().any(|s| s.as_str() == tag),
                "missing {tag}"
            );
        }
        for (fi, (old, new)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let changed = p.probes.iter().any(|p| p.fi == fi);
            assert_eq!(
                old.ops.len() != new.ops.len(),
                changed,
                "fn@{}",
                old.findex.0
            );
            if changed {
                check_flow(new);
                assert_eq!(&new.regs[..old.regs.len()], &old.regs);
                let ranges = logger_ranges(&back, new);
                assert_eq!(ranges.len(), p.probes.iter().filter(|p| p.fi == fi).count());
                for &(start, end) in &ranges {
                    check_types(&back, new, start..end);
                }
                let positions: Vec<_> = (0..new.ops.len())
                    .filter(|i| !ranges.iter().any(|&(s, e)| s <= *i && *i < e))
                    .collect();
                assert_eq!(positions.len(), old.ops.len());
                // Every original instruction survives. Only relative jump
                // distances change, and every target still lands on its old op.
                for (i, &at) in positions.iter().enumerate() {
                    let targets = jump_targets(old, i);
                    if targets.is_empty() {
                        assert_eq!(
                            format!("{:?}", old.ops[i]),
                            format!("{:?}", new.ops[at]),
                            "fn@{} op{i}",
                            old.findex.0
                        );
                    } else {
                        assert_eq!(
                            jump_targets(new, at),
                            targets.iter().map(|&t| positions[t]).collect::<Vec<_>>(),
                            "fn@{} op{i} targets",
                            old.findex.0
                        );
                    }
                }
            } else {
                assert_eq!(format!("{:?}", old.ops), format!("{:?}", new.ops));
                assert_eq!(old.regs, new.regs);
            }
        }
        let mut again = read(&bytes);
        patch_camp_choice(&mut again);
        assert_eq!(write(&again), bytes);
    }

    #[test]
    fn refuses_changed_award_without_editing_image() {
        let image = std::fs::read(HLBOOT).expect("installed game");
        let mut code = read(&image);
        let p = plan(&code).unwrap();
        let award = p.probes.iter().find(|pr| pr.tag == BEFORE).unwrap();
        code.functions[award.fi].ops[award.at] = Opcode::Label;
        let before = write(&code);
        assert!(plan(&code).is_err());
        patch_camp_choice(&mut code);
        assert_eq!(write(&code), before);
    }

    #[test]
    fn survives_full_patch_chain_and_party_inventory() {
        let image = std::fs::read(HLBOOT).expect("installed game");
        let orig = read(&image);
        let p = plan(&orig).unwrap();
        let back = read(&crate::patch_image(&image).expect("full patch chain"));
        for fi in p.probes.iter().map(|p| p.fi) {
            let f = &back.functions[fi];
            let ranges = logger_ranges(&back, f);
            assert_eq!(
                ranges.len(),
                p.probes.iter().filter(|p| p.fi == fi).count(),
                "full chain skipped fn@{}",
                f.findex.0
            );
            for (start, end) in ranges {
                check_types(&back, f, start..end);
            }
            check_flow(f);
        }
    }
}
