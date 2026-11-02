//! The shimcheck scenarios (sdr, nosdr, partial, the bridge and the lobby
//! advert) against the in-process fake flat API. What only the in-game
//! harness covers: hooking steam.hdll through hlp_<name>, the HashLink
//! calling convention of the detours, and a real Steam client.

// why: each scenario keeps its peers next to the step that uses them, and
// ports one long shimcheck check as one test.
#![allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::sync::{Arc, Mutex};

use super::fake::{self, *};
use super::lobby::Lobby;
use super::sdr::{Transport, BRIDGE_CHANNEL};
use crate::Log;

type Lines = Arc<Mutex<Vec<String>>>;

struct T {
    t: Box<Transport>,
    lines: Lines,
}

fn transport(load: fn() -> Result<super::sdr::Api, String>) -> T {
    let (log, lines) = Log::capture();
    let t = Box::new(Transport::new(load, fake::now, log));
    t.set_hl_copy_bytes(Some(copy_bytes));
    T { t, lines }
}

/// A transport brought up like the game: 6 natives diverted, warm-up done.
fn ready() -> T {
    let t = transport(load_ok);
    t.t.diverted(6, 6);
    t.t.warm_up(|_| {});
    assert!(
        t.has("sdr: READY: ISteamNetworkingMessages at"),
        "{:#?}",
        t.lines
    );
    t
}

impl T {
    fn has(&self, needle: &str) -> bool {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(needle))
    }

    /// The diag thread for ms of fake time, one tick a second.
    fn sleep(&self, ms: u32) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(1000);
            advance_clock(step);
            self.t.tick();
            left -= step;
        }
    }

    /// Ticks until needle is logged, up to ms.
    fn wait(&self, needle: &str, ms: u32) -> bool {
        let mut waited = 0;
        while !self.has(needle) {
            if waited >= ms {
                return false;
            }
            self.sleep(1000);
            waited += 1000;
        }
        true
    }

    fn send(&self, id: u64, data: &[u8], typ: i32, ch: i32) -> bool {
        let uid = id.to_le_bytes();
        // SAFETY: valid buffers for the call.
        unsafe {
            self.t
                .send_p2p(uid.as_ptr(), data.as_ptr(), data.len() as i32, typ, ch)
        }
    }

    fn avail(&self, ch: i32) -> Option<u32> {
        let mut size = 0;
        // SAFETY: size is writable.
        unsafe { self.t.available(&raw mut size, ch) }.then_some(size)
    }

    fn read_n(&self, cap: usize, ch: i32) -> Option<(u64, Vec<u8>)> {
        let mut buf = vec![0u8; cap];
        let mut len = 0u32;
        // SAFETY: buf has cap bytes, len writable.
        let from = unsafe { self.t.read(buf.as_mut_ptr(), cap as i32, &raw mut len, ch) };
        if from.is_null() {
            return None;
        }
        buf.truncate(len as usize);
        // SAFETY: copy_bytes returned 8 bytes.
        Some((
            u64::from_le_bytes(unsafe { from.cast::<[u8; 8]>().read() }),
            buf,
        ))
    }

    fn read(&self, ch: i32) -> Option<(u64, Vec<u8>)> {
        self.read_n(64, ch)
    }

    fn close(&self, id: u64) -> bool {
        // SAFETY: 8 valid bytes.
        unsafe { self.t.close(id.to_le_bytes().as_ptr()) }
    }

    fn accept(&self, id: u64) -> bool {
        // SAFETY: 8 valid bytes.
        unsafe { self.t.accept(id.to_le_bytes().as_ptr()) }
    }

    /// Makes id a session the game uses: Steam says connected and the game
    /// reads one game packet [code 5][pid 1 0][sid][abc] from it.
    fn arm_peer(&self, id: u64, sid: [u8; 4]) {
        let mut pkt = vec![5, 1, 0];
        pkt.extend_from_slice(&sid);
        pkt.extend_from_slice(b"abc");
        set_session_state(id, 3, 0);
        inject(id, 0, &pkt);
        let got = self.read(0);
        assert!(
            got.as_ref().is_some_and(|(f, d)| *f == id && d.len() == 10),
            "arm read {got:?}"
        );
        assert!(
            self.wait(
                &format!("sdr: session with {id}: state 3 (connected)"),
                5000
            ),
            "arm: not connected"
        );
    }
}

