//! The one-byte HashLink bytecode patches applied before the patcher passes
//! (the why of every patch is in the comment below). Every patch rewrites
//! exactly one byte, so no register, op count, jump target or debug line can
//! shift. Error texts are the old C shim's shim.log lines.

// Patch 1+2 -- the game's own timeout path is fatal:
//
//    hxbit.NetworkHost.flush@3969 (hxbit/NetworkHost.hx:1387,1390) calls
//    c.timeout() on every client whose lastMessage is older than clientTimeout
//    (60 s). mpman.net.Client.timeout@24013 then runs, in order:
//      1. super.timeout() -> NetworkClient.stop@3982, which REMOVES the client
//         from host.clients, and then service.onStop();
//      2. for a lib.HostWT service that onStop is @55008 (src/lib/HostWT.hx:93),
//         whose first statement is clients[0].stop() -- on the array the step
//         above just shortened, so a host with a single client reads index 0 of
//         an empty array: "Null access", HostWT.hx:93;
//      3. if it got that far, s.onConnect(null) -> @55007 (HostWT.hx:85) builds
//         new ClientWT(this, null) whenever isAuth is set, and
//         mpman.net.Client.__constructor__@24016 null-checks the service at
//         Client.hx:13: "Null access", the crash users actually hit.
//
// So no client of any role can time out without killing the process: nothing
// working is lost by never letting the check fire. It fires in an idle lobby
// because the lobby owner's own clients[0] (HostWT.hx:70) and the per-user
// service client pushed by @55007 never receive anything -- for the owner
// LobbyService.onMessage@54929 routes every inbound packet to a per-guest
// LobbyUserService (get_isAuth@54925 is true), so their lastMessage is frozen
// from the moment the lobby is created. Real disconnects keep being reported by
// the transport itself (Host.close@12150 -> service.onStop) and by the master's
// lobby/leave pushes (onUserLeft@54928), neither of which goes through timeout.
//
// The patch turns each of the two "did this client time out" comparisons into a
// comparison of clientTimeout with itself, which JNotLt always takes, so the
// call is skipped and the loop moves on. One byte each, same opcode, same
// length.
//
// Patches 3..6 -- the title screen's "join by code" field is built by
// joinWithCode2@23875 (src/ui/win/TitleScreen.hx:638-661) around the length
// of the game's own lobby codes, 5, in four places; ours are 13, 16 or 25
// symbols (see code.rs). All four load the 5 from int constant #30:
//
//   - the submit handler @34236 (TitleScreen.hx:646) and the validate
//     closure @34233 (:641) refuse any other length: their JNotEq (0x39)
//     becomes JSLt (0x30) with the same operands, so only a code shorter than
//     5 is refused, vanilla 5-symbol codes keep working and everything longer
//     reaches joinCode@24714, where the mod's master takes over;
//   - InputText.maxLen (:640) and the substr(0, 5) in the formatText closure
//     @34234 (:642) cap what can be typed and what is kept: their Int operand
//     is redirected from int constant #30 (5) to #19 (32), which leaves room
//     for a 25-symbol code with a few separators and keeps the widget sane.
//     Both indices fit one varint byte, so nothing shifts.
//
// Patch 7 -- allPlayersAssigned@24643 (Lobby.hx:392) requires each player
// to own a non-animal unit. Modded starting parties can have three humans and
// one animal for four players. Skip its player loop by comparing the player
// count with itself: op 13 jumps to op 62, where r3 is still null and the
// function returns true. Both canStart@24646 and CustomizeScreen.update@26311
// call this predicate, so the start gate and its UI agree. allUnitsAssigned,
// allPlayersIn, lobby status and loaded-save owner checks remain intact.
//
// No patch touches DLC ownership: LobbyState.missingDlcs / canStart's DLC
// check and every other DLC gate run exactly as in the vanilla game.
//
// The first two needles cover the same seven ops of hxbit.NetworkHost.flush@3969, once
// for the pendingClients loop (NetworkHost.hx:1387) and once for the clients
// loop (NetworkHost.hx:1390); they differ only in the JNotLt jump offset:
//
//    47 10           NullCheck   r16
//    26 0b 10 07     Field       r11 = r16.lastMessage
//    08 09 06 0b     Sub         r9  = r6 - r11          (now - lastMessage)
//    28 0b 06        GetThis     r11 = this.clientTimeout
//    36 0b 09 kk     JNotLt      if r11 !< r9 jump over  <- r9 becomes r11
//    1e 01 02 01 10  CallMethod  r1  = r16.timeout()     (proto index 2)
//
// The third, fifth and sixth needles stop before renderer-specific function
// indices. Each sequence of complete ops is unique in both shipped images.
// The third locates the join handler's length check:
//
//    47 02                    NullCheck     r2
//    19 02 00 02              Call1         r2 = fn@0(r2)
//    47 02                    NullCheck     r2
//    26 04 02 01              Field         r4 = r2.length
//    01 06 1e                 Int           r6 = int@30 (= 5)
//    39 04 06 03              JNotEq        if r4 != r6 jump +3  <- becomes JSLt (0x30)
//
// The fourth is the whole validate closure @34233 (8 ops):
//
//    47 00          NullCheck r0
//    26 02 00 01    Field     r2 = r0.length
//    01 03 1e       Int       r3 = int@30 (= 5)
//    39 02 03 02    JNotEq    if r2 != r3 jump +2  <- becomes JSLt (0x30)
//    03 01 01       Bool      r1 = true
//    3a 01          JAlways   jump +1
//    03 01 00       Bool      r1 = false
//    43 01          Ret       r1
//
// The fifth locates the assignment to maxLen in joinWithCode2:
//
//    47 05                NullCheck     r5
//    26 02 05 22          Field         r2 = r5.field@34
//    52 06                New           r6 = new {formatText, maxLen, validate}
//    01 07 1e             Int           r7 = int@30 (= 5)  <- int@19 (= 32)
//    3b 08 07             ToDyn         r8 = r7
//    27 06 01 08          SetField      r6.maxLen = r8
//
// The sixth locates the substr cap in the formatText closure:
//
//    47 01                NullCheck r1
//    01 06 01             Int       r6 = int@1 (= 0)
//    01 07 1e             Int       r7 = int@30 (= 5)  <- int@19 (= 32)
//    3b 08 07             ToDyn     r8 = r7
//    1b 01 08 01 06 08    Call3     r1 = substr@8(r1, r6, r8)

