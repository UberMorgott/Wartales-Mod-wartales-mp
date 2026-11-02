//! The SDR transport (port of shim/proxy/sdr.c): the game's six legacy Steam
//! P2P natives re-implemented on ISteamNetworkingMessages through the flat C
//! API of steam_api64.dll, plus the master-link's direct access on channel 100.
//!
//! Semantics the game depends on (hlsteam native/networking.cpp, Valve's
//! ISteamNetworking docs), see DESIGN.md "SDR в шиме":
//! - a vuid is an 8-byte little-endian SteamID64 in HashLink bytes;
//! - one SendMessageToUser per send, one message per read (boundaries kept);
//! - EP2PSend 0/1/2/3 -> Unreliable / Unreliable|NoDelay|NoNagle /
//!   Reliable|NoNagle / Reliable, always + AutoRestartBrokenSession;
//! - per-channel FIFOs: available reports the size of the NEXT message, read
//!   truncates to the caller's buffer like ReadP2PPacket;
//! - incoming sessions are accepted on SteamNetworkingMessagesSessionRequest_t,
//!   registered with SteamAPI_RegisterCallback (the only path the Steam client
//!   was measured to use), dispatched by the game's SteamAPI_RunCallbacks.
//!
//! Lost peers get a vanilla code-8 frame injected; reliable sends Steam has no
//! room for (EResult 25) are held and handed over in order. Fail closed: when
//! anything is missing the natives act as a transport that never connects.
//!
//! Every Steam call goes through `Api`, a table of flat-API function pointers:
//! resolved from steam_api64.dll in the game, faked in the tests.

// why: the Steam and HashLink ABIs carry sizes and channels as i32/u32; each
// is range-checked (non-negative, < 512 KiB, 0..7) before it is cast.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::sdrbridge::Status;
use crate::Log;

pub const IDENTITY_STEAMID: i32 = 16; // k_ESteamNetworkingIdentityType_SteamID
pub const RESULT_OK: i32 = 1; // k_EResultOK
pub const RESULT_LIMIT_EXCEEDED: i32 = 25; // k_EResultLimitExceeded

// k_nSteamNetworkingSend_*
pub const SEND_UNRELIABLE: i32 = 0;
pub const SEND_NO_NAGLE: i32 = 1;
pub const SEND_NO_DELAY: i32 = 4;
pub const SEND_RELIABLE: i32 = 8;
pub const SEND_AUTO_RESTART: i32 = 32;

/// Channels served to the game: 0..7.
pub const CHANNELS: i32 = 8;
/// The master-link's channel; the game never touches it.
pub const BRIDGE_CHANNEL: i32 = 100;
const PULL_BATCH: usize = 64;

pub const CB_SESSION_REQUEST: i32 = 1251; // k_iSteamNetworkingMessagesCallbacks + 1
pub const CB_SESSION_FAILED: i32 = 1252; // k_iSteamNetworkingMessagesCallbacks + 2

const WATCH_MAX: usize = 16;
const LOST_IDLE_MS: u32 = 60_000;
/// A re-dial that reaches connected within this cancels the loss.
const LOST_CONFIRM_MS: u32 = 2000;
const FRAME_CLOSE: u8 = 8; // SteamService message code: stop
const FRAME_JOIN: u8 = 3; // SteamService message code: a guest's join (connectTo@54893)

const BACKLOG_MAX: usize = 64 << 20; // bytes, bookkeeping included
const BACKLOG_MSGS: usize = 65536;
const BACKLOG_PEERS: usize = 64;
/// Bookkeeping per held message, as sizeof(sdr_out) in the C shim.
const OUT_COST: usize = 40;

// ------------------------------------------------------------------ FFI types

/// SteamNetworkingIdentity (steamnetworkingtypes.h), 136 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Identity {
    pub typ: i32,
    pub cb: i32,
    pub steam_id: u64,
    pub rest: [u8; 120],
}

/// SteamNetworkingMessage_t, 216 bytes.
#[repr(C)]
pub struct Msg {
    pub data: *mut c_void,
    pub size: i32,
    pub conn: u32,
    pub peer: Identity,
    pub conn_user_data: i64,
    pub usec_received: i64,
    pub message_number: i64,
    pub free_data: Option<unsafe extern "C" fn(*mut Msg)>,
    pub release: Option<unsafe extern "C" fn(*mut Msg)>,
    pub channel: i32,
    pub flags: i32,
    pub user_data: i64,
    pub lane: u16,
    pub pad: u16,
}

/// SteamNetConnectionInfo_t (and SteamNetworkingMessagesSessionFailed_t's
/// only member), 696 bytes.
#[repr(C)]
pub struct ConnInfo {
    pub peer: Identity,
    pub user_data: i64,
    pub listen_socket: u32,
    pub addr: [u8; 18],
    pub pad1: u16,
    pub pop_remote: u32,
    pub pop_relay: u32,
    pub state: i32,
    pub end_reason: i32,
    pub end_debug: [u8; 128],
    pub description: [u8; 128],
    pub flags: i32,
    pub reserved: [u32; 63],
}

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<Identity>() == 136);
    assert!(offset_of!(Msg, peer) == 16);
    assert!(offset_of!(Msg, release) == 184);
    assert!(offset_of!(Msg, channel) == 192);
    assert!(size_of::<Msg>() == 216);
    assert!(offset_of!(ConnInfo, state) == 176);
    assert!(offset_of!(ConnInfo, end_reason) == 180);
    assert!(offset_of!(ConnInfo, end_debug) == 184);
    assert!(offset_of!(ConnInfo, description) == 312);
    assert!(offset_of!(ConnInfo, flags) == 440);
    assert!(size_of::<ConnInfo>() == 696);
};

impl Identity {
    pub fn steam(id: u64) -> Identity {
        Identity {
            typ: IDENTITY_STEAMID,
            cb: 8,
            steam_id: id,
            rest: [0; 120],
        }
    }
}

impl ConnInfo {
    pub fn zeroed() -> ConnInfo {
        // SAFETY: plain C data; all-zero is a valid value of every field.
        unsafe { std::mem::zeroed() }
    }
}

/// CCallbackBase (steam_api_common.h): vtable pointer, uint8 m_nCallbackFlags,
/// int m_iCallback. `owner` is ours, past what Steam reads.
#[repr(C)]
pub struct Cb {
    pub vtable: *const Vtable,
    pub flags: u8,
    pub icallback: i32,
    owner: *const Transport,
}

