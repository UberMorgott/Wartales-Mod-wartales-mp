//! Keeps the HashLink JIT from hashing a NULL function name.
//!
//! When the JIT compiles `NullCheck r`, it looks ahead for the op that uses `r`
//! (skipping constant and arithmetic ops) to build a better "Null access" message.
//! If that op is `Call1..CallN` with `r` in the `p3` slot (the first argument;
//! for `CallN` the argument count), it calls `hl_hash_gen(name)` on the callee's
//! field name: `f->obj ? f->field.name : f->field.ref ? f->field.ref->field.name
//! : NULL`. A function appended by a patch has no `obj` (it is no class method);
//! it only gets a `field.ref` when `hl_module_init` sees a call or closure to it
//! from a function whose `field.ref` chain already reaches a method, in a single
//! pass in function order. A nameless callee there makes `hl_hash_gen(NULL)`
//! crash the game at startup (libhl.dll, access violation in hl_hash_gen).
//!
//! `nameless` mirrors that module-init pass, `offending` lists the JIT pattern,
//! and `patch_jit_names` puts a `Nop` right after each offending `NullCheck`, so
//! the lookahead stops there and nothing is hashed.

use crate::insert_ops;
use hlbc::opcodes::Opcode;
use hlbc::types::{Reg, Type};
use hlbc::Bytecode;
use std::collections::HashMap;

/// Function index (into `code.functions`) by findex; natives are absent.
fn indexes(code: &Bytecode) -> HashMap<usize, usize> {
    code.functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.findex.0, i))
        .collect()
}

/// For each function: true when the JIT would see a NULL field name for it.
pub(crate) fn nameless(code: &Bytecode) -> Vec<bool> {
    let idx = indexes(code);
    let n = code.functions.len();
    let mut has_obj = vec![false; n];
    for t in &code.types {
        let Type::Obj(o) = t else { continue };
        for p in &o.protos {
            if let Some(&i) = idx.get(&p.findex.0) {
                has_obj[i] = true;
            }
        }
        for (fid, mid) in &o.bindings {
            let is_fun = o.fields.get(fid.0).is_some_and(|f| {
                matches!(
                    code.types[f.t.0],
                    Type::Fun(_) | Type::Method(_) | Type::Dyn
                )
            });
            if let (true, Some(&i)) = (is_fun, idx.get(&mid.0)) {
                has_obj[i] = true;
            }
        }
    }
    // field.ref: None, or the function index it points at (a method)
    let mut fref: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let mut real = Some(i);
        let mut guard = 0;
        while let Some(r) = real {
            if has_obj[r] || guard > n {
                break;
            }
            real = fref[r];
            guard += 1;
        }
        let Some(real) = real.filter(|&r| has_obj[r]) else {
            continue;
        };
        for op in &code.functions[i].ops {
            let target = match op {
                Opcode::Call0 { fun, .. }
                | Opcode::Call1 { fun, .. }
                | Opcode::Call2 { fun, .. }
                | Opcode::Call3 { fun, .. }
                | Opcode::Call4 { fun, .. }
                | Opcode::CallN { fun, .. }
                | Opcode::StaticClosure { fun, .. }
                | Opcode::InstanceClosure { fun, .. } => fun.0,
                _ => continue,
            };
            if let Some(&t) = idx.get(&target) {
                if !has_obj[t] {
                    fref[t] = Some(real);
                }
            }
        }
    }
    (0..n).map(|i| !has_obj[i] && fref[i].is_none()).collect()
}

/// Ops the JIT's null-check lookahead skips (OInt .. ODecr).
fn skipped(op: &Opcode) -> bool {
    matches!(
        op,
        Opcode::Int { .. }
            | Opcode::Float { .. }
            | Opcode::Bool { .. }
            | Opcode::Bytes { .. }
            | Opcode::String { .. }
            | Opcode::Null { .. }
            | Opcode::Add { .. }
            | Opcode::Sub { .. }
            | Opcode::Mul { .. }
            | Opcode::SDiv { .. }
            | Opcode::UDiv { .. }
            | Opcode::SMod { .. }
            | Opcode::UMod { .. }
            | Opcode::Shl { .. }
            | Opcode::SShr { .. }
            | Opcode::UShr { .. }
            | Opcode::And { .. }
            | Opcode::Or { .. }
            | Opcode::Xor { .. }
            | Opcode::Neg { .. }
            | Opcode::Not { .. }
            | Opcode::Incr { .. }
            | Opcode::Decr { .. }
    )
}

/// `(function index, NullCheck op index)` of every NullCheck whose JIT lookahead
/// lands on a call to a nameless function.
pub(crate) fn offending(code: &Bytecode) -> Vec<(usize, usize)> {
    let idx = indexes(code);
    let anon = nameless(code);
    let mut out = Vec::new();
    for (fi, f) in code.functions.iter().enumerate() {
        let n = f.ops.len();
        for (i, op) in f.ops.iter().enumerate() {
            let Opcode::NullCheck { reg } = *op else {
                continue;
            };
            let mut j = i + 1;
            while j + 1 < n && skipped(&f.ops[j]) {
                j += 1;
            }
            if j >= n {
                continue;
            }
            let callee = match &f.ops[j] {
                Opcode::Call1 { fun, arg0, .. }
                | Opcode::Call2 { fun, arg0, .. }
                | Opcode::Call3 { fun, arg0, .. }
                | Opcode::Call4 { fun, arg0, .. }
                    if *arg0 == reg =>
                {
                    fun.0
                }
                Opcode::CallN { fun, args, .. } if Reg(args.len() as u32) == reg => fun.0,
                _ => continue,
            };
            if idx.get(&callee).is_some_and(|&c| anon[c]) {
                out.push((fi, i));
            }
        }
    }
    out
}

/// Breaks every offending NullCheck lookahead with a `Nop`.
pub(crate) fn patch_jit_names(code: &mut Bytecode) {
    let bad = offending(code);
    for &(fi, at) in bad.iter().rev() {
        insert_ops(&mut code.functions[fi], at + 1, vec![Opcode::Nop]);
    }
    if !bad.is_empty() {
        let sites: Vec<String> = bad
            .iter()
            .map(|&(fi, at)| format!("fn@{} op {at}", code.functions[fi].findex.0))
            .collect();
        eprintln!(
            "patched jit names: Nop after {} NullCheck(s) before a call to an unnamed function ({})",
            bad.len(),
            sites.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::*;

    /// The full patch chain leaves no NullCheck whose JIT lookahead hashes a NULL
    /// function name (the coop_spectate window closure did: startup crash in
    /// libhl!hl_hash_gen), and without this pass the chain does produce one.
    #[test]
    fn no_null_name_hash_on_installed_game() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let orig = read(&image);
        assert!(offending(&orig).is_empty(), "vanilla game already offends");
        let full = read(&crate::patch_image(&image).expect("patch"));
        assert_eq!(offending(&full), vec![], "full chain offends");

        let mut code = read(&image);
        crate::coop_spectate::patch_coop_spectate(&mut code);
        let bad = offending(&code);
        assert!(!bad.is_empty(), "coop_spectate window closure not detected");
        patch_jit_names(&mut code);
        assert_eq!(offending(&code), vec![]);
        for &(fi, at) in &bad {
            assert!(matches!(
                code.functions[fi].ops[at],
                Opcode::NullCheck { .. }
            ));
            assert!(matches!(code.functions[fi].ops[at + 1], Opcode::Nop));
        }
        let back = read(&write(&code));
        assert_eq!(offending(&back), vec![]);
    }
}
