//! Inline hooks via MinHook (the `minhook` crate): a detour plus the callable
//! original. MinHook suspends the process's other threads while it writes the
//! jump and moves any thread caught inside the patched bytes, so a function
//! the game is already calling (`SteamAPI_RunCallbacks`) is hooked safely.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use minhook::MinHook;

use crate::dll::shim_log;

/// Hooks the function at `target` so every call runs `detour` (same signature
/// and ABI) and returns the callable original, or None (logged, nothing
/// patched). orig gets the original before the jump is written: a detour that
/// runs at once (CreateFileW, from install's own log line) can already call it.
/// The hook stays for the life of the process.
///
/// # Safety
/// `target` must be the start of a function; `detour` must have the target's
/// exact signature and ABI.
pub unsafe fn install(
    name: &str,
    target: *const c_void,
    detour: *const c_void,
    orig: &AtomicUsize,
) -> Option<*const c_void> {
    if target.is_null() {
        shim_log(&format!("hook {name}: symbol not found"));
        return None;
    }
    // SAFETY: the caller guarantees target is a function start and detour matches it.
    let p = match unsafe { MinHook::create_hook(target.cast_mut(), detour.cast_mut()) } {
        Ok(p) => p,
        Err(e) => {
            shim_log(&format!("hook {name}: MH_CreateHook failed: {e:?}"));
            return None;
        }
    };
    orig.store(p as usize, Ordering::Release);
    // SAFETY: the hook was just created for target.
    if let Err(e) = unsafe { MinHook::enable_hook(target.cast_mut()) } {
        shim_log(&format!("hook {name}: MH_EnableHook failed: {e:?}"));
        return None;
    }
    shim_log(&format!("hook {name}: installed at {target:p}"));
    Some(p.cast_const())
}