// SAFETY: the pointers are to a static vtable and the (Sync) transport that
// owns the box; Steam reads the object from its callback thread.
unsafe impl Send for Cb {}
/// Run(void*), Run(void*, bool, SteamAPICall_t), GetCallbackSizeBytes(). Both
/// Run overloads get the same handler, so the compiler's order does not matter.
#[repr(C)]
pub struct Vtable {
    pub run: unsafe extern "C" fn(*mut Cb, *mut c_void),
    pub run_result: unsafe extern "C" fn(*mut Cb, *mut c_void, u8, u64),
    pub size: unsafe extern "C" fn(*mut Cb) -> i32,
}

/// Flat API (steam_api_flat.h): C++ references arrive as pointers, bool as a byte.
#[derive(Clone, Copy)]
pub struct Api {
    pub get_hsteamuser: unsafe extern "C" fn() -> i32,
    pub messages_v002: unsafe extern "C" fn() -> *mut c_void,
    pub utils_v004: unsafe extern "C" fn() -> *mut c_void,
    pub send:
        unsafe extern "C" fn(*mut c_void, *const Identity, *const c_void, u32, i32, i32) -> i32,
    pub receive: unsafe extern "C" fn(*mut c_void, i32, *mut *mut Msg, i32) -> i32,
    pub accept: unsafe extern "C" fn(*mut c_void, *const Identity) -> u8,
    pub release: unsafe extern "C" fn(*mut Msg),
    pub init_relay: unsafe extern "C" fn(*mut c_void),
    pub relay_status: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    pub register_cb: unsafe extern "C" fn(*mut Cb, i32),
    /// Optional: diagnostics only.
    pub conn_info: Option<
        unsafe extern "C" fn(*mut c_void, *const Identity, *mut ConnInfo, *mut c_void) -> i32,
    >,
}

/// hl_copy_bytes(ptr, size): a fresh HashLink bytes value.
pub type HlCopyBytes = unsafe extern "C" fn(*const u8, i32) -> *mut u8;

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Ready,
    Failed,
}

/// A received message, copied out of Steam's (released at once).
struct In {
    peer: u64,
    data: Vec<u8>,
    /// The injected close: never Steam's.
    synth: bool,
}

/// A reliable message held back while Steam's send buffer is full.
struct Out {
    peer: u64,
    flags: i32,
    channel: i32,
    /// Bytes 3..6 of a game frame (>= 7 bytes): its session.
    sid: Option<[u8; 4]>,
    data: Vec<u8>,
}

/// One followed peer (see sdr.c "Lost-peer close").
// why: the C shim's per-peer state machine, flag for flag.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default, Clone)]
struct Watch {
    id: u64,
    state: i32,
    end_reason: i32,
    state_since: u32,
    stuck_logged: bool,
    armed: bool,    // the game holds a session with this peer
    lost: bool,     // ... and it dropped; inject when the queue is clear
    lost_at: u32,   // when that was seen
    injected: bool, // the close was handed to the game for this session
    doomed: bool,   // ... lost for good: held messages of it were destroyed
    sid: [u8; 4],   // bytes 3..6 of the last game packet, as received
    channel: i32,   // where the game read it
    last_recv: u32,
    stopping: bool, // the game sent the peer a code 8 for stop_sid ...
    stop_sid: [u8; 4],
    let_go: bool, // ... and then closed: gone_sid is over
    gone_sid: [u8; 4],
    last_seen: u32,
}

struct State {
    phase: Phase,
    api: Option<Api>,
    msgs: usize,  // ISteamNetworkingMessages*
    utils: usize, // ISteamNetworkingUtils*
    retry_at: Option<u32>,
    last_reason: String,
    hl_copy_bytes: Option<HlCopyBytes>,
    cbs: Option<(Box<Cb>, Box<Cb>)>,
    queues: [VecDeque<In>; CHANNELS as usize],
    watch: Vec<Watch>,
    relay_last: i32,
    last_runs: Option<u32>,
    last_run_change: u32,
    run_warned: bool,
    send_last_res: i32,
    send_last_log: u32,
    bridge_seen: Vec<u64>,
    out: VecDeque<Out>,
    out_bytes: usize,
    out_peers: HashMap<u64, usize>, // held messages per peer (> 0)
    held: u64,                      // messages ever held
    sent: u64,
    send_failed: u64,
    received: u64,
    dropped: u64,
    accepted: u64,
}

/// The transport. One per process in the game (`super::sdr()`); the tests
/// build their own against a fake Api. Must not move once used: the session
/// callbacks registered with Steam point at it.
pub struct Transport {
    load: fn() -> Result<Api, String>,
    now: fn() -> u32,
    log: Log,
    st: Mutex<State>,
    runs: AtomicU32,
    pub(super) runs_hooked: AtomicBool,
    session_data_warned: AtomicBool,
}

pub fn state_name(s: i32) -> &'static str {
    match s {
        0 => "none",
        1 => "connecting",
        2 => "finding route",
        3 => "connected",
        4 => "closed by peer",
        5 => "problem detected locally",
        -1 => "fin wait",
        -2 => "linger",
        -3 => "dead",
        _ => "?",
    }
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

fn sid_of(data: &[u8]) -> Option<[u8; 4]> {
    data.get(3..7).map(|s| s.try_into().unwrap())
}

/// Reads a vuid: 8 bytes, little-endian.
///
/// # Safety
/// v points at 8 readable bytes.
unsafe fn uid(v: *const u8) -> u64 {
    // SAFETY: as the caller guarantees.
    u64::from_le_bytes(unsafe { v.cast::<[u8; 8]>().read_unaligned() })
}

