//! Steam in the game process: the six legacy P2P natives of steam.hdll
//! diverted onto SDR (`sdr`), the optional friends-list lobby advert
//! (`lobby`), and the master-link's direct access to both
//! (`crate::sdrbridge::Bridge` over `Sdr`).
//!
//! steam.hdll is loaded lazily by the HashLink module loader: a thread polls
//! for it (200 ms, ~10 minutes), hooks the natives exactly where `hlp_<name>`
//! points (the bytecode resolves them that way), never calls the originals,
//! and starts the SDR warm-up and the once-a-second diagnostics.

pub mod lobby;
pub mod sdr;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};

use crate::dll::shim_log;
use crate::sdrbridge::{Status, Wire};
use crate::Log;
use sdr::{Api, Transport};

static SDR: OnceLock<Transport> = OnceLock::new();
static LOBBY: OnceLock<lobby::Lobby> = OnceLock::new();
/// The trampoline of SteamAPI_RunCallbacks; 0 until hooked.
static REAL_RUN_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

const NATIVES: [&str; 6] = [
    "send_p2p_packet",
    "read_p2p_packet",
    "is_p2p_packet_available",
    "accept_p2p_session",
    "close_p2p_session",
    "get_p2p_session_data",
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn module(name: &str) -> HMODULE {
    // SAFETY: a NUL-terminated wide string; no reference is taken.
    unsafe { GetModuleHandleW(wide(name).as_ptr()) }
}

/// GetProcAddress by name; null when absent.
pub(crate) fn proc(m: HMODULE, name: &str) -> *const c_void {
    let c = format!("{name}\0");
    // SAFETY: m is a loaded module, c NUL-terminated.
    unsafe { GetProcAddress(m, c.as_ptr()) }.map_or(std::ptr::null(), |f| f as *const c_void)
}

/// Resolves a flat export into a function pointer type F.
macro_rules! flat {
    ($m:expr, $name:literal) => {{
        let p = proc($m, $name);
        if p.is_null() {
            Err(concat!("steam_api64.dll lacks ", $name).to_string())
        } else {
            // SAFETY: the export has the steam_api_flat.h signature of the field it fills.
            Ok(unsafe { fn_ptr(p) })
        }
    }};
}
pub(crate) use flat;

/// A function pointer of type F at p.
///
/// # Safety
/// p is a function with F's signature and ABI.
pub(crate) unsafe fn fn_ptr<F: Copy>(p: *const c_void) -> F {
    const { assert!(std::mem::size_of::<F>() == std::mem::size_of::<*const c_void>()) };
    // SAFETY: same size (asserted); validity as the caller guarantees.
    unsafe { std::mem::transmute_copy::<*const c_void, F>(&p) }
}

fn now() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetTickCount() }
}

fn now64() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() }
}

/// The flat API from the steam_api64.dll in this process.
fn load_api() -> Result<Api, String> {
    let m = module("steam_api64.dll");
    if m.is_null() {
        return Err("steam_api64.dll is not loaded in this process".into());
    }
    let conn_info = proc(
        m,
        "SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo",
    );
    Ok(Api {
        get_hsteamuser: flat!(m, "SteamAPI_GetHSteamUser")?,
        messages_v002: flat!(m, "SteamAPI_SteamNetworkingMessages_SteamAPI_v002")?,
        utils_v004: flat!(m, "SteamAPI_SteamNetworkingUtils_SteamAPI_v004")?,
        send: flat!(m, "SteamAPI_ISteamNetworkingMessages_SendMessageToUser")?,
        receive: flat!(
            m,
            "SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel"
        )?,
        accept: flat!(m, "SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser")?,
        release: flat!(m, "SteamAPI_SteamNetworkingMessage_t_Release")?,
        init_relay: flat!(m, "SteamAPI_ISteamNetworkingUtils_InitRelayNetworkAccess")?,
        relay_status: flat!(m, "SteamAPI_ISteamNetworkingUtils_GetRelayNetworkStatus")?,
        register_cb: flat!(m, "SteamAPI_RegisterCallback")?,
        conn_info: (!conn_info.is_null()).then(|| {
            // SAFETY: GetSessionConnectionInfo's flat signature; optional (diagnostics).
            unsafe { fn_ptr(conn_info) }
        }),
    })
}

