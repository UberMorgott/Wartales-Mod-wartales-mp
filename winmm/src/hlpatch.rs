//! The HashLink bytecode patches wartales-mp applies to the game, and the C
//! table the winmm.dll proxy compiles in (port of internal/hlpatch; the why
//! of every patch is documented there). Every patch rewrites exactly one byte,
//! so no register, op count, jump target or debug line can shift.

use anyhow::{anyhow, bail, Result};
use std::fmt::Write as _;

/// One byte rewritten inside a HashLink image, found by an exact match of
/// `needle` that must occur exactly once in the whole image.
pub struct Patch {
    pub name: &'static str,
    pub needle: &'static [u8],
    pub index: usize,
    pub from: u8,
    pub to: u8,
}

/// All patches applied to the bytecode image, in order (see internal/hlpatch
/// for the disassembly behind every needle).
pub const PATCHES: &[Patch] = &[
    Patch {
        name: "flush/pendingClients timeout (NetworkHost.hx:1387)",
        needle: &[0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36, 0x0b, 0x09, 0x01, 0x1e, 0x01, 0x02, 0x01, 0x10],
        index: 15,
        from: 0x09, // r9  = now - lastMessage
        to: 0x0b,   // r11 = clientTimeout, i.e. !(x < x) -> always jump
    },
    Patch {
        name: "flush/clients timeout (NetworkHost.hx:1390)",
        needle: &[0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36, 0x0b, 0x09, 0x02, 0x1e, 0x01, 0x02, 0x01, 0x10],
        index: 15,
        from: 0x09,
        to: 0x0b,
    },
    Patch {
        name: "TitleScreen join code length check (TitleScreen.hx:646)",
        needle: &[0x47, 0x02, 0x26, 0x04, 0x02, 0x01, 0x01, 0x06, 0x1e, 0x39, 0x04, 0x06, 0x03, 0x21, 0x07, 0xc0, 0x00, 0x85, 0xbd, 0x1a, 0x01, 0xc0, 0x00, 0x60, 0x8a, 0x02, 0x07, 0x3a, 0x04],
        index: 9,
        from: 0x39, // JNotEq: error unless length == 5
        to: 0x30,   // JSLt:   error only if length < 5
    },
    Patch {
        name: "TitleScreen join code validate closure (TitleScreen.hx:641)",
        needle: &[0x47, 0x00, 0x26, 0x02, 0x00, 0x01, 0x01, 0x03, 0x1e, 0x39, 0x02, 0x03, 0x02, 0x03, 0x01, 0x01, 0x3a, 0x01, 0x03, 0x01, 0x00, 0x43, 0x01],
        index: 9,
        from: 0x39, // JNotEq: false unless length == 5
        to: 0x30,   // JSLt:   false only if length < 5
    },
    Patch {
        name: "TitleScreen join code maxLen (TitleScreen.hx:640)",
        needle: &[0x52, 0x06, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07, 0x27, 0x06, 0x01, 0x08, 0x21, 0x09, 0xc0, 0x00, 0x85, 0xb9],
        index: 4,
        from: 0x1e, // int@30 = 5
        to: 0x13,   // int@19 = 32
    },
    Patch {
        name: "TitleScreen join code formatText substr (TitleScreen.hx:642)",
        needle: &[0x47, 0x01, 0x01, 0x06, 0x01, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07, 0x1b, 0x01, 0x08, 0x01, 0x06, 0x08, 0x19, 0x01, 0xc0, 0x00, 0x5b, 0x53, 0x01],
        index: 7,
        from: 0x1e, // int@30 = 5
        to: 0x13,   // int@19 = 32
    },
    Patch {
        name: "LobbyState.allPlayersAssigned allows unassigned players (Lobby.hx:392)",
        needle: &[0x42, 0x47, 0x04, 0x26, 0x09, 0x04, 0x00, 0x31, 0x07, 0x09, 0x30, 0x00, 0x08, 0x07, 0x16, 0x07, 0x26, 0x09, 0x04, 0x00, 0x34, 0x08, 0x09, 0x02],
        index: 8,
        from: 0x07, // r7 = player loop index
        to: 0x09,   // r9 = players.length: always exit the loop
    },
];

/// Int constant pool entries (index, value) the patches rely on: a needle pins
/// the index an Int op loads, never the value behind it.
pub const INTS: &[(usize, i32)] = &[
    (30, 5),  // the vanilla code length the needles load
    (19, 32), // what maxLen and substr are redirected to
];

/// First bytes of a HashLink bytecode image.
pub const MAGIC: &str = "HLB";

fn count(buf: &[u8], needle: &[u8]) -> usize {
    buf.windows(needle.len()).filter(|w| *w == needle).count()
}

fn find(buf: &[u8], needle: &[u8]) -> Option<usize> {
    buf.windows(needle.len()).position(|w| w == needle)
}