impl Transport {
    pub fn new(load: fn() -> Result<Api, String>, now: fn() -> u32, log: Log) -> Transport {
        Transport {
            load,
            now,
            log,
            st: Mutex::new(State {
                phase: Phase::Idle,
                api: None,
                msgs: 0,
                utils: 0,
                retry_at: None,
                last_reason: String::new(),
                hl_copy_bytes: None,
                cbs: None,
                queues: Default::default(),
                watch: Vec::new(),
                relay_last: -1000,
                last_runs: None,
                last_run_change: 0,
                run_warned: false,
                send_last_res: 0,
                send_last_log: 0,
                bridge_seen: Vec::new(),
                out: VecDeque::new(),
                out_bytes: 0,
                out_peers: HashMap::new(),
                held: 0,
                sent: 0,
                send_failed: 0,
                received: 0,
                dropped: 0,
                accepted: 0,
            }),
            runs: AtomicU32::new(0),
            runs_hooked: AtomicBool::new(false),
            session_data_warned: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic under the lock is logged by the caller's guard; the state
        // itself stays consistent enough to keep the game running.
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn logf(&self, s: &str) {
        self.log.line(s);
    }

    pub fn set_hl_copy_bytes(&self, f: Option<HlCopyBytes>) {
        self.lock().hl_copy_bytes = f;
    }

    // ------------------------------------------------------------- watch

    /// Starts following id; true when it is new. When every slot is taken,
    /// the slot idle longest whose peer the game does not hold is reused.
    fn watch_peer(&self, st: &mut State, id: u64) -> bool {
        let now = (self.now)();
        if let Some(w) = st.watch.iter_mut().find(|w| w.id == id) {
            w.last_seen = now;
            return false;
        }
        let slot = if st.watch.len() < WATCH_MAX {
            st.watch.push(Watch::default());
            st.watch.len() - 1
        } else {
            let mut best: Option<usize> = None;
            for (i, w) in st.watch.iter().enumerate() {
                if w.armed || (w.lost && !w.injected) {
                    continue;
                }
                if best.is_none_or(|b| {
                    now.wrapping_sub(w.last_seen) > now.wrapping_sub(st.watch[b].last_seen)
                }) {
                    best = Some(i);
                }
            }
            let Some(b) = best else {
                return false; // every slot is a session in use
            };
            self.logf(&format!(
                "sdr: watch slot of idle peer {} reused for {id}",
                st.watch[b].id
            ));
            b
        };
        st.watch[slot] = Watch {
            id,
            state: -1000, // unknown yet
            state_since: now,
            last_seen: now,
            ..Watch::default()
        };
        true
    }

    fn find_watch(st: &State, id: u64) -> Option<usize> {
        st.watch.iter().position(|w| w.id == id)
    }

    /// Flags an armed session as gone; the diag tick injects.
    fn mark_lost(&self, st: &mut State, w: Option<usize>, state: i32, end_reason: i32, why: &str) {
        let Some(w) = w else { return };
        let now = (self.now)();
        let x = &mut st.watch[w];
        if !x.armed || x.lost || x.injected {
            return;
        }
        x.lost = true;
        x.lost_at = now;
        self.logf(&format!(
            "sdr: session with {} lost ({why}: state {state} ({}), end reason {end_reason}); the game still holds it, a close will be injected",
            x.id,
            state_name(state)
        ));
    }

    /// The once-a-second diagnostics and lost-peer pass (the C diag thread).
    pub fn tick(&self) {
        let mut st = self.lock();
        self.diag_tick(&mut st);
    }

    // why: one pass in the order of the C diag thread; splitting it hides that order.
    #[allow(clippy::too_many_lines)]
    fn diag_tick(&self, st: &mut State) {
        let now = (self.now)();
        let runs = self.runs.load(Ordering::SeqCst);
        if st.phase != Phase::Ready {
            return;
        }
        let api = st.api.expect("ready");
        self.backlog_flush(st); // also while the game is busy loading and sends nothing
        if st.utils != 0 {
            // SAFETY: utils is the live ISteamNetworkingUtils*.
            let relay =
                unsafe { (api.relay_status)(st.utils as *mut c_void, std::ptr::null_mut()) };
            if relay != st.relay_last {
                self.logf(&format!(
                    "sdr: relay network status {} -> {relay} (100 = current, 2 = waiting, 3 = attempting, <0 = failed)",
                    st.relay_last
                ));
                st.relay_last = relay;
            }
        }
        if st.last_runs != Some(runs) {
            st.last_runs = Some(runs);
            st.last_run_change = now;
            st.run_warned = false;
        } else if self.runs_hooked.load(Ordering::SeqCst)
            && !st.watch.is_empty()
            && !st.run_warned
            && now.wrapping_sub(st.last_run_change) > 5000
        {
            st.run_warned = true;
            self.logf(&format!(
                "sdr: WARNING: the game has not called SteamAPI_RunCallbacks for 5 s ({runs} calls so far); session request/failed callbacks cannot be delivered while that lasts"
            ));
        }
        let Some(conn_info) = api.conn_info else {
            self.lost_tick(st, now);
            return;
        };
        for i in 0..st.watch.len() {
            let peer = Identity::steam(st.watch[i].id);
            let mut info = ConnInfo::zeroed();
            // SAFETY: msgs is live; peer and info are valid for the call.
            let s = unsafe {
                conn_info(
                    st.msgs as *mut c_void,
                    &raw const peer,
                    &raw mut info,
                    std::ptr::null_mut(),
                )
            };
            let (dbg, desc) = (cstr(&info.end_debug), cstr(&info.description));
            let w = &st.watch[i];
            if s != w.state || info.end_reason != w.end_reason {
                let prev = w.state;
                let was = if prev == -1000 {
                    String::new()
                } else {
                    format!(" was {}", state_name(prev))
                };
                self.logf(&format!(
                    "sdr: session with {}: state {s} ({}){was}, end reason {} '{dbg}', relay POP {}, RunCallbacks {runs}; {desc}",
                    w.id,
                    state_name(s),
                    info.end_reason,
                    info.pop_relay
                ));
                let w = &mut st.watch[i];
                w.state = s;
                w.end_reason = info.end_reason;
                w.state_since = now;
                w.stuck_logged = false;
                if s == 3 && w.lost && !w.injected && !w.doomed {
                    w.lost = false;
                    let id = w.id;
                    self.logf(&format!(
                        "sdr: session with {id} connected again, no close injected"
                    ));
                } else if s == 4 || s == 5 || s == -3 {
                    self.mark_lost(st, Some(i), s, info.end_reason, "session ended");
                } else if s == 0
                    && (1..=3).contains(&prev)
                    && w.armed
                    && now.wrapping_sub(w.last_recv) < LOST_IDLE_MS
                {
                    self.mark_lost(
                        st,
                        Some(i),
                        s,
                        info.end_reason,
                        "session vanished while in use",
                    );
                }
            } else if (s == 1 || s == 2)
                && !w.stuck_logged
                && now.wrapping_sub(w.state_since) > 10000
            {
                let id = w.id;
                st.watch[i].stuck_logged = true;
                self.logf(&format!(
                    "sdr: session with {id} still {} after 10 s (no answer from the peer's Steam client yet); {desc}",
                    state_name(s)
                ));
            }
        }
        self.lost_tick(st, now);
    }

    // ------------------------------------------------------------- init

    /// Records a reason. Hard failures are final; soft ones (the Steam API is
    /// not up yet) are retried every two seconds.
    fn fail(&self, st: &mut State, hard: bool, reason: &str) {
        if st.last_reason != reason {
            st.last_reason = reason.to_string();
            let what = if hard {
                "UNAVAILABLE, transport disabled"
            } else {
                "not ready"
            };
            self.logf(&format!("sdr: {what}: {reason}"));
        }
        if hard {
            st.phase = Phase::Failed;
        } else {
            st.retry_at = Some((self.now)().wrapping_add(2000));
        }
    }

    /// The modern counterpart of P2PSessionRequest_t, which the game answers
    /// but the new interface never raises.
    fn session_request(&self, peer: *const Identity, via: &str) {
        if peer.is_null() {
            return;
        }
        // SAFETY: Steam hands a valid SteamNetworkingMessagesSessionRequest_t.
        let peer = unsafe { &*peer };
        let mut st = self.lock();
        let Some(api) = st.api.filter(|_| st.msgs != 0) else {
            return;
        };
        if peer.typ == IDENTITY_STEAMID {
            self.watch_peer(&mut st, peer.steam_id);
        }
        // SAFETY: msgs is live, peer valid.
        let ok = unsafe { (api.accept)(st.msgs as *mut c_void, peer) } != 0;
        st.accepted += 1;
        drop(st);
        self.logf(&format!(
            "sdr: session request from {} (type {}) via {via}: {} (RunCallbacks {})",
            peer.steam_id,
            peer.typ,
            if ok { "accepted" } else { "accept FAILED" },
            self.runs.load(Ordering::SeqCst)
        ));
    }

    fn session_failed(&self, info: *const ConnInfo, via: &str) {
        if info.is_null() {
            return;
        }
        // SAFETY: Steam hands a valid SteamNetworkingMessagesSessionFailed_t.
        let info = unsafe { &*info };
        self.logf(&format!(
            "sdr: session with {} FAILED (via {via}): state {} ({}), end reason {} '{}'; {}",
            info.peer.steam_id,
            info.state,
            state_name(info.state),
            info.end_reason,
            cstr(&info.end_debug),
            cstr(&info.description)
        ));
        if info.peer.typ == IDENTITY_STEAMID {
            let mut st = self.lock();
            let w = Self::find_watch(&st, info.peer.steam_id);
            self.mark_lost(&mut st, w, info.state, info.end_reason, "SessionFailed");
        }
    }

    /// Resolves the flat API and the interfaces once. Lock held.
    fn try_init(&self, st: &mut State) -> bool {
        match st.phase {
            Phase::Ready => return true,
            Phase::Failed => return false,
            Phase::Idle => {}
        }
        if st
            .retry_at
            .is_some_and(|at| ((self.now)().wrapping_sub(at) as i32) < 0)
        {
            return false;
        }
        let api = match (self.load)() {
            Ok(a) => a,
            Err(e) => {
                self.fail(st, true, &e);
                return false;
            }
        };
        if st.hl_copy_bytes.is_none() {
            self.fail(st, true, "libhl.dll lacks hl_copy_bytes");
            return false;
        }
        // SAFETY: flat-API accessors take no arguments.
        if unsafe { (api.get_hsteamuser)() } == 0 {
            self.fail(
                st,
                false,
                "Steam API not initialised yet (SteamAPI_GetHSteamUser() == 0)",
            );
            return false;
        }
        // SAFETY: as above.
        let msgs = unsafe { (api.messages_v002)() };
        if msgs.is_null() {
            self.fail(
                st,
                true,
                "SteamNetworkingMessages002 unavailable (Steam client too old, or not running)",
            );
            return false;
        }
        st.api = Some(api);
        st.msgs = msgs as usize;
        // Session callbacks, the way the game itself receives Steam events.
        let owner: *const Transport = self;
        let (req, failed) = st.cbs.get_or_insert_with(|| {
            let cb = |vtable, icallback| {
                Box::new(Cb {
                    vtable,
                    flags: 0,
                    icallback,
                    owner,
                })
            };
            (
                cb(&raw const REQUEST_VTABLE, CB_SESSION_REQUEST),
                cb(&raw const FAILED_VTABLE, CB_SESSION_FAILED),
            )
        });
        // SAFETY: the boxes live as long as the transport (process lifetime).
        unsafe {
            (api.register_cb)(&raw mut **req, CB_SESSION_REQUEST);
            (api.register_cb)(&raw mut **failed, CB_SESSION_FAILED);
        }
        let (f1, f2) = (req.flags, failed.flags);
        self.logf(&format!(
            "sdr: session request/failed callbacks registered with SteamAPI_RegisterCallback ({CB_SESSION_REQUEST}/{CB_SESSION_FAILED}, flags 0x{f1:02x}/0x{f2:02x}; 0x01 = registered)"
        ));
        // SAFETY: accessor without arguments.
        let utils = unsafe { (api.utils_v004)() };
        st.utils = utils as usize;
        if utils.is_null() {
            self.logf("sdr: SteamNetworkingUtils004 unavailable: no relay warm-up");
        } else {
            // SAFETY: utils is the live interface.
            let relay = unsafe {
                (api.init_relay)(utils);
                (api.relay_status)(utils, std::ptr::null_mut())
            };
            st.relay_last = relay;
            self.logf(&format!(
                "sdr: InitRelayNetworkAccess called, relay status {relay} (100 = current)"
            ));
        }
        if api.conn_info.is_none() {
            self.logf("sdr: steam_api64.dll lacks GetSessionConnectionInfo; session states will not be logged");
        }
        st.phase = Phase::Ready;
        st.last_reason.clear();
        self.logf(&format!(
            "sdr: READY: ISteamNetworkingMessages at {msgs:p}, utils at {utils:p}"
        ));
        true
    }

    /// The verdict for the master: ok, unavailable (final) or unknown (pending).
    pub fn status(&self) -> Status {
        let mut st = self.lock();
        self.try_init(&mut st);
        match st.phase {
            Phase::Ready => Status {
                known: true,
                ok: true,
                reason: String::new(),
            },
            Phase::Failed => Status {
                known: true,
                ok: false,
                reason: st.last_reason.clone(),
            },
            Phase::Idle => Status {
                known: false,
                ok: false,
                reason: if st.last_reason.is_empty() {
                    "SDR not initialised yet".into()
                } else {
                    st.last_reason.clone()
                },
            },
        }
    }

    /// Fails the transport closed unless every native was diverted: one left
    /// on the legacy path would split the game's traffic between two transports.
    pub fn diverted(&self, hooked: u32, total: u32) {
        self.logf(&format!(
            "sdr: {hooked} of {total} legacy P2P natives diverted; the legacy ISteamNetworking path is never used"
        ));
        if hooked != total {
            let mut st = self.lock();
            self.fail(
                &mut st,
                true,
                &format!("only {hooked} of {total} legacy P2P natives diverted"),
            );
        }
    }

    /// Requests relay access as soon as the Steam API is up (~5 minutes).
    pub fn warm_up(&self, sleep: impl Fn(u32)) {
        for _ in 0..600 {
            {
                let mut st = self.lock();
                if st.phase != Phase::Idle {
                    return;
                }
                st.retry_at = None; // the warm-up sets its own pace
                self.try_init(&mut st);
                if st.phase != Phase::Idle {
                    return;
                }
            }
            sleep(500);
        }
        self.logf("sdr: warm-up gave up waiting for the Steam API; will retry on first use");
    }

    /// Counts the game's SteamAPI_RunCallbacks (they deliver the session callbacks).
    pub fn on_run_callbacks(&self) {
        if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
            self.logf(
                "sdr: the game called SteamAPI_RunCallbacks for the first time (Steam callbacks are being dispatched)",
            );
        }
    }

    // ------------------------------------------------------------- master-link

    /// One reliable message to peer on BRIDGE_CHANNEL; the EResult, or -1
    /// when the transport is not ready.
    pub fn bridge_send(&self, peer: u64, data: &[u8]) -> i32 {
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return -1;
        }
        let api = st.api.expect("ready");
        let to = Identity::steam(peer);
        let Ok(len) = u32::try_from(data.len()) else {
            return 8; // k_EResultInvalidParam
        };
        let flags = SEND_RELIABLE | SEND_NO_NAGLE | SEND_AUTO_RESTART;
        // SAFETY: msgs live; to and data valid for the call.
        let res = unsafe {
            (api.send)(
                st.msgs as *mut c_void,
                &raw const to,
                data.as_ptr().cast(),
                len,
                flags,
                BRIDGE_CHANNEL,
            )
        };
        if self.watch_peer(&mut st, peer) || res != RESULT_OK {
            let relay = if st.utils == 0 {
                -1000
            } else {
                // SAFETY: utils live.
                unsafe { (api.relay_status)(st.utils as *mut c_void, std::ptr::null_mut()) }
            };
            self.logf(&format!(
                "sdr: bridge send to {peer}: {len} bytes on channel {BRIDGE_CHANNEL} = EResult {res} (1 = OK), relay status {relay}, RunCallbacks {}",
                self.runs.load(Ordering::SeqCst)
            ));
        }
        res
    }