fn frame(code: u8, sid: [u8; 4]) -> Vec<u8> {
    let mut f = vec![code, 0, 0];
    f.extend_from_slice(&sid);
    f
}

// ------------------------------------------------------------- scenarios

/// shimcheck "nosdr": steam_api64.dll absent; every native fails closed.
#[test]
fn nosdr_fails_closed() {
    let _g = serial();
    let t = transport(load_missing);
    t.t.diverted(6, 6);
    t.t.warm_up(|_| {});
    assert!(t.has("sdr: 6 of 6 legacy P2P natives diverted"));
    assert!(t.has("sdr: UNAVAILABLE, transport disabled: steam_api64.dll is not loaded"));
    let st = t.t.status();
    assert!(
        st.known && !st.ok && st.reason.starts_with("steam_api64.dll is not loaded"),
        "{st:?}"
    );
    assert!(!t.send(1, b"abcde", 2, 0), "send must fail");
    assert_eq!(t.avail(0), None);
    assert!(t.read(0).is_none());
    assert!(!t.accept(1));
    assert!(t.close(1), "close reports nothing open");
    assert!(t.t.session_data().is_null());
    assert_eq!(t.t.bridge_send(1, b"x"), -1);
}

/// shimcheck "partial": one native could not be hooked; SDR never comes up.
#[test]
fn partial_fails_closed() {
    let _g = serial();
    let t = transport(load_ok);
    t.t.diverted(5, 6);
    assert!(t.has("sdr: 5 of 6 legacy P2P natives diverted"));
    assert!(t.has("sdr: UNAVAILABLE, transport disabled: only 5 of 6 legacy P2P natives diverted"));
    t.t.warm_up(|_| {});
    let st = t.t.status();
    assert!(
        st.known && !st.ok && st.reason.starts_with("only 5 of 6"),
        "{st:?}"
    );
    assert!(!t.has("sdr: READY"));
    assert!(!t.send(1, b"abcde", 2, 0));
    assert!(t.avail(0).is_none() && t.read(0).is_none());
    assert_eq!(stats().sends, 0, "nothing reached SendMessageToUser");
}

/// The Steam API comes up late: pending (unknown) with the reason, then ready.
#[test]
fn pending_until_steam_is_up() {
    let _g = serial();
    set_hsteamuser(0);
    let t = transport(load_ok);
    t.t.diverted(6, 6);
    let st = t.t.status();
    assert!(!st.known && st.reason.contains("not initialised"), "{st:?}");
    assert!(t.has("sdr: not ready: Steam API not initialised yet"));
    set_hsteamuser(1);
    assert!(!t.t.status().ok, "retried before its 2 s pace");
    advance_clock(2000);
    assert!(t.t.status().ok);
}

