// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A UI build that throws inside a domkit style pass no longer breaks every
// later UI action, and the co-op game-over window cannot leave both players
// with a dead popup.
//
// Field report (co-op, escort NPC died -> "<name> dies." NetConfirm ->
// Continue): `st.Controller.gameOver__impl` (Controller.hx:59-61, play the
// sound, `new ui.win.Pause(true)`) threw domkit's "Infinite loop in apply
// style" from `Properties.checkLoop` (Properties.hx:120) while the Pause tree
// was being styled. On the guest net_guard swallowed it; on the host it left
// NetConfirm.onClose, so the popup stayed. After that no button answered on
// either machine.
//
// Why everything died afterwards (domkit, Properties.hx:172-185):
//
//   public function applyStyle(style) {
//       var prev = APPLY_LOOPS;  APPLY_LOOPS = 0;
//       var cnt = 0;
//       do { cnt = dirty.addCount; style.applyStyle(this, true); APPLY_LOOPS++; }
//       while (dirty.addCount > cnt);
//       APPLY_LOOPS = prev;  dirty.addCount = 0;
//   }
//
// `checkLoop()` (`if (APPLY_LOOPS > 100) throw ...`) runs on every DOM change:
// node creation, add/remove/toggleClass, set_hover/active/focus/disabled,
// needRefresh. A pass that keeps dirtying nodes throws on pass 101, and the
// throw skips `APPLY_LOOPS = prev`: the static stays at 101, every top-level
// applyStyle saves and restores 101, and from then on every hover, class change
// or new node anywhere throws the same error until the game restarts.
//
// The pass:
//   S. `Properties.applyStyle`: the loop runs under a trap. On an exception:
//      `APPLY_LOOPS = prev; dirty.addCount = 0;` (vanilla's own tail), one
//      log line (capped, LOG_CAP per run) with the pass count, the exception,
//      the first DUMP_MAX entries of the dirty list (component name and object:
//      on a runaway loop these are the nodes the last pass dirtied, i.e. the
//      culprit) and the full stack, then the exception is rethrown unchanged.
//   G. `Controller.gameOver__impl` runs under a trap. On an exception:
//      `APPLY_LOOPS = 0`, a log line with the full stack, the half-built Pause
//      is closed (`Window.close`) and its `windowRoot` removed (a modal
//      backdrop left in the scene would swallow every click), then the normal
//      pause menu `new Pause(false)` opens instead (Load / Quit stay usable;
//      an exception there is logged too). Nothing is rethrown, so the host's
//      NetConfirm closes and the guest's RPC returns normally.
//
// Validated before editing; each part is skipped (logged) on mismatch.

use super::asm::Asm;
use super::diag::{index_of, static_fn};
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefGlobal, RefInt, ValBool};

const LOG_CAP: i32 = 20;
const DUMP_MAX: i32 = 12;
const S_A: &str = "mp: style: applyStyle threw after ";
const S_B: &str = " passes (APPLY_LOOPS restored to ";
const S_C: &str = "): ";
const S_D: &str = "\nmp: style: root ";
const S_E: &str = " dirty:";
const SP: &str = " ";
const COLON: &str = ":";
const NL: &str = "\n";
const G_A: &str = "mp: game over window failed, opening the pause menu instead: ";
const G_B: &str = "mp: pause menu failed too: ";

struct Common {
    str_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    i32_t: RefType,
    ref_bool_t: RefType,
    arr_t: RefType,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    exc_stack: RefFun,
    stack_str: RefFun,
    /// `domkit.$Properties` class global, its type and `APPLY_LOOPS`.
    cls: RefGlobal,
    cls_t: RefType,
    loops: RefField,
}

