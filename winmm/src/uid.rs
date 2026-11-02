//! Mints the Session ids our master hands out (user/login "sid", the
//! proxy-link's own player id) and reads the game's Steam ids (port of
//! internal/uid).
//!
//! Lobby members are NOT named by minted ids: a lobby renders each member by
//! the id its game calls itself (master idOf), on both transports, so saves
//! and games agree on who a player is.

use ring::digest::{digest, SHA256};

use crate::wsx::hex;

/// The platform characters mpman.UserID.getPlatform@25195 accepts: S = Steam,
/// W = WServer, R = RelayP2P, X = Session, M = Multi, L = Lobby.
pub const PF_CHARS: &str = "SWRXML";

/// The platform char of the ids we mint.
pub const SESSION: u8 = b'X';

/// The shortest id getPlatform can still split into a token and a signature.
const MIN_LEN: usize = 5;

/// Hex digits of a full Steam id: 'S' + the 8 little-endian bytes of the
/// SteamID64 with its high dword xor'ed with STEAM_XOR (UserID.hx:113-115).
/// The printer strips trailing '0's (UserID.hx:116-118) and the parser
/// zero-fills (UserID.hx:22-29), so a live id is "S" + 1..16 hex digits.
const STEAM_HEX: usize = 16;

/// What mpman.UserID xors into the high dword (UserID.hx:29, :114): the
/// constant high dword of every player SteamID64.
const STEAM_XOR: u64 = 0x0110_0001;

/// Maps whatever id a player reports (normally a Steam "S<steamid>") onto a
/// stable Session id, the same across reconnects and both proxy-link ends.
pub fn mint(raw: &str) -> String {
    let sum = digest(&SHA256, format!("wartales-mp/uid/{raw}").as_bytes());
    format!("{}{}", SESSION as char, hex(&sum.as_ref()[..10]))
}

/// Whether id already has the shape mint produces.
pub fn is_session(id: &str) -> bool {
    id.len() >= MIN_LEN && id.as_bytes()[0] == SESSION
}

/// Whether id is a well-formed Steam id the game can turn back into a
/// SteamID64: only such an id may go on the wire when a lobby runs over SDR.
pub fn is_steam(id: &str) -> bool {
    (2..=1 + STEAM_HEX).contains(&id.len())
        && id.as_bytes()[0] == b'S'
        && id.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

/// The SteamID64 the game derives from a Steam id; None for anything
/// is_steam rejects.
pub fn steam_id64(id: &str) -> Option<u64> {
    if !is_steam(id) {
        return None;
    }
    let digits = format!("{:0<STEAM_HEX$}", &id[1..]); // the parser's zero-filled buffer
    let mut b = [0u8; 8];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(u64::from_le_bytes(b) ^ STEAM_XOR << 32)
}

/// The id the game reports for a SteamID64, trailing zeros stripped the way
/// UserID.hx:117 prints it.
pub fn from_steam_id64(steam_id64: u64) -> String {
    let h = hex(&(steam_id64 ^ STEAM_XOR << 32).to_le_bytes());
    format!("S{}", h.trim_end_matches('0'))
}

/// id when it is already a Session id, a minted one otherwise: a guest that
/// claims a Steam shaped id must not re-enable the Steam only path.
pub fn ensure(id: &str) -> String {
    if is_session(id) {
        id.to_string()
    } else {
        mint(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_steam_shapes() {
        // The game strips trailing zeros (UserID.hx:117): a live id is short.
        for id in [
            "S9f792402",
            "S4e61bc",
            "S0011223344556677",
            "S0123456789abcdef",
            "S0123456789ABCDEF",
            "S1",
        ] {
            assert!(is_steam(id), "{id}");
        }
        let minted = mint("S0011223344556677");
        for id in [
            "",
            "S",
            "S00112233445566778",
            "X0011223344556677",
            "Snot-a-steam-id!!",
            "S9f79240g",
            &minted,
        ] {
            assert!(!is_steam(id), "{id}");
        }
    }

    #[test]
    fn steam_id64_of() {
        // Account 12345678 (0x00bc614e): little-endian bytes 4e 61 bc 00, the
        // high dword xors to zero, printed without the trailing zeros.
        let id = "S4e61bc";
        assert_eq!(steam_id64(id), Some(0x0110_0001_00BC_614E));
        assert_eq!(from_steam_id64(0x0110_0001_00BC_614E), id);
        assert_eq!(
            steam_id64("S4e61bc0000000000"),
            Some(0x0110_0001_00BC_614E),
            "padded spelling"
        );
        assert_eq!(
            steam_id64("S9f792402"),
            Some(0x0110_0001_0224_799f),
            "live log id"
        );
        assert_eq!(
            steam_id64("X4e61bc0000000000"),
            None,
            "a Session id has no SteamID64"
        );
        assert_eq!(
            steam_id64("S1"),
            Some(0x0110_0001_0000_0010),
            "odd digit count is zero-filled"
        );
        assert_eq!(
            steam_id64("S4E61BC"),
            Some(0x0110_0001_00BC_614E),
            "upper case"
        );
        assert_eq!(
            from_steam_id64(0x0170_0000_0000_0001),
            "S0100000001006",
            "non-player high dword"
        );
    }

    #[test]
    fn mint_is_session_and_stable() {
        let (a, b) = (mint("S0011223344556677"), mint("S0011223344556677"));
        assert!(a == b && is_session(&a) && !is_steam(&a), "{a} / {b}");
        assert_eq!(
            a, "X8e95182f3e823aab2077",
            "the Go helper's id for the same player"
        );
        assert_eq!(ensure(&a), a);
        assert_eq!(ensure("S0011223344556677"), a);
        assert!(!is_session("") && !is_session("X123") && is_session("X1234"));
    }
}