/// shimcheck "sdr": flags, queues, truncation, channels, identities, batch
/// pulls, diagnostics, sessions, close.
#[test]
fn sdr_semantics() {
    let _g = serial();
    const PEER: u64 = 0x0102_0304_0506_0708;
    const OTHER: u64 = 0x0000_0000_AABB_CCDD;
    const THIRD: u64 = 0x1234_5678_90AB_CDEF;
    let t = ready();
    assert!(t.has("sdr: InitRelayNetworkAccess called, relay status 100"));
    assert!(t.t.status().ok);
    let st = stats();
    assert!(st.relay_inits == 1 && st.registered_1251 && st.registered_1252);
    assert!(st.size_1251 == 136 && st.size_1252 == 696, "{st:?}");
    assert!(t.has(
        "sdr: session request/failed callbacks registered with SteamAPI_RegisterCallback (1251/1252, flags 0x01/0x01"
    ));

    // Send type -> reliability flags.
    assert!(t.send(PEER, b"abcde", 2, 0));
    let st = stats();
    assert!(st.sends == 1 && st.last_flags == 8 | 1 | 32);
    assert!(st.last_send_to == PEER && st.last_send_id_type == 16 && st.last_channel == 0);
    assert!(t.send(PEER, b"u", 0, 0));
    assert_eq!(stats().last_flags, 32);
    assert!(t.send(PEER, b"xy", 1, 0));
    assert_eq!(stats().last_flags, 4 | 1 | 32);
    assert!(t.send(PEER, b"rwb", 3, 0));
    assert_eq!(stats().last_flags, 8 | 32);
    assert!(!t.send(PEER, b"x", 7, 0), "unknown send type refused");
    assert!(!t.send(PEER, b"x", 2, 99), "out-of-range channel refused");
    assert_eq!(stats().sends, 4, "refused sends never reach Steam");

    // Queue order, next-size reporting, boundaries and truncation.
    assert_eq!(t.avail(0), Some(5));
    let (from, d) = t.read_n(3, 0).unwrap();
    assert!(from == PEER && d == b"abc");
    assert_eq!(t.avail(0), Some(1), "the truncated remainder is gone");
    assert_eq!(t.read(0).unwrap().1, b"u");
    assert_eq!(t.read(0).unwrap().1, b"xy");
    assert_eq!(t.read(0).unwrap().1, b"rwb");
    assert!(t.read(0).is_none() && t.avail(0).is_none());

    // Channels are independent queues.
    assert!(t.send(PEER, b"one", 2, 1) && t.send(PEER, b"zero", 2, 0));
    assert_eq!(t.avail(0), Some(4));
    assert_eq!(t.avail(1), Some(3));
    assert_eq!(t.read(1).unwrap().1, b"one");
    assert_eq!(t.read(0).unwrap().1, b"zero");

    // Sender identity comes from the message.
    inject(OTHER, 0, b"from-other");
    let (from, d) = t.read(0).unwrap();
    assert!(from == OTHER && d.len() == 10);

    // A backlog is pulled one batch per poll and comes out whole and in order.
    let before = stats().queued;
    for k in 0u32..200 {
        inject(OTHER, 3, &k.to_le_bytes());
    }
    assert_eq!(t.avail(3), Some(4));
    assert_eq!(
        stats().queued - before,
        200 - 64,
        "one batch of 64 per poll"
    );
    for k in 0u32..200 {
        assert_eq!(t.read(3).unwrap().1, k.to_le_bytes(), "message {k}");
    }
    assert!(t.read(3).is_none());

    // Diagnostics.
    assert!(t.has("sdr: first packet to 72623859790382856: 5 bytes, type 2 -> flags 0x29, channel 0 = EResult 1"));
    assert!(t.wait(
        "sdr: session with 72623859790382856: state 3 (connected), end reason 0 '', relay POP 1718314273",
        5000
    ));
    t.t.on_run_callbacks();
    t.t.on_run_callbacks();
    assert!(t.has("sdr: the game called SteamAPI_RunCallbacks for the first time"));

    // Sessions: incoming requests auto-accepted; explicit accept/close.
    assert!(fire_session_request(THIRD));
    let st = stats();
    assert!(st.accepts == 1 && st.last_accept == THIRD);
    assert!(t.has("sdr: session request from 1311768467294899695 (type 16) via SteamAPI_RegisterCallback: accepted"));
    assert!(fire_session_failed(
        THIRD,
        5003,
        "Timed out attempting to connect"
    ));
    assert!(t.has("sdr: session with 1311768467294899695 FAILED (via SteamAPI_RegisterCallback): state 5 (problem detected locally), end reason 5003 'Timed out attempting to connect'; fake P2P connection"));
    assert!(t.accept(PEER));
    let st = stats();
    assert!(st.accepts == 2 && st.last_accept == PEER);
    inject(PEER, 0, b"p1");
    inject(OTHER, 0, b"o1");
    inject(PEER, 1, b"p2");
    assert_eq!(t.avail(0), Some(2));
    assert!(t.close(PEER));
    assert!(t.has("sdr: session with 72623859790382856 closed by the game (Steam's session kept)"));
    let (from, d) = t.read(0).unwrap();
    assert!(
        from == OTHER && d == b"o1",
        "close kept the other peer's message"
    );
    assert!(
        t.read(0).is_none() && t.read(1).is_none(),
        "nothing from the closed peer remains"
    );
    assert!(t.t.session_data().is_null());
    let st = stats();
    assert_eq!(
        st.allocated, st.released,
        "every message handed out was released"
    );
}