fn common(code: &Bytecode) -> Result<Common> {
    let str_t = obj_type(code, "String")?;
    let dyn_t = prim_type(code, "Dyn", |t| matches!(t, Type::Dyn))?;
    let void_t = prim_type(code, "Void", |t| matches!(t, Type::Void))?;
    let i32_t = prim_type(code, "I32", |t| matches!(t, Type::I32))?;
    let bool_t = prim_type(code, "Bool", |t| matches!(t, Type::Bool))?;
    let ref_bool_t = prim_type(
        code,
        "Ref<Bool>",
        |t| matches!(t, Type::Ref(b) if *b == bool_t),
    )?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let println = static_fn(code, "$Sys", "println")?;
    let std_string = static_fn(code, "$Std", "string")?;
    if fun_args(code, println) != [dyn_t] || fun_args(code, std_string) != [dyn_t] {
        bail!("Sys.println / Std.string do not take one Dyn");
    }
    let str_add = static_fn(code, "$String", "__add__")?.findex;
    let exc_stack = static_fn(code, "haxe._CallStack.$CallStack_Impl_", "exceptionStack")?;
    let stack_str = static_fn(code, "haxe._CallStack.$CallStack_Impl_", "toString")?;
    if fun_args(code, exc_stack) != [ref_bool_t]
        || exc_stack.t.as_fun(code).map(|t| t.ret) != Some(arr_t)
        || fun_args(code, stack_str) != [arr_t]
        || stack_str.t.as_fun(code).map(|t| t.ret) != Some(str_t)
    {
        bail!("unexpected CallStack.exceptionStack / toString signatures");
    }
    let cls_t = obj_type(code, "domkit.$Properties")?;
    let props_t = obj_type(code, "domkit.Properties")?;
    let cls = RefGlobal(
        obj(code, props_t)?
            .global
            .0
            .checked_sub(1)
            .context("domkit.Properties: no class global")?,
    );
    if code.globals.get(cls.0) != Some(&cls_t) {
        bail!("domkit.Properties: the class global is not a domkit.$Properties");
    }
    let (loops, loops_t) = field(code, cls_t, "APPLY_LOOPS")?;
    if loops_t != i32_t {
        bail!("APPLY_LOOPS is not an I32");
    }
    Ok(Common {
        str_t,
        dyn_t,
        void_t,
        i32_t,
        ref_bool_t,
        arr_t,
        println: println.findex,
        std_string: std_string.findex,
        str_add,
        exc_stack: exc_stack.findex,
        stack_str: stack_str.findex,
        cls,
        cls_t,
        loops,
    })
}

// ---------- S: Properties.applyStyle ----------

struct StylePlan {
    fi: usize,
    /// Register holding the saved APPLY_LOOPS (`prev`).
    prev: Reg,
    /// First op of the loop (right after `APPLY_LOOPS = 0`), trap goes here.
    start: usize,
    /// `GetGlobal cls` of the `APPLY_LOOPS = prev` tail: the only loop exit.
    exit: usize,
    dirty: (RefField, RefType),
    add_count: RefField,
    head: RefField,
    next: RefField,
    component: (RefField, RefType),
    name: RefField,
    obj: RefField,
    props_t: RefType,
    obj_t: RefType,
}

fn style_plan(code: &Bytecode, c: &Common) -> Result<StylePlan> {
    let props_t = obj_type(code, "domkit.Properties")?;
    let f = method(code, props_t, "applyStyle")?;
    let fi = index_of(code, f.findex)?;
    let o = &f.ops;
    if o.iter().any(|x| matches!(x, Opcode::Trap { .. })) {
        bail!("Properties.applyStyle already has a trap (applied)");
    }
    // prev = APPLY_LOOPS
    let (
        Opcode::GetGlobal { dst: g0, global },
        Opcode::Field {
            dst: prev,
            obj: g1,
            field: f1,
        },
    ) = (&o[0], &o[1])
    else {
        bail!("applyStyle does not start with prev = APPLY_LOOPS");
    };
    if *global != c.cls || g0 != g1 || *f1 != c.loops || f.regs[prev.0 as usize] != c.i32_t {
        bail!("applyStyle does not start with prev = APPLY_LOOPS");
    }
    let sets: Vec<(usize, Reg)> = o
        .iter()
        .enumerate()
        .filter_map(|(i, x)| match x {
            Opcode::SetField { field, src, .. } if *field == c.loops => Some((i, *src)),
            _ => None,
        })
        .collect();
    // APPLY_LOOPS = 0, APPLY_LOOPS++ (in the loop), APPLY_LOOPS = prev.
    let [(zero_at, _), (_, _), (tail_at, tail_src)] = sets[..] else {
        bail!("applyStyle: {} APPLY_LOOPS writes (want 3)", sets.len());
    };
    if tail_src != *prev || tail_at < 1 {
        bail!("applyStyle: last APPLY_LOOPS write is not prev");
    }
    let exit = tail_at - 1;
    if !matches!(o[exit], Opcode::GetGlobal { global, .. } if global == c.cls) {
        bail!("applyStyle: the tail does not reload the class global");
    }
    let start = zero_at + 1;
    let rets: Vec<usize> = (0..o.len())
        .filter(|&i| matches!(o[i], Opcode::Ret { .. }))
        .collect();
    if rets != [o.len() - 1] || exit >= o.len() - 1 {
        bail!("applyStyle: expected a single final Ret after the tail");
    }
    // Nothing jumps into the loop from outside, out of it other than to the
    // tail, or onto the trap position from before it.
    for (i, op) in o.iter().enumerate() {
        if matches!(op, Opcode::Switch { .. }) {
            bail!("applyStyle: unexpected switch");
        }
        for t in jump_targets(f, i) {
            let inside = (start..exit).contains(&i);
            if inside && !(start..=exit).contains(&t) {
                bail!("applyStyle: op {i} leaves the loop");
            }
            if !inside && (start..exit).contains(&t) {
                bail!("applyStyle: op {i} jumps into the loop");
            }
            if !inside && t == exit && i > exit {
                bail!("applyStyle: op {i} jumps back onto the tail");
            }
        }
    }
    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let dirty = field(code, props_t, "dirty")?;
    let comp = field(code, props_t, "component")?;
    let (obj_f, obj_t) = field(code, props_t, "obj")?;
    if obj(code, dirty.1).is_err() || obj(code, comp.1).is_err() || obj(code, obj_t).is_err() {
        bail!("Properties.dirty / component / obj are not objects");
    }
    Ok(StylePlan {
        fi,
        prev: *prev,
        start,
        exit,
        add_count: typed(dirty.1, "addCount", c.i32_t)?,
        head: typed(dirty.1, "head", props_t)?,
        next: typed(props_t, "dirtyNext", props_t)?,
        name: typed(comp.1, "name", c.str_t)?,
        obj: obj_f,
        props_t,
        obj_t,
        dirty,
        component: comp,
    })
}

