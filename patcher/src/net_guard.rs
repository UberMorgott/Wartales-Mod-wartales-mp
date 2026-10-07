// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// An exception thrown by a game handler of a co-op network call no longer
// takes the session down: it is logged and the call reads as having returned
// a default value.
//
// Vanilla: such an exception unwinds out of hxbit's processMessage /
// processMessagesData -> mpman.net.Client's onData closure -> the relay
// service's onBinaryData -> WSConnection.processData -> SocketGroup.check,
// none of which catch. On the host that ends its relay connection and the
// relay drops every guest; on a guest it drops that guest.
//
// What the pass relies on (bytecode, Wartales 1.0.48274): a generated
// `networkRPC` case decodes every argument, returns false on a decode error,
// checks networkAllow, forwards the call to the other clients (finishing any
// per-client dispatch and flushNewRefs), and only then calls the game's
// `<name>__impl`; a message (8/9) is read whole before `host.onMessage` runs.
// So the impl / onMessage call is the one place where only game code runs.
//
// The pass (host and guests alike):
//   W. A writer depth counter (new I32 global): +1 on entry and -1 before every
//      return of each hxbit.NetworkHost function that writes or sends ctx.out
//      (WRITERS), +1 when `beginRPC` starts an RPC and -1 when `endRPC` ends it.
//      An exception inside a writer leaves it raised for good: a partial
//      message may be pending in ctx.out, or a send may be half done, and no
//      handler exception is swallowed any more (vanilla from then on).
//   G. A receive generation (new I32 global), +1 at the start of every
//      `processMessage`: any message handled while a handler runs changes it.
//   I. Every `__impl` call of every generated `networkRPC`, and
//   M. every `host.onMessage(client, msg)` call of every `processMessage`,
//      run under a trap. Before the call: the client's host, its ctx, the
//      ctx input and position, targetClient, receivingClient, isDispatching,
//      the client's processID, the writer depth and the receive generation
//      are saved. The exception is swallowed only when the handler started
//      outside any writer and dispatch (depth 0, isDispatching false) and all
//      of them are unchanged (same host and ctx, no message handled meanwhile,
//      no writer or dispatch left open); then targetClient is restored, a line
//      is logged and the call's result reads as its type's default (0, false,
//      0.0, null; an RPC with result answers that default). Otherwise it is
//      rethrown: vanilla behaviour.
//   A. Inside the trap, right before the handler call, a guest (Game.isAuth
//      false; Game.inst, Game.state set, not reloading: Game.update's own
//      conditions) runs `Game.initAlive()`. Vanilla makes objects received in a
//      batch alive (hxbit makeAlive -> State.alive -> Game.aliveStates ->
//      init, which sets battle.Entity.battle) only on the next Game.update,
//      while RPCs of the same batch already ran: e.g. Rat Matriarch's
//      ThroatyHowl spawns rats and damages them in one host flush, the guest's
//      feedbackDamages / battleNextTurn then hit `Null access .grid`. initAlive
//      is a few length checks when nothing is pending; GameUI.netUpdatePick
//      already calls makeAlive on demand the same way. lockAlives is honoured
//      by initAlive itself.
// Everything else stays vanilla: argument decode errors, property sync, object
// registration, full sync, RPC result callbacks, protocol errors.
//
// Logs are `Sys.println` lines ("mp: net: ..."), which the winmm shim copies
// into shim.log (see diag.rs), with the exception and its stack, at most
// LOG_CAP per run (a second new global counts them).
//
// Not undone: whatever game state the failed handler changed before it threw
// (on the host it replicates to the guests as usual).
//
// Validated before editing; a mismatch skips the pass (logged).

use super::asm::Asm;
use super::diag::static_fn;
use super::job_xp::str_global;
use super::*;
use hlbc::types::{RefFloat, RefGlobal, RefInt, ValBool};

const LOG_CAP: i32 = 50;
const IMPL_A: &str = "mp: net: RPC handler ";
const IMPL_B: &str = " threw, ignored: ";
const MSG_A: &str = "mp: net: message handler threw, ignored: ";
const NL: &str = "\n";

/// hxbit.NetworkHost functions that write messages to ctx.out or send it.
const WRITERS: [&str; 13] = [
    "sendMessage",
    "ping",
    "fullSync",
    "flushNewRefs",
    "onNewObject",
    "flushRegister",
    "unregister",
    "flushProps",
    "flush",
    "flushSend",
    "send",
    "targetRPC",
    "doRPC",
];

struct Common {
    str_t: RefType,
    dyn_t: RefType,
    void_t: RefType,
    i32_t: RefType,
    bool_t: RefType,
    ref_bool_t: RefType,
    host_t: RefType,
    client_t: RefType,
    ser_t: RefType,
    arr_t: RefType,
    bytes_t: RefType,
    println: RefFun,
    std_string: RefFun,
    str_add: RefFun,
    exc_stack: RefFun,
    stack_str: RefFun,
    h_ctx: RefField,
    h_dispatching: RefField,
    h_target: RefField,
    h_recv: RefField,
    c_host: RefField,
    c_pid: RefField,
    s_in_pos: RefField,
    s_input: RefField,
    /// `$Game` class global and its type; `inst`.
    g_cls: RefGlobal,
    g_cls_t: RefType,
    g_inst: RefField,
    game_t: RefType,
    /// Game.isAuth, Game.state (and its type), Game.reloading.
    g_auth: RefField,
    g_state: RefField,
    g_state_t: RefType,
    g_reloading: RefField,
    /// Game.initAlive (static, `Game -> Void`).
    init_alive: RefFun,
}