/// A held session drops: exactly one vanilla code-8 frame, after the peer's
/// last real packet; never for a closed, never-read or reconnected session.
#[test]
fn lost_peer_gets_one_close() {
    let _g = serial();
    const HOST: u64 = 76_561_198_000_000_001;
    const GONE: u64 = 76_561_198_000_000_002;
    const NEVER: u64 = 76_561_198_000_000_003;
    const FAILED: u64 = 76_561_198_000_000_004;
    const FLAKY: u64 = 76_561_198_000_000_005;
    let sid_host = [0x11, 0x22, 0x33, 0x44];
    let sid_failed = [0x01, 0x00, 0x00, 0x80];
    let sid_x = [7, 0, 0, 0];
    let t = ready();

    t.arm_peer(HOST, sid_host);
    inject(HOST, 0, b"\x05\x02\x00\x11\x22\x33\x44last");
    set_session_state(HOST, 4, 1000);
    assert!(t.wait(
        "sdr: session with 76561198000000001 lost (session ended: state 4 (closed by peer)",
        5000
    ));
    t.sleep(3500);
    assert!(
        !t.has("injected close for 76561198000000001"),
        "no close while the last packet is unread"
    );
    let (from, d) = t.read(0).unwrap();
    assert!(from == HOST && d.len() == 11 && &d[7..] == b"last");
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000001 sid 1144201745 on channel 0",
        4000
    ));
    assert_eq!(t.avail(0), Some(7));
    let (from, d) = t.read(0).unwrap();
    assert!(from == HOST && d == frame(8, sid_host));
    t.sleep(3500);
    assert!(t.avail(0).is_none(), "exactly one close per lost session");
    assert!(t.close(HOST));
    assert!(t.has("sdr: session with 76561198000000001 closed by the game (Steam's session kept)"));

    // A session the game closed itself is never reported back.
    t.arm_peer(GONE, sid_x);
    assert!(t.close(GONE));
    set_session_state(GONE, 5, 5003);
    assert!(t.wait(
        "sdr: session with 76561198000000002: state 5 (problem detected locally) was connected",
        4000
    ));

    // A peer the game never read from.
    set_session_state(NEVER, 3, 0);
    inject(NEVER, 1, b"x");
    assert!(fire_session_failed(
        NEVER,
        5003,
        "Timed out attempting to connect"
    ));

    // SessionFailed_t is a loss too.
    t.arm_peer(FAILED, sid_failed);
    assert!(fire_session_failed(
        FAILED,
        5003,
        "Timed out attempting to connect"
    ));
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000004 sid -2147483647 on channel 0",
        8000
    ));
    let (from, d) = t.read(0).unwrap();
    assert!(from == FAILED && d == frame(8, sid_failed));

    // A drop that reconnects within the confirmation window is not a loss.
    t.arm_peer(FLAKY, sid_x);
    set_session_state(FLAKY, 5, 4000);
    t.t.tick(); // the poll sees it at once (the C diag thread's next second)
    assert!(t.has("sdr: session with 76561198000000005 lost"));
    set_session_state(FLAKY, 3, 0);
    advance_clock(1000);
    t.t.tick();
    assert!(t.has("sdr: session with 76561198000000005 connected again, no close injected"));

    t.sleep(3500);
    assert!(t.avail(0).is_none());
    for id in [
        "76561198000000002",
        "76561198000000003",
        "76561198000000005",
    ] {
        assert!(
            !t.has(&format!("injected close for {id}")),
            "close for {id}"
        );
    }
    let (from, d) = t.read(1).unwrap();
    assert!(from == NEVER && d.len() == 1);
}