struct StyleConsts {
    logged: RefGlobal,
    cap: RefInt,
    max: RefInt,
    zero: RefInt,
    a: RefGlobal,
    b: RefGlobal,
    c: RefGlobal,
    d: RefGlobal,
    e: RefGlobal,
    sp: RefGlobal,
    colon: RefGlobal,
    nl: RefGlobal,
}

/// `acc += Std.string(x)` (x: any Dyn-compatible register).
fn add_str(a: &mut Asm, c: &Common, acc: Reg, t: Reg, x: Reg) {
    a.op(Opcode::Call1 {
        dst: t,
        fun: c.std_string,
        arg0: x,
    });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: c.str_add,
        arg0: acc,
        arg1: t,
    });
}

/// `acc += <String global g>`.
fn add_g(a: &mut Asm, c: &Common, acc: Reg, t: Reg, g: RefGlobal) {
    a.op(Opcode::GetGlobal { dst: t, global: g });
    a.op(Opcode::Call2 {
        dst: acc,
        fun: c.str_add,
        arg0: acc,
        arg1: t,
    });
}

struct SRegs {
    exc: Reg,
    lexc: Reg,
    cls: Reg,
    loops: Reg,
    dirty: Reg,
    zero: Reg,
    n: Reg,
    lim: Reg,
    rnull: Reg,
    arr: Reg,
    stk: Reg,
    acc: Reg,
    t: Reg,
    v: Reg,
    d: Reg,
    node: Reg,
    comp: Reg,
    name: Reg,
    obj: Reg,
}

fn sregs(f: &mut Function, c: &Common, p: &StylePlan) -> SRegs {
    let mut r = |t| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    SRegs {
        exc: r(c.dyn_t),
        lexc: r(c.dyn_t),
        cls: r(c.cls_t),
        loops: r(c.i32_t),
        dirty: r(p.dirty.1),
        zero: r(c.i32_t),
        n: r(c.i32_t),
        lim: r(c.i32_t),
        rnull: r(c.ref_bool_t),
        arr: r(c.arr_t),
        stk: r(c.str_t),
        acc: r(c.str_t),
        t: r(c.str_t),
        v: r(c.void_t),
        d: r(c.dyn_t),
        node: r(p.props_t),
        comp: r(p.component.1),
        name: r(c.str_t),
        obj: r(p.obj_t),
    }
}

