//! The short join code: Crockford base32 over a small payload with a
//! length-specific layout, plus one check symbol (port of internal/code).
//!
//! Compact direct layouts omit flags and infer the default port from length.
//! Legacy layouts retain their flags byte and remain readable:
//!
//! ```text
//! direct    [ipv4:4]                                                   4 bytes -> 7 + 1 = 8 symbols
//! direct    [ipv4:4][port:2]                                            6 bytes -> 10 + 1 = 11 symbols
//! endpoint  [flags:1 bit7=0][ipv4:4][port:2]                              7 bytes -> 12 + 1 = 13 symbols
//! steam     [flags:1 bit7=1][account:4 BE][key:4 BE]                       9 bytes -> 15 + 1 = 16 symbols
//! combined  [flags:1 bit7=1 bit6=1][ipv4:4][port:2][account:4 BE][key:4 BE] 15 bytes -> 24 + 1 = 25 symbols
//! punch     [flags:1 = 0][ipv4:4][tcp port:2][udp port:2]                    9 bytes -> 15 + 1 = 16 symbols
//! reply     [flags:1 bit7=0 bit6=1][ipv4:4][udp port:2]                     7 bytes -> 12 + 1 = 13 symbols
//! ```
//!
//! A punch code is a host's manual code: the direct TCP route plus the UDP
//! address STUN saw for the host's punch socket (same IP). It shares the steam
//! length and is told apart by bit 7. A reply code goes the other way: a guest
//! whose punch got no answer hands the host its own UDP address. It shares the
//! endpoint length and is told apart by bit 6, which older helpers reject.
//!
//! An endpoint route is the direct transport: the guest connects to ip:port.
//! A steam route is the SDR transport: the guest reaches the host through
//! Valve's relay by SteamID64 (rebuilt from the 32-bit account id: every
//! player account is universe 1, type Individual, instance 1), and must
//! present the 32-bit key to the host's master. A combined code carries both
//! routes and the guest tries them in that order.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{bail, Result};

/// Crockford base32: digits plus letters without I, L, O and U.
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The port omitted from compact direct codes.
pub const DEFAULT_PORT: u16 = 14250;
/// An IPv4 address plus one check symbol.
pub const DIRECT_LENGTH: usize = 8;
/// A direct code that also carries a custom port.
pub const DIRECT_PORT_LENGTH: usize = 11;

const PAYLOAD_LEN: usize = 7; // flags(1) + ipv4(4) + port(2)
const BODY_LEN: usize = 12; // ceil(7*8/5)
/// Total symbols of an endpoint join code.
pub const LENGTH: usize = BODY_LEN + 1;

const STEAM_PAYLOAD_LEN: usize = 9; // flags(1) + account(4) + key(4)
const STEAM_BODY_LEN: usize = 15; // ceil(9*8/5)
/// Total symbols of a steam join code.
pub const STEAM_LENGTH: usize = STEAM_BODY_LEN + 1;

const COMBINED_PAYLOAD_LEN: usize = 15; // flags(1) + ipv4(4) + port(2) + account(4) + key(4)
const COMBINED_BODY_LEN: usize = 24; // 15*8/5 exactly
/// Total symbols of a combined join code.
pub const COMBINED_LENGTH: usize = COMBINED_BODY_LEN + 1;

// The length tells the three code kinds apart.
const _: () =
    assert!(LENGTH != STEAM_LENGTH && STEAM_LENGTH != COMBINED_LENGTH && LENGTH != COMBINED_LENGTH);

/// Flags bit 7: a steam route.
pub const FLAG_STEAM: u8 = 0x80;
/// Flags bit 6, together with FLAG_STEAM: the combined layout.
pub const FLAG_BOTH: u8 = 0x40;
/// Bit 6 without bit 7 on an endpoint-length code: a guest's reply code.
pub const FLAG_REPLY: u8 = FLAG_BOTH;

/// SteamID64 of account 0 in universe Public, type Individual, instance
/// Desktop: what every player account is.
const STEAM_ID_BASE: u64 = 0x0110_0001_0000_0000;