/// A co-op reload: stop (code 8 + close) then rejoin under a new sid over
/// the same Steam session; the closed sid's echoes are dropped.
#[test]
fn reload_rejoin_keeps_the_session() {
    let _g = serial();
    const BACK: u64 = 76_561_198_000_000_006;
    const EARLY: u64 = 76_561_198_000_000_007;
    let t = ready();

    t.arm_peer(BACK, [9, 0, 0, 0]);
    assert!(
        t.send(BACK, &frame(8, [9, 0, 0, 0]), 2, 0),
        "stop() sends code 8 for sid 9"
    );
    // The loopback echo of that code 8 is the old session's: dropped.
    assert!(t.close(BACK));
    inject(BACK, 0, &frame(8, [9, 0, 0, 0]));
    inject(BACK, 0, b"\x05\x02\x00\x09\x00\x00\x00ol");
    inject(BACK, 0, &frame(3, [0x0a, 0, 0, 0]));
    assert_eq!(
        t.avail(0),
        Some(9),
        "the old code 8s dropped; the late packet first"
    );
    let (from, d) = t.read(0).unwrap();
    assert!(from == BACK && d.len() == 9);
    set_session_state(BACK, 4, 1000);
    t.sleep(3500);
    assert!(
        !t.has("injected close for 76561198000000006"),
        "a late packet does not arm"
    );
    set_session_state(BACK, 3, 0);
    assert!(t.wait(
        "sdr: session with 76561198000000006: state 3 (connected) was closed by peer",
        4000
    ));
    let (_, d) = t.read(0).unwrap();
    assert!(d.len() == 7 && d[0] == 3, "the rejoin reaches the game");
    set_session_state(BACK, 4, 1000);
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000006 sid 10 on channel 0",
        8000
    ));
    assert_eq!(t.read(0).unwrap().1, frame(8, [0x0a, 0, 0, 0]));
    // A code 8 with another sid is the game's business.
    inject(BACK, 0, &frame(8, [0x0b, 0, 0, 0]));
    assert_eq!(t.avail(0), Some(7));
    t.read(0);

    // The rejoin comes first; the delayed close ends sid 12 only.
    t.arm_peer(EARLY, [0x0c, 0, 0, 0]);
    inject(EARLY, 0, &frame(3, [0x0d, 0, 0, 0]));
    let (_, d) = t.read(0).unwrap();
    assert_eq!(d[0], 3);
    assert!(t.send(EARLY, &frame(8, [0x0c, 0, 0, 0]), 2, 0));
    inject(EARLY, 0, b"\x05\x01\x00\x0d\x00\x00\x00jn");
    assert!(t.close(EARLY));
    assert!(t.close(EARLY), "a second delayed close (no new code 8)");
    assert_eq!(t.avail(0), Some(9), "the new session's packet survives");
    let (_, d) = t.read(0).unwrap();
    assert_eq!(&d[7..], b"jn");
    set_session_state(EARLY, 4, 1000);
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000007 sid 13 on channel 0",
        8000
    ));
    assert_eq!(t.read(0).unwrap().1, frame(8, [0x0d, 0, 0, 0]));
}

/// A guest's join dial that never connects still gets its close (issue #7).
#[test]
fn failed_dial_gets_a_close() {
    let _g = serial();
    const DIAL: u64 = 76_561_198_000_000_008;
    let sid = [0x2a, 0x2b, 0x2c, 0x2d];
    let t = ready();
    set_session_state(DIAL, 2, 0);
    let mut join = frame(3, sid);
    join[1] = 1;
    assert!(t.send(DIAL, &join, 2, 0));
    set_session_state(DIAL, 5, 5008);
    assert!(fire_session_failed(
        DIAL,
        5008,
        "Timed out attempting to negotiate rendezvous"
    ));
    assert!(
        t.has("sdr: session with 76561198000000008 lost (SessionFailed"),
        "armed by the send alone"
    );
    let (_, d) = t.read(0).unwrap();
    assert_eq!(d[0], 3, "the loopback echo goes first");
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000008 sid 757869354 on channel 0",
        8000
    ));
    assert_eq!(t.read(0).unwrap(), (DIAL, frame(8, sid)));
}

/// Watch slots are reused: after many peers a new session still gets its close.
#[test]
fn watch_slots_are_reused() {
    let _g = serial();
    const LATE: u64 = 76_561_198_000_000_100;
    let t = ready();
    for k in 0..20 {
        inject(76_561_198_000_000_200 + k, 2, b"p");
        assert_eq!(t.read(2).unwrap().0, 76_561_198_000_000_200 + k);
        advance_clock(1);
    }
    assert!(t.has("sdr: watch slot of idle peer "));
    t.arm_peer(LATE, [0x21, 0, 0, 0]);
    set_session_state(LATE, 4, 1000);
    assert!(t.wait("sdr: peer lost, injected close for 76561198000000100", 8000));
    assert_eq!(t.read(0).unwrap(), (LATE, frame(8, [0x21, 0, 0, 0])));
}

