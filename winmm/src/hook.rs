//! Inline hooks (ilhook): a plain detour plus the callable original, the way
//! MinHook's MH_CreateHook + MH_EnableHook gave them to the C shim.
//!
//! ilhook 2.3.0 has no "detour + trampoline" mode. `JmpToAddr(detour, noop)`
//! is one: the stub saves the registers, calls `noop`, restores them and jumps
//! to the detour with the caller's stack and return address intact. The
//! relocated original instructions sit in the same stub, and ilhook only hands
//! their address to the per-call routine, so `original` reads it back from the
//! stub (`mov rdx, imm64` right after the 139-byte prolog and `mov r8, imm64`).
//! The layout is pinned (`=2.3.0`), checked before a hook is reported live, and
//! tests/hook.rs calls a hooked function's original.

use std::ffi::c_void;

use ilhook::x64::{CallbackOption, HookFlags, HookType, Hooker, Registers};

use crate::dll::shim_log;

/// Offset of `mov rdx, <original>` in ilhook 2.3.0's JmpToAddr stub.
const ORIGINAL_MOV_AT: usize = 139 + 10;
/// ilhook's TRAMPOLINE_MAX_LEN: the original's copy lies inside the stub.
const STUB_LEN: usize = 1024;

unsafe extern "win64" fn noop(_regs: *mut Registers, _original: usize, _user: usize) {}

/// Hooks the function at `target` so every call runs `detour` (same signature
/// and ABI) and returns the callable original, or None (logged, nothing
/// patched). The hook stays for the life of the process.
///
/// # Safety
/// `target` must be the first instruction of a function of at least 14 bytes
/// that no other thread is executing while it is patched; `detour` must have
/// the target's exact signature and ABI.
pub unsafe fn install(
    name: &str,
    target: *const c_void,
    detour: *const c_void,
) -> Option<*const c_void> {
    if target.is_null() {
        shim_log(&format!("hook {name}: symbol not found"));
        return None;
    }
    let hooker = Hooker::new(
        target as usize,
        HookType::JmpToAddr(detour as usize, noop),
        CallbackOption::None,
        0,
        HookFlags::empty(),
    );
    // SAFETY: the caller guarantees target is a hookable function start.
    let point = match unsafe { hooker.hook() } {
        Ok(p) => p,
        Err(e) => {
            shim_log(&format!("hook {name}: {e}"));
            return None;
        }
    };
    // SAFETY: hook() just wrote its jump at target.
    let Some(orig) = (unsafe { original(target) }) else {
        drop(point); // unhooks
        shim_log(&format!(
            "hook {name}: unexpected ilhook stub layout, not hooked"
        ));
        return None;
    };
    // Never unhook: the stub (and the original's copy in it) must outlive every call.
    std::mem::forget(point);
    shim_log(&format!("hook {name}: installed at {target:p}"));
    Some(orig)
}

/// The original-function entry of the ilhook stub `target` now jumps to.
///
/// # Safety
/// `target` must be a function `install` hooked (or is hooking).
pub(crate) unsafe fn original(target: *const c_void) -> Option<*const c_void> {
    // SAFETY: the hook wrote a 5- or 14-byte jump over >= 14 bytes of code.
    let code = unsafe { &*target.cast::<[u8; 14]>() };
    let stub = match code {
        [0xe9, a, b, c, d, ..] => (target as usize)
            .wrapping_add(5)
            .wrapping_add_signed(i32::from_le_bytes([*a, *b, *c, *d]) as isize),
        [0xff, 0x25, 0, 0, 0, 0, abs @ ..] => usize::try_from(u64::from_le_bytes(*abs)).ok()?,
        _ => return None,
    };
    // SAFETY: stub is ilhook's live STUB_LEN-byte trampoline buffer.
    let mov = unsafe { &*((stub + ORIGINAL_MOV_AT) as *const [u8; 10]) };
    let [0x48, 0xba, imm @ ..] = mov else {
        return None;
    };
    let orig = usize::try_from(u64::from_le_bytes(*imm)).ok()?;
    (stub + ORIGINAL_MOV_AT + 10..stub + STUB_LEN)
        .contains(&orig)
        .then_some(orig as *const c_void)
}