    /// Up to max messages that arrived on BRIDGE_CHANNEL, with their sender.
    pub fn bridge_receive(&self, max: usize) -> Vec<(u64, Vec<u8>)> {
        let mut st = self.lock();
        let mut out = Vec::new();
        if st.phase != Phase::Ready {
            return out;
        }
        let api = st.api.expect("ready");
        let mut batch = vec![std::ptr::null_mut::<Msg>(); max];
        let n = i32::try_from(max).unwrap_or(i32::MAX);
        // SAFETY: msgs live; batch has room for n pointers.
        let n = unsafe {
            (api.receive)(
                st.msgs as *mut c_void,
                BRIDGE_CHANNEL,
                batch.as_mut_ptr(),
                n,
            )
        };
        for &m in &batch[..usize::try_from(n).unwrap_or(0)] {
            // SAFETY: Steam handed out m; it stays valid until released.
            let msg = unsafe { &*m };
            if msg.peer.typ == IDENTITY_STEAMID && msg.size >= 0 {
                let id = msg.peer.steam_id;
                // SAFETY: data holds size bytes.
                let data =
                    unsafe { std::slice::from_raw_parts(msg.data.cast::<u8>(), msg.size as usize) }
                        .to_vec();
                if st.bridge_seen.len() < 16 && !st.bridge_seen.contains(&id) {
                    st.bridge_seen.push(id);
                    self.logf(&format!(
                        "bridge: first message from {id} on channel {BRIDGE_CHANNEL}, {} bytes",
                        data.len()
                    ));
                }
                out.push((id, data));
            }
            // SAFETY: m came from receive and is released once.
            unsafe { (api.release)(m) };
        }
        out
    }