/// EResult 25: reliable messages are held and sent in order; unreliable may
/// drop; a backlog Steam refuses dooms the session the game holds.
#[test]
fn send_backlog() {
    let _g = serial();
    const SLOW: u64 = 76_561_198_000_000_300;
    const DEAD: u64 = 76_561_198_000_000_301;
    let t = ready();

    let sends = stats().sends;
    set_send_result(25);
    assert!(
        t.send(SLOW, b"h1", 2, 0) && t.send(SLOW, b"h2", 3, 0),
        "held, answer true"
    );
    assert!(!t.send(SLOW, b"u", 0, 0), "an unreliable send fails");
    assert_eq!(stats().sends, sends);
    assert!(t.has(
        "sdr: send buffer to 76561198000000300 full (EResult 25): reliable messages are held"
    ));
    assert!(
        t.avail(0).is_none(),
        "a poll with the buffer still full sends nothing"
    );
    set_send_result(1);
    assert!(t.send(SLOW, b"h3", 2, 0));
    let st = stats();
    assert!(st.sends == sends + 3 && st.last_flags == 8 | 1 | 32);
    for want in [b"h1", b"h2", b"h3"] {
        assert_eq!(t.read(0).unwrap(), (SLOW, want.to_vec()));
    }
    assert!(
        t.read(0).is_none(),
        "the dropped unreliable one never comes"
    );
    assert!(t.has("sdr: send backlog to 76561198000000300 drained"));

    // Held messages also go out on the game's receive poll alone.
    set_send_result(25);
    assert!(t.send(SLOW, b"p1", 2, 0));
    set_send_result(1);
    assert_eq!(t.avail(0), Some(2));
    assert_eq!(t.read(0).unwrap().1, b"p1");
    t.close(SLOW);

    // A backlog Steam refuses for good: given up, the held session is lost.
    t.arm_peer(DEAD, [0x31, 0, 0, 0]);
    set_send_result(25);
    assert!(t.send(DEAD, b"d1", 2, 0));
    set_send_result(3);
    t.avail(0);
    set_send_result(1);
    assert!(t.has("sdr: reliable messages to 76561198000000301 cannot be delivered (SendMessageToUser = EResult 3): 1 held message(s) dropped; the session counts as lost"));
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000301 sid 49 on channel 0",
        8000
    ));
    assert_eq!(t.read(0).unwrap(), (DEAD, frame(8, [0x31, 0, 0, 0])));
    t.close(DEAD);

    // Given up while re-dialing, and the re-dial then connects: still closed.
    const REDIAL: u64 = 76_561_198_000_000_302;
    t.arm_peer(REDIAL, [0x32, 0, 0, 0]);
    set_session_state(REDIAL, 1, 0);
    assert!(t.wait(
        "sdr: session with 76561198000000302: state 1 (connecting) was connected",
        4000
    ));
    set_send_result(25);
    assert!(t.send(REDIAL, b"\x05\x02\x00\x32\x00\x00\x00rd", 2, 0));
    set_send_result(3);
    t.avail(0);
    set_send_result(1);
    set_session_state(REDIAL, 3, 0);
    assert!(t.has("sdr: reliable messages to 76561198000000302 cannot be delivered (SendMessageToUser = EResult 3): 1 held message(s) dropped; the session counts as lost"));
    assert!(t.wait(
        "sdr: session with 76561198000000302: state 3 (connected) was connecting",
        4000
    ));
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000302 sid 50 on channel 0",
        8000
    ));
    assert!(!t.has("sdr: session with 76561198000000302 connected again, no close injected"));
    assert_eq!(t.read(0).unwrap(), (REDIAL, frame(8, [0x32, 0, 0, 0])));
    t.close(REDIAL);

    // Close and rejoin: the closed session's held data goes, its goodbye stays.
    const REJ: u64 = 76_561_198_000_000_303;
    const REJ2: u64 = 76_561_198_000_000_304;
    t.arm_peer(REJ, [0x41, 0, 0, 0]);
    let sends = stats().sends;
    set_send_result(25);
    assert!(t.send(REJ, b"\x05\x03\x00\x41\x00\x00\x00old", 2, 0));
    assert!(t.send(REJ, b"\x08\x04\x00\x41\x00\x00\x00", 2, 0));
    assert!(t.close(REJ));
    assert!(t.has(
        "sdr: session with 76561198000000303 closed by the game: 1 held message(s) of it dropped"
    ));
    inject(REJ, 0, &frame(3, [0x42, 0, 0, 0]));
    assert_eq!(t.read(0).unwrap().1[0], 3, "the peer rejoins with sid 0x42");
    assert!(
        t.send(REJ, b"\x05\x00\x00\x42\x00\x00\x00new", 2, 0),
        "queued behind the goodbye"
    );
    set_send_result(1);
    assert_eq!(
        t.avail(0),
        Some(10),
        "the goodbye echo is dropped as the old session's"
    );
    assert_eq!(
        stats().sends,
        sends + 2,
        "Steam took exactly the goodbye and the new frame"
    );
    assert_eq!(&t.read(0).unwrap().1[7..], b"new");
    assert!(t.read(0).is_none(), "the old session's data never arrives");
    t.close(REJ);

    t.arm_peer(REJ2, [0x43, 0, 0, 0]);
    set_send_result(25);
    assert!(
        t.send(REJ2, b"\x08\x01\x00\x43\x00\x00\x00", 2, 0),
        "its goodbye is held"
    );
    t.close(REJ2);
    inject(REJ2, 0, &frame(3, [0x44, 0, 0, 0]));
    assert_eq!(t.read(0).unwrap().1[0], 3);
    set_send_result(3);
    t.avail(0);
    set_send_result(1);
    assert!(t.has("sdr: reliable messages to 76561198000000304 cannot be delivered (SendMessageToUser = EResult 3): 1 held message(s) dropped; not the session the game holds"));
    t.sleep(3500);
    assert!(
        !t.has("injected close for 76561198000000304"),
        "the rejoined session is not closed"
    );
    t.close(REJ2);

    // The backlog is capped by entry count too.
    const FLOOD: u64 = 76_561_198_000_000_305;
    const JOIN: u64 = 76_561_198_000_000_306;
    set_send_result(25);
    for k in 0..65536 {
        assert!(t.send(FLOOD, b"", 2, 0), "empty message {k} not held");
    }
    assert!(!t.send(FLOOD, b"", 2, 0), "the 65537th is refused");
    assert!(t.has("sdr: reliable messages to 76561198000000305 cannot be delivered (send backlog over its limit): 65536 held message(s) dropped"));
    // A join whose hold is refused: its sid is doomed, the game gets its close.
    for _ in 0..65536 {
        assert!(t.send(FLOOD, b"", 2, 0));
    }
    set_session_state(JOIN, 1, 0);
    let sid_j = [0x51, 0x52, 0x53, 0x54];
    assert!(
        !t.send(JOIN, &frame(3, sid_j), 2, 0),
        "a join whose hold is refused fails"
    );
    assert!(t.has("sdr: reliable messages to 76561198000000306 cannot be delivered (send backlog over its limit): 0 held message(s) dropped; the session counts as lost"));
    t.close(FLOOD);
    set_send_result(1);
    set_session_state(JOIN, 3, 0);
    assert!(t.wait(
        "sdr: peer lost, injected close for 76561198000000306 sid 1414746705 on channel 0",
        8000
    ));
    assert_eq!(t.read(0).unwrap(), (JOIN, frame(8, sid_j)));
    t.close(JOIN);
    assert!(t.send(FLOOD, b"ok", 2, 0), "sends work again afterwards");
    assert_eq!(t.read(0).unwrap(), (FLOOD, b"ok".to_vec()));
    let st = stats();
    assert_eq!(st.allocated, st.released);
}

