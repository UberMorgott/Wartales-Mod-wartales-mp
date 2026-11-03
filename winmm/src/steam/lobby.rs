//! Optional Steam friends-list advertisement (port of shim/proxy/lobby.c): a
//! friends-only lobby whose "invite" metadata carries the host's join code.
//! It carries no game packets and never opens the overlay. Flat signatures:
//! steam_api_flat.h; result: LobbyCreated_t (callback 513, Windows pack 8).
//! Pumped on the game thread from the SteamAPI_RunCallbacks hook.

// why: size_of::<LobbyCreated>() (16) as the flat API's int.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};

use super::{flat, fn_ptr, module, proc};
use crate::Log;

/// LobbyCreated_t.
#[repr(C)]
#[derive(Default)]
pub struct LobbyCreated {
    pub result: i32,
    pub lobby: u64,
}
const _: () = assert!(
    std::mem::size_of::<LobbyCreated>() == 16 && std::mem::offset_of!(LobbyCreated, lobby) == 8
);

#[derive(Clone, Copy)]
pub struct LobbyApi {
    pub matchmaking: unsafe extern "C" fn() -> *mut c_void,
    pub utils: unsafe extern "C" fn() -> *mut c_void,
    pub create: unsafe extern "C" fn(*mut c_void, i32, i32) -> u64,
    pub set_data: unsafe extern "C" fn(*mut c_void, u64, *const u8, *const u8) -> u8,
    pub leave: unsafe extern "C" fn(*mut c_void, u64),
    pub completed: unsafe extern "C" fn(*mut c_void, u64, *mut u8) -> u8,
    pub result: unsafe extern "C" fn(*mut c_void, u64, *mut c_void, i32, i32, *mut u8) -> u8,
}

/// None while steam_api64.dll is not loaded (retried); Err names a missing export.
pub fn load_api() -> Option<Result<LobbyApi, String>> {
    let m = module("steam_api64.dll");
    if m.is_null() {
        return None;
    }
    let get = || -> Result<LobbyApi, String> {
        Ok(LobbyApi {
            matchmaking: flat!(m, "SteamAPI_SteamMatchmaking_v009")?,
            utils: flat!(m, "SteamAPI_SteamUtils_v010")?,
            create: flat!(m, "SteamAPI_ISteamMatchmaking_CreateLobby")?,
            set_data: flat!(m, "SteamAPI_ISteamMatchmaking_SetLobbyData")?,
            leave: flat!(m, "SteamAPI_ISteamMatchmaking_LeaveLobby")?,
            completed: flat!(m, "SteamAPI_ISteamUtils_IsAPICallCompleted")?,
            result: flat!(m, "SteamAPI_ISteamUtils_GetAPICallResult")?,
        })
    };
    Some(get().map_err(|e| e.trim_start_matches("steam_api64.dll lacks ").to_string()))
}

#[derive(Default)]
struct State {
    desired: String,
    generation: u64,
    pending: u64,
    pending_generation: u64,
    published: u64,
    published_generation: u64,
    attempts: u32,
    retry_at: u64,
    api: Option<LobbyApi>,
    /// Exports are missing: never retried.
    missing: bool,
}

pub struct Lobby {
    load: fn() -> Option<Result<LobbyApi, String>>,
    now: fn() -> u64,
    log: Log,
    st: Mutex<State>,
}

impl Lobby {
    pub fn new(
        load: fn() -> Option<Result<LobbyApi, String>>,
        now: fn() -> u64,
        log: Log,
    ) -> Lobby {
        Lobby {
            load,
            now,
            log,
            st: Mutex::new(State::default()),
        }
    }

    /// Records the desired invite (printable ASCII, <= 32; empty clears);
    /// false for an invalid one. Steam is only touched by pump.
    pub fn set_invite(&self, value: &[u8]) -> bool {
        if value.len() > 32 || value.iter().any(|c| !(33..=126).contains(c)) {
            return false;
        }
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        if st.desired.as_bytes() != value {
            st.desired = String::from_utf8_lossy(value).into_owned();
            st.generation += 1;
            st.attempts = 0;
            st.retry_at = 0;
        }
        true
    }

