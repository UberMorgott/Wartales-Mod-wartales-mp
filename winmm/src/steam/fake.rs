//! An in-process stand-in for steam_api64.dll's flat API (port of
//! shim/check/fake_steam_api64.c): a loopback where a message sent to
//! identity X is queued as if received from X on the same channel, with
//! test controls to inject messages, force session states, fire the session
//! callbacks and read counters. One global instance: tests take `serial()`.

// why: the fakes stand in for C exports; their signatures are fixed by the Api table.
#![allow(
    clippy::missing_safety_doc,
    clippy::unnecessary_wraps,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::lobby::{LobbyApi, LobbyCreated};
use super::sdr::{Api, Cb, ConnInfo, Identity, Msg, IDENTITY_STEAMID};

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub sends: u32,
    pub last_flags: i32,
    pub last_channel: i32,
    pub last_send_to: u64,
    pub last_send_id_type: i32,
    pub queued: i64,
    pub allocated: u32,
    pub released: u32,
    pub accepts: u32,
    pub last_accept: u64,
    pub relay_inits: u32,
    pub registered_1251: bool,
    pub registered_1252: bool,
    pub size_1251: i32,
    pub size_1252: i32,
}

#[derive(Default, Clone, Debug)]
pub struct LobbyStats {
    pub creates: u32,
    pub writes: u32,
    pub leaves: u32,
    pub typ: i32,
    pub capacity: i32,
    pub active: u64,
    pub last_left: u64,
    pub invite: String,
    pub bad_abi: u32,
}

struct Fake {
    queues: HashMap<i32, VecDeque<usize>>, // *mut Msg
    hsteamuser: i32,
    send_result: i32,
    st: Stats,
    forced: HashMap<u64, (i32, i32)>,
    cb_1251: usize,
    cb_1252: usize,
    lobby: LobbyStats,
    lobby_call: u64,
    lobby_mode: (u8, i32, u8), // complete, result, write_ok
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            queues: HashMap::new(),
            hsteamuser: 1,
            send_result: 1,
            st: Stats::default(),
            forced: HashMap::new(),
            cb_1251: 0,
            cb_1252: 0,
            lobby: LobbyStats::default(),
            lobby_call: 0,
            lobby_mode: (1, 1, 1),
        }
    }
}

static FAKE: Mutex<Option<Fake>> = Mutex::new(None);
static SERIAL: Mutex<()> = Mutex::new(());
static CLOCK: AtomicU32 = AtomicU32::new(0);
static CLOCK64: AtomicU64 = AtomicU64::new(0);
// The "interfaces": only their addresses matter.
static MSGS: u8 = 0;
static UTILS: u8 = 0;
static MATCHMAKING: u8 = 0;
static STEAM_UTILS: u8 = 0;

fn fake() -> MutexGuard<'static, Option<Fake>> {
    FAKE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with<T>(f: impl FnOnce(&mut Fake) -> T) -> T {
    f(fake().as_mut().expect("fake reset"))
}

/// Serializes the tests on the one fake and resets it.
pub fn serial() -> MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    *fake() = Some(Fake::default());
    CLOCK.store(1_000_000, Ordering::SeqCst);
    CLOCK64.store(1_000_000, Ordering::SeqCst);
    g
}

fn is(p: *mut c_void, s: &'static u8) -> bool {
    std::ptr::eq(p.cast_const().cast::<u8>(), s)
}

fn slot(channel: i32) -> bool {
    (0..8).contains(&channel) || channel == 100
}

unsafe extern "C" fn msg_free(m: *mut Msg) {
    // SAFETY: m and its data were made by enqueue below.
    unsafe {
        let m = Box::from_raw(m);
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            m.data.cast::<u8>(),
            m.size as usize,
        )));
    }
}