/// The value an interrupted call's result register gets.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fallback {
    None,
    Int,
    Bool,
    Float,
    Null,
}

#[derive(Debug)]
struct Site {
    /// The call op.
    at: usize,
    dst: Reg,
    def: Fallback,
    /// "Class.method" for an impl; None for onMessage.
    name: Option<String>,
}

#[derive(Debug)]
struct Guarded {
    fi: usize,
    /// The client register (networkRPC's client argument, processMessage's this).
    client: Reg,
    sites: Vec<Site>,
    /// processMessage: bumps the receive generation on entry.
    gen_bump: bool,
}

struct Plan {
    c: Common,
    guarded: Vec<Guarded>,
    /// (function index, op where +1 goes, Ret ops where -1 goes)
    counted: Vec<(usize, Option<usize>, Vec<usize>)>,
}

fn extends(code: &Bytecode, t: RefType, base: RefType) -> bool {
    let mut cur = Some(t);
    while let Some(c) = cur {
        if c == base {
            return true;
        }
        cur = code.types[c.0].get_type_obj().and_then(|o| o.super_);
    }
    false
}

/// Sets the target of the single-target jump at `i` to `t`.
fn retarget(op: &mut Opcode, i: usize, t: usize) {
    let off = t as i32 - i as i32 - 1;
    match op {
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
        | Opcode::JAlways { offset }
        | Opcode::Trap { offset, .. } => *offset = off,
        o => unreachable!("not a single jump: {o:?}"),
    }
}

/// Inserts `ops` before op `at` so that jumps to `at` land on the inserted block.
fn insert_at_target(f: &mut Function, at: usize, ops: Vec<Opcode>) {
    let n = ops.len();
    insert_ops(f, at, ops);
    for i in (0..at).chain(at + n..f.ops.len()) {
        if jump_targets(f, i) == [at + n] {
            retarget(&mut f.ops[i], i, at);
        }
    }
}

/// No Switch of `f` lands on `at` (insert_at_target only moves single jumps).
fn no_switch_to(f: &Function, at: usize) -> bool {
    !(0..f.ops.len())
        .any(|i| matches!(f.ops[i], Opcode::Switch { .. }) && jump_targets(f, i).contains(&at))
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
    let host_t = obj_type(code, "hxbit.NetworkHost")?;
    let client_t = obj_type(code, "hxbit.NetworkClient")?;
    let ser_t = obj_type(code, "hxbit.NetworkSerializer")?;
    let arr_t = obj_type(code, "hl.types.ArrayObj")?;
    let bytes_t = obj_type(code, "haxe.io.Bytes")?;
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
    let typed = |t: RefType, name: &str, want: RefType| -> Result<RefField> {
        let (f, ft) = field(code, t, name)?;
        if ft != want {
            bail!("field {name} has an unexpected type");
        }
        Ok(f)
    };
    let game_t = obj_type(code, "Game")?;
    let g_cls = RefGlobal(
        obj(code, game_t)?
            .global
            .0
            .checked_sub(1)
            .context("Game: no class global")?,
    );
    let g_cls_t = code.globals[g_cls.0];
    let g_inst = typed(g_cls_t, "inst", game_t)?;
    let (g_state, g_state_t) = field(code, game_t, "state")?;
    if obj(code, g_state_t).is_err() {
        bail!("Game.state is not an object");
    }
    let init = method(code, game_t, "initAlive")?;
    if fun_args(code, init) != [game_t] || init.t.as_fun(code).map(|t| t.ret) != Some(void_t) {
        bail!("unexpected Game.initAlive signature");
    }
    Ok(Common {
        g_cls,
        g_cls_t,
        g_inst,
        game_t,
        g_auth: typed(game_t, "isAuth", bool_t)?,
        g_state,
        g_state_t,
        g_reloading: typed(game_t, "reloading", bool_t)?,
        init_alive: init.findex,
        str_t,
        dyn_t,
        void_t,
        i32_t,
        bool_t,
        ref_bool_t,
        host_t,
        client_t,
        ser_t,
        arr_t,
        bytes_t,
        println: println.findex,
        std_string: std_string.findex,
        str_add,
        exc_stack: exc_stack.findex,
        stack_str: stack_str.findex,
        h_ctx: typed(host_t, "ctx", ser_t)?,
        h_dispatching: typed(host_t, "isDispatching", bool_t)?,
        h_target: typed(host_t, "targetClient", client_t)?,
        h_recv: typed(host_t, "receivingClient", client_t)?,
        c_host: typed(client_t, "host", host_t)?,
        c_pid: typed(client_t, "processID", i32_t)?,
        s_in_pos: typed(ser_t, "inPos", i32_t)?,
        s_input: typed(ser_t, "input", bytes_t)?,
    })
}