/// Rewrites buf in place; on any mismatch nothing is written and an error is
/// returned, so a game update that moves this code leaves the bytecode alone.
pub fn apply(buf: &mut [u8]) -> Result<()> {
    let pool = int_pool(buf)?;
    for &(index, value) in INTS {
        if pool.get(index) != Some(&value) {
            bail!("hlpatch: int constant #{index} is not {value}");
        }
    }
    let mut at = Vec::with_capacity(PATCHES.len());
    for p in PATCHES {
        let n = count(buf, p.needle);
        if n != 1 {
            bail!("hlpatch {:?}: {n} matches, want 1", p.name);
        }
        let pos = find(buf, p.needle).unwrap() + p.index;
        if buf[pos] != p.from {
            bail!("hlpatch {:?}: byte at {pos} is 0x{:02x}, want 0x{:02x}", p.name, buf[pos], p.from);
        }
        at.push(pos);
    }
    for (p, pos) in PATCHES.iter().zip(at) {
        buf[pos] = p.to;
    }
    Ok(())
}

/// Reads the int constant pool: after "HLB" and the version byte come the
/// flags and table sizes as hl indexes (nints first), then the ints as LE i32s.
fn int_pool(buf: &[u8]) -> Result<Vec<i32>> {
    let m = MAGIC.len();
    if buf.len() < m + 1 || &buf[..m] != MAGIC.as_bytes() {
        bail!("hlpatch: not a HashLink image");
    }
    let version = buf[m];
    if !(4..=5).contains(&version) {
        bail!("hlpatch: unsupported bytecode version {version}");
    }
    let mut pos = m + 1;
    // flags, nints, nfloats, nstrings, [nbytes], ntypes, nglobals, nnatives,
    // nfunctions, nconstants, entrypoint
    let fields = if version >= 5 { 11 } else { 10 };
    let mut nints = 0;
    for i in 0..fields {
        let (v, n) = hl_index(&buf[pos..])?;
        if i == 1 {
            nints = v;
        }
        pos += n;
    }
    if nints > (buf.len() - pos) / 4 {
        bail!("hlpatch: int pool of {nints} entries runs past the image");
    }
    Ok(buf[pos..pos + 4 * nints].chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect())
}

/// Decodes one non-negative hl index (HashLink hl_read_index): (value, length).
fn hl_index(b: &[u8]) -> Result<(usize, usize)> {
    let (v, n) = match b {
        [b0, ..] if b0 & 0x80 == 0 => return Ok((*b0 as usize, 1)),
        [b0, b1, ..] if b0 & 0x40 == 0 => (((*b0 as usize) & 31) << 8 | *b1 as usize, 2),
        [b0, b1, b2, b3, ..] if b0 & 0x40 != 0 => {
            (((*b0 as usize) & 31) << 24 | (*b1 as usize) << 16 | (*b2 as usize) << 8 | *b3 as usize, 4)
        }
        _ => return Err(anyhow!("hlpatch: truncated header")),
    };
    if b[0] & 0x20 != 0 {
        bail!("hlpatch: negative table size in the header");
    }
    Ok((v, n))
}