fn enqueue(f: &mut Fake, from: u64, channel: i32, data: &[u8]) {
    let bytes: Box<[u8]> = data.into();
    let size = bytes.len() as i32;
    // SAFETY: Msg is plain C data; zero is valid for every field.
    let mut m: Box<Msg> = Box::new(unsafe { std::mem::zeroed() });
    m.data = Box::into_raw(bytes).cast();
    m.size = size;
    m.peer = Identity::steam(from);
    m.channel = channel;
    m.release = Some(msg_free);
    f.queues
        .entry(channel)
        .or_default()
        .push_back(Box::into_raw(m) as usize);
    f.st.queued += 1;
}

// ---------------------------------------------------------------- flat API

unsafe extern "C" fn get_hsteamuser() -> i32 {
    with(|f| f.hsteamuser)
}
unsafe extern "C" fn messages_v002() -> *mut c_void {
    (&raw const MSGS).cast_mut().cast()
}
unsafe extern "C" fn utils_v004() -> *mut c_void {
    (&raw const UTILS).cast_mut().cast()
}

unsafe extern "C" fn send(
    s: *mut c_void,
    to: *const Identity,
    data: *const c_void,
    len: u32,
    flags: i32,
    channel: i32,
) -> i32 {
    with(|f| {
        // SAFETY: the transport passes a valid identity.
        let to = unsafe { &*to };
        let res = if !is(s, &MSGS) || to.typ != IDENTITY_STEAMID || !slot(channel) {
            8 // k_EResultInvalidParam
        } else {
            f.send_result
        };
        if res == 1 {
            f.st.sends += 1;
            f.st.last_flags = flags;
            f.st.last_channel = channel;
            f.st.last_send_to = to.steam_id;
            f.st.last_send_id_type = to.typ;
            // SAFETY: data holds len bytes.
            let d = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len as usize) };
            enqueue(f, to.steam_id, channel, d); // loopback
        }
        res
    })
}

unsafe extern "C" fn receive(s: *mut c_void, channel: i32, out: *mut *mut Msg, max: i32) -> i32 {
    if !is(s, &MSGS) || !slot(channel) {
        return -1;
    }
    with(|f| {
        let mut n = 0;
        let q = f.queues.entry(channel).or_default();
        while n < max {
            let Some(m) = q.pop_front() else { break };
            // SAFETY: out has room for max pointers.
            unsafe { *out.add(n as usize) = m as *mut Msg };
            n += 1;
            f.st.queued -= 1;
            f.st.allocated += 1;
        }
        n
    })
}

unsafe extern "C" fn accept(s: *mut c_void, peer: *const Identity) -> u8 {
    // SAFETY: the transport passes a valid identity.
    let peer = unsafe { &*peer };
    if !is(s, &MSGS) || peer.typ != IDENTITY_STEAMID {
        return 0;
    }
    with(|f| {
        f.st.accepts += 1;
        f.st.last_accept = peer.steam_id;
    });
    1
}

unsafe extern "C" fn release(m: *mut Msg) {
    with(|f| f.st.released += 1);
    // SAFETY: m was handed out by receive; its own release frees it.
    unsafe { ((*m).release.expect("release set"))(m) };
}

unsafe extern "C" fn init_relay(s: *mut c_void) {
    if is(s, &UTILS) {
        with(|f| f.st.relay_inits += 1);
    }
}

unsafe extern "C" fn relay_status(s: *mut c_void, _: *mut c_void) -> i32 {
    if is(s, &UTILS) {
        100 // k_ESteamNetworkingAvailability_Current
    } else {
        0
    }
}

/// SteamAPI_RegisterCallback as steam_api does it: remember the object, set
/// the registered flag.
unsafe extern "C" fn register_cb(cb: *mut Cb, icallback: i32) {
    // SAFETY: cb is the transport's live callback object.
    let size = unsafe {
        (*cb).flags |= 0x01; // k_ECallbackFlagsRegistered
        ((*(*cb).vtable).size)(cb)
    };
    with(|f| match icallback {
        1251 => (f.cb_1251, f.st.registered_1251, f.st.size_1251) = (cb as usize, true, size),
        1252 => (f.cb_1252, f.st.registered_1252, f.st.size_1252) = (cb as usize, true, size),
        _ => {}
    });
}