/// What an endpoint join code carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub flags: u8,
    pub ip: IpAddr, // IPv4 on the wire
    pub port: u16,
}

impl Endpoint {
    /// An endpoint without flags.
    pub fn new(ip: impl Into<IpAddr>, port: u16) -> Self {
        Endpoint {
            flags: 0,
            ip: ip.into(),
            port,
        }
    }

    /// The endpoint as a socket address ("ip:port").
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port)
    }

    fn ip4(&self, what: &str) -> Result<[u8; 4]> {
        let v4 = match self.ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(v6) => v6.to_ipv4_mapped(), // Go's To4 accepts ::ffff:a.b.c.d
        };
        match v4 {
            Some(v4) => Ok(v4.octets()),
            None => bail!("{what} needs an IPv4 address, got {}", self.ip),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

/// What a steam join code carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Steam {
    /// Low 7 bits are free; bit 7 is always set on the wire.
    pub flags: u8,
    /// The low 32 bits of the host's SteamID64.
    pub account_id: u32,
    /// The proxy-link key the host's master expects.
    pub key: u32,
}

impl Steam {
    /// Rebuilds the host's SteamID64.
    pub fn steam_id64(&self) -> u64 {
        STEAM_ID_BASE | u64::from(self.account_id)
    }
}

/// The account id of a SteamID64, and whether the id is an ordinary player
/// account (the only kind a steam code can carry).
pub fn account_id(steam_id64: u64) -> (u32, bool) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the account id is the low dword"
    )]
    (
        steam_id64 as u32,
        steam_id64 & !0xffff_ffff == STEAM_ID_BASE,
    )
}

/// A decoded join code: the routes it carries. At least one is set; a
/// combined code sets both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Code {
    pub endpoint: Option<Endpoint>,
    pub steam: Option<Steam>,
    /// The host's UDP punch address (punch layout, with endpoint).
    pub punch: Option<Endpoint>,
    /// A guest's UDP address (reply layout, alone): not a route.
    pub reply: Option<Endpoint>,
}

/// A host's 16 symbol code: the TCP endpoint e and the UDP port of the punch
/// socket as STUN saw it, on the same IP.
pub fn encode_punch(e: Endpoint, udp_port: u16) -> Result<String> {
    let ip = e.ip4("join code")?;
    let mut p = [0u8; STEAM_PAYLOAD_LEN]; // flags 0
    p[1..5].copy_from_slice(&ip);
    p[5..7].copy_from_slice(&e.port.to_be_bytes());
    p[7..9].copy_from_slice(&udp_port.to_be_bytes());
    Ok(encode(&p, STEAM_BODY_LEN))
}

/// A guest's 13 symbol reply code from its UDP address.
pub fn encode_reply(e: Endpoint) -> Result<String> {
    let ip = e.ip4("reply code")?;
    let [a, b] = e.port.to_be_bytes();
    Ok(encode(
        &[FLAG_REPLY, ip[0], ip[1], ip[2], ip[3], a, b],
        BODY_LEN,
    ))
}

/// An endpoint and a steam route as a 25 symbol code.
pub fn encode_combined(e: Endpoint, s: Steam) -> Result<String> {
    let ip = e.ip4("join code")?;
    if (e.flags | s.flags) & (FLAG_STEAM | FLAG_BOTH) != 0 {
        bail!("flags bits 7 and 6 are reserved for the layout");
    }
    let mut p = [0u8; COMBINED_PAYLOAD_LEN];
    p[0] = e.flags | s.flags | FLAG_STEAM | FLAG_BOTH;
    p[1..5].copy_from_slice(&ip);
    p[5..7].copy_from_slice(&e.port.to_be_bytes());
    p[7..11].copy_from_slice(&s.account_id.to_be_bytes());
    p[11..15].copy_from_slice(&s.key.to_be_bytes());
    Ok(encode(&p, COMBINED_BODY_LEN))
}

