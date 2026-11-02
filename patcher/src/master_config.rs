// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// The game's master server is the mod's own, in-process, on loopback.
//
// `Const.getMPManConfig` (called by `Main.initMpman`) builds the mpman master
// list: for each of `master.shirogames.com` and `master2.shirogames.com` a
// `{host, port: 60442, ssl: true, game, user, pass}` object. The pass loads
// "127.0.0.1" instead of both host strings and `false` instead of `ssl`'s
// `true` (same op count, same registers): the game talks plain WebSocket to
// winmm.dll's master on 127.0.0.1:60442, no DNS hook, no TLS, no CA install.
// Under the harness feature, WARTALES_MP_TEST_INSTANCE=N (1..9) at patch time
// makes it 127.0.0.N, so side-by-side instances each reach their own master.
//
// All-or-nothing: a mismatch skips the pass and `patch_image` refuses the
// image, so the game never talks plaintext to Shiro's public hosts.

use super::*;

const MASTER_HOSTS: [&str; 2] = ["master.shirogames.com", "master2.shirogames.com"];
const LOOPBACK: [&str; 9] = [
    "127.0.0.1", "127.0.0.2", "127.0.0.3", "127.0.0.4", "127.0.0.5", "127.0.0.6", "127.0.0.7",
    "127.0.0.8", "127.0.0.9",
];

/// Where the master listens: 127.0.0.1, or 127.0.0.N for harness instance N.
fn master_host() -> &'static str {
    #[cfg(feature = "harness")]
    if let Ok(v) = std::env::var("WARTALES_MP_TEST_INSTANCE") {
        if let [d @ b'1'..=b'9'] = v.as_bytes() {
            return LOOPBACK[usize::from(d - b'1')];
        }
    }
    LOOPBACK[0]
}

struct Plan {
    /// Position of getMPManConfig in `code.functions`.
    fi: usize,
    /// Ops loading the two master host strings.
    hosts: [usize; 2],
    /// The `Bool true` stored into `ssl`.
    ssl: usize,
}

/// The global of the String constant `value`, if the image has one.
fn string_constant(code: &Bytecode, g: RefGlobal) -> Option<&str> {
    code.constants
        .iter()
        .flatten()
        .find(|c| c.global == g)
        .and_then(|c| c.fields.first())
        .and_then(|&si| code.strings.get(si))
        .map(|s| s.as_str())
}

fn plan(code: &Bytecode) -> Result<Plan> {
    let f = crate::diag::static_fn(code, "$Const", "getMPManConfig")?;
    let fi = fun_index(code, f.findex)?;
    let ssl_s = string_index(code, "ssl")?;
    let mut hosts = [None, None];
    for (i, op) in f.ops.iter().enumerate() {
        let Opcode::GetGlobal { global, .. } = op else {
            continue;
        };
        let Some(k) = string_constant(code, *global).and_then(|v| MASTER_HOSTS.iter().position(|h| *h == v))
        else {
            continue;
        };
        if hosts[k].replace(i).is_some() {
            bail!("getMPManConfig fn@{}: {} loaded twice", f.findex.0, MASTER_HOSTS[k]);
        }
    }
    let [Some(h0), Some(h1)] = hosts else {
        bail!("getMPManConfig fn@{}: master hosts not found", f.findex.0);
    };
    // `Bool r = true; DynSet obj.ssl = r`, exactly once.
    let ssl: Vec<usize> = f
        .ops
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| match w {
            [Opcode::Bool { dst, value }, Opcode::DynSet { field, src, .. }]
                if *field == ssl_s && src == dst && value.0 =>
            {
                Some(i)
            }
            _ => None,
        })
        .collect();
    let ssl_sets = f
        .ops
        .iter()
        .filter(|op| matches!(op, Opcode::DynSet { field, .. } if *field == ssl_s))
        .count();
    let (&[ssl], 1) = (&ssl[..], ssl_sets) else {
        bail!(
            "getMPManConfig fn@{}: expected one ssl = true, found {} of {ssl_sets} ssl sets",
            f.findex.0,
            ssl.len()
        );
    };
    Ok(Plan { fi, hosts: [h0, h1], ssl })
}

fn apply(code: &mut Bytecode, p: Plan) -> Result<()> {
    let host = master_host();
    let str_t = obj_type(code, "String")?;
    let g = crate::job_xp::str_global(code, str_t, host);
    let f = &mut code.functions[p.fi];
    for at in p.hosts {
        let Opcode::GetGlobal { dst, .. } = f.ops[at] else {
            unreachable!("validated by plan");
        };
        f.ops[at] = Opcode::GetGlobal { dst, global: g };
    }
    let Opcode::Bool { dst, .. } = f.ops[p.ssl] else {
        unreachable!("validated by plan");
    };
    f.ops[p.ssl] = Opcode::Bool {
        dst,
        value: hlbc::types::ValBool(false),
    };
    eprintln!(
        "patched master config fn@{}: both master hosts -> {host}, ssl off",
        f.findex.0
    );
    Ok(())
}

/// Points the game's master list at the in-process master, or leaves `code`
/// untouched and logs why.
pub(crate) fn patch_master_config(code: &mut Bytecode) {
    match plan(code).and_then(|p| apply(code, p)) {
        Ok(()) => {}
        Err(e) => crate::skipped(format!("master config skipped: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::testutil::{game, read, write};

    #[test]
    fn patches_installed_game() {
        let Some(image) = game() else { return };
        let orig = read(&image);
        let p = plan(&orig).expect("plan");
        let mut code = read(&image);
        patch_master_config(&mut code);
        let patched = write(&code);
        let back = read(&patched);

        for (i, (a, b)) in orig.functions.iter().zip(&back.functions).enumerate() {
            let same = format!("{:?}", a.ops) == format!("{:?}", b.ops) && a.regs == b.regs;
            assert_eq!(same, i != p.fi, "function #{i} (fn@{})", a.findex.0);
        }
        let (a, b) = (&orig.functions[p.fi], &back.functions[p.fi]);
        assert_eq!(b.ops.len(), a.ops.len());
        assert_eq!(b.regs, a.regs);
        for (i, (x, y)) in a.ops.iter().zip(&b.ops).enumerate() {
            let changed = format!("{x:?}") != format!("{y:?}");
            assert_eq!(changed, p.hosts.contains(&i) || i == p.ssl, "op {i}");
        }
        for at in p.hosts {
            let Opcode::GetGlobal { global, .. } = b.ops[at] else {
                panic!("op {at} is not GetGlobal");
            };
            assert_eq!(string_constant(&back, global), Some("127.0.0.1"));
        }
        assert!(matches!(b.ops[p.ssl], Opcode::Bool { value: hlbc::types::ValBool(false), .. }));
        // The port stays 60442.
        assert!(b.ops.iter().any(|op| matches!(op, Opcode::Int { ptr, .. } if back.ints[ptr.0] == 60442)));

        let mut again = read(&patched);
        assert!(plan(&again).is_err(), "applies twice");
        patch_master_config(&mut again);
        assert!(write(&again) == patched);
    }

    #[test]
    fn refuses_a_changed_config() {
        let Some(image) = game() else { return };
        let mut code = read(&image);
        let p = plan(&code).expect("plan");
        // ssl already false: not the config this pass was written against.
        let f = &mut code.functions[p.fi];
        f.ops[p.ssl] = Opcode::Bool {
            dst: Reg(13),
            value: hlbc::types::ValBool(false),
        };
        let before = write(&code);
        patch_master_config(&mut code);
        assert!(write(&code) == before, "a mismatch left the image touched");
    }
}