/// The master-link's direct access: reliable on channel 100, invisible to the
/// game's channels, refusals reported with their EResult.
#[test]
fn bridge_channel() {
    let _g = serial();
    const PEER: u64 = 0x0102_0304_0506_0708;
    const OTHER: u64 = 0x0000_0000_AABB_CCDD;
    let t = ready();
    assert_eq!(t.t.bridge_send(PEER, b"lobby-hello"), 1);
    let st = stats();
    assert!(
        st.last_channel == BRIDGE_CHANNEL && st.last_flags == 8 | 1 | 32 && st.last_send_to == PEER
    );
    assert_eq!(
        t.t.bridge_receive(32),
        vec![(PEER, b"lobby-hello".to_vec())]
    );
    inject(OTHER, BRIDGE_CHANNEL, b"from-other");
    assert!(
        t.avail(0).is_none(),
        "bridge traffic is invisible on channel 0"
    );
    assert_eq!(
        t.t.bridge_receive(32),
        vec![(OTHER, b"from-other".to_vec())]
    );
    assert!(t.has("bridge: first message from 72623859790382856 on channel 100, 11 bytes"));
    set_send_result(3);
    assert_eq!(t.t.bridge_send(PEER, b"x"), 3);
    assert!(t.has("sdr: bridge send to 72623859790382856: 1 bytes on channel 100 = EResult 3"));
    set_send_result(1);
    let st = stats();
    assert_eq!(
        st.allocated, st.released,
        "bridge released every message it pulled"
    );
}