    // ------------------------------------------------------------- queues

    /// Is m a game packet of the session the game closed with that peer?
    fn of_gone(st: &State, peer: u64, data: &[u8]) -> bool {
        Self::find_watch(st, peer).is_some_and(|w| {
            let w = &st.watch[w];
            w.let_go && sid_of(data) == Some(w.gone_sid)
        })
    }

    /// Moves up to one batch Steam holds for channel to our queue's tail;
    /// returns how many Steam handed over.
    fn pump_batch(st: &mut State, channel: i32) -> usize {
        let api = st.api.expect("ready");
        let mut batch = [std::ptr::null_mut::<Msg>(); PULL_BATCH];
        // SAFETY: msgs live; batch has room for PULL_BATCH pointers.
        let n = unsafe {
            (api.receive)(
                st.msgs as *mut c_void,
                channel,
                batch.as_mut_ptr(),
                PULL_BATCH as i32,
            )
        };
        let n = usize::try_from(n).unwrap_or(0);
        for &m in &batch[..n] {
            // SAFETY: Steam handed out m; valid until released.
            let msg = unsafe { &*m };
            let kept = (msg.size >= 0 && msg.peer.typ == IDENTITY_STEAMID).then(|| In {
                peer: msg.peer.steam_id,
                // SAFETY: data holds size bytes.
                data: unsafe {
                    std::slice::from_raw_parts(msg.data.cast::<u8>(), msg.size as usize)
                }
                .to_vec(),
                synth: false,
            });
            // SAFETY: m came from receive and is released once.
            unsafe { (api.release)(m) };
            match kept {
                // Not a Steam peer, or an echo of the session the game let go:
                // the game would only answer that code 8 with its own.
                None => st.dropped += 1,
                Some(m)
                    if m.data.first() == Some(&FRAME_CLOSE)
                        && Self::of_gone(st, m.peer, &m.data) =>
                {
                    st.dropped += 1;
                }
                Some(m) => {
                    st.queues[channel as usize].push_back(m);
                    st.received += 1;
                }
            }
        }
        n
    }

    /// Everything Steam holds for channel: before a close or an injected
    /// frame, which must come after every real message already sent.
    fn pump(st: &mut State, channel: i32) {
        while Self::pump_batch(st, channel) == PULL_BATCH {}
    }

    /// The game's per-poll pull: one batch, only once our queue ran dry, so a
    /// poll costs O(1) however large a backlog is.
    fn pump_game(st: &mut State, channel: i32) {
        if st.queues[channel as usize].is_empty() {
            Self::pump_batch(st, channel);
        }
    }