/// The session with whoever was last sent to is "connected"; everyone else
/// "none", unless a test forced a state for that peer.
unsafe extern "C" fn conn_info(
    s: *mut c_void,
    peer: *const Identity,
    info: *mut ConnInfo,
    _: *mut c_void,
) -> i32 {
    // SAFETY: the transport passes valid pointers.
    let (peer, info) = unsafe { (&*peer, &mut *info) };
    if !is(s, &MSGS) || peer.typ != IDENTITY_STEAMID {
        return 0;
    }
    let (state, end) = with(|f| {
        let default = if f.st.sends > 0 && peer.steam_id == f.st.last_send_to {
            3
        } else {
            0
        };
        f.forced
            .get(&peer.steam_id)
            .copied()
            .unwrap_or((default, 0))
    });
    *info = ConnInfo::zeroed();
    info.peer = *peer;
    info.state = state;
    info.end_reason = end;
    info.pop_relay = if state == 3 { 0x666b_6521 } else { 0 }; // "fke!"
    let desc: &[u8] = if state == 3 {
        b"fake loopback connection"
    } else {
        b"no connection"
    };
    info.description[..desc.len()].copy_from_slice(desc);
    state
}

pub fn api() -> Api {
    Api {
        get_hsteamuser,
        messages_v002,
        utils_v004,
        send,
        receive,
        accept,
        release,
        init_relay,
        relay_status,
        register_cb,
        conn_info: Some(conn_info),
    }
}

pub fn load_ok() -> Result<Api, String> {
    Ok(api())
}

pub fn load_missing() -> Result<Api, String> {
    Err("steam_api64.dll is not loaded in this process".into())
}

pub fn now() -> u32 {
    CLOCK.load(Ordering::SeqCst)
}

pub fn now64() -> u64 {
    CLOCK64.load(Ordering::SeqCst)
}

/// Moves both fake clocks on by ms.
pub fn advance_clock(ms: u32) {
    CLOCK.fetch_add(ms, Ordering::SeqCst);
    CLOCK64.fetch_add(u64::from(ms), Ordering::SeqCst);
}

pub unsafe extern "C" fn copy_bytes(p: *const u8, n: i32) -> *mut u8 {
    // SAFETY: p holds n bytes; leaked like a HashLink GC value.
    let v: Box<[u8]> = unsafe { std::slice::from_raw_parts(p, n as usize) }.into();
    Box::leak(v).as_mut_ptr()
}

// ---------------------------------------------------------------- controls

pub fn stats() -> Stats {
    with(|f| f.st.clone())
}

pub fn inject(from: u64, channel: i32, data: &[u8]) {
    if slot(channel) {
        with(|f| enqueue(f, from, channel, data));
    }
}

pub fn set_send_result(res: i32) {
    with(|f| f.send_result = res);
}

pub fn set_hsteamuser(v: i32) {
    with(|f| f.hsteamuser = v);
}

/// GetSessionConnectionInfo reports state / end reason for peer from now on.
pub fn set_session_state(peer: u64, state: i32, end_reason: i32) {
    with(|f| f.forced.insert(peer, (state, end_reason)));
}

/// Posts SteamNetworkingMessagesSessionRequest_t through the registered
/// object, the way SteamAPI_RunCallbacks does (vtable slot 1, MSVC order).
pub fn fire_session_request(from: u64) -> bool {
    let cb = with(|f| f.cb_1251) as *mut Cb;
    if cb.is_null() {
        return false;
    }
    let mut req = Identity::steam(from);
    // SAFETY: cb is the transport's registered object.
    unsafe { ((*(*cb).vtable).run_result)(cb, (&raw mut req).cast(), 0, 0) };
    true
}

/// Posts SteamNetworkingMessagesSessionFailed_t with end reason and debug text.
pub fn fire_session_failed(from: u64, end_reason: i32, debug: &str) -> bool {
    let cb = with(|f| f.cb_1252) as *mut Cb;
    if cb.is_null() {
        return false;
    }
    let mut info = ConnInfo::zeroed();
    info.peer = Identity::steam(from);
    info.state = 5; // problem detected locally
    info.end_reason = end_reason;
    info.end_debug[..debug.len()].copy_from_slice(debug.as_bytes());
    let desc = b"fake P2P connection";
    info.description[..desc.len()].copy_from_slice(desc);
    // SAFETY: cb is the transport's registered object.
    unsafe { ((*(*cb).vtable).run)(cb, (&raw mut info).cast()) };
    true
}