fn lobby() -> &'static lobby::Lobby {
    LOBBY.get_or_init(|| lobby::Lobby::new(lobby::load_api, now64, Log::new(shim_log)))
}

/// Runs f; a panic is logged and answered with fallback (never unwinds into the game).
fn guard<T>(what: &str, fallback: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        shim_log(&format!("sdr: {what} panicked"));
        fallback
    })
}

// ---------------------------------------------------------------- detours
// Signatures are hlsteam's (native/networking.cpp). Before the transport
// exists they fail closed like a transport that never connects.

extern "C" fn detour_send_p2p_packet(
    uid: *const u8,
    data: *const u8,
    length: i32,
    typ: i32,
    channel: i32,
) -> u8 {
    let Some(t) = SDR.get() else { return 0 };
    guard("send_p2p_packet", false, || {
        // SAFETY: HashLink passes a vuid (8 bytes) and length bytes of data.
        unsafe { t.send_p2p(uid, data, length, typ, channel) }
    })
    .into()
}

extern "C" fn detour_read_p2p_packet(
    data: *mut u8,
    max_length: i32,
    length: *mut u32,
    channel: i32,
) -> *mut u8 {
    let Some(t) = SDR.get() else {
        return std::ptr::null_mut();
    };
    guard("read_p2p_packet", std::ptr::null_mut(), || {
        // SAFETY: HashLink passes a buffer of max_length bytes and an out int.
        unsafe { t.read(data, max_length, length, channel) }
    })
}

extern "C" fn detour_is_p2p_packet_available(msg_size: *mut u32, channel: i32) -> u8 {
    let Some(t) = SDR.get() else { return 0 };
    guard("is_p2p_packet_available", false, || {
        // SAFETY: HashLink passes an out int.
        unsafe { t.available(msg_size, channel) }
    })
    .into()
}

extern "C" fn detour_accept_p2p_session(uid: *const u8) -> u8 {
    let Some(t) = SDR.get() else { return 0 };
    guard("accept_p2p_session", false, || {
        // SAFETY: HashLink passes a vuid.
        unsafe { t.accept(uid) }
    })
    .into()
}

extern "C" fn detour_close_p2p_session(uid: *const u8) -> u8 {
    let Some(t) = SDR.get() else { return 1 };
    guard("close_p2p_session", true, || {
        // SAFETY: HashLink passes a vuid.
        unsafe { t.close(uid) }
    })
    .into()
}

extern "C" fn detour_get_p2p_session_data(_uid: *const u8) -> *mut c_void {
    guard("get_p2p_session_data", std::ptr::null_mut(), || {
        SDR.get()
            .map_or(std::ptr::null_mut(), Transport::session_data)
    })
}

/// The game's SteamAPI_RunCallbacks delivers the session callbacks; counted,
/// and the lobby advert is pumped on the same thread right after.
extern "C" fn detour_run_callbacks() {
    let real = REAL_RUN_CALLBACKS.load(Ordering::SeqCst);
    if real == 0 {
        return; // the hook is in, its trampoline not stored yet: one frame skipped
    }
    // SAFETY: real is the trampoline of SteamAPI_RunCallbacks (void(void)).
    unsafe { std::mem::transmute::<usize, extern "C" fn()>(real)() };
    guard("SteamAPI_RunCallbacks hook", (), || {
        if let Some(t) = SDR.get() {
            t.on_run_callbacks();
        }
        lobby().pump();
    });
}

const DETOURS: [*const c_void; 6] = [
    detour_send_p2p_packet as *const c_void,
    detour_read_p2p_packet as *const c_void,
    detour_is_p2p_packet_available as *const c_void,
    detour_accept_p2p_session as *const c_void,
    detour_close_p2p_session as *const c_void,
    detour_get_p2p_session_data as *const c_void,
];

/// The address hlp_<name> resolves to (what the bytecode calls); the
/// steam_<name> export when there is no hlp_.
fn native_target(steam: HMODULE, name: &str) -> *const c_void {
    let direct = proc(steam, &format!("steam_{name}"));
    let hlp = proc(steam, &format!("hlp_{name}"));
    if hlp.is_null() {
        return direct;
    }
    let mut sign: *const i8 = std::ptr::null();
    // SAFETY: hlp_<name>(const char **sign) returns the native's address.
    let target = unsafe {
        std::mem::transmute::<*const c_void, extern "C" fn(*mut *const i8) -> *const c_void>(hlp)(
            &raw mut sign,
        )
    };
    if target != direct {
        shim_log(&format!(
            "sdr: hlp_{name} resolves to {target:p}, export steam_{name} is {direct:p} (hooking the resolved one)"
        ));
    }
    target
}