/// "Class.method" of the function a call op reaches (direct, or a CallThis
/// through the vtable of `f`'s class), with the call's result register.
fn callee_name(code: &Bytecode, f: &Function, op: &Opcode) -> Option<(String, Reg)> {
    let named = |fun: RefFun| {
        code.functions.iter().find(|g| g.findex == fun).map(|g| {
            let cls = g
                .parent
                .and_then(|p| code.types[p.0].get_type_obj())
                .map(|o| s(code, o.name).trim_start_matches('$').to_string())
                .unwrap_or_default();
            format!("{cls}.{}", s(code, g.name))
        })
    };
    match op {
        Opcode::Call0 { dst, fun }
        | Opcode::Call1 { dst, fun, .. }
        | Opcode::Call2 { dst, fun, .. }
        | Opcode::Call3 { dst, fun, .. }
        | Opcode::Call4 { dst, fun, .. }
        | Opcode::CallN { dst, fun, .. } => named(*fun).map(|n| (n, *dst)),
        Opcode::CallThis { dst, field, .. } => {
            let mut cur = f.regs.first().copied();
            while let Some(t) = cur {
                let o = code.types[t.0].get_type_obj()?;
                if let Some(p) = o.protos.iter().find(|p| p.pindex == field.0 as i32) {
                    return named(p.findex).map(|n| (n, *dst));
                }
                cur = o.super_;
            }
            None
        }
        _ => None,
    }
}

fn default_of(t: &Type) -> Option<Fallback> {
    Some(match t {
        Type::Void => Fallback::None,
        Type::I32 | Type::UI8 | Type::UI16 => Fallback::Int,
        Type::Bool => Fallback::Bool,
        Type::F64 => Fallback::Float,
        Type::Obj(_)
        | Type::Struct(_)
        | Type::Dyn
        | Type::DynObj
        | Type::Virtual { .. }
        | Type::Null(_)
        | Type::Fun(_)
        | Type::Method(_)
        | Type::Array
        | Type::Bytes
        | Type::Enum { .. }
        | Type::Abstract { .. }
        | Type::Type => Fallback::Null,
        _ => return None,
    })
}

fn rpc_fns(code: &Bytecode, c: &Common) -> Result<Vec<Guarded>> {
    let mut out = Vec::new();
    for (fi, f) in code.functions.iter().enumerate() {
        if s(code, f.name) != "networkRPC" {
            continue;
        }
        let args = fun_args(code, f);
        let ok = args.len() == 4
            && extends(code, args[1], c.ser_t)
            && args[2] == c.i32_t
            && args[3] == c.client_t
            && f.t.as_fun(code).map(|t| t.ret) == Some(c.bool_t);
        if !ok {
            bail!("fn@{}: unexpected networkRPC signature", f.findex.0);
        }
        if f.ops.iter().any(|op| matches!(op, Opcode::Trap { .. })) {
            bail!("already applied (fn@{} has a trap)", f.findex.0);
        }
        let mut sites = Vec::new();
        for (at, op) in f.ops.iter().enumerate() {
            let Some((name, dst)) = callee_name(code, f, op) else {
                continue;
            };
            let Some(base) = name.strip_suffix("__impl") else {
                continue;
            };
            let def = default_of(&code.types[f.regs[dst.0 as usize].0])
                .with_context(|| format!("fn@{} op {at}: impl result type", f.findex.0))?;
            if !no_switch_to(f, at) {
                bail!("fn@{} op {at}: a switch lands on the impl call", f.findex.0);
            }
            sites.push(Site {
                at,
                dst,
                def,
                name: Some(base.to_string()),
            });
        }
        if !sites.is_empty() {
            out.push(Guarded {
                fi,
                client: Reg(3),
                sites,
                gen_bump: false,
            });
        }
    }
    if out.is_empty() {
        bail!("no networkRPC calls an impl");
    }
    Ok(out)
}

/// The message-id switch of processMessage: (start, end) of every case.
fn case_ranges(f: &Function) -> Result<Vec<(usize, usize)>> {
    let sw: Vec<(usize, &Vec<i32>, i32)> = f
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            Opcode::Switch { offsets, end, .. }
                if offsets.len() > 9 && [5, 6, 8, 9].iter().all(|&k| offsets[k] != 0) =>
            {
                Some((i, offsets, *end))
            }
            _ => None,
        })
        .collect();
    let [(i, offsets, end)] = sw[..] else {
        bail!("fn@{}: no single message switch", f.findex.0);
    };
    let abs = |o: i32| (i as i64 + 1 + o as i64) as usize;
    let mut starts: Vec<usize> = offsets.iter().map(|&o| abs(o)).collect();
    starts.push(abs(end));
    starts.sort_unstable();
    starts.dedup();
    Ok(offsets
        .iter()
        .map(|&o| {
            let s0 = abs(o);
            let e = starts
                .iter()
                .copied()
                .find(|&x| x > s0)
                .unwrap_or(f.ops.len());
            (s0, e)
        })
        .collect())
}

/// The `host.onMessage(this, msg)` calls of a processMessage (cases 8 and 9).
fn message_fn(code: &Bytecode, c: &Common, fi: usize) -> Result<Guarded> {
    let f = &code.functions[fi];
    if f.ops.iter().any(|op| matches!(op, Opcode::Trap { .. })) {
        bail!("already applied (fn@{} has a trap)", f.findex.0);
    }
    let ranges = case_ranges(f)?;
    let (on_message, _) = field(code, c.host_t, "onMessage")?;
    let mut sites = Vec::new();
    for case in [8usize, 9] {
        let (s0, e) = ranges[case];
        let hits: Vec<usize> = (s0..e)
            .filter(|&at| match &f.ops[at] {
                Opcode::CallClosure { fun, args, .. } => {
                    args.first() == Some(&Reg(0))
                        && f.ops[s0..at].iter().any(|op| {
                            matches!(op, Opcode::Field { dst, field, .. } if dst == fun && *field == on_message)
                        })
                }
                _ => false,
            })
            .collect();
        let [at] = hits[..] else {
            bail!(
                "fn@{} case {case}: {} onMessage calls, want 1",
                f.findex.0,
                hits.len()
            );
        };
        let Opcode::CallClosure { dst, .. } = f.ops[at] else {
            unreachable!()
        };
        if !no_switch_to(f, at) || !matches!(code.types[f.regs[dst.0 as usize].0], Type::Void) {
            bail!("fn@{} op {at}: unexpected onMessage call", f.findex.0);
        }
        sites.push(Site {
            at,
            dst,
            def: Fallback::None,
            name: None,
        });
    }
    sites.sort_by_key(|st| st.at);
    // The generation bump goes before op 0, which nothing may jump back to.
    if (0..f.ops.len()).any(|i| jump_targets(f, i).contains(&0)) {
        bail!("fn@{}: op 0 is a jump target", f.findex.0);
    }
    Ok(Guarded {
        fi,
        client: Reg(0),
        sites,
        gen_bump: true,
    })
}

