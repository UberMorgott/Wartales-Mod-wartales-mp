// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// A tiny bytecode interpreter for the behaviour tests of the appended
// functions (forge / work mirror, co-op spectating). Objects are string-keyed
// maps: an object field is "f<index>", a dynamic field "d<name>", an array
// slot "i<k>", an enum value "idx" + "e<k>". Vanilla calls are answered by the
// test's `stub` (None: the callee must be an appended function, run here);
// virtual calls go to `method`. Traps are no-ops: the code under test must
// not throw in the scenarios a test builds.
#![allow(dead_code)]

use crate::fun_index;
use hlbc::opcodes::Opcode;
use hlbc::types::{RefField, RefFun, RefGlobal};
use hlbc::Bytecode;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum V {
    Null,
    B(bool),
    I(i32),
    F(f64),
    O(usize),
    S(String),
    Clo(RefFun, Box<V>),
    Fun(RefFun),
}

type Stub<'a> = Box<dyn Fn(&mut Core, RefFun, &[V]) -> Option<V> + 'a>;
type Method<'a> = Box<dyn Fn(&mut Core, usize, &[V]) -> V + 'a>;

/// Heap, globals, call log and free-form test state.
pub(crate) struct Core {
    pub(crate) globals: HashMap<usize, V>,
    pub(crate) heap: Vec<HashMap<String, V>>,
    pub(crate) log: Vec<(&'static str, Vec<V>)>,
    /// Named lookup tables a stub may consult (hxbit refs, scene names, ...).
    pub(crate) maps: HashMap<&'static str, HashMap<String, V>>,
}

impl Core {
    pub(crate) fn obj(&mut self, fields: &[(RefField, V)]) -> V {
        self.heap.push(
            fields
                .iter()
                .map(|(f, v)| (format!("f{}", f.0), v.clone()))
                .collect(),
        );
        V::O(self.heap.len() - 1)
    }
    pub(crate) fn key_get(&self, o: &V, k: &str) -> V {
        let V::O(i) = o else {
            panic!("null access {k}")
        };
        self.heap[*i].get(k).cloned().unwrap_or(V::Null)
    }
    pub(crate) fn key_set(&mut self, o: &V, k: String, v: V) {
        let V::O(i) = o else {
            panic!("set {k} on {o:?}")
        };
        self.heap[*i].insert(k, v);
    }
    pub(crate) fn get(&self, o: &V, f: RefField) -> V {
        self.key_get(o, &format!("f{}", f.0))
    }
    pub(crate) fn set(&mut self, o: &V, f: RefField, v: V) {
        self.key_set(o, format!("f{}", f.0), v)
    }
    /// An ArrayObj (fields `len` / `arr`) holding `items`.
    pub(crate) fn arr(&mut self, len: RefField, arr: RefField, items: Vec<V>) -> V {
        let n = items.len() as i32;
        let raw = self.obj(&[]);
        for (k, v) in items.into_iter().enumerate() {
            self.key_set(&raw, format!("i{k}"), v);
        }
        self.obj(&[(len, V::I(n)), (arr, raw)])
    }
    /// An enum value: constructor `idx` with `args`.
    pub(crate) fn enm(&mut self, idx: i32, args: Vec<V>) -> V {
        let e = self.obj(&[]);
        self.key_set(&e, "idx".into(), V::I(idx));
        for (k, v) in args.into_iter().enumerate() {
            self.key_set(&e, format!("e{k}"), v);
        }
        e
    }
    pub(crate) fn map(&self, m: &str, k: &str) -> V {
        self.maps
            .get(m)
            .and_then(|t| t.get(k))
            .cloned()
            .unwrap_or(V::Null)
    }
    pub(crate) fn put(&mut self, m: &'static str, k: &str, v: V) {
        self.maps.entry(m).or_default().insert(k.to_string(), v);
    }
    /// Removes and returns the logged calls named `what`, in order.
    pub(crate) fn take(&mut self, what: &str) -> Vec<Vec<V>> {
        let (hit, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.log)
            .into_iter()
            .partition(|(k, _)| *k == what);
        self.log = rest;
        hit.into_iter().map(|(_, a)| a).collect()
    }
}

pub(crate) struct Sim<'a> {
    pub(crate) code: &'a Bytecode,
    /// Functions below this index are vanilla: never interpreted.
    pub(crate) orig_n: usize,
    pub(crate) c: Core,
    stub: Stub<'a>,
    method: Method<'a>,
}