    /// Steam backend failures are transient. Three attempts per desired
    /// generation bound load and log noise; a clear/new host resets the budget.
    fn retry_later(&self, st: &mut State) {
        if st.pending_generation != st.generation || st.desired.is_empty() {
            return;
        }
        if st.attempts < 3 {
            let delay = if st.attempts == 1 { 2000 } else { 5000 };
            st.retry_at = (self.now)() + delay;
            self.log.line(&format!(
                "lobby: automatic Steam publication retry {}/3 in {delay} ms",
                st.attempts + 1
            ));
        } else {
            self.log
                .line("lobby: automatic Steam publication stopped after 3 attempts; recreate the host lobby to retry");
        }
    }

    /// One step, on the game thread. Steam calls and desired updates are
    /// serialized, so a clear cannot race publication and a pending result
    /// always belongs to one generation.
    pub fn pump(&self) {
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        if st.desired.is_empty() && st.pending == 0 && st.published == 0 {
            return;
        }
        if st.missing {
            return;
        }
        if st.api.is_none() {
            match (self.load)() {
                None => return,
                Some(Ok(a)) => st.api = Some(a),
                Some(Err(name)) => {
                    self.log.line(&format!(
                        "lobby: automatic Steam publication unavailable: missing {name}"
                    ));
                    st.missing = true;
                    return;
                }
            }
        }
        let Some(api) = st.api else { return };
        // SAFETY: flat accessors without arguments; the calls below pass the
        // interfaces they return and buffers valid for each call.
        unsafe {
            let (mm, utils) = ((api.matchmaking)(), (api.utils)());
            if mm.is_null() || utils.is_null() {
                return; // Steam may not be initialized yet
            }
            if st.published != 0 && st.published_generation != st.generation {
                (api.leave)(mm, st.published);
                self.log.line(&format!(
                    "lobby: left automatic Steam lobby {}",
                    st.published
                ));
                st.published = 0;
            }
            if st.pending != 0 {
                let mut failed = 0u8;
                let mut result = LobbyCreated::default();
                if (api.completed)(utils, st.pending, &raw mut failed) == 0 {
                    return;
                }
                let got = (api.result)(
                    utils,
                    st.pending,
                    (&raw mut result).cast(),
                    std::mem::size_of::<LobbyCreated>() as i32,
                    513,
                    &raw mut failed,
                );
                let invite = format!("{}\0", st.desired);
                if got == 0 || failed != 0 || result.result != 1 || result.lobby == 0 {
                    self.log.line(&format!(
                        "lobby: automatic Steam creation failed (I/O {failed}, EResult {})",
                        result.result
                    ));
                    self.retry_later(&mut st);
                } else if st.pending_generation != st.generation || st.desired.is_empty() {
                    (api.leave)(mm, result.lobby);
                    self.log.line(&format!(
                        "lobby: discarded stale automatic Steam lobby {}",
                        result.lobby
                    ));
                } else if (api.set_data)(
                    mm,
                    result.lobby,
                    c"invite".as_ptr().cast(),
                    invite.as_ptr(),
                ) == 0
                {
                    (api.leave)(mm, result.lobby);
                    self.log.line(
                        "lobby: automatic Steam publication failed: SetLobbyData rejected invite",
                    );
                    self.retry_later(&mut st);
                } else {
                    st.published = result.lobby;
                    st.published_generation = st.generation;
                    self.log.line(&format!(
                        "lobby: automatic Steam lobby {} published for friends",
                        st.published
                    ));
                }
                st.pending = 0;
            }
            if !st.desired.is_empty()
                && st.published == 0
                && st.attempts < 3
                && (self.now)() >= st.retry_at
            {
                st.pending_generation = st.generation;
                st.attempts += 1;
                st.pending = (api.create)(mm, 1, 256); // same friends-only settings as MPLobby
                if st.pending == 0 {
                    self.log
                        .line("lobby: automatic Steam creation refused (invalid API call)");
                    self.retry_later(&mut st);
                }
            }
        }
    }
}