fn rets(f: &Function) -> Vec<usize> {
    (0..f.ops.len())
        .filter(|&i| matches!(f.ops[i], Opcode::Ret { .. }))
        .collect()
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let c = common(code)?;
    let mut guarded = rpc_fns(code, &c)?;
    let mut n_msg = 0;
    for (fi, f) in code.functions.iter().enumerate() {
        if s(code, f.name) == "processMessage"
            && f.regs
                .first()
                .is_some_and(|&t| extends(code, t, c.client_t))
        {
            guarded.push(message_fn(code, &c, fi)?);
            n_msg += 1;
        }
    }
    if n_msg == 0 {
        bail!("no processMessage");
    }
    let mut counted = Vec::new();
    for name in WRITERS {
        let f = method(code, c.host_t, name)?;
        let r = rets(f);
        if r.is_empty() || r.iter().any(|&i| !no_switch_to(f, i)) {
            bail!("{name}: unexpected returns");
        }
        counted.push((fun_index(code, f.findex)?, Some(0), r));
    }
    let begin = method(code, c.host_t, "beginRPC")?;
    counted.push((fun_index(code, begin.findex)?, Some(0), vec![]));
    let end = method(code, c.host_t, "endRPC")?;
    let r = rets(end);
    if r.len() != 1 || !no_switch_to(end, r[0]) {
        bail!("endRPC: expected one Ret");
    }
    counted.push((fun_index(code, end.findex)?, None, r));
    Ok(Plan {
        c,
        guarded,
        counted,
    })
}

/// `str_global` for a runtime string (the impl names).
fn name_global(code: &mut Bytecode, str_t: RefType, value: &str) -> RefGlobal {
    let found = code.constants.iter().flatten().find(|k| {
        code.globals.get(k.global.0) == Some(&str_t)
            && matches!(k.fields[..], [si, _] if code.strings.get(si).is_some_and(|x| x.as_str() == value))
    });
    if let Some(k) = found {
        return k.global;
    }
    let si = super::asm::string_ref(code, value).0;
    let len = int_const(code, value.encode_utf16().count() as i32);
    code.globals.push(str_t);
    let g = RefGlobal(code.globals.len() - 1);
    let consts = code.constants.get_or_insert_with(Vec::new);
    consts.push(hlbc::types::ConstantDef {
        global: g,
        fields: vec![si, len.0],
    });
    let ci = consts.len() - 1;
    code.globals_initializers.insert(g, ci);
    g
}

struct Consts {
    depth: RefGlobal,
    gen: RefGlobal,
    logged: RefGlobal,
    nl: RefGlobal,
    impl_a: RefGlobal,
    impl_b: RefGlobal,
    msg_a: RefGlobal,
    zero: RefInt,
    one: RefInt,
    minus_one: RefInt,
    cap: RefInt,
    fzero: RefFloat,
}

/// Registers of the guards of one function (shared: one handler runs at a time).
struct Regs {
    exc: Reg,
    lexc: Reg,
    host_b: Reg,
    ctx_b: Reg,
    in_b: Reg,
    target_b: Reg,
    disp_b: Reg,
    pid_b: Reg,
    depth_b: Reg,
    gen_b: Reg,
    input_b: Reg,
    recv_b: Reg,
    host: Reg,
    ctx: Reg,
    i: Reg,
    flag: Reg,
    bytes: Reg,
    client: Reg,
    zero: Reg,
    n: Reg,
    lim: Reg,
    rnull: Reg,
    arr: Reg,
    stk: Reg,
    acc: Reg,
    t: Reg,
    v: Reg,
    gcls: Reg,
    game: Reg,
    gstate: Reg,
}

fn regs(f: &mut Function, c: &Common) -> Regs {
    let mut r = |t| new_reg(f, t);
    Regs {
        exc: r(c.dyn_t),
        lexc: r(c.dyn_t),
        host_b: r(c.host_t),
        ctx_b: r(c.ser_t),
        in_b: r(c.i32_t),
        target_b: r(c.client_t),
        disp_b: r(c.bool_t),
        pid_b: r(c.i32_t),
        depth_b: r(c.i32_t),
        gen_b: r(c.i32_t),
        input_b: r(c.bytes_t),
        recv_b: r(c.client_t),
        host: r(c.host_t),
        ctx: r(c.ser_t),
        i: r(c.i32_t),
        flag: r(c.bool_t),
        bytes: r(c.bytes_t),
        client: r(c.client_t),
        zero: r(c.i32_t),
        n: r(c.i32_t),
        lim: r(c.i32_t),
        rnull: r(c.ref_bool_t),
        arr: r(c.arr_t),
        stk: r(c.str_t),
        acc: r(c.str_t),
        t: r(c.str_t),
        v: r(c.void_t),
        gcls: r(c.g_cls_t),
        game: r(c.game_t),
        gstate: r(c.g_state_t),
    }
}