/// EndTrap, the jump to the tail, and the handler (restore, log, rethrow).
fn style_handler(c: &Common, p: &StylePlan, k: &StyleConsts, r: &SRegs) -> Vec<Opcode> {
    let mut a = Asm::new();
    a.op(Opcode::EndTrap { exc: r.exc });
    a.jmp(Opcode::JAlways { offset: 0 }, "tail");
    // Handler: vanilla's tail first, so nothing below can leave it poisoned.
    a.op(Opcode::GetGlobal {
        dst: r.cls,
        global: c.cls,
    });
    a.op(Opcode::Field {
        dst: r.loops,
        obj: r.cls,
        field: c.loops,
    });
    a.op(Opcode::SetField {
        obj: r.cls,
        field: c.loops,
        src: p.prev,
    });
    a.op(Opcode::GetThis {
        dst: r.dirty,
        field: p.dirty.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.dirty,
            offset: 0,
        },
        "reset_done",
    );
    a.op(Opcode::Int {
        dst: r.zero,
        ptr: k.zero,
    });
    a.op(Opcode::SetField {
        obj: r.dirty,
        field: p.add_count,
        src: r.zero,
    });
    a.label("reset_done");
    // Capped, best-effort log line.
    a.op(Opcode::GetGlobal {
        dst: r.n,
        global: k.logged,
    });
    a.op(Opcode::Int {
        dst: r.lim,
        ptr: k.cap,
    });
    a.jmp(
        Opcode::JSGte {
            a: r.n,
            b: r.lim,
            offset: 0,
        },
        "rethrow",
    );
    a.op(Opcode::Incr { dst: r.n });
    a.op(Opcode::SetGlobal {
        global: k.logged,
        src: r.n,
    });
    a.jmp(
        Opcode::Trap {
            exc: r.lexc,
            offset: 0,
        },
        "rethrow",
    );
    // The stack first, before anything else can throw and replace it.
    a.op(Opcode::Null { dst: r.rnull });
    a.op(Opcode::Call1 {
        dst: r.arr,
        fun: c.exc_stack,
        arg0: r.rnull,
    });
    a.op(Opcode::Call1 {
        dst: r.stk,
        fun: c.stack_str,
        arg0: r.arr,
    });
    a.op(Opcode::GetGlobal {
        dst: r.acc,
        global: k.a,
    });
    a.op(Opcode::ToDyn {
        dst: r.d,
        src: r.loops,
    });
    add_str(&mut a, c, r.acc, r.t, r.d);
    add_g(&mut a, c, r.acc, r.t, k.b);
    a.op(Opcode::ToDyn {
        dst: r.d,
        src: p.prev,
    });
    add_str(&mut a, c, r.acc, r.t, r.d);
    add_g(&mut a, c, r.acc, r.t, k.c);
    add_str(&mut a, c, r.acc, r.t, r.exc);
    add_g(&mut a, c, r.acc, r.t, k.d);
    a.op(Opcode::Mov {
        dst: r.node,
        src: Reg(0),
    });
    describe_root(&mut a, c, p, k, r);
    add_g(&mut a, c, r.acc, r.t, k.e);
    // The dirty list, head first, at most DUMP_MAX entries.
    a.op(Opcode::GetThis {
        dst: r.dirty,
        field: p.dirty.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.dirty,
            offset: 0,
        },
        "list_done",
    );
    a.op(Opcode::Field {
        dst: r.node,
        obj: r.dirty,
        field: p.head,
    });
    a.op(Opcode::Int {
        dst: r.n,
        ptr: k.zero,
    });
    a.op(Opcode::Int {
        dst: r.lim,
        ptr: k.max,
    });
    a.loop_head("list");
    a.jmp(
        Opcode::JNull {
            reg: r.node,
            offset: 0,
        },
        "list_done",
    );
    a.jmp(
        Opcode::JSGte {
            a: r.n,
            b: r.lim,
            offset: 0,
        },
        "list_done",
    );
    describe_entry(&mut a, c, p, k, r);
    a.op(Opcode::Field {
        dst: r.node,
        obj: r.node,
        field: p.next,
    });
    a.op(Opcode::Incr { dst: r.n });
    a.jmp(Opcode::JAlways { offset: 0 }, "list");
    a.label("list_done");
    add_g(&mut a, c, r.acc, r.t, k.nl);
    a.op(Opcode::Call2 {
        dst: r.acc,
        fun: c.str_add,
        arg0: r.acc,
        arg1: r.stk,
    });
    a.op(Opcode::Call1 {
        dst: r.v,
        fun: c.println,
        arg0: r.acc,
    });
    a.op(Opcode::EndTrap { exc: r.lexc });
    a.label("rethrow");
    a.op(Opcode::Rethrow { exc: r.exc });
    a.label("tail");
    a.finish()
}

