// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Small helpers for appending hand-written functions: an op list with symbolic
// jump labels, a register list, and a function appender. Used by the
// chest_buttons, party_inventory, force_leave and tip_overflow passes.

use super::*;
use std::collections::HashMap;

/// A value of this type already carries its runtime type (HL `is_dynamic`): it
/// goes to a Dyn register or argument as is. `ToDyn` on it would allocate a
/// box around the pointer itself, which Std.string / Sys.println then read as
/// garbage. `ToDyn` is only for plain values (Int, Float, Bool, Bytes, ...).
pub(crate) fn self_describing(t: &Type) -> bool {
    matches!(
        t,
        Type::Dyn
            | Type::Fun(_)
            | Type::Obj(_)
            | Type::Struct(_)
            | Type::Array
            | Type::Virtual { .. }
            | Type::DynObj
            | Type::Null(_)
            | Type::Enum { .. }
    )
}

/// Function-local assembler with symbolic jump labels.
pub(crate) struct Asm {
    ops: Vec<Opcode>,
    fix: Vec<(usize, &'static str)>,
    labels: HashMap<&'static str, usize>,
}

impl Asm {
    pub(crate) fn new() -> Self {
        Asm {
            ops: vec![],
            fix: vec![],
            labels: HashMap::new(),
        }
    }

    pub(crate) fn op(&mut self, o: Opcode) {
        self.ops.push(o);
    }

    /// A jump op (offset placeholder) to `label`.
    pub(crate) fn jmp(&mut self, o: Opcode, label: &'static str) {
        self.fix.push((self.ops.len(), label));
        self.ops.push(o);
    }

    /// Marks the next op as `label`; a backward jump target gets a `Label` op.
    pub(crate) fn label(&mut self, l: &'static str) {
        assert!(
            self.labels.insert(l, self.ops.len()).is_none(),
            "label {l} twice"
        );
    }

    /// A loop head: `Label` op that backward jumps may target.
    pub(crate) fn loop_head(&mut self, l: &'static str) {
        self.label(l);
        self.ops.push(Opcode::Label);
    }

    pub(crate) fn finish(mut self) -> Vec<Opcode> {
        for (i, l) in std::mem::take(&mut self.fix) {
            let t = *self.labels.get(l).unwrap_or_else(|| panic!("label {l}"));
            let off = t as i32 - i as i32 - 1;
            match &mut self.ops[i] {
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
                | Opcode::JEq { offset, .. }
                | Opcode::JNotEq { offset, .. }
                | Opcode::JAlways { offset }
                | Opcode::Trap { offset, .. } => *offset = off,
                o => panic!("not a jump: {o:?}"),
            }
            if off < 0 {
                assert!(
                    matches!(self.ops[t], Opcode::Label),
                    "backward jump to a non-Label"
                );
            }
        }
        self.ops
    }
}

/// Register list of a new function (arguments first).
pub(crate) struct Regs(pub(crate) Vec<RefType>);

impl Regs {
    pub(crate) fn r(&mut self, t: RefType) -> Reg {
        self.0.push(t);
        Reg((self.0.len() - 1) as u32)
    }
}

/// Index of `value` in the string pool, appended when missing.
pub(crate) fn string_ref(code: &mut Bytecode, value: &str) -> hlbc::types::RefString {
    if let Some(i) = code.strings.iter().position(|v| v.as_str() == value) {
        return hlbc::types::RefString(i);
    }
    code.strings.push(hlbc::Str::from(value));
    hlbc::types::RefString(code.strings.len() - 1)
}

/// Appends a function `args -> ret` and returns its findex.
pub(crate) fn push_fn(
    code: &mut Bytecode,
    args: Vec<RefType>,
    ret: RefType,
    regs: Vec<RefType>,
    ops: Vec<Opcode>,
    dbg_file: usize,
) -> Result<RefFun> {
    let findex = next_findex(code)?;
    code.types.push(Type::Fun(TypeFun { args, ret }));
    let fun_t = RefType(code.types.len() - 1);
    let n = ops.len();
    code.functions.push(Function {
        name: hlbc::types::RefString(0),
        t: fun_t,
        findex,
        regs,
        ops,
        debug_info: Some(vec![(dbg_file, 1); n]),
        assigns: Some(vec![]),
        parent: None,
    });
    Ok(findex)
}

/// Snapshot of the append-only pools, to roll back a pass that failed half way.
pub(crate) struct Snap(usize, usize, usize, usize, usize, usize, usize);

impl Snap {
    pub(crate) fn take(code: &Bytecode) -> Self {
        Snap(
            code.types.len(),
            code.functions.len(),
            code.globals.len(),
            code.strings.len(),
            code.ints.len(),
            code.floats.len(),
            code.constants.as_ref().map_or(0, |c| c.len()),
        )
    }