    /// Discards queued messages from id, like CloseP2PSessionWithUser; once
    /// the closed session's sid is known (let_go), only that session's.
    fn drop_peer(st: &mut State, id: u64) {
        let gone = Self::find_watch(st, id)
            .filter(|&w| st.watch[w].let_go)
            .map(|w| st.watch[w].gone_sid);
        let mut dropped = 0;
        for q in &mut st.queues {
            q.retain(|n| {
                let drop = n.peer == id
                    && gone.is_none_or(|g| n.data.len() < 7 || sid_of(&n.data) == Some(g));
                dropped += u64::from(drop);
                !drop
            });
        }
        st.dropped += dropped;
    }

    /// The diag tick: lost sessions get their close once the game has read
    /// what the peer sent.
    fn lost_tick(&self, st: &mut State, now: u32) {
        for i in 0..st.watch.len() {
            let w = &st.watch[i];
            if !(w.lost && !w.injected && now.wrapping_sub(w.lost_at) >= LOST_CONFIRM_MS) {
                continue;
            }
            let (c, id, sid) = (w.channel, w.id, w.sid);
            Self::pump(st, c);
            if st.queues[c as usize].iter().any(|n| n.peer == id) {
                continue; // the peer's last real messages go first
            }
            // [code 8][pid 0 0: code 8 skips the order check][sid]
            let mut frame = vec![FRAME_CLOSE, 0, 0];
            frame.extend_from_slice(&sid);
            st.queues[c as usize].push_back(In {
                peer: id,
                data: frame,
                synth: true,
            });
            st.watch[i].injected = true;
            self.logf(&format!(
                "sdr: peer lost, injected close for {id} sid {} on channel {c} (code 8: the game runs its own disconnect)",
                i32::from_le_bytes(sid) // the game's getInt32(3)
            ));
        }
    }

    /// Marks the session with sid as one the game holds with peer w: from a
    /// game packet it read, or the join it sent.
    fn arm(&self, st: &mut State, w: Option<usize>, sid: [u8; 4], channel: i32) {
        let Some(w) = w else { return };
        let now = (self.now)();
        let x = &mut st.watch[w];
        if !x.armed || x.sid != sid {
            x.lost = false; // a new session: its own close
            x.injected = false;
            x.doomed = false;
        }
        x.armed = true;
        x.sid = sid;
        x.channel = channel;
        x.last_recv = now;
    }

    // ------------------------------------------------------------- backlog
    //
    // A legacy reliable SendP2PPacket was queued by Steam and delivered;
    // SendMessageToUser answers EResult 25 when the session's send buffer is
    // full and the game ignores the result: a host's reload burst lost the
    // save's tail to a slow guest (4-player cave reload, v0.2.13). So a
    // reliable message Steam has no room for is held and handed over later,
    // in order; later reliable ones to that peer wait behind it. Held
    // messages belong to their game session (sid): the game's close drops
    // that session's (its code 8 excepted). A backlog Steam can never take is
    // given up and the session the game holds is doomed: it always gets its close.

    fn backlog_unlink(st: &mut State, i: usize) {
        let o = st.out.remove(i).expect("index in range");
        st.out_bytes -= OUT_COST + o.data.len();
        if let Some(n) = st.out_peers.get_mut(&o.peer) {
            *n -= 1;
            if *n == 0 {
                st.out_peers.remove(&o.peer);
            }
        }
    }

    /// Removes held messages matching f; how many.
    fn backlog_remove(st: &mut State, f: impl Fn(&Out) -> bool) -> u64 {
        let mut n = 0;
        let mut i = 0;
        while i < st.out.len() {
            if f(&st.out[i]) {
                Self::backlog_unlink(st, i);
                n += 1;
            } else {
                i += 1;
            }
        }
        n
    }

    /// Drops what is held for the session of peer the game closed: all of the
    /// peer's, or with a known closed sid only that session's, its goodbye
    /// (code 8) excepted.
    fn backlog_close(&self, st: &mut State, peer: u64, gone: Option<[u8; 4]>) {
        let n = Self::backlog_remove(st, |o| {
            o.peer == peer
                && gone.is_none_or(|g| o.sid.is_none_or(|s| s == g))
                && !(o.data.len() >= 7 && o.data[0] == FRAME_CLOSE)
        });
        st.send_failed += n;
        if n > 0 {
            self.logf(&format!(
                "sdr: session with {peer} closed by the game: {n} held message(s) of it dropped, like the legacy close"
            ));
        }
    }

    /// Drops what is held for peer's session sid (None: the current one) and,
    /// when that is the session the game holds, dooms it.
    fn backlog_give_up(&self, st: &mut State, peer: u64, sid: Option<[u8; 4]>, why: &str) {
        let w = Self::find_watch(st, peer);
        let current =
            w.is_some_and(|w| st.watch[w].armed && sid.is_none_or(|s| s == st.watch[w].sid));
        let drop_sid = sid.or(w.map(|w| st.watch[w].sid));
        let n = Self::backlog_remove(st, |o| {
            o.peer == peer && drop_sid.is_none_or(|d| o.sid.is_none_or(|s| s == d))
        });
        st.send_failed += n;
        self.logf(&format!(
            "sdr: reliable messages to {peer} cannot be delivered ({why}): {n} held message(s) dropped; {}",
            if current {
                "the session counts as lost, the game gets its close"
            } else {
                "not the session the game holds"
            }
        ));
        if let Some(w) = w.filter(|_| current) {
            let now = (self.now)();
            let x = &mut st.watch[w];
            if !x.injected {
                if !x.lost {
                    x.lost_at = now;
                }
                x.lost = true;
                x.doomed = true;
            }
        }
    }

    /// Hands Steam what it now has room for, oldest first; a peer whose
    /// buffer is still full keeps the rest in order. Stops once every peer
    /// with a backlog is blocked: one send per such peer per poll.
    fn backlog_flush(&self, st: &mut State) {
        let api = st.api.expect("ready");
        let mut blocked: Vec<u64> = Vec::new();
        let mut i = 0;
        while i < st.out.len() && blocked.len() < st.out_peers.len() {
            let o = &st.out[i];
            if blocked.contains(&o.peer) {
                i += 1;
                continue;
            }
            let to = Identity::steam(o.peer);
            // SAFETY: msgs live; to and data valid for the call.
            let res = unsafe {
                (api.send)(
                    st.msgs as *mut c_void,
                    &raw const to,
                    o.data.as_ptr().cast(),
                    o.data.len() as u32,
                    o.flags,
                    o.channel,
                )
            };
            if res == RESULT_LIMIT_EXCEEDED {
                blocked.push(o.peer);
                i += 1;
                continue;
            }
            let (peer, sid) = (o.peer, o.sid);
            if res != RESULT_OK {
                self.backlog_give_up(st, peer, sid, &format!("SendMessageToUser = EResult {res}"));
                i = 0; // the list changed: start over
                blocked.clear();
                continue;
            }
            Self::backlog_unlink(st, i);
            st.sent += 1;
            if !st.out_peers.contains_key(&peer) {
                self.logf(&format!(
                    "sdr: send backlog to {peer} drained (Steam took every held message, in order); {} held so far, {} bytes still held",
                    st.held, st.out_bytes
                ));
            }
        }
    }