// `describe` uses fixed labels; one copy per call site needs its own names.
fn describe_root(a: &mut Asm, c: &Common, p: &StylePlan, k: &StyleConsts, r: &SRegs) {
    describe_with(a, c, p, k, r, "root_no_comp");
}
fn describe_entry(a: &mut Asm, c: &Common, p: &StylePlan, k: &StyleConsts, r: &SRegs) {
    describe_with(a, c, p, k, r, "entry_no_comp");
}
fn describe_with(
    a: &mut Asm,
    c: &Common,
    p: &StylePlan,
    k: &StyleConsts,
    r: &SRegs,
    label: &'static str,
) {
    add_g(a, c, r.acc, r.t, k.sp);
    a.op(Opcode::Field {
        dst: r.comp,
        obj: r.node,
        field: p.component.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.comp,
            offset: 0,
        },
        label,
    );
    a.op(Opcode::Field {
        dst: r.name,
        obj: r.comp,
        field: p.name,
    });
    add_str(a, c, r.acc, r.t, r.name);
    a.label(label);
    add_g(a, c, r.acc, r.t, k.colon);
    a.op(Opcode::Field {
        dst: r.obj,
        obj: r.node,
        field: p.obj,
    });
    add_str(a, c, r.acc, r.t, r.obj);
}

fn apply_style(code: &mut Bytecode, c: &Common, p: StylePlan) {
    code.globals.push(c.i32_t);
    let logged = RefGlobal(code.globals.len() - 1);
    let k = StyleConsts {
        logged,
        cap: int_const(code, LOG_CAP),
        max: int_const(code, DUMP_MAX),
        zero: int_const(code, 0),
        a: str_global(code, c.str_t, S_A),
        b: str_global(code, c.str_t, S_B),
        c: str_global(code, c.str_t, S_C),
        d: str_global(code, c.str_t, S_D),
        e: str_global(code, c.str_t, S_E),
        sp: str_global(code, c.str_t, SP),
        colon: str_global(code, c.str_t, COLON),
        nl: str_global(code, c.str_t, NL),
    };
    let f = &mut code.functions[p.fi];
    let r = sregs(f, c, &p);
    let block = style_handler(c, &p, &k, &r);
    // Handler block at the tail (the loop's exit jump lands on its EndTrap),
    // then the Trap in front of the loop.
    insert_at_target(f, p.exit, block);
    // After the Trap is inserted, the EndTrap sits at exit + 1 and the handler
    // starts two ops later.
    let handler = p.exit + 1 + 2;
    insert_ops(
        f,
        p.start,
        vec![Opcode::Trap {
            exc: r.exc,
            offset: (handler - p.start - 1) as i32,
        }],
    );
    eprintln!(
        "patched style guard fn@{}: applyStyle restores APPLY_LOOPS on an exception (trap op {}, handler op {})",
        f.findex.0, p.start, handler
    );
}

/// Inserts `ops` before op `at` so that jumps to `at` land on the inserted block.
fn insert_at_target(f: &mut Function, at: usize, ops: Vec<Opcode>) {
    let n = ops.len();
    insert_ops(f, at, ops);
    for i in (0..at).chain(at + n..f.ops.len()) {
        if jump_targets(f, i) == [at + n] {
            let off = at as i32 - i as i32 - 1;
            match &mut f.ops[i] {
                Opcode::JTrue { offset, .. }
                | Opcode::JFalse { offset, .. }
                | Opcode::JNull { offset, .. }
                | Opcode::JNotNull { offset, .. }
                | Opcode::JSLt { offset, .. }
                | Opcode::JSGte { offset, .. }
                | Opcode::JSGt { offset, .. }
                | Opcode::JSLte { offset, .. }
                | Opcode::JULt { offset, .. }
                | Opcode::JUGte { offset, .. }
                | Opcode::JNotLt { offset, .. }
                | Opcode::JNotGte { offset, .. }
                | Opcode::JEq { offset, .. }
                | Opcode::JNotEq { offset, .. }
                | Opcode::JAlways { offset } => *offset = off,
                o => unreachable!("not a single jump: {o:?}"),
            }
        }
    }
}

// ---------- G: Controller.gameOver__impl ----------

struct OverPlan {
    fi: usize,
    pause_t: RefType,
    ctor: RefFun,
    /// `New` destination of the original body.
    pause: Reg,
    /// Void result register returned by the original `Ret`.
    ret: Reg,
    close: RefFun,
    remove: RefFun,
    window_root: (RefField, RefType),
    bool_t: RefType,
}