use anyhow::{anyhow, bail, Result};

/// One byte rewritten inside a HashLink image, found by an exact match of
/// `needle` that must occur exactly once in the whole image.
pub struct Patch {
    pub name: &'static str,
    pub needle: &'static [u8],
    pub index: usize,
    pub from: u8,
    pub to: u8,
}

/// All patches applied to the bytecode image, in order (the disassembly
/// behind every needle is in the comment at the top).
pub const PATCHES: &[Patch] = &[
    Patch {
        name: "flush/pendingClients timeout (NetworkHost.hx:1387)",
        needle: &[
            0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36,
            0x0b, 0x09, 0x01, 0x1e, 0x01, 0x02, 0x01, 0x10,
        ],
        index: 15,
        from: 0x09, // r9  = now - lastMessage
        to: 0x0b,   // r11 = clientTimeout, i.e. !(x < x) -> always jump
    },
    Patch {
        name: "flush/clients timeout (NetworkHost.hx:1390)",
        needle: &[
            0x47, 0x10, 0x26, 0x0b, 0x10, 0x07, 0x08, 0x09, 0x06, 0x0b, 0x28, 0x0b, 0x06, 0x36,
            0x0b, 0x09, 0x02, 0x1e, 0x01, 0x02, 0x01, 0x10,
        ],
        index: 15,
        from: 0x09,
        to: 0x0b,
    },
    Patch {
        name: "TitleScreen join code length check (TitleScreen.hx:646)",
        needle: &[
            0x47, 0x02, 0x19, 0x02, 0x00, 0x02, 0x47, 0x02, 0x26, 0x04, 0x02, 0x01, 0x01, 0x06,
            0x1e, 0x39, 0x04, 0x06, 0x03,
        ],
        index: 15,
        from: 0x39, // JNotEq: error unless length == 5
        to: 0x30,   // JSLt:   error only if length < 5
    },
    Patch {
        name: "TitleScreen join code validate closure (TitleScreen.hx:641)",
        needle: &[
            0x47, 0x00, 0x26, 0x02, 0x00, 0x01, 0x01, 0x03, 0x1e, 0x39, 0x02, 0x03, 0x02, 0x03,
            0x01, 0x01, 0x3a, 0x01, 0x03, 0x01, 0x00, 0x43, 0x01,
        ],
        index: 9,
        from: 0x39, // JNotEq: false unless length == 5
        to: 0x30,   // JSLt:   false only if length < 5
    },
    Patch {
        name: "TitleScreen join code maxLen (TitleScreen.hx:640)",
        needle: &[
            0x47, 0x05, 0x26, 0x02, 0x05, 0x22, 0x52, 0x06, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07,
            0x27, 0x06, 0x01, 0x08,
        ],
        index: 10,
        from: 0x1e, // int@30 = 5
        to: 0x13,   // int@19 = 32
    },
    Patch {
        name: "TitleScreen join code formatText substr (TitleScreen.hx:642)",
        needle: &[
            0x47, 0x01, 0x01, 0x06, 0x01, 0x01, 0x07, 0x1e, 0x3b, 0x08, 0x07, 0x1b, 0x01, 0x08,
            0x01, 0x06, 0x08,
        ],
        index: 7,
        from: 0x1e, // int@30 = 5
        to: 0x13,   // int@19 = 32
    },
    Patch {
        name: "LobbyState.allPlayersAssigned allows unassigned players (Lobby.hx:392)",
        needle: &[
            0x42, 0x47, 0x04, 0x26, 0x09, 0x04, 0x00, 0x31, 0x07, 0x09, 0x30, 0x00, 0x08, 0x07,
            0x16, 0x07, 0x26, 0x09, 0x04, 0x00, 0x34, 0x08, 0x09, 0x02,
        ],
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

/// Rewrites buf in place and returns the offset of every patched byte (in
/// PATCHES order); on any mismatch nothing is written and an error is
/// returned, so a game update that moves this code leaves the bytecode alone.
pub fn apply(buf: &mut [u8]) -> Result<Vec<usize>> {
    let pool = int_pool(buf)?;
    for &(index, value) in INTS {
        match pool.get(index) {
            None => bail!(
                "patch: int constant #{index} missing ({} in the table)",
                pool.len()
            ),
            Some(&got) if got != value => {
                bail!("patch: int constant #{index} is {got}, want {value}")
            }
            Some(_) => {}
        }
    }
    let mut at = Vec::with_capacity(PATCHES.len());
    for (i, p) in PATCHES.iter().enumerate() {
        if count(buf, p.needle) != 1 {
            bail!("patch {i}: signature missing or ambiguous");
        }
        let pos = find(buf, p.needle).unwrap() + p.index;
        if buf[pos] != p.from {
            bail!(
                "patch {i}: byte at {pos} is 0x{:02x}, want 0x{:02x}",
                buf[pos],
                p.from
            );
        }
        at.push(pos);
    }
    for (p, &pos) in PATCHES.iter().zip(&at) {
        buf[pos] = p.to;
    }
    Ok(at)
}

/// Reads the int constant pool: after "HLB" and the version byte come the
/// flags and table sizes as hl indexes (nints first), then the ints as LE i32s.
fn int_pool(buf: &[u8]) -> Result<Vec<i32>> {
    let m = MAGIC.len();
    if buf.len() < m + 1 || &buf[..m] != MAGIC.as_bytes() {
        bail!("patch: image does not start with {MAGIC}");
    }
    let version = buf[m];
    if !(4..=5).contains(&version) {
        bail!("patch: bytecode format {version}, want 4 or 5");
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
        bail!("patch: int table runs past the image");
    }
    Ok(buf[pos..pos + 4 * nints]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_le_bytes(*c))
        .collect())
}

/// Decodes one non-negative hl index (HashLink hl_read_index): (value, length).
fn hl_index(b: &[u8]) -> Result<(usize, usize)> {
    let (v, n) = match b {
        [b0, ..] if b0 & 0x80 == 0 => return Ok((*b0 as usize, 1)),
        [b0, b1, ..] if b0 & 0x40 == 0 => (((*b0 as usize) & 31) << 8 | *b1 as usize, 2),
        [b0, b1, b2, b3, ..] if b0 & 0x40 != 0 => (
            ((*b0 as usize) & 31) << 24 | (*b1 as usize) << 16 | (*b2 as usize) << 8 | *b3 as usize,
            4,
        ),
        _ => return Err(anyhow!("patch: bytecode header unreadable")),
    };
    if b[0] & 0x20 != 0 {
        bail!("patch: bytecode header unreadable");
    }
    Ok((v, n))
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
        b.extend([0, u8::try_from(ints.len()).unwrap(), 0, 0, 0, 0, 0, 0, 0, 0]);
        for v in ints {
            b.extend(v.to_le_bytes());
        }
        b
    }

    fn all_needles() -> Vec<u8> {
        PATCHES
            .iter()
            .flat_map(|p| p.needle.iter().copied())
            .collect()
    }

    #[test]
    fn apply_patches_live_bytecode() {
        let path = std::env::var("WARTALES_HLBOOT").unwrap_or_else(|_| GAME_PATH.into());
        check_live_image(std::path::Path::new(&path));
    }

    #[test]
    fn apply_patches_live_opengl_bytecode() {
        let path = std::env::var("WARTALES_HLBOOT").unwrap_or_else(|_| GAME_PATH.into());
        check_live_image(&std::path::Path::new(&path).with_file_name("sdlboot.dat"));
    }

    fn check_live_image(path: &std::path::Path) {
        let Ok(mut buf) = std::fs::read(path) else {
            eprintln!("skip: no game bytecode at {}", path.display());
            return;
        };
        assert!(
            buf.starts_with(MAGIC.as_bytes()),
            "{} does not start with {MAGIC:?}",
            path.display()
        );
        let orig = buf.clone();
        apply(&mut buf).expect("apply on the real bytecode");
        let changed = buf.iter().zip(&orig).filter(|(a, b)| a != b).count();
        assert_eq!(changed, PATCHES.len());
        for p in PATCHES {
            assert!(
                find(&buf, &patched(p)).is_some(),
                "patch {:?}: patched signature not found",
                p.name
            );
            assert_eq!(
                count(&buf, p.needle),
                0,
                "patch {:?}: original signature still present",
                p.name
            );
        }
        assert!(
            apply(&mut buf).is_err(),
            "apply on an already patched image"
        );
    }

    #[test]
    fn apply_requires_exactly_one_match() {
        let p = &PATCHES[0];
        // missing
        let mut buf = image(&good_ints());
        buf.extend([0u8; 4096]);
        assert!(apply(&mut buf).is_err());
        // A second match must reject even an otherwise complete image.
        for duplicate in PATCHES {
            let mut buf = image(&good_ints());
            buf.extend(all_needles());
            buf.extend(duplicate.needle);
            let before = buf.clone();
            assert!(apply(&mut buf).is_err());
            assert_eq!(buf, before);
        }
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
            assert!(
                p.index < p.needle.len(),
                "patch {:?}: index outside the needle",
                p.name
            );
            assert_eq!(p.needle[p.index], p.from, "patch {:?}", p.name);
            assert_ne!(p.from, p.to, "patch {:?}: no-op", p.name);
        }
    }

    #[test]
    fn all_players_assigned_allows_player_without_human() {
        let head: &[u8] = &[
            0x42, 0x47, 0x04, 0x26, 0x09, 0x04, 0x00, 0x31, 0x07, 0x09, 0x30, 0x00, 0x08, 0x07,
            0x16, 0x07, 0x26, 0x09, 0x04, 0x00, 0x34, 0x08, 0x09, 0x02,
        ];
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
            assert!(
                buf[7] == 0x31 && buf[10] == 48,
                "player loop no longer branches to the success epilogue"
            );
            assert!(
                regs[buf[8] as usize] >= regs[buf[9] as usize],
                "{players} players: still enters the loop"
            );
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
        for (inp, v, n) in [
            (&[0x1e][..], 30, 1),
            (&[0x81, 0x02], 0x102, 2),
            (&[0xc1, 0x02, 0x03, 0x04], 0x0102_0304, 4),
        ] {
            assert_eq!(hl_index(inp).unwrap(), (v, n), "{inp:x?}");
        }
        for bad in [&[][..], &[0x81], &[0xa1, 0], &[0xc1, 0, 0]] {
            assert!(hl_index(bad).is_err(), "{bad:x?}");
        }
    }
}