/// On a guest, `Game.initAlive()` as `Game.update` would run it (Game.inst set,
/// a state, not reloading): objects that arrived earlier in the same network
/// batch get `alive()` / `init()` before a handler uses them. A host (isAuth)
/// skips it. Every exit lands on the op after the block (the handler call).
fn init_alive_ops(c: &Common, r: &Regs) -> Vec<Opcode> {
    let mut a = Asm::new();
    a.op(Opcode::GetGlobal {
        dst: r.gcls,
        global: c.g_cls,
    });
    a.op(Opcode::Field {
        dst: r.game,
        obj: r.gcls,
        field: c.g_inst,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.game,
            offset: 0,
        },
        "skip",
    );
    a.op(Opcode::Field {
        dst: r.flag,
        obj: r.game,
        field: c.g_auth,
    });
    a.jmp(
        Opcode::JTrue {
            cond: r.flag,
            offset: 0,
        },
        "skip",
    );
    a.op(Opcode::Field {
        dst: r.gstate,
        obj: r.game,
        field: c.g_state,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.gstate,
            offset: 0,
        },
        "skip",
    );
    a.op(Opcode::Field {
        dst: r.flag,
        obj: r.game,
        field: c.g_reloading,
    });
    a.jmp(
        Opcode::JTrue {
            cond: r.flag,
            offset: 0,
        },
        "skip",
    );
    a.op(Opcode::Call1 {
        dst: r.v,
        fun: c.init_alive,
        arg0: r.game,
    });
    a.label("skip");
    a.finish()
}

/// Saves the state the handler must leave alone, then opens the trap whose
/// handler starts after the init-alive block, the call, EndTrap and JAlways.
fn prefix(c: &Common, k: &Consts, r: &Regs, client: Reg) -> Vec<Opcode> {
    let mut a = Asm::new();
    a.op(Opcode::Null { dst: r.host_b });
    a.jmp(
        Opcode::JNull {
            reg: client,
            offset: 0,
        },
        "saved",
    );
    a.op(Opcode::Field {
        dst: r.host_b,
        obj: client,
        field: c.c_host,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.host_b,
            offset: 0,
        },
        "saved",
    );
    a.op(Opcode::Field {
        dst: r.ctx_b,
        obj: r.host_b,
        field: c.h_ctx,
    });
    a.op(Opcode::Field {
        dst: r.target_b,
        obj: r.host_b,
        field: c.h_target,
    });
    a.op(Opcode::Field {
        dst: r.disp_b,
        obj: r.host_b,
        field: c.h_dispatching,
    });
    a.op(Opcode::Field {
        dst: r.recv_b,
        obj: r.host_b,
        field: c.h_recv,
    });
    a.op(Opcode::Field {
        dst: r.pid_b,
        obj: client,
        field: c.c_pid,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.ctx_b,
            offset: 0,
        },
        "saved",
    );
    a.op(Opcode::Field {
        dst: r.in_b,
        obj: r.ctx_b,
        field: c.s_in_pos,
    });
    a.op(Opcode::Field {
        dst: r.input_b,
        obj: r.ctx_b,
        field: c.s_input,
    });
    a.label("saved");
    a.op(Opcode::GetGlobal {
        dst: r.depth_b,
        global: k.depth,
    });
    a.op(Opcode::GetGlobal {
        dst: r.gen_b,
        global: k.gen,
    });
    let init = init_alive_ops(c, r);
    a.op(Opcode::Trap {
        exc: r.exc,
        offset: 3 + init.len() as i32,
    });
    let mut ops = a.finish();
    // Inside the trap: an exception from alive()/init() is handled like one
    // from the handler (the handler call is then skipped).
    ops.extend(init);
    ops
}

/// Capped, best-effort `Sys.println(parts... + Std.string(exc) + "\n" + stack)`;
/// an exception inside only skips the line.
fn log_ops(a: &mut Asm, c: &Common, k: &Consts, r: &Regs, parts: &[RefGlobal]) {
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
        "log_end",
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
        "log_end",
    );
    // First, before anything else can throw and replace it.
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
    let add = |a: &mut Asm, x: Reg| {
        a.op(Opcode::Call2 {
            dst: r.acc,
            fun: c.str_add,
            arg0: r.acc,
            arg1: x,
        })
    };
    let (first, rest) = parts.split_first().expect("a log line has a prefix");
    a.op(Opcode::GetGlobal {
        dst: r.acc,
        global: *first,
    });
    for p in rest {
        a.op(Opcode::GetGlobal {
            dst: r.t,
            global: *p,
        });
        add(a, r.t);
    }
    a.op(Opcode::Call1 {
        dst: r.t,
        fun: c.std_string,
        arg0: r.exc,
    });
    add(a, r.t);
    a.op(Opcode::GetGlobal {
        dst: r.t,
        global: k.nl,
    });
    add(a, r.t);
    add(a, r.stk);
    // A String goes to println's Dyn argument as is (see diag.rs).
    a.op(Opcode::Call1 {
        dst: r.v,
        fun: c.println,
        arg0: r.acc,
    });
    a.op(Opcode::EndTrap { exc: r.lexc });
    a.label("log_end");
}