/// A direct-only code. The standard port is implicit in the 8-symbol form;
/// custom ports use 11 symbols. Nonzero legacy flags keep the 13 symbol
/// encoding so callers do not silently lose flag bits.
pub fn encode_direct(e: Endpoint) -> Result<String> {
    if e.flags != 0 {
        return encode_endpoint(e);
    }
    let ip = e.ip4("join code")?;
    if e.port == DEFAULT_PORT {
        return Ok(encode(&ip, 7));
    }
    let [a, b] = e.port.to_be_bytes();
    Ok(encode(&[ip[0], ip[1], ip[2], ip[3], a, b], 10))
}

/// An endpoint as a 13 symbol join code (Go's `Encode`).
pub fn encode_endpoint(e: Endpoint) -> Result<String> {
    let ip = e.ip4("join code")?;
    if e.flags & (FLAG_STEAM | FLAG_BOTH) != 0 {
        bail!(
            "endpoint flags 0x{:02x}: bits 7 and 6 are reserved for the layout",
            e.flags
        );
    }
    let [a, b] = e.port.to_be_bytes();
    Ok(encode(
        &[e.flags, ip[0], ip[1], ip[2], ip[3], a, b],
        BODY_LEN,
    ))
}

/// A host SteamID + key as a 16 symbol join code.
pub fn encode_steam(s: Steam) -> String {
    let mut p = [0u8; STEAM_PAYLOAD_LEN];
    p[0] = s.flags & !(FLAG_STEAM | FLAG_BOTH) | FLAG_STEAM;
    p[1..5].copy_from_slice(&s.account_id.to_be_bytes());
    p[5..9].copy_from_slice(&s.key.to_be_bytes());
    encode(&p, STEAM_BODY_LEN)
}

/// Packs payload MSB first into body symbols (zero padded at the low end) and
/// appends the check symbol, the sum of the body symbols mod 32.
fn encode(payload: &[u8], body: usize) -> String {
    let bits = payload.len() * 8;
    let mut out = String::with_capacity(body + 1);
    let mut sum = 0;
    for i in 0..body {
        let mut v = 0usize;
        for b in 0..5 {
            let bit = i * 5 + b;
            v <<= 1;
            if bit < bits {
                v |= (payload[bit / 8] >> (7 - bit % 8) & 1) as usize;
            }
        }
        sum += v;
        out.push(ALPHABET[v] as char);
    }
    out.push(ALPHABET[sum % 32] as char);
    out
}

/// Reverses encode for a code of body symbols carrying n payload bytes.
fn decode(norm: &[u8], body: usize, n: usize) -> Result<Vec<u8>> {
    let mut vals = Vec::with_capacity(body + 1);
    for c in &norm[..=body] {
        match ALPHABET.iter().position(|a| a == c) {
            Some(v) => vals.push(u8::try_from(v)?), // v < 32
            None => bail!("invalid join code"),
        }
    }
    let sum: usize = vals[..body].iter().map(|&v| v as usize).sum();
    if sum % 32 != vals[body] as usize {
        bail!("invalid join code: checksum mismatch");
    }
    let bit = |i: usize| vals[i / 5] >> (4 - i % 5) & 1;
    let mut payload = vec![0u8; n];
    for i in 0..n * 8 {
        payload[i / 8] |= bit(i) << (7 - i % 8);
    }
    // Padding bits must be zero: a non-zero tail is not one of our codes.
    if (n * 8..body * 5).any(|i| bit(i) != 0) {
        bail!("invalid join code: non-zero padding");
    }
    Ok(payload)
}

/// Parses an endpoint join code. Input is case insensitive and may contain
/// dashes or spaces; the Crockford look-alikes I/L (-> 1) and O (-> 0) are
/// accepted.
pub fn decode_endpoint(s: &str) -> Result<Endpoint> {
    match decode_any(s)? {
        Code {
            endpoint: Some(e),
            steam: None,
            ..
        } => Ok(e),
        _ => bail!("invalid join code: not an endpoint-only code"),
    }
}