fn over_plan(code: &Bytecode, c: &Common) -> Result<OverPlan> {
    let ctrl_t = obj_type(code, "st.Controller")?;
    let f = method(code, ctrl_t, "gameOver__impl")?;
    let fi = index_of(code, f.findex)?;
    let o = &f.ops;
    if o.iter().any(|x| matches!(x, Opcode::Trap { .. })) {
        bail!("gameOver__impl already has a trap (applied)");
    }
    let pause_t = obj_type(code, "ui.win.Pause")?;
    // ... New p; Bool b = true; Ref r = &b; Call2 Pause.__constructor__(p, r); Ret
    let n = o.len();
    if n < 5 {
        bail!("gameOver__impl is too short");
    }
    let (
        Opcode::New { dst: p },
        Opcode::Bool {
            dst: b,
            value: ValBool(true),
        },
        Opcode::Ref { dst: rb, src },
        Opcode::Call2 {
            fun: ctor,
            arg0,
            arg1,
            ..
        },
        Opcode::Ret { ret },
    ) = (&o[n - 5], &o[n - 4], &o[n - 3], &o[n - 2], &o[n - 1])
    else {
        bail!("gameOver__impl does not end with new Pause(true)");
    };
    if f.regs[p.0 as usize] != pause_t || src != b || arg0 != p || arg1 != rb {
        bail!("gameOver__impl: the Pause construction has unexpected registers");
    }
    let ctor_f = &code.functions[index_of(code, *ctor)?];
    if s(code, ctor_f.name) != "__constructor__" || fun_args(code, ctor_f).first() != Some(&pause_t)
    {
        bail!("gameOver__impl: the call is not Pause's constructor");
    }
    if f.regs[ret.0 as usize] != c.void_t {
        bail!("gameOver__impl does not return Void");
    }
    // A body without jumps, so wrapping it whole changes no target.
    if (0..n).any(|i| !jump_targets(f, i).is_empty()) {
        bail!("gameOver__impl has jumps");
    }
    let window_t = obj_type(code, "ui.Window")?;
    let close = method(code, window_t, "close")?;
    if fun_args(code, close) != [window_t] {
        bail!("unexpected Window.close signature");
    }
    let h2d_t = obj_type(code, "h2d.Object")?;
    let remove = method(code, h2d_t, "remove")?;
    if fun_args(code, remove) != [h2d_t] {
        bail!("unexpected h2d.Object.remove signature");
    }
    let window_root = field(code, pause_t, "windowRoot")?;
    if obj(code, window_root.1).is_err() {
        bail!("Window.windowRoot is not an object");
    }
    Ok(OverPlan {
        fi,
        pause_t,
        ctor: *ctor,
        pause: *p,
        ret: *ret,
        close: close.findex,
        remove: remove.findex,
        window_root,
        bool_t: f.regs[b.0 as usize],
    })
}