/// Hooks steam.hdll's natives and SteamAPI_RunCallbacks, then publishes the
/// transport. Fails closed ("partial") unless all six are diverted.
fn hook_steam(steam: HMODULE) -> &'static Transport {
    let t = Transport::new(load_api, now, Log::new(shim_log));
    let copy = proc(module("libhl.dll"), "hl_copy_bytes");
    if copy.is_null() {
        shim_log("sdr: libhl!hl_copy_bytes not found; read_p2p_packet cannot return a sender");
    } else {
        // SAFETY: hl_copy_bytes(const void *ptr, int size) -> vbyte*.
        t.set_hl_copy_bytes(Some(unsafe {
            std::mem::transmute::<*const c_void, sdr::HlCopyBytes>(copy)
        }));
    }
    let mut hooked = 0;
    for (name, detour) in NATIVES.iter().zip(DETOURS) {
        let target = native_target(steam, name);
        if crate::hook::install(&format!("steam!steam_{name}"), target, detour).is_some() {
            hooked += 1;
        }
    }
    t.diverted(hooked, 6);
    // Hooked now, before the game dispatches callbacks every frame: ilhook
    // does not suspend threads. steam.hdll imports steam_api64.dll, so it is mapped.
    let rc = proc(module("steam_api64.dll"), "SteamAPI_RunCallbacks");
    let real = (!rc.is_null())
        .then(|| {
            crate::hook::install(
                "steam_api64!SteamAPI_RunCallbacks",
                rc,
                detour_run_callbacks as *const c_void,
            )
        })
        .flatten();
    match real {
        Some(p) => {
            REAL_RUN_CALLBACKS.store(p as usize, Ordering::SeqCst);
            t.runs_hooked.store(true, Ordering::SeqCst);
        }
        None => {
            shim_log(
                "sdr: SteamAPI_RunCallbacks not hooked; callback dispatch will not be counted",
            );
        }
    }
    SDR.get_or_init(|| t)
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let what = name.to_string();
    let r = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                shim_log(&format!("sdr: {what} thread panicked"));
            }
        });
    if let Err(e) = r {
        shim_log(&format!("sdr: thread {name} failed to start: {e}"));
    }
}

/// Called once from the dll worker: polls for steam.hdll and brings SDR up.
pub fn start() {
    spawn("wartales-mp steam", || {
        // P2P natives are first called only once a lobby starts a game, so
        // the poll wins the race comfortably.
        for _ in 0..3000 {
            let steam = module("steam.hdll");
            if !steam.is_null() {
                let t = hook_steam(steam);
                spawn("wartales-mp sdr diag", move || loop {
                    std::thread::sleep(Duration::from_secs(1));
                    t.tick();
                });
                t.warm_up(|ms| std::thread::sleep(Duration::from_millis(ms.into())));
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        shim_log("sdr: steam.hdll never loaded, gave up (no Steam transport in this build?)");
    });
}

/// The in-process SDR wire the master-link's Bridge runs on.
pub struct Sdr;

impl Wire for Sdr {
    fn status(&self) -> Status {
        SDR.get().map_or_else(
            || Status {
                reason: "steam.hdll is not loaded yet".into(),
                ..Status::default()
            },
            Transport::status,
        )
    }

    fn send(&self, peer: u64, msg: &[u8]) -> Result<(), String> {
        match SDR.get().map_or(-1, |t| t.bridge_send(peer, msg)) {
            sdr::RESULT_OK => Ok(()),
            -1 => Err("SDR not ready".into()),
            res => Err(format!("SendMessageToUser failed: EResult {res}")),
        }
    }

    fn recv(&self) -> Vec<(u64, Vec<u8>)> {
        SDR.get().map_or_else(Vec::new, |t| t.bridge_receive(32))
    }

    fn set_lobby_invite(&self, invite: &str) -> bool {
        lobby().set_invite(invite.as_bytes())
    }
}