    /// Keeps a copy of a reliable message Steam has no room for (or that must
    /// wait behind one). False: it cannot be held.
    fn backlog_hold(
        &self,
        st: &mut State,
        peer: u64,
        data: &[u8],
        flags: i32,
        channel: i32,
    ) -> bool {
        let sid = sid_of(data);
        if st.out_bytes + OUT_COST + data.len() > BACKLOG_MAX || st.out.len() >= BACKLOG_MSGS {
            self.backlog_give_up(st, peer, sid, "send backlog over its limit");
            return false;
        }
        if !st.out_peers.contains_key(&peer) {
            self.logf(&format!(
                "sdr: send buffer to {peer} full (EResult 25): reliable messages are held and handed to Steam in order as it takes them"
            ));
            if st.out_peers.len() >= BACKLOG_PEERS {
                self.backlog_give_up(st, peer, sid, "too many peers with a send backlog");
                return false;
            }
        }
        *st.out_peers.entry(peer).or_insert(0) += 1;
        st.out_bytes += OUT_COST + data.len();
        st.out.push_back(Out {
            peer,
            flags,
            channel,
            sid,
            data: data.to_vec(),
        });
        st.held += 1;
        true
    }

    fn channel_ok(&self, channel: i32) -> bool {
        if (0..CHANNELS).contains(&channel) {
            return true;
        }
        self.logf(&format!(
            "sdr: channel {channel} is outside 0..{}, refused",
            CHANNELS - 1
        ));
        false
    }

    // ------------------------------------------------------------- natives

    /// send_p2p_packet(uid, data, length, type, channel) -> bool.
    ///
    /// # Safety
    /// uid points at 8 bytes and data at length bytes (or either is null).
    pub unsafe fn send_p2p(
        &self,
        uid_p: *const u8,
        data: *const u8,
        length: i32,
        typ: i32,
        channel: i32,
    ) -> bool {
        if uid_p.is_null() || data.is_null() || length < 0 || !self.channel_ok(channel) {
            return false;
        }
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return false;
        }
        let mut flags = match typ {
            0 => SEND_UNRELIABLE,                                 // k_EP2PSendUnreliable
            1 => SEND_UNRELIABLE | SEND_NO_DELAY | SEND_NO_NAGLE, // k_EP2PSendUnreliableNoDelay
            2 => SEND_RELIABLE | SEND_NO_NAGLE,                   // k_EP2PSendReliable
            3 => SEND_RELIABLE,                                   // k_EP2PSendReliableWithBuffering
            _ => {
                self.logf(&format!("sdr: send type {typ} unknown, packet refused"));
                return false;
            }
        };
        flags |= SEND_AUTO_RESTART;
        // SAFETY: as the caller guarantees.
        let (id, data) = unsafe {
            (
                uid(uid_p),
                std::slice::from_raw_parts(data, length as usize),
            )
        };
        let api = st.api.expect("ready");