fn apply_over(code: &mut Bytecode, c: &Common, p: OverPlan) {
    let zero = int_const(code, 0);
    let ga = str_global(code, c.str_t, G_A);
    let gb = str_global(code, c.str_t, G_B);
    let nl = str_global(code, c.str_t, NL);
    let f = &mut code.functions[p.fi];
    let mut r = |t| {
        f.regs.push(t);
        Reg((f.regs.len() - 1) as u32)
    };
    let exc = r(c.dyn_t);
    let lexc = r(c.dyn_t);
    let cexc = r(c.dyn_t);
    let cls = r(c.cls_t);
    let z = r(c.i32_t);
    let rnull = r(c.ref_bool_t);
    let arr = r(c.arr_t);
    let stk = r(c.str_t);
    let acc = r(c.str_t);
    let t = r(c.str_t);
    let v = r(c.void_t);
    let root = r(p.window_root.1);
    let menu = r(p.pause_t);
    let fb = r(p.bool_t);
    let rfb = r(c.ref_bool_t);
    let body: Vec<Opcode> = std::mem::take(&mut f.ops);
    let body_dbg: Vec<(usize, usize)> = f.debug_info.take().unwrap_or_default();
    let n_body = body.len() - 1; // without the final Ret
    let dbg_line = body_dbg.last().copied().unwrap_or((0, 0));

    let log = |a: &mut Asm, prefix: RefGlobal, e: Reg, done: &'static str| {
        a.jmp(
            Opcode::Trap {
                exc: lexc,
                offset: 0,
            },
            done,
        );
        a.op(Opcode::Null { dst: rnull });
        a.op(Opcode::Call1 {
            dst: arr,
            fun: c.exc_stack,
            arg0: rnull,
        });
        a.op(Opcode::Call1 {
            dst: stk,
            fun: c.stack_str,
            arg0: arr,
        });
        a.op(Opcode::GetGlobal {
            dst: acc,
            global: prefix,
        });
        add_str(a, c, acc, t, e);
        add_g(a, c, acc, t, nl);
        a.op(Opcode::Call2 {
            dst: acc,
            fun: c.str_add,
            arg0: acc,
            arg1: stk,
        });
        a.op(Opcode::Call1 {
            dst: v,
            fun: c.println,
            arg0: acc,
        });
        a.op(Opcode::EndTrap { exc: lexc });
        a.label(done);
    };

    let mut a = Asm::new();
    // The handler tests the Pause register: null until `New` ran.
    a.op(Opcode::Null { dst: p.pause });
    a.jmp(Opcode::Trap { exc, offset: 0 }, "handler");
    for op in &body[..n_body] {
        a.op(op.clone());
    }
    a.op(Opcode::EndTrap { exc });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("handler");
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: c.cls,
    });
    a.op(Opcode::Int { dst: z, ptr: zero });
    a.op(Opcode::SetField {
        obj: cls,
        field: c.loops,
        src: z,
    });
    log(&mut a, ga, exc, "logged");
    // Close the half-built window and drop its root (a modal backdrop).
    a.jmp(
        Opcode::JNull {
            reg: p.pause,
            offset: 0,
        },
        "menu",
    );
    a.jmp(
        Opcode::Trap {
            exc: cexc,
            offset: 0,
        },
        "closed",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.close,
        arg0: p.pause,
    });
    a.op(Opcode::EndTrap { exc: cexc });
    a.label("closed");
    a.op(Opcode::Field {
        dst: root,
        obj: p.pause,
        field: p.window_root.0,
    });
    a.jmp(
        Opcode::JNull {
            reg: root,
            offset: 0,
        },
        "menu",
    );
    a.jmp(
        Opcode::Trap {
            exc: cexc,
            offset: 0,
        },
        "menu",
    );
    a.op(Opcode::Call1 {
        dst: v,
        fun: p.remove,
        arg0: root,
    });
    a.op(Opcode::EndTrap { exc: cexc });
    // The normal pause menu instead.
    a.label("menu");
    a.jmp(
        Opcode::Trap {
            exc: cexc,
            offset: 0,
        },
        "menu_failed",
    );
    a.op(Opcode::New { dst: menu });
    a.op(Opcode::Bool {
        dst: fb,
        value: ValBool(false),
    });
    a.op(Opcode::Ref { dst: rfb, src: fb });
    a.op(Opcode::Call2 {
        dst: v,
        fun: p.ctor,
        arg0: menu,
        arg1: rfb,
    });
    a.op(Opcode::EndTrap { exc: cexc });
    a.jmp(Opcode::JAlways { offset: 0 }, "end");
    a.label("menu_failed");
    a.op(Opcode::GetGlobal {
        dst: cls,
        global: c.cls,
    });
    a.op(Opcode::SetField {
        obj: cls,
        field: c.loops,
        src: z,
    });
    log(&mut a, gb, cexc, "logged2");
    a.label("end");
    a.op(Opcode::Ret { ret: p.ret });
    let ops = a.finish();
    // Debug lines: the original body keeps its own, the new ops take the last line.
    let mut dbg = vec![dbg_line; 2];
    dbg.extend(body_dbg.iter().take(n_body).copied());
    dbg.resize(ops.len(), dbg_line);
    f.ops = ops;
    f.debug_info = Some(dbg);
    if let Some(assigns) = &mut f.assigns {
        for (_, pos) in assigns.iter_mut() {
            *pos += 2;
        }
    }
    eprintln!(
        "patched style guard fn@{}: a failing game-over window is logged and replaced by the pause menu",
        f.findex.0
    );
}