    pub(crate) fn restore(&self, code: &mut Bytecode) {
        code.types.truncate(self.0);
        code.functions.truncate(self.1);
        code.globals.truncate(self.2);
        code.strings.truncate(self.3);
        code.ints.truncate(self.4);
        code.floats.truncate(self.5);
        if let Some(c) = code.constants.as_mut() {
            c.truncate(self.6);
        }
        let ng = self.2;
        code.globals_initializers.retain(|g, _| g.0 < ng);
    }
}

/// Test helpers shared by the passes built on this module.
#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub(crate) const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    pub(crate) fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    pub(crate) fn write(code: &Bytecode) -> Vec<u8> {
        let mut out = Vec::new();
        code.serialize(&mut out).expect("write");
        out
    }

    /// `from` may be stored where `to` is expected (same type, Dyn target, subclass,
    /// or both dynamic-ish pointers the VM converts implicitly: Null<T>/virtual/DynObj -> Dyn).
    pub(crate) fn assignable(code: &Bytecode, from: RefType, to: RefType) -> bool {
        let tf = &code.types[from.0];
        let tt = &code.types[to.0];
        from == to
            || tf == tt
            || matches!(tt, Type::Dyn)
            || (tf.get_type_obj().is_some()
                && tt.get_type_obj().is_some()
                && is_sub(code, from, to))
    }

    fn field_type(code: &Bytecode, t: RefType, f: RefField) -> RefType {
        match &code.types[t.0] {
            Type::Virtual { fields } => fields[f.0].t,
            _ => code.types[t.0].get_type_obj().expect("object").fields[f.0].t,
        }
    }

    fn callee(code: &Bytecode, f: RefFun) -> TypeFun {
        let t = code
            .functions
            .iter()
            .find(|g| g.findex == f)
            .map(|g| g.t)
            .or_else(|| code.natives.iter().find(|n| n.findex == f).map(|n| n.t))
            .expect("callee");
        t.as_fun(code).expect("fun type").clone()
    }

    fn call_ok(code: &Bytecode, f: &Function, dst: Reg, fun: RefFun, args: &[Reg]) -> bool {
        let rt = |r: &Reg| f.regs[r.0 as usize];
        let t = callee(code, fun);
        t.args.len() == args.len()
            && args
                .iter()
                .zip(&t.args)
                .all(|(r, a)| assignable(code, rt(r), *a))
            && (assignable(code, t.ret, rt(&dst)) || matches!(code.types[rt(&dst).0], Type::Void))
    }

    /// Register types agree with every field, global, call, closure and arithmetic op in `range`.
    pub(crate) fn check_types(code: &Bytecode, f: &Function, range: std::ops::Range<usize>) {
        let rt = |r: &Reg| f.regs[r.0 as usize];
        let is = |r: &Reg, want: fn(&Type) -> bool| want(&code.types[rt(r).0]);
        for i in range {
            let op = &f.ops[i];
            let ok = match op {
                Opcode::Field { dst, obj, field } => {
                    let ft = field_type(code, rt(obj), *field);
                    assignable(code, ft, rt(dst))
                        // virtual fields are read with a dynamic cast to the register type
                        || (matches!(code.types[rt(obj).0], Type::Virtual { .. })
                            && matches!(code.types[ft.0], Type::Dyn))
                }
                Opcode::GetThis { dst, field } => {
                    assignable(code, field_type(code, f.regs[0], *field), rt(dst))
                }
                Opcode::SetField { obj, field, src } => {
                    assignable(code, rt(src), field_type(code, rt(obj), *field))
                }
                Opcode::GetGlobal { dst, global } => {
                    assignable(code, code.globals[global.0], rt(dst))
                }
                Opcode::Mov { dst, src } => assignable(code, rt(src), rt(dst)),
                Opcode::Bool { dst, .. } | Opcode::Not { dst, .. } => {
                    is(dst, |t| matches!(t, Type::Bool))
                }
                Opcode::Int { dst, .. } | Opcode::Incr { dst } => {
                    is(dst, |t| matches!(t, Type::I32))
                }
                // `String` loads the raw UTF-16 bytes, not a String object: the
                // JIT trusts the register type, so a String register gets a
                // pointer into the char data and the first use is an access
                // violation. String objects come from constant globals (str_global).
                Opcode::String { dst, .. } => is(dst, |t| matches!(t, Type::Bytes)),
                Opcode::Add { dst, a, b } | Opcode::Sub { dst, a, b } => {
                    rt(dst) == rt(a)
                        && rt(a) == rt(b)
                        && is(dst, |t| matches!(t, Type::I32 | Type::F64))
                }
                Opcode::JSLt { a, b, .. }
                | Opcode::JSGte { a, b, .. }
                | Opcode::JSGt { a, b, .. }
                | Opcode::JSLte { a, b, .. }
                | Opcode::JULt { a, b, .. } => rt(a) == rt(b),
                Opcode::JEq { a, b, .. } | Opcode::JNotEq { a, b, .. } => {
                    assignable(code, rt(a), rt(b)) || assignable(code, rt(b), rt(a))
                }
                Opcode::JTrue { cond, .. } | Opcode::JFalse { cond, .. } => {
                    is(cond, |t| matches!(t, Type::Bool))
                }
                Opcode::Ref { dst, src } => {
                    matches!(code.types[rt(dst).0], Type::Ref(t) if t == rt(src))
                }
                Opcode::Call0 { dst, fun } => call_ok(code, f, *dst, *fun, &[]),
                Opcode::Call1 { dst, fun, arg0 } => call_ok(code, f, *dst, *fun, &[*arg0]),
                Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                } => call_ok(code, f, *dst, *fun, &[*arg0, *arg1]),
                Opcode::Call3 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                } => call_ok(code, f, *dst, *fun, &[*arg0, *arg1, *arg2]),
                Opcode::Call4 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                    arg3,
                } => call_ok(code, f, *dst, *fun, &[*arg0, *arg1, *arg2, *arg3]),
                Opcode::CallN { dst, fun, args } => call_ok(code, f, *dst, *fun, args),
                Opcode::InstanceClosure { dst, fun, obj } => {
                    let t = callee(code, *fun);
                    let Type::Fun(ct) = &code.types[rt(dst).0] else {
                        panic!(
                            "fn@{} op {i}: closure into a non-function register",
                            f.findex.0
                        )
                    };
                    !t.args.is_empty()
                        && assignable(code, rt(obj), t.args[0])
                        && t.args[1..] == ct.args[..]
                        && t.ret == ct.ret
                }
                Opcode::MakeEnum {
                    dst,
                    construct,
                    args,
                } => match &code.types[rt(dst).0] {
                    Type::Enum { constructs, .. } => {
                        let c = &constructs[construct.0];
                        c.params.len() == args.len()
                            && args
                                .iter()
                                .zip(&c.params)
                                .all(|(r, p)| assignable(code, rt(r), *p))
                    }
                    _ => false,
                },
                Opcode::DynSet { obj, .. } => is(obj, |t| matches!(t, Type::DynObj | Type::Dyn)),
                // Boxing a String/object prints garbage (see self_describing).
                Opcode::ToDyn { src, .. } => !super::self_describing(&code.types[rt(src).0]),
                Opcode::Ret { ret } => {
                    let fr = f.t.as_fun(code).expect("fun").ret;
                    assignable(code, rt(ret), fr) || matches!(code.types[fr.0], Type::Void)
                }
                _ => true,
            };
            assert!(
                ok,
                "fn@{} op {i} {op:?}: register types do not match",
                f.findex.0
            );
        }
    }

    /// Jumps stay in range, backward jumps land on a Label, the function ends in Ret.
    pub(crate) fn check_flow(f: &Function) {
        let n = f.ops.len();
        for i in 0..n {
            for t in jump_targets(f, i) {
                assert!(t < n, "fn@{} op {i} jumps out of range", f.findex.0);
                if t <= i {
                    assert!(
                        matches!(f.ops[t], Opcode::Label),
                        "fn@{} op {i}",
                        f.findex.0
                    );
                }
            }
        }
        assert!(matches!(f.ops[n - 1], Opcode::Ret { .. }));
        assert_eq!(f.debug_info.as_ref().map(|d| d.len()), Some(n));
    }

    /// `b` is `a` with `n` ops inserted at `at`: every original op and jump target kept.
    pub(crate) fn shifted(a: &Function, b: &Function, at: usize, n: usize) {
        assert_eq!(b.ops.len(), a.ops.len() + n);
        let map = |t: usize| if t < at { t } else { t + n };
        for i in 0..a.ops.len() {
            let tb: Vec<usize> = jump_targets(a, i).into_iter().map(map).collect();
            assert_eq!(jump_targets(b, map(i)), tb, "fn@{} op {i}", a.findex.0);
            if tb.is_empty() {
                assert_eq!(format!("{:?}", b.ops[map(i)]), format!("{:?}", a.ops[i]));
            }
        }
        assert_eq!(b.regs[..a.regs.len()], a.regs[..]);
    }
}