        // The peer is watched and the game's session bookkeeping is done before
        // anything is sent or held: a join whose hold is refused must doom its
        // own sid (the game ignores the result and would wait for ever).
        let first = self.watch_peer(&mut st, id);
        if let (Some(sid), Some(w)) = (sid_of(data), Self::find_watch(&st, id)) {
            if data[0] == FRAME_CLOSE {
                // SteamService.stop / FORCE_DISCONNECT: closeSession follows.
                st.watch[w].stopping = true;
                st.watch[w].stop_sid = sid;
            } else if data[0] == FRAME_JOIN && !(st.watch[w].let_go && st.watch[w].gone_sid == sid)
            {
                // connectTo's join: in use before anything came back.
                self.arm(&mut st, Some(w), sid, channel);
            }
        }
        self.backlog_flush(&mut st);
        let reliable = flags & SEND_RELIABLE != 0;
        let mut res = if reliable && st.out_peers.contains_key(&id) {
            RESULT_LIMIT_EXCEEDED // behind what is held: never overtake it
        } else {
            let to = Identity::steam(id);
            // SAFETY: msgs live; to and data valid for the call.
            unsafe {
                (api.send)(
                    st.msgs as *mut c_void,
                    &raw const to,
                    data.as_ptr().cast(),
                    length as u32,
                    flags,
                    channel,
                )
            }
        };
        // As the legacy reliable send queued it; counted as sent when Steam takes it.
        let held = reliable
            && res == RESULT_LIMIT_EXCEEDED
            && self.backlog_hold(&mut st, id, data, flags, channel);
        if first {
            self.logf(&format!(
                "sdr: first packet to {id}: {length} bytes, type {typ} -> flags 0x{flags:x}, channel {channel} = EResult {res} (1 = OK)"
            ));
        }
        if held {
            res = RESULT_OK;
        } else if res == RESULT_OK {
            st.sent += 1;
        } else {
            st.send_failed += 1;
            let now = (self.now)();
            if res != st.send_last_res || now.wrapping_sub(st.send_last_log) > 5000 {
                st.send_last_res = res;
                st.send_last_log = now;
                self.logf(&format!(
                    "sdr: SendMessageToUser({id}, {length} bytes, flags 0x{flags:x}, channel {channel}) = EResult {res} ({} failed so far)",
                    st.send_failed
                ));
            }
        }
        res == RESULT_OK
    }

    /// is_p2p_packet_available(&size, channel) -> bool.
    ///
    /// # Safety
    /// msg_size is null or writable.
    pub unsafe fn available(&self, msg_size: *mut u32, channel: i32) -> bool {
        if !self.channel_ok(channel) {
            return false;
        }
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return false;
        }
        self.backlog_flush(&mut st); // the game polls every frame: held messages go out
        Self::pump_game(&mut st, channel);
        let Some(head) = st.queues[channel as usize].front() else {
            return false;
        };
        if !msg_size.is_null() {
            // SAFETY: as the caller guarantees.
            unsafe { *msg_size = head.data.len() as u32 };
        }
        true
    }

    /// read_p2p_packet(data, max_length, &length, channel) -> sender vuid.
    ///
    /// # Safety
    /// data has max_length writable bytes; length is null or writable.
    pub unsafe fn read(
        &self,
        data: *mut u8,
        max_length: i32,
        length: *mut u32,
        channel: i32,
    ) -> *mut u8 {
        if data.is_null() || max_length < 0 || !self.channel_ok(channel) {
            return std::ptr::null_mut();
        }
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return std::ptr::null_mut();
        }
        Self::pump_game(&mut st, channel);
        let Some(m) = st.queues[channel as usize].pop_front() else {
            return std::ptr::null_mut();
        };
        let n = m.data.len().min(max_length as usize); // truncate, like ReadP2PPacket
                                                       // SAFETY: data has max_length >= n bytes.
        unsafe { std::ptr::copy_nonoverlapping(m.data.as_ptr(), data, n) };
        let from = m.peer;
        if self.watch_peer(&mut st, from) || st.received == 1 {
            self.logf(&format!(
                "sdr: first packet received from {from}, {n} bytes, channel {channel}"
            ));
        }
        if let Some(sid) =
            sid_of(&m.data).filter(|_| !m.synth && !Self::of_gone(&st, from, &m.data))
        {
            // A game packet [code][pid:2][sid:4]...: the session is in use. A
            // late packet of the session the game closed does not arm it again.
            let w = Self::find_watch(&st, from);
            self.arm(&mut st, w, sid, channel);
        }
        let copy = st.hl_copy_bytes;
        drop(st);
        if !length.is_null() {
            // SAFETY: as the caller guarantees.
            unsafe { *length = n as u32 };
        }
        let bytes = from.to_le_bytes();
        // SAFETY: hl_copy_bytes copies 8 bytes into a fresh HashLink value.
        copy.map_or(std::ptr::null_mut(), |f| unsafe { f(bytes.as_ptr(), 8) })
    }

    /// accept_p2p_session(uid) -> bool.
    ///
    /// # Safety
    /// uid is null or points at 8 bytes.
    pub unsafe fn accept(&self, uid_p: *const u8) -> bool {
        if uid_p.is_null() {
            return false;
        }
        // SAFETY: as the caller guarantees.
        let id = unsafe { uid(uid_p) };
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return false;
        }
        let api = st.api.expect("ready");
        let peer = Identity::steam(id);
        // SAFETY: msgs live, peer valid.
        let ok = unsafe { (api.accept)(st.msgs as *mut c_void, &raw const peer) } != 0;
        drop(st);
        self.logf(&format!(
            "sdr: game accepted session with {id}: {}",
            if ok { "ok" } else { "no pending session" }
        ));
        ok
    }

    /// close_p2p_session(uid) -> bool. Ends the game's session with the peer
    /// and drops what it queued, like CloseP2PSessionWithUser, but keeps
    /// Steam's session: a reload's rejoin may never get a fresh SDR session up
    /// on a throttled network, while the old one keeps working. The session
    /// that ends is the one the game's last code 8 named (stop_sid).
    ///
    /// # Safety
    /// uid is null or points at 8 bytes.
    pub unsafe fn close(&self, uid_p: *const u8) -> bool {
        if uid_p.is_null() {
            return true; // nothing was open
        }
        // SAFETY: as the caller guarantees.
        let id = unsafe { uid(uid_p) };
        let mut st = self.lock();
        if !self.try_init(&mut st) {
            return true;
        }
        for c in 0..CHANNELS {
            Self::pump(&mut st, c); // what Steam still holds from the peer goes too
        }
        let w = Self::find_watch(&st, id);
        if let Some(w) = w {
            let x = &mut st.watch[w];
            if x.stopping {
                // else: a repeated close (FORCE_DISCONNECT timers) of gone_sid
                x.let_go = true;
                x.stopping = false;
                x.gone_sid = x.stop_sid;
            }
            if !x.let_go || x.sid == x.gone_sid {
                // the game let go
                x.armed = false;
                x.lost = false;
                x.injected = false;
                x.doomed = false;
            }
        }
        let gone = w
            .filter(|&w| st.watch[w].let_go)
            .map(|w| st.watch[w].gone_sid);
        self.backlog_close(&mut st, id, gone);
        Self::drop_peer(&mut st, id);
        let line = format!(
            "sdr: session with {id} closed by the game (Steam's session kept); sent {}, failed {}, received {}, dropped {}, auto-accepted {}",
            st.sent, st.send_failed, st.received, st.dropped, st.accepted
        );
        drop(st);
        self.logf(&line);
        true
    }

    /// get_p2p_session_data(uid): the legacy "no session" answer; the game
    /// never calls this.
    pub fn session_data(&self) -> *mut c_void {
        if !self.session_data_warned.swap(true, Ordering::SeqCst) {
            self.logf(
                "sdr: get_p2p_session_data is not provided by the SDR transport, answering null",
            );
        }
        std::ptr::null_mut()
    }
}

// ------------------------------------------------------------------ callbacks

/// Runs a callback body; a panic is logged, never unwound into Steam.
fn on_cb(this: *mut Cb, f: impl FnOnce(&Transport)) {
    // SAFETY: this is one of our Cb boxes; owner outlives it.
    let t = unsafe { &*(*this).owner };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(t))).is_err() {
        t.logf("sdr: session callback panicked");
    }
}

unsafe extern "C" fn cb_request_run(this: *mut Cb, ev: *mut c_void) {
    on_cb(this, |t| {
        t.session_request(ev.cast(), "SteamAPI_RegisterCallback");
    });
}
unsafe extern "C" fn cb_request_run_result(this: *mut Cb, ev: *mut c_void, _: u8, _: u64) {
    on_cb(this, |t| {
        t.session_request(ev.cast(), "SteamAPI_RegisterCallback");
    });
}
unsafe extern "C" fn cb_request_size(_: *mut Cb) -> i32 {
    std::mem::size_of::<Identity>() as i32 // SteamNetworkingMessagesSessionRequest_t
}
unsafe extern "C" fn cb_failed_run(this: *mut Cb, ev: *mut c_void) {
    on_cb(this, |t| {
        t.session_failed(ev.cast(), "SteamAPI_RegisterCallback");
    });
}
unsafe extern "C" fn cb_failed_run_result(this: *mut Cb, ev: *mut c_void, _: u8, _: u64) {
    on_cb(this, |t| {
        t.session_failed(ev.cast(), "SteamAPI_RegisterCallback");
    });
}
unsafe extern "C" fn cb_failed_size(_: *mut Cb) -> i32 {
    std::mem::size_of::<ConnInfo>() as i32 // SteamNetworkingMessagesSessionFailed_t
}

static REQUEST_VTABLE: Vtable = Vtable {
    run: cb_request_run,
    run_result: cb_request_run_result,
    size: cb_request_size,
};
static FAILED_VTABLE: Vtable = Vtable {
    run: cb_failed_run,
    run_result: cb_failed_run_result,
    size: cb_failed_size,
};