/// The friends-list lobby advert (shimcheck check_lobby_publish).
#[test]
fn lobby_publish() {
    let _g = serial();
    let (log, lines) = Log::capture();
    let l = Lobby::new(lobby_load, fake::now64, log);
    let has = |s: &str| lines.lock().unwrap().iter().any(|l| l.contains(s));
    set_hsteamuser(0);
    assert!(l.set_invite(b"S-AUTO"));
    l.pump();
    assert_eq!(lobby_stats().creates, 0, "waits for Steam interfaces");
    set_hsteamuser(1);
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 1
            && st.writes == 1
            && st.typ == 1
            && st.capacity == 256
            && st.invite == "S-AUTO",
        "{st:?}"
    );
    assert!(!l.set_invite(b"BAD\0X") && !l.set_invite(&[b'1'; 33]) && !l.set_invite(b"a b"));
    assert!(l.set_invite(b"S-AUTO"));
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 1 && st.writes == 1,
        "replayed state does not duplicate"
    );
    l.set_invite(b"");
    l.pump();
    let st = lobby_stats();
    assert!(st.leaves == 1 && st.active == 0, "clearing leaves");

    lobby_mode(0, 1, 1);
    l.set_invite(b"S-STALE");
    l.pump();
    l.set_invite(b"");
    l.pump();
    lobby_mode(1, 1, 1);
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 2 && st.writes == 1 && st.leaves == 2,
        "late completion after clear left: {st:?}"
    );

    lobby_mode(0, 1, 1);
    l.set_invite(b"S-OLD");
    l.pump();
    l.set_invite(b"S-NEW");
    l.pump();
    lobby_mode(1, 1, 1);
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 4 && st.writes == 2 && st.leaves == 3 && st.invite == "S-NEW",
        "{st:?}"
    );
    l.set_invite(b"");
    l.pump();

    lobby_mode(1, 2, 1);
    l.set_invite(b"S-FAIL");
    l.pump();
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 5 && st.writes == 2 && st.active == 0,
        "no retry every frame: {st:?}"
    );
    l.set_invite(b"");
    l.pump();
    lobby_mode(1, 1, 0);
    l.set_invite(b"S-DATAFAIL");
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == 6 && st.writes == 3 && st.leaves == 5 && st.active == 0,
        "{st:?}"
    );
    l.set_invite(b"");
    l.pump();

    // Backoff: 2 s, then 5 s, then stop after three attempts.
    lobby_mode(1, 2, 1);
    l.set_invite(b"S-RETRY");
    l.pump();
    l.pump();
    let before = lobby_stats().creates;
    l.pump();
    assert_eq!(lobby_stats().creates, before, "backs off");
    lobby_mode(1, 1, 0);
    advance_clock(2050);
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(st.creates == before + 1 && st.active == 0);
    lobby_mode(1, 1, 1);
    advance_clock(5050);
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(st.creates == before + 2 && st.invite == "S-RETRY", "{st:?}");
    l.set_invite(b"");
    l.pump();
    lobby_mode(1, 2, 1);
    l.set_invite(b"S-EXHAUST");
    l.pump();
    l.pump();
    let before = lobby_stats().creates;
    advance_clock(2050);
    l.pump();
    l.pump();
    advance_clock(5050);
    l.pump();
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(
        st.creates == before + 2 && st.active == 0,
        "stops after three attempts: {st:?}"
    );
    assert!(has(
        "lobby: automatic Steam publication stopped after 3 attempts"
    ));
    l.set_invite(b"");
    l.pump();

    lobby_mode(1, 1, 1);
    l.set_invite(b"S-FINAL");
    l.pump();
    l.pump();
    let st = lobby_stats();
    assert!(st.active != 0 && st.bad_abi == 0, "{st:?}");
    assert!(has("published for friends"));
}