/// Jumps to "rethrow" unless the shared state is what the prefix saved and no
/// writer or dispatch is open (run before and again after the log line, whose
/// string conversion runs arbitrary code).
fn checks(a: &mut Asm, c: &Common, k: &Consts, r: &Regs, client: Reg) {
    let ne = |a: &mut Asm, x: Reg, y: Reg| {
        a.jmp(
            Opcode::JNotEq {
                a: x,
                b: y,
                offset: 0,
            },
            "rethrow",
        )
    };
    a.jmp(
        Opcode::JNull {
            reg: r.host_b,
            offset: 0,
        },
        "rethrow",
    );
    a.op(Opcode::Field {
        dst: r.host,
        obj: client,
        field: c.c_host,
    });
    ne(a, r.host, r.host_b);
    a.op(Opcode::Field {
        dst: r.ctx,
        obj: r.host,
        field: c.h_ctx,
    });
    a.jmp(
        Opcode::JNull {
            reg: r.ctx,
            offset: 0,
        },
        "rethrow",
    );
    ne(a, r.ctx, r.ctx_b);
    a.op(Opcode::Field {
        dst: r.i,
        obj: r.ctx,
        field: c.s_in_pos,
    });
    ne(a, r.i, r.in_b);
    a.op(Opcode::Field {
        dst: r.bytes,
        obj: r.ctx,
        field: c.s_input,
    });
    ne(a, r.bytes, r.input_b);
    a.op(Opcode::Field {
        dst: r.client,
        obj: r.host,
        field: c.h_recv,
    });
    ne(a, r.client, r.recv_b);
    a.op(Opcode::Field {
        dst: r.i,
        obj: client,
        field: c.c_pid,
    });
    ne(a, r.i, r.pid_b);
    a.op(Opcode::GetGlobal {
        dst: r.i,
        global: k.gen,
    });
    ne(a, r.i, r.gen_b);
    // Started and ended outside any writer and dispatch.
    a.op(Opcode::Int {
        dst: r.zero,
        ptr: k.zero,
    });
    ne(a, r.depth_b, r.zero);
    a.op(Opcode::GetGlobal {
        dst: r.i,
        global: k.depth,
    });
    ne(a, r.i, r.zero);
    a.jmp(
        Opcode::JTrue {
            cond: r.disp_b,
            offset: 0,
        },
        "rethrow",
    );
    a.op(Opcode::Field {
        dst: r.flag,
        obj: r.host,
        field: c.h_dispatching,
    });
    a.jmp(
        Opcode::JTrue {
            cond: r.flag,
            offset: 0,
        },
        "rethrow",
    );
}

/// EndTrap, the jump over the handler, and the handler: swallow (log, restore
/// targetClient, default result) when nothing shared changed, else rethrow.
fn suffix(
    c: &Common,
    k: &Consts,
    r: &Regs,
    client: Reg,
    site: &Site,
    name: Option<RefGlobal>,
) -> Vec<Opcode> {
    let mut a = Asm::new();
    a.op(Opcode::EndTrap { exc: r.exc });
    a.jmp(Opcode::JAlways { offset: 0 }, "cont");
    checks(&mut a, c, k, r, client);
    match name {
        Some(n) => log_ops(&mut a, c, k, r, &[k.impl_a, n, k.impl_b]),
        None => log_ops(&mut a, c, k, r, &[k.msg_a]),
    }
    checks(&mut a, c, k, r, client);
    a.op(Opcode::SetField {
        obj: r.host,
        field: c.h_target,
        src: r.target_b,
    });
    let dst = site.dst;
    match site.def {
        Fallback::None => {}
        Fallback::Int => a.op(Opcode::Int { dst, ptr: k.zero }),
        Fallback::Bool => a.op(Opcode::Bool {
            dst,
            value: ValBool(false),
        }),
        Fallback::Float => a.op(Opcode::Float { dst, ptr: k.fzero }),
        Fallback::Null => a.op(Opcode::Null { dst }),
    }
    a.jmp(Opcode::JAlways { offset: 0 }, "cont");
    a.label("rethrow");
    a.op(Opcode::Rethrow { exc: r.exc });
    a.label("cont");
    a.finish()
}

/// `g += delta` (an I32 global) through new registers of `f`.
fn bump(f: &mut Function, c: &Common, g: RefGlobal, delta: RefInt) -> Vec<Opcode> {
    let (x, d) = (new_reg(f, c.i32_t), new_reg(f, c.i32_t));
    vec![
        Opcode::GetGlobal { dst: x, global: g },
        Opcode::Int { dst: d, ptr: delta },
        Opcode::Add { dst: x, a: x, b: d },
        Opcode::SetGlobal { global: g, src: x },
    ]
}