// ---------------------------------------------------------------- lobby

unsafe extern "C" fn matchmaking() -> *mut c_void {
    if with(|f| f.hsteamuser) != 0 {
        (&raw const MATCHMAKING).cast_mut().cast()
    } else {
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn steam_utils() -> *mut c_void {
    if with(|f| f.hsteamuser) != 0 {
        (&raw const STEAM_UTILS).cast_mut().cast()
    } else {
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn create_lobby(s: *mut c_void, typ: i32, capacity: i32) -> u64 {
    with(|f| {
        if !is(s, &MATCHMAKING) {
            f.lobby.bad_abi += 1;
        }
        f.lobby.creates += 1;
        f.lobby.typ = typ;
        f.lobby.capacity = capacity;
        f.lobby_call += 1;
        f.lobby_call
    })
}
unsafe extern "C" fn call_completed(s: *mut c_void, call: u64, failed: *mut u8) -> u8 {
    with(|f| {
        if !is(s, &STEAM_UTILS) || call != f.lobby_call {
            f.lobby.bad_abi += 1;
        }
        // SAFETY: failed is writable.
        unsafe { *failed = 0 };
        f.lobby_mode.0
    })
}
unsafe extern "C" fn call_result(
    s: *mut c_void,
    call: u64,
    data: *mut c_void,
    size: i32,
    expected: i32,
    failed: *mut u8,
) -> u8 {
    with(|f| {
        // SAFETY: failed is writable; data holds size bytes.
        unsafe {
            if !is(s, &STEAM_UTILS) || call != f.lobby_call || size != 16 || expected != 513 {
                f.lobby.bad_abi += 1;
                *failed = 1;
                return 0;
            }
            *failed = 0;
            *data.cast::<LobbyCreated>() = LobbyCreated {
                result: f.lobby_mode.1,
                lobby: 1000 + call,
            };
        }
        1
    })
}
unsafe extern "C" fn set_lobby_data(
    s: *mut c_void,
    id: u64,
    key: *const u8,
    value: *const u8,
) -> u8 {
    // SAFETY: NUL-terminated strings from the lobby code.
    let (key, value) = unsafe {
        (
            std::ffi::CStr::from_ptr(key.cast())
                .to_string_lossy()
                .into_owned(),
            std::ffi::CStr::from_ptr(value.cast())
                .to_string_lossy()
                .into_owned(),
        )
    };
    with(|f| {
        if !is(s, &MATCHMAKING) || key != "invite" || value.len() > 32 {
            f.lobby.bad_abi += 1;
        }
        f.lobby.writes += 1;
        if f.lobby_mode.2 != 0 {
            f.lobby.active = id;
            f.lobby.invite = value;
        }
        f.lobby_mode.2
    })
}
unsafe extern "C" fn leave_lobby(s: *mut c_void, id: u64) {
    with(|f| {
        if !is(s, &MATCHMAKING) {
            f.lobby.bad_abi += 1;
        }
        f.lobby.leaves += 1;
        f.lobby.last_left = id;
        if f.lobby.active == id {
            f.lobby.active = 0;
            f.lobby.invite.clear();
        }
    });
}

pub fn lobby_load() -> Option<Result<LobbyApi, String>> {
    Some(Ok(LobbyApi {
        matchmaking,
        utils: steam_utils,
        create: create_lobby,
        set_data: set_lobby_data,
        leave: leave_lobby,
        completed: call_completed,
        result: call_result,
    }))
}

pub fn lobby_mode(complete: u8, result: i32, write_ok: u8) {
    with(|f| f.lobby_mode = (complete, result, write_ok));
}

pub fn lobby_stats() -> LobbyStats {
    with(|f| f.lobby.clone())
}