/// The C header src/bin/hlpatchgen writes for the winmm.dll proxy.
pub fn header() -> String {
    let mut b = String::new();
    // The text is unchanged from the Go generator so the committed header stays byte-identical.
    b.push_str("/* Generated by tools/hlpatchgen from internal/hlpatch. Do not edit. */\n");
    b.push_str("#ifndef WARTALES_MP_HLPATCH_H\n#define WARTALES_MP_HLPATCH_H\n\n");
    let _ = write!(b, "#define HL_MAGIC {MAGIC:?}\n\n");
    b.push_str("struct hl_patch {\n\tconst unsigned char *needle;\n\tunsigned len;\n\tunsigned index;\n\tunsigned char from;\n\tunsigned char to;\n};\n\n");
    for (i, p) in PATCHES.iter().enumerate() {
        let _ = write!(b, "/* {} */\nstatic const unsigned char hl_needle_{i}[] = {{", p.name);
        for (j, c) in p.needle.iter().enumerate() {
            if j % 11 == 0 {
                b.push_str("\n\t");
            }
            let _ = write!(b, "0x{c:02x},");
        }
        b.push_str("\n};\n\n");
    }
    b.push_str("static const struct hl_patch hl_patches[] = {\n");
    for (i, p) in PATCHES.iter().enumerate() {
        let _ = writeln!(b, "\t{{ hl_needle_{i}, {}, {}, 0x{:02x}, 0x{:02x} }},", p.needle.len(), p.index, p.from, p.to);
    }
    b.push_str("};\n\n");
    let _ = write!(b, "#define HL_PATCH_COUNT {}\n\n#endif /* WARTALES_MP_HLPATCH_H */\n", PATCHES.len());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    // Read-only; WARTALES_HLBOOT overrides, absent file skips the live check.
    const GAME_PATH: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    fn patched(p: &Patch) -> Vec<u8> {
        let mut out = p.needle.to_vec();
        out[p.index] = p.to;
        out
    }

    fn good_ints() -> Vec<i32> {
        let mut pool = vec![0; 40];
        for &(i, v) in INTS {
            pool[i] = v;
        }
        pool
    }

    /// Header of a version 4 image with the given int pool (one-byte indexes).
    fn image(ints: &[i32]) -> Vec<u8> {
        let mut b = format!("{MAGIC}\x04").into_bytes();
        b.extend([0, ints.len() as u8, 0, 0, 0, 0, 0, 0, 0, 0]);
        for v in ints {
            b.extend(v.to_le_bytes());
        }
        b
    }

    fn all_needles() -> Vec<u8> {
        PATCHES.iter().flat_map(|p| p.needle.iter().copied()).collect()
    }

    #[test]
    fn apply_patches_live_bytecode() {
        let path = std::env::var("WARTALES_HLBOOT").unwrap_or_else(|_| GAME_PATH.into());
        let Ok(mut buf) = std::fs::read(&path) else {
            eprintln!("skip: no game bytecode at {path}");
            return;
        };
        assert!(buf.starts_with(MAGIC.as_bytes()), "{path} does not start with {MAGIC:?}");
        let orig = buf.clone();
        apply(&mut buf).expect("apply on the real bytecode");
        let changed = buf.iter().zip(&orig).filter(|(a, b)| a != b).count();
        assert_eq!(changed, PATCHES.len());
        for p in PATCHES {
            assert!(find(&buf, &patched(p)).is_some(), "patch {:?}: patched signature not found", p.name);
            assert_eq!(count(&buf, p.needle), 0, "patch {:?}: original signature still present", p.name);
        }
        assert!(apply(&mut buf).is_err(), "apply on an already patched image");
    }

    #[test]
    fn apply_requires_exactly_one_match() {
        let p = &PATCHES[0];
        // missing
        let mut buf = image(&good_ints());
        buf.extend([0u8; 4096]);
        assert!(apply(&mut buf).is_err());
        // duplicate
        let mut buf = image(&good_ints());
        buf.extend(p.needle);
        buf.extend(p.needle);
        assert!(apply(&mut buf).is_err());
        // broken signature
        let mut buf = image(&good_ints());
        let hdr = buf.len();
        buf.extend(all_needles());
        buf[hdr + p.index] = 0xff;
        assert!(apply(&mut buf).is_err());
        // nothing written on failure
        let mut buf = image(&good_ints());
        buf.extend(p.needle);
        let before = buf.clone();
        assert!(apply(&mut buf).is_err());
        assert_eq!(buf, before);
    }

    #[test]
    fn patches_are_well_formed() {
        for p in PATCHES {
            assert!(p.index < p.needle.len(), "patch {:?}: index outside the needle", p.name);
            assert_eq!(p.needle[p.index], p.from, "patch {:?}", p.name);
            assert_ne!(p.from, p.to, "patch {:?}: no-op", p.name);
        }
    }

    #[test]
    fn header_matches_shim() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../shim/proxy/hlpatch.h");
        let got = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
        assert!(got == header(), "{path} is stale: run cargo run --manifest-path helper/Cargo.toml --bin hlpatchgen -- -out shim/proxy/hlpatch.h");
    }

    #[test]
    fn all_players_assigned_allows_player_without_human() {
        let head: &[u8] = &[0x42, 0x47, 0x04, 0x26, 0x09, 0x04, 0x00, 0x31, 0x07, 0x09, 0x30, 0x00, 0x08, 0x07, 0x16, 0x07, 0x26, 0x09, 0x04, 0x00, 0x34, 0x08, 0x09, 0x02];
        let hdr = image(&good_ints());
        let mut buf = hdr.clone();
        buf.extend(head);
        for p in PATCHES {
            if find(p.needle, head).is_none() {
                buf.extend(p.needle);
            }
        }
        apply(&mut buf).unwrap();
        let buf = &buf[hdr.len()..];
        for players in 1..=4 {
            let mut regs = [0i32; 10];
            regs[7] = 0;
            regs[9] = players;
            assert!(buf[7] == 0x31 && buf[10] == 48, "player loop no longer branches to the success epilogue");
            assert!(regs[buf[8] as usize] >= regs[buf[9] as usize], "{players} players: still enters the loop");
        }
    }

    #[test]
    fn apply_checks_the_int_pool() {
        let code = all_needles();
        let mut buf = image(&good_ints());
        buf.extend(&code);
        apply(&mut buf).expect("a matching pool");
        for &(i, _) in INTS {
            let mut pool = good_ints();
            pool[i] = 1;
            let mut buf = image(&pool);
            buf.extend(&code);
            let before = buf.clone();
            assert!(apply(&mut buf).is_err(), "int #{i} changed to 1");
            assert_eq!(buf, before);
        }
        let mut buf = image(&good_ints()[..20]);
        buf.extend(&code);
        assert!(apply(&mut buf).is_err(), "a pool too short for #30");
    }

    #[test]
    fn hl_index_decodes() {
        for (inp, v, n) in [(&[0x1e][..], 30, 1), (&[0x81, 0x02], 0x102, 2), (&[0xc1, 0x02, 0x03, 0x04], 0x01020304, 4)] {
            assert_eq!(hl_index(inp).unwrap(), (v, n), "{inp:x?}");
        }
        for bad in [&[][..], &[0x81], &[0xa1, 0], &[0xc1, 0, 0]] {
            assert!(hl_index(bad).is_err(), "{bad:x?}");
        }
    }
}