fn apply(code: &mut Bytecode, p: Plan) {
    let Plan {
        c,
        guarded,
        counted,
    } = p;
    code.globals.push(c.i32_t);
    let depth = RefGlobal(code.globals.len() - 1);
    code.globals.push(c.i32_t);
    let gen = RefGlobal(code.globals.len() - 1);
    code.globals.push(c.i32_t);
    let logged = RefGlobal(code.globals.len() - 1);
    let k = Consts {
        depth,
        gen,
        logged,
        nl: str_global(code, c.str_t, NL),
        impl_a: str_global(code, c.str_t, IMPL_A),
        impl_b: str_global(code, c.str_t, IMPL_B),
        msg_a: str_global(code, c.str_t, MSG_A),
        zero: int_const(code, 0),
        one: int_const(code, 1),
        minus_one: int_const(code, -1),
        cap: int_const(code, LOG_CAP),
        fzero: float_const(code, 0.0),
    };

    // W. Writer depth: -1 before each return (highest first), then +1 at entry.
    for (fi, enter, leaves) in &counted {
        for &at in leaves.iter().rev() {
            let ops = bump(&mut code.functions[*fi], &c, depth, k.minus_one);
            insert_at_target(&mut code.functions[*fi], at, ops);
        }
        if let Some(at) = *enter {
            let ops = bump(&mut code.functions[*fi], &c, depth, k.one);
            insert_ops(&mut code.functions[*fi], at, ops);
        }
    }

    // I + M.
    let (mut n_impl, mut n_msg) = (0, 0);
    for g in &guarded {
        let names: Vec<Option<RefGlobal>> = g
            .sites
            .iter()
            .map(|st| st.name.as_ref().map(|n| name_global(code, c.str_t, n)))
            .collect();
        let f = &mut code.functions[g.fi];
        let r = regs(f, &c);
        for (st, name) in g.sites.iter().zip(names).rev() {
            match name {
                Some(_) => n_impl += 1,
                None => n_msg += 1,
            }
            // The handler after the call first, then the saves and the trap
            // before it (jumps to the call now land on the saves).
            insert_ops(f, st.at + 1, suffix(&c, &k, &r, g.client, st, name));
            insert_at_target(f, st.at, prefix(&c, &k, &r, g.client));
        }
        // G. Every message processMessage handles bumps the generation.
        if g.gen_bump {
            let ops = bump(f, &c, gen, k.one);
            insert_ops(f, 0, ops);
        }
    }
    eprintln!(
        "patched network guard: {n_impl} RPC handler calls and {n_msg} message handler calls trapped, writer depth in {} functions",
        counted.len()
    );
}