/// Makes domkit's style pass exception-safe and guards the game-over window,
/// or leaves `code` untouched (per part) and logs why.
pub(crate) fn patch_style_guard(code: &mut Bytecode) {
    let c = match common(code) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("style guard skipped: {e:#}");
            return;
        }
    };
    match style_plan(code, &c) {
        Ok(p) => apply_style(code, &c, p),
        Err(e) => eprintln!("style guard (applyStyle) skipped: {e:#}"),
    }
    match over_plan(code, &c) {
        Ok(p) => apply_over(code, &c, p),
        Err(e) => eprintln!("style guard (game over) skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::asm::testutil::{check_flow, check_types, read, write, HLBOOT};
    use super::*;

    /// Each Trap is closed by an EndTrap on the same register; no op inside the
    /// protected block returns or jumps out of it; the handler lies after it.
    fn check_traps(f: &Function) -> usize {
        let mut n = 0;
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::Trap { exc, .. } = *op else {
                continue;
            };
            n += 1;
            let [handler] = jump_targets(f, i)[..] else {
                unreachable!()
            };
            let end = (i + 1..f.ops.len())
                .find(|&j| matches!(f.ops[j], Opcode::EndTrap { exc: e } if e == exc))
                .expect("EndTrap");
            assert!(handler > end, "fn@{} op {i}: handler inside", f.findex.0);
            for j in i + 1..end {
                assert!(
                    !matches!(f.ops[j], Opcode::Ret { .. }),
                    "fn@{} op {j}",
                    f.findex.0
                );
                for t in jump_targets(f, j) {
                    assert!(
                        t > i && t <= end,
                        "fn@{} op {j} leaves the trap",
                        f.findex.0
                    );
                }
            }
        }
        n
    }

    #[test]
    fn patches_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        let c = common(&orig).expect("common");
        let sp = style_plan(&orig, &c).expect("style plan");
        let op = over_plan(&orig, &c).expect("game over plan");
        let (sfi, ofi, start, exit, prev) = (sp.fi, op.fi, sp.start, sp.exit, sp.prev);
        assert_eq!((start, exit), (6, 24), "Properties.hx:172-185 layout");

        let mut code = read(&image);
        patch_style_guard(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.types, orig.types);
        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(&back.globals[..orig.globals.len()], &orig.globals[..]);
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                i != sfi && i != ofi,
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // applyStyle: the original ops in order, a Trap before the loop, the
        // loop's exit lands on the EndTrap, the handler restores prev and rethrows.
        let (a, b) = (&orig.functions[sfi], &back.functions[sfi]);
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());
        assert_eq!(check_traps(b), 2, "loop trap + log trap");
        assert!(matches!(b.ops[start], Opcode::Trap { .. }));
        let n_blk = b.ops.len() - a.ops.len() - 1;
        let map = |i: usize| {
            if i < start {
                i
            } else if i < exit {
                i + 1
            } else {
                i + 1 + n_blk
            }
        };
        for i in 0..a.ops.len() {
            let want: Vec<usize> = jump_targets(a, i)
                .into_iter()
                .map(|t| if t == exit { exit + 1 } else { map(t) })
                .collect();
            assert_eq!(jump_targets(b, map(i)), want, "applyStyle op {i}");
            if want.is_empty() {
                assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]));
            }
        }
        let Opcode::EndTrap { exc } = b.ops[exit + 1] else {
            panic!("no EndTrap at the loop exit")
        };
        assert_eq!(jump_targets(b, exit + 2), [map(exit)]);
        assert_eq!(jump_targets(b, start), [exit + 3]);
        let h = exit + 3;
        assert!(matches!(b.ops[h], Opcode::GetGlobal { global, .. } if global == c.cls));
        assert!(
            matches!(b.ops[h + 2], Opcode::SetField { field, src, .. } if field == c.loops && src == prev)
        );
        assert!(matches!(b.ops[map(exit) - 1], Opcode::Rethrow { exc: e } if e == exc));

        // gameOver__impl: the original body inside one trap; the handler resets
        // APPLY_LOOPS, closes the half-built window and opens Pause(false).
        let (a, b) = (&orig.functions[ofi], &back.functions[ofi]);
        check_flow(b);
        check_types(&back, b, 0..b.ops.len());
        assert_eq!(check_traps(b), 6);
        let n = a.ops.len() - 1;
        for i in 0..n {
            assert_eq!(format!("{:?}", b.ops[2 + i]), format!("{:?}", a.ops[i]));
        }
        assert!(matches!(b.ops[2 + n], Opcode::EndTrap { .. }));
        let ctors: Vec<&Opcode> = b
            .ops
            .iter()
            .filter(|o| matches!(o, Opcode::Call2 { fun, .. } if *fun == op.ctor))
            .collect();
        assert_eq!(ctors.len(), 2);
        assert!(b.ops.iter().any(|o| matches!(
            o,
            Opcode::Bool {
                value: ValBool(false),
                ..
            }
        )));
        assert!(b
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == op.close)));
        assert!(b
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == op.remove)));
        assert!(!b
            .ops
            .iter()
            .any(|o| matches!(o, Opcode::Rethrow { .. } | Opcode::Throw { .. })));

        // A second run changes nothing.
        let mut again = read(&patched);
        let c2 = common(&again).expect("common");
        assert!(style_plan(&again, &c2).is_err());
        assert!(over_plan(&again, &c2).is_err());
        patch_style_guard(&mut again);
        assert!(write(&again) == patched);
    }

    /// An applyStyle without the vanilla tail is refused and left as it was.
    #[test]
    fn refuses_unexpected_shapes() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut code = read(&image);
        let c = common(&code).expect("common");
        let p = style_plan(&code, &c).expect("plan");
        code.functions[p.fi].ops[p.exit + 1] = Opcode::Label;
        let before = format!("{:?}", code.functions[p.fi].ops);
        assert!(style_plan(&code, &c).is_err());
        patch_style_guard(&mut code);
        assert_eq!(format!("{:?}", code.functions[p.fi].ops), before);
    }
}