/// Parses a steam join code.
pub fn decode_steam(s: &str) -> Result<Steam> {
    match decode_any(s)? {
        Code {
            steam: Some(st),
            endpoint: None,
            ..
        } => Ok(st),
        _ => bail!("invalid join code: not a steam-only code"),
    }
}

fn v4(p: &[u8]) -> IpAddr {
    Ipv4Addr::new(p[0], p[1], p[2], p[3]).into()
}

fn be16(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[0], p[1]])
}

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

/// Parses a join code of any layout.
pub fn decode_any(s: &str) -> Result<Code> {
    let norm = normalize(s);
    let norm = norm.as_bytes();
    match norm.len() {
        DIRECT_LENGTH | DIRECT_PORT_LENGTH => {
            let n = if norm.len() == DIRECT_PORT_LENGTH {
                6
            } else {
                4
            };
            let p = decode(norm, norm.len() - 1, n)?;
            let port = if n == 6 { be16(&p[4..]) } else { DEFAULT_PORT };
            Ok(Code {
                endpoint: Some(Endpoint::new(v4(&p), port)),
                ..Code::default()
            })
        }
        LENGTH => {
            let p = decode(norm, BODY_LEN, PAYLOAD_LEN)?;
            let e = Endpoint {
                flags: p[0],
                ip: v4(&p[1..]),
                port: be16(&p[5..]),
            };
            match p[0] & (FLAG_STEAM | FLAG_BOTH) {
                0 => Ok(Code {
                    endpoint: Some(e),
                    ..Code::default()
                }),
                FLAG_REPLY => Ok(Code {
                    reply: Some(Endpoint { flags: 0, ..e }),
                    ..Code::default()
                }),
                _ => bail!("invalid join code: layout flags on an endpoint-length code"),
            }
        }
        STEAM_LENGTH => {
            let p = decode(norm, STEAM_BODY_LEN, STEAM_PAYLOAD_LEN)?;
            if p[0] == 0 {
                return Ok(Code {
                    endpoint: Some(Endpoint::new(v4(&p[1..]), be16(&p[5..]))),
                    punch: Some(Endpoint::new(v4(&p[1..]), be16(&p[7..]))),
                    ..Code::default()
                });
            }
            if p[0] & (FLAG_STEAM | FLAG_BOTH) != FLAG_STEAM {
                bail!("invalid join code: wrong layout flags on a steam-length code");
            }
            let st = Steam {
                flags: p[0] & !FLAG_STEAM,
                account_id: be32(&p[1..]),
                key: be32(&p[5..]),
            };
            Ok(Code {
                steam: Some(st),
                ..Code::default()
            })
        }
        COMBINED_LENGTH => {
            let p = decode(norm, COMBINED_BODY_LEN, COMBINED_PAYLOAD_LEN)?;
            if p[0] & (FLAG_STEAM | FLAG_BOTH) != FLAG_STEAM | FLAG_BOTH {
                bail!("invalid join code: wrong layout flags on a combined-length code");
            }
            let flags = p[0] & !(FLAG_STEAM | FLAG_BOTH);
            Ok(Code {
                endpoint: Some(Endpoint {
                    flags,
                    ip: v4(&p[1..]),
                    port: be16(&p[5..]),
                }),
                steam: Some(Steam {
                    flags,
                    account_id: be32(&p[7..]),
                    key: be32(&p[11..]),
                }),
                ..Code::default()
            })
        }
        _ => bail!("invalid join code"),
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            // Go's ToUpper maps rune to rune: ß stays ß, never "SS".
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .filter(|c| !matches!(c, '-' | ' ' | '\t')) // separators are cosmetic
        .map(|c| match c {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(flags: u8, ip: [u8; 4], port: u16) -> Endpoint {
        Endpoint {
            flags,
            ip: Ipv4Addr::from(ip).into(),
            port,
        }
    }

    /// Bumps the symbol at i to the next one in the alphabet.
    fn bump(s: &str, i: usize) -> String {
        let mut b = s.as_bytes().to_vec();
        let pos = ALPHABET.iter().position(|&a| a == b[i]).unwrap();
        b[i] = ALPHABET[(pos + 1) % 32];
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn round_trip() {
        for want in [
            ep(0, [127, 0, 0, 1], 14250),
            ep(1, [0, 0, 0, 0], 0),
            ep(0x3f, [255, 255, 255, 255], 65535),
            ep(0, [88, 12, 200, 3], 1),
        ] {
            let s = encode_endpoint(want).unwrap();
            assert_eq!(s.len(), LENGTH, "{s}");
            assert_eq!(decode_endpoint(&s).unwrap(), want, "code {s}");
        }
    }

    #[test]
    fn decode_is_forgiving() {
        let s = encode_endpoint(ep(0, [10, 0, 0, 7], 14250)).unwrap();
        // lower case, dashes, and the Crockford look-alikes must all work
        let munged = format!("{}-{}-{}", &s[..4], &s[4..8], &s[8..])
            .to_lowercase()
            .replace('1', "l")
            .replace('0', "o");
        let got = decode_endpoint(&munged).unwrap();
        assert_eq!((got.ip, got.port), (IpAddr::from([10, 0, 0, 7]), 14250));
    }

    #[test]
    fn decode_rejects_bad_checksum() {
        let s = encode_endpoint(ep(0, [192, 168, 1, 5], 4242)).unwrap();
        assert!(
            decode_endpoint(&bump(&s, LENGTH - 1)).is_err(),
            "wrong checksum"
        );
        assert!(decode_endpoint(&bump(&s, 0)).is_err(), "corrupted body");
    }

    /// Pins the endpoint layout to the first release's encoder, so codes
    /// issued by earlier builds keep decoding.
    #[test]
    fn endpoint_codes_are_unchanged() {
        // 7 bytes into one word, shifted left by 4, 12 symbols MSB first.
        let legacy = |e: Endpoint| {
            let IpAddr::V4(ip) = e.ip else { unreachable!() };
            let o = ip.octets();
            let mut acc = 0u64;
            for b in [
                e.flags,
                o[0],
                o[1],
                o[2],
                o[3],
                e.port.to_be_bytes()[0],
                e.port.to_be_bytes()[1],
            ] {
                acc = acc << 8 | u64::from(b);
            }
            acc <<= 4;
            let mut s = String::new();
            let mut sum = 0;
            for i in (0..BODY_LEN).rev() {
                let v = (acc >> (i * 5) & 31) as usize;
                sum += v;
                s.push(ALPHABET[v] as char);
            }
            s.push(ALPHABET[sum % 32] as char);
            s
        };
        // The DESIGN.md example.
        assert_eq!(
            decode_endpoint("01C0SJ036YN0M").unwrap(),
            ep(0, [88, 12, 200, 3], 14250)
        );
        for e in [
            ep(0, [88, 12, 200, 3], 14250),
            ep(0x3f, [255, 255, 255, 255], 65535),
            ep(0, [0, 0, 0, 0], 0),
        ] {
            let s = encode_endpoint(e).unwrap();
            assert_eq!(s, legacy(e));
            let c = decode_any(&s).unwrap();
            assert!(c.steam.is_none());
            assert_eq!(c.endpoint.unwrap(), e);
        }
        assert!(
            encode_endpoint(ep(FLAG_STEAM, [1, 2, 3, 4], 1)).is_err(),
            "bit 7 is reserved"
        );
    }

    #[test]
    fn steam_round_trip() {
        for want in [
            Steam {
                flags: 0,
                account_id: 0,
                key: 0,
            },
            Steam {
                flags: 0,
                account_id: 0xffff_ffff,
                key: 0xffff_ffff,
            },
            Steam {
                flags: 0,
                account_id: 12_345_678,
                key: 0xdead_beef,
            },
            Steam {
                flags: 0x3f,
                account_id: 1,
                key: 2,
            }, // bits 7 and 6 belong to the layout
        ] {
            let s = encode_steam(want);
            assert_eq!(s.len(), STEAM_LENGTH);
            let got = decode_steam(&s).unwrap();
            assert_eq!(got, want, "code {s}");
            assert_eq!(
                got.steam_id64(),
                0x0110_0001_0000_0000 | u64::from(want.account_id)
            );
            let c = decode_any(&s).unwrap();
            assert!(c.steam.is_some() && c.endpoint.is_none());
            assert!(
                decode_endpoint(&s).is_err(),
                "Decode must refuse the steam code {s}"
            );
        }
        // [80][00 bc 61 4e][de ad be ef] as a 75-bit stream, computed independently.
        let s = encode_steam(Steam {
            flags: 0,
            account_id: 12_345_678,
            key: 0xdead_beef,
        });
        assert_eq!(s, "G00BRRAEVTPVXVRS");
        for i in 0..STEAM_LENGTH {
            assert!(
                decode_steam(&bump(&s, i)).is_err(),
                "accepted a corrupted symbol {i}"
            );
        }
    }

    #[test]
    fn combined_round_trip() {
        let e = ep(0, [45, 154, 88, 66], 14250);
        let st = Steam {
            flags: 0,
            account_id: 12_345_678,
            key: 0xdead_beef,
        };
        let s = encode_combined(e, st).unwrap();
        assert_eq!(s.len(), COMBINED_LENGTH);
        // [c0][2d 9a 58 42][37 aa][00 bc 61 4e][de ad be ef] as a 120-bit
        // stream, computed independently.
        assert_eq!(s, "R0PSMP226YN01F319VFAVFQFF");
        let c = decode_any(&s).unwrap();
        assert_eq!((c.endpoint, c.steam), (Some(e), Some(st)));
        assert_eq!(c.steam.unwrap().steam_id64(), 0x0110_0001_00BC_614E);
        // The single-route decoders refuse it: a caller that ignores one route
        // would silently lose the cascade.
        assert!(decode_endpoint(&s).is_err());
        assert!(decode_steam(&s).is_err());
        for i in 0..COMBINED_LENGTH {
            assert!(
                decode_any(&bump(&s, i)).is_err(),
                "accepted a corrupted symbol {i}"
            );
        }
        assert!(
            encode_combined(ep(FLAG_BOTH, [45, 154, 88, 66], 1), st).is_err(),
            "bit 6 is reserved"
        );
    }

    #[test]
    fn account_id_of() {
        assert_eq!(account_id(0x0110_0001_00BC_614E), (12_345_678, true));
        assert!(
            !account_id(0x0170_0000_0000_0001).1,
            "a clan id is not a player account"
        );
    }

    #[test]
    fn decode_rejects_bad_shape() {
        for s in [
            "",
            "ABC",
            &"Z".repeat(LENGTH + 1),
            &"!".repeat(LENGTH),
            &"!".repeat(STEAM_LENGTH),
        ] {
            assert!(decode_endpoint(s).is_err(), "{s:?} should have failed");
        }
    }

    #[test]
    fn direct_codes_round_trip() {
        for (ip, port, len) in [
            ([45, 154, 88, 66], 14250, 8),
            ([127, 0, 0, 1], 14250, 8),
            ([255, 255, 255, 255], 65535, 11),
            ([0, 0, 0, 0], 0, 11),
            ([192, 168, 1, 7], 4242, 11),
        ] {
            let want = ep(0, ip, port);
            let s = encode_direct(want).unwrap();
            assert_eq!(s.len(), len, "{want}: {s}");
            for input in [s.clone(), format!("{}-{}", &s[..4], &s[4..]).to_lowercase()] {
                let got = decode_any(&input).unwrap();
                assert!(got.steam.is_none());
                assert_eq!(got.endpoint, Some(want), "{input}");
            }
            for i in 0..s.len() {
                assert!(
                    decode_any(&bump(&s, i)).is_err(),
                    "accepted typo at {i} in {s}"
                );
            }
        }
    }

    /// Codes the Go helper produced, so both builds read each other's codes.
    #[test]
    fn go_vectors() {
        assert_eq!(
            encode_direct(ep(0, [45, 154, 88, 66], 14250)).unwrap(),
            "5PD5GGGX"
        );
        assert_eq!(
            encode_direct(ep(0, [192, 168, 1, 7], 4242)).unwrap(),
            "R2M021RGJ8K"
        );
        assert_eq!(
            encode_punch(ep(0, [45, 154, 88, 66], 14250), 8104).unwrap(),
            "00PSMP226YN1ZA00"
        );
        assert_eq!(
            encode_reply(ep(0, [100, 64, 7, 9], 50123)).unwrap(),
            "81J401R9RF5GX"
        );
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(
            encode_direct(Endpoint::new(mapped, 14250)).unwrap(),
            "R0004084"
        );
        // A right checksum over a wrong tail or layout is still refused.
        assert!(decode_any("00000011")
            .unwrap_err()
            .to_string()
            .contains("non-zero padding"));
        let layout = encode(&[FLAG_STEAM | FLAG_BOTH, 1, 2, 3, 4, 0, 1], BODY_LEN);
        assert!(decode_any(&layout)
            .unwrap_err()
            .to_string()
            .contains("layout flags"));
        // ß upper-cases to "SS" in Rust but stays one rune in Go.
        assert!(decode_any("0000ß0J").is_err());
    }

    #[test]
    fn direct_code_preserves_flags() {
        let want = ep(3, [192, 0, 2, 1], 14250);
        assert_eq!(
            decode_endpoint(&encode_direct(want).unwrap()).unwrap(),
            want
        );
    }

    #[test]
    fn direct_code_rejects_ipv6() {
        let e = Endpoint::new(std::net::Ipv6Addr::LOCALHOST, 14250);
        assert!(encode_direct(e).is_err(), "accepted IPv6");
    }

    #[test]
    fn punch_round_trip() {
        for (tcp, udp) in [
            (ep(0, [45, 154, 88, 66], DEFAULT_PORT), 8104),
            (ep(0, [255, 255, 255, 255], 65535), 65535),
            (ep(0, [1, 2, 3, 4], 1), 0),
        ] {
            let s = encode_punch(tcp, udp).unwrap();
            assert_eq!(s.len(), STEAM_LENGTH);
            let got = decode_any(&s).unwrap();
            assert!(got.steam.is_none() && got.reply.is_none());
            assert_eq!(got.endpoint.unwrap().addr(), tcp.addr());
            assert_eq!(got.punch, Some(Endpoint::new(tcp.ip, udp)));
            // decode_endpoint still gives the TCP route.
            assert_eq!(decode_endpoint(&s).unwrap().addr(), tcp.addr());
        }
        // A steam code of the same length is still a steam code.
        let st = Steam {
            flags: 0,
            account_id: 0x1234_5678,
            key: 0x9abc_def0,
        };
        let got = decode_any(&encode_steam(st)).unwrap();
        assert!(got.punch.is_none());
        assert_eq!(got.steam.unwrap().account_id, st.account_id);
    }

    #[test]
    fn reply_round_trip() {
        let want = ep(0, [100, 64, 7, 9], 50123);
        let s = encode_reply(want).unwrap();
        assert_eq!(s.len(), LENGTH);
        let got = decode_any(&s).unwrap();
        assert_eq!(
            got,
            Code {
                reply: Some(want),
                ..Code::default()
            }
        );
        assert!(
            decode_endpoint(&s).is_err(),
            "a reply code must not decode as an endpoint"
        );
        // Old 13-symbol endpoint codes, every legacy flag bit included, still decode.
        for flags in [0, 1, 0x20, 0x3f] {
            let got =
                decode_any(&encode_endpoint(ep(flags, [88, 12, 200, 3], 14250)).unwrap()).unwrap();
            assert!(got.reply.is_none());
            assert_eq!(got.endpoint.unwrap().flags, flags);
        }
        // The 8-symbol direct layout is untouched.
        let d = encode_direct(ep(0, [127, 0, 0, 1], DEFAULT_PORT)).unwrap();
        let got = decode_any(&d).unwrap();
        assert!(got.endpoint.is_some() && got.punch.is_none());
    }
}