/// Keeps a throwing network handler from ending the co-op session, or leaves
/// `code` untouched and logs why.
pub(crate) fn patch_net_guard(code: &mut Bytecode) {
    match plan(code) {
        Ok(p) => apply(code, p),
        Err(e) => eprintln!("network guard skipped: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{check_flow, check_types, game, read, write};

    fn dummy_consts() -> Consts {
        Consts {
            depth: RefGlobal(0),
            gen: RefGlobal(0),
            logged: RefGlobal(0),
            nl: RefGlobal(0),
            impl_a: RefGlobal(0),
            impl_b: RefGlobal(0),
            msg_a: RefGlobal(0),
            zero: RefInt(0),
            one: RefInt(0),
            minus_one: RefInt(0),
            cap: RefInt(0),
            fzero: RefFloat(0),
        }
    }

    /// Each Trap is closed by an EndTrap on the same register; no op inside the
    /// protected block jumps out of it or returns; the handler lies after it.
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
                    !matches!(f.ops[j], Opcode::Ret { .. } | Opcode::Trap { .. }),
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
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let n_impl: usize = p
            .guarded
            .iter()
            .flat_map(|g| &g.sites)
            .filter(|s| s.name.is_some())
            .count();
        let n_msg: usize = p
            .guarded
            .iter()
            .flat_map(|g| &g.sites)
            .filter(|s| s.name.is_none())
            .count();
        assert_eq!((n_impl, n_msg, p.counted.len()), (701, 4, 15));

        // Block lengths, independent of register and constant numbers.
        let k = dummy_consts();
        let mut scratch = orig.functions[p.guarded[0].fi].clone();
        let r = regs(&mut scratch, &p.c);
        let pre = prefix(&p.c, &k, &r, Reg(0)).len();
        let suf = |s: &Site| {
            suffix(
                &p.c,
                &k,
                &r,
                Reg(0),
                s,
                s.name.as_ref().map(|_| RefGlobal(0)),
            )
            .len()
        };
        let init_n = init_alive_ops(&p.c, &r).len();
        assert_eq!(init_n, 10);
        const BUMP: usize = 4;

        let mut code = read(&image);
        patch_net_guard(&mut code);
        let patched = write(&code);
        let back = read(&patched);
        assert_eq!(back.types, orig.types);
        assert_eq!(back.functions.len(), orig.functions.len());
        assert_eq!(&back.globals[..orig.globals.len()], &orig.globals[..]);
        // Two counters plus at most one String global per message part and impl name.
        let added = back.globals.len() - orig.globals.len();
        assert!(
            added >= 2 && added <= 2 + 4 + n_impl_names(&p),
            "{added} globals"
        );

        let mut touched = std::collections::HashSet::new();
        for g in &p.guarded {
            touched.insert(g.fi);
            let (a, b) = (&orig.functions[g.fi], &back.functions[g.fi]);
            // Original op i sits at map(i); the blocks of a site go around its call.
            let gen = if g.gen_bump { BUMP } else { 0 };
            let map = |i: usize| {
                i + gen
                    + g.sites
                        .iter()
                        .map(|s| match s.at.cmp(&i) {
                            std::cmp::Ordering::Less => pre + suf(s),
                            std::cmp::Ordering::Equal => pre,
                            std::cmp::Ordering::Greater => 0,
                        })
                        .sum::<usize>()
            };
            let added: usize = gen + g.sites.iter().map(|s| pre + suf(s)).sum::<usize>();
            assert_eq!(b.ops.len(), a.ops.len() + added, "fn@{}", a.findex.0);
            for i in 0..a.ops.len() {
                let want: Vec<usize> = jump_targets(a, i)
                    .into_iter()
                    .map(|t| {
                        if g.sites.iter().any(|s| s.at == t) {
                            map(t) - pre // jumps to a call land on its saves
                        } else {
                            map(t)
                        }
                    })
                    .collect();
                assert_eq!(jump_targets(b, map(i)), want, "fn@{} op {i}", a.findex.0);
                if want.is_empty() {
                    assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]));
                }
            }
            for s in &g.sites {
                let at = map(s.at);
                // Trap right before the init-alive block, its handler right
                // after the call, EndTrap and JAlways.
                let trap = at - 1 - init_n;
                assert!(matches!(b.ops[trap], Opcode::Trap { .. }));
                assert_eq!(jump_targets(b, trap), [at + 3]);
                // The block: one Game.initAlive(Game.inst) call, right before
                // the handler call; each guard skips straight to the handler.
                let blk = trap + 1..at;
                let calls: Vec<&Opcode> = b.ops[blk.clone()]
                    .iter()
                    .filter(|o| matches!(o, Opcode::Call1 { fun, .. } if *fun == p.c.init_alive))
                    .collect();
                assert_eq!(calls.len(), 1, "fn@{} site {}", a.findex.0, s.at);
                let Opcode::Call1 { arg0, .. } = b.ops[at - 1] else {
                    panic!("fn@{}: initAlive is not last", a.findex.0)
                };
                assert!(
                    matches!(b.ops[trap + 1], Opcode::GetGlobal { global, .. } if global == p.c.g_cls)
                );
                assert!(
                    matches!(b.ops[trap + 2], Opcode::Field { dst, field, .. } if dst == arg0 && field == p.c.g_inst)
                );
                let guards: Vec<usize> = blk
                    .clone()
                    .filter(|&j| !jump_targets(b, j).is_empty())
                    .collect();
                assert_eq!(guards.len(), 4);
                for j in guards {
                    assert_eq!(jump_targets(b, j), [at], "fn@{} op {j}", a.findex.0);
                }
                let auth_read = b.ops[blk]
                    .iter()
                    .any(|o| matches!(o, Opcode::Field { field, .. } if *field == p.c.g_auth));
                assert!(auth_read);
                assert!(matches!(b.ops[at + 1], Opcode::EndTrap { .. }));
                assert_eq!(jump_targets(b, at + 2), [at + 1 + suf(s)]);
                // The handler ends with the jump back and the rethrow.
                let end = at + suf(s);
                assert!(matches!(b.ops[end], Opcode::Rethrow { .. }));
                assert_eq!(jump_targets(b, end - 1), [end + 1]);
                // The default result is written to the call's own register.
                if s.def != Fallback::None {
                    let w = match b.ops[end - 2] {
                        Opcode::Int { dst, .. }
                        | Opcode::Bool { dst, .. }
                        | Opcode::Float { dst, .. }
                        | Opcode::Null { dst } => dst,
                        ref o => panic!("fn@{}: {o:?}", a.findex.0),
                    };
                    assert_eq!(w, s.dst);
                }
                // The state checks run before and again after the log line.
                let host_reads = b.ops[at + 1..end]
                    .iter()
                    .filter(|o| matches!(o, Opcode::Field { obj, field, .. } if *obj == g.client && *field == p.c.c_host))
                    .count();
                assert_eq!(host_reads, 2, "fn@{} site {}", a.findex.0, s.at);
                check_types(&back, b, at - pre..at);
                check_types(&back, b, at + 1..end + 1);
            }
            assert_eq!(check_traps(b), 2 * g.sites.len());
            if g.gen_bump {
                assert!(matches!(b.ops[BUMP - 1], Opcode::SetGlobal { .. }));
                check_types(&back, b, 0..BUMP);
            }
            check_flow(b);
        }
        for (fi, enter, leaves) in &p.counted {
            touched.insert(*fi);
            let (a, b) = (&orig.functions[*fi], &back.functions[*fi]);
            let n = BUMP * (leaves.len() + usize::from(enter.is_some()));
            assert_eq!(b.ops.len(), a.ops.len() + n, "fn@{}", a.findex.0);
            let map = |i: usize| {
                i + BUMP * usize::from(enter.is_some())
                    + BUMP * leaves.iter().filter(|&&r| r <= i).count()
            };
            for i in 0..a.ops.len() {
                let want: Vec<usize> = jump_targets(a, i)
                    .into_iter()
                    .map(|t| {
                        if leaves.contains(&t) {
                            map(t) - BUMP
                        } else {
                            map(t)
                        }
                    })
                    .collect();
                assert_eq!(jump_targets(b, map(i)), want, "fn@{} op {i}", a.findex.0);
            }
            for &r in leaves {
                assert!(matches!(b.ops[map(r)], Opcode::Ret { .. }));
                assert!(matches!(b.ops[map(r) - 1], Opcode::SetGlobal { .. }));
            }
            if enter.is_some() {
                assert!(matches!(b.ops[BUMP - 1], Opcode::SetGlobal { .. }));
            }
            check_types(&back, b, 0..b.ops.len().min(BUMP));
            check_flow(b);
        }
        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(
                same,
                !touched.contains(&i),
                "function #{i} (fn@{})",
                a.findex.0
            );
        }

        // Idempotent: a second run plans nothing and changes nothing.
        let mut again = read(&patched);
        assert!(plan(&again).is_err());
        patch_net_guard(&mut again);
        assert!(write(&again) == patched);
    }

    /// Distinct impl names (each a new String global unless the pool had it).
    fn n_impl_names(p: &Plan) -> usize {
        let names: std::collections::HashSet<&String> = p
            .guarded
            .iter()
            .flat_map(|g| &g.sites)
            .filter_map(|s| s.name.as_ref())
            .collect();
        names.len()
    }
}