impl<'a> Sim<'a> {
    pub(crate) fn new(
        code: &'a Bytecode,
        orig_n: usize,
        stub: impl Fn(&mut Core, RefFun, &[V]) -> Option<V> + 'a,
        method: impl Fn(&mut Core, usize, &[V]) -> V + 'a,
    ) -> Sim<'a> {
        Sim {
            code,
            orig_n,
            c: Core {
                globals: HashMap::new(),
                heap: vec![],
                log: vec![],
                maps: HashMap::new(),
            },
            stub: Box::new(stub),
            method: Box::new(method),
        }
    }

    pub(crate) fn global(&self, g: RefGlobal) -> V {
        if let Some(v) = self.c.globals.get(&g.0) {
            return v.clone();
        }
        match crate::job_xp::const_str(self.code, g) {
            Some(s) => V::S(s.to_string()),
            None => V::Null,
        }
    }

    pub(crate) fn call(&mut self, f: RefFun, args: Vec<V>) -> V {
        if let Some(v) = (self.stub)(&mut self.c, f, &args) {
            return v;
        }
        let fi = fun_index(self.code, f).unwrap();
        assert!(fi >= self.orig_n, "unexpected vanilla call fn@{}", f.0);
        self.run(f, args)
    }

    pub(crate) fn run(&mut self, f: RefFun, args: Vec<V>) -> V {
        let code = self.code;
        let fun = &code.functions[fun_index(code, f).unwrap()];
        let mut r = vec![V::Null; fun.regs.len()];
        for (i, a) in args.into_iter().enumerate() {
            r[i] = a;
        }
        let mut pc = 0usize;
        let num = |v: &V| match v {
            V::I(x) => *x,
            o => panic!("not an int: {o:?}"),
        };
        loop {
            let op = &fun.ops[pc];
            let mut next = pc + 1;
            let jump = |off: i32| (pc as i64 + 1 + off as i64) as usize;
            let rr = |x: &Reg| x.0 as usize;
            use hlbc::types::Reg;
            match op {
                Opcode::Label | Opcode::Trap { .. } | Opcode::EndTrap { .. } => {}
                Opcode::Bool { dst, value } => r[rr(dst)] = V::B(value.0),
                Opcode::Int { dst, ptr } => r[rr(dst)] = V::I(code.ints[ptr.0]),
                Opcode::Float { dst, ptr } => r[rr(dst)] = V::F(code.floats[ptr.0]),
                Opcode::Null { dst } => r[rr(dst)] = V::Null,
                Opcode::NullCheck { reg } => assert_ne!(r[rr(reg)], V::Null, "null check"),
                Opcode::Mov { dst, src }
                | Opcode::SafeCast { dst, src }
                | Opcode::UnsafeCast { dst, src }
                | Opcode::ToVirtual { dst, src }
                | Opcode::ToDyn { dst, src } => r[rr(dst)] = r[rr(src)].clone(),
                Opcode::Not { dst, src } => r[rr(dst)] = V::B(r[rr(src)] != V::B(true)),
                Opcode::Neg { dst, src } => {
                    r[rr(dst)] = match r[rr(src)] {
                        V::F(x) => V::F(-x),
                        ref o => V::I(-num(o)),
                    }
                }
                Opcode::ToInt { dst, src } => {
                    let V::F(x) = r[rr(src)] else { panic!("ToInt") };
                    r[rr(dst)] = V::I(x as i32)
                }
                Opcode::ToSFloat { dst, src } => {
                    r[rr(dst)] = match r[rr(src)] {
                        V::F(x) => V::F(x),
                        ref o => V::F(num(o) as f64),
                    }
                }
                Opcode::GetGlobal { dst, global } => r[rr(dst)] = self.global(*global),
                Opcode::SetGlobal { global, src } => {
                    self.c.globals.insert(global.0, r[rr(src)].clone());
                }
                // String { bytes, length }: only its length is readable here.
                Opcode::Field { dst, obj, field } => {
                    r[rr(dst)] = match &r[rr(obj)] {
                        V::S(x) if field.0 == 1 => V::I(x.encode_utf16().count() as i32),
                        o => self.c.get(o, *field),
                    }
                }
                Opcode::GetThis { dst, field } => r[rr(dst)] = self.c.get(&r[0], *field),
                Opcode::SetField { obj, field, src } => {
                    let (o, v) = (r[rr(obj)].clone(), r[rr(src)].clone());
                    self.c.set(&o, *field, v);
                }
                Opcode::SetThis { field, src } => {
                    let (o, v) = (r[0].clone(), r[rr(src)].clone());
                    self.c.set(&o, *field, v);
                }
                Opcode::DynGet { dst, obj, field } => {
                    r[rr(dst)] = self
                        .c
                        .key_get(&r[rr(obj)], &format!("d{}", code.strings[field.0].as_str()))
                }
                Opcode::DynSet { obj, field, src } => {
                    let (o, v) = (r[rr(obj)].clone(), r[rr(src)].clone());
                    self.c
                        .key_set(&o, format!("d{}", code.strings[field.0].as_str()), v);
                }
                Opcode::New { dst } => r[rr(dst)] = self.c.obj(&[]),
                Opcode::GetArray { dst, array, index } => {
                    let i = num(&r[rr(index)]);
                    r[rr(dst)] = self.c.key_get(&r[rr(array)], &format!("i{i}"));
                }
                Opcode::EnumIndex { dst, value } => {
                    r[rr(dst)] = self.c.key_get(&r[rr(value)], "idx")
                }
                Opcode::EnumField {
                    dst, value, field, ..
                } => r[rr(dst)] = self.c.key_get(&r[rr(value)], &format!("e{}", field.0)),
                Opcode::MakeEnum {
                    dst,
                    construct,
                    args,
                } => {
                    let vals = args.iter().map(|a| r[rr(a)].clone()).collect();
                    r[rr(dst)] = self.c.enm(construct.0 as i32, vals)
                }
                Opcode::Incr { dst } => r[rr(dst)] = V::I(num(&r[rr(dst)]) + 1),
                // A ref is passed by value: the callee is always a stub.
                Opcode::Ref { dst, src } => r[rr(dst)] = r[rr(src)].clone(),
                Opcode::Add { dst, a, b }
                | Opcode::Sub { dst, a, b }
                | Opcode::Mul { dst, a, b }
                | Opcode::SDiv { dst, a, b }
                | Opcode::SMod { dst, a, b }
                    if matches!(r[rr(a)], V::F(_)) =>
                {
                    let (V::F(x), V::F(y)) = (&r[rr(a)], &r[rr(b)]) else {
                        panic!("float op on {:?}", r[rr(b)])
                    };
                    r[rr(dst)] = V::F(match op {
                        Opcode::Add { .. } => x + y,
                        Opcode::Sub { .. } => x - y,
                        Opcode::Mul { .. } => x * y,
                        Opcode::SDiv { .. } => x / y,
                        _ => x % y,
                    })
                }
                Opcode::Mul { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) * num(&r[rr(b)])),
                Opcode::Add { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) + num(&r[rr(b)])),
                Opcode::Sub { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) - num(&r[rr(b)])),
                // i32 division truncates toward zero, like the JIT's idiv.
                Opcode::SDiv { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) / num(&r[rr(b)])),
                Opcode::And { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) & num(&r[rr(b)])),
                Opcode::Or { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) | num(&r[rr(b)])),
                Opcode::Shl { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) << num(&r[rr(b)])),
                Opcode::SShr { dst, a, b } => r[rr(dst)] = V::I(num(&r[rr(a)]) >> num(&r[rr(b)])),
                Opcode::UShr { dst, a, b } => {
                    r[rr(dst)] = V::I(((num(&r[rr(a)]) as u32) >> num(&r[rr(b)])) as i32)
                }
                Opcode::InstanceClosure { dst, fun, obj } => {
                    r[rr(dst)] = V::Clo(*fun, Box::new(r[rr(obj)].clone()))
                }
                Opcode::StaticClosure { dst, fun } => r[rr(dst)] = V::Fun(*fun),
                Opcode::CallClosure { dst, fun, args } => {
                    let a: Vec<V> = args.iter().map(|x| r[rr(x)].clone()).collect();
                    r[rr(dst)] = match r[rr(fun)].clone() {
                        V::Clo(f, o) => {
                            let mut all = vec![*o];
                            all.extend(a);
                            self.call(f, all)
                        }
                        V::Fun(f) => self.call(f, a),
                        o => panic!("call of {o:?}"),
                    }
                }
                Opcode::CallMethod { dst, field, args } => {
                    let a: Vec<V> = args.iter().map(|x| r[rr(x)].clone()).collect();
                    r[rr(dst)] = (self.method)(&mut self.c, field.0, &a);
                }
                Opcode::JAlways { offset } => next = jump(*offset),
                Opcode::JTrue { cond, offset } => {
                    if r[rr(cond)] == V::B(true) {
                        next = jump(*offset)
                    }
                }
                Opcode::JFalse { cond, offset } => {
                    if r[rr(cond)] != V::B(true) {
                        next = jump(*offset)
                    }
                }
                Opcode::JNull { reg, offset } => {
                    if r[rr(reg)] == V::Null {
                        next = jump(*offset)
                    }
                }
                Opcode::JNotNull { reg, offset } => {
                    if r[rr(reg)] != V::Null {
                        next = jump(*offset)
                    }
                }
                Opcode::JEq { a, b, offset } => {
                    if r[rr(a)] == r[rr(b)] {
                        next = jump(*offset)
                    }
                }
                Opcode::JNotEq { a, b, offset } => {
                    if r[rr(a)] != r[rr(b)] {
                        next = jump(*offset)
                    }
                }
                Opcode::JSGte { a, b, offset }
                | Opcode::JSLte { a, b, offset }
                | Opcode::JSLt { a, b, offset }
                | Opcode::JSGt { a, b, offset } => {
                    let f = |v: &V| match v {
                        V::F(x) => *x,
                        o => num(o) as f64,
                    };
                    let (x, y) = (f(&r[rr(a)]), f(&r[rr(b)]));
                    let t = match op {
                        Opcode::JSGte { .. } => x >= y,
                        Opcode::JSLte { .. } => x <= y,
                        Opcode::JSLt { .. } => x < y,
                        _ => x > y,
                    };
                    if t {
                        next = jump(*offset)
                    }
                }
                Opcode::Call0 { dst, fun } => r[rr(dst)] = self.call(*fun, vec![]),
                Opcode::Call1 { dst, fun, arg0 } => {
                    let a = vec![r[rr(arg0)].clone()];
                    r[rr(dst)] = self.call(*fun, a)
                }
                Opcode::Call2 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                } => {
                    let a = vec![r[rr(arg0)].clone(), r[rr(arg1)].clone()];
                    r[rr(dst)] = self.call(*fun, a)
                }
                Opcode::Call3 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                } => {
                    let a = vec![
                        r[rr(arg0)].clone(),
                        r[rr(arg1)].clone(),
                        r[rr(arg2)].clone(),
                    ];
                    r[rr(dst)] = self.call(*fun, a)
                }
                Opcode::Call4 {
                    dst,
                    fun,
                    arg0,
                    arg1,
                    arg2,
                    arg3,
                } => {
                    let a = [arg0, arg1, arg2, arg3]
                        .iter()
                        .map(|x| r[rr(x)].clone())
                        .collect();
                    r[rr(dst)] = self.call(*fun, a)
                }
                Opcode::CallN { dst, fun, args } => {
                    let a = args.iter().map(|x| r[rr(x)].clone()).collect();
                    r[rr(dst)] = self.call(*fun, a)
                }
                Opcode::Ret { ret } => return r[rr(ret)].clone(),
                o => panic!("interpreter: unsupported {o:?}"),
            }
            pc = next;
        }
    }
}
