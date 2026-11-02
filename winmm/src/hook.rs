//! Inline hooks (ilhook). Minimal version for the steam track; the bytecode
//! track owns hook.rs with the same signature, the lead keeps one on merge.

use std::ffi::c_void;

use ilhook::x64::{CallbackOption, HookFlags, HookType, Hooker, Registers};

use crate::dll::shim_log;

/// A JmpToAddr hook only needs the registers restored before the jump.
unsafe extern "win64" fn no_op(_: *mut Registers, _: usize, _: usize) {}

/// Redirects target to detour for the life of the process and returns the
/// callable original (the trampoline). Logs "hook <name>: installed at <p>"
/// or why not.
pub fn install(name: &str, target: *const c_void, detour: *const c_void) -> Option<*const c_void> {
    if target.is_null() {
        shim_log(&format!("hook {name}: symbol not found"));
        return None;
    }
    let hooker = Hooker::new(
        target as usize,
        HookType::JmpToAddr(detour as usize, no_op),
        CallbackOption::None,
        0,
        HookFlags::empty(),
    );
    // SAFETY: target is the entry of a function in a loaded module (callers
    // pass GetProcAddress/hlp_ results); no thread runs it yet (see DESIGN).
    match unsafe { hooker.hook() } {
        Ok(point) => {
            // Hooks are permanent: never unhook.
            std::mem::forget(point);
            // SAFETY: the hook was just written at target.
            let orig = unsafe { original(target.cast()) };
            if orig.is_null() {
                shim_log(&format!(
                    "hook {name}: installed at {target:p}, but its trampoline was not found"
                ));
                return None;
            }
            shim_log(&format!("hook {name}: installed at {target:p}"));
            Some(orig)
        }
        Err(e) => {
            shim_log(&format!("hook {name}: failed: {e:?}"));
            None
        }
    }
}

/// Finds the relocated original in ilhook 2.3's JmpToAddr trampoline: the
/// patched entry jumps (E9 rel32 or FF 25 abs64) to a block that loads
/// `mov r8, user_data; mov rdx, ori_func`.
/// shortcut: depends on ilhook's trampoline layout; replaced by the bytecode track's hook.rs.
///
/// # Safety
/// target must carry a hook just written by ilhook.
unsafe fn original(target: *const u8) -> *const c_void {
    // SAFETY: the hook wrote at least 5 (E9) or 14 (FF 25) bytes at target,
    // and the trampoline is a 1024-byte buffer.
    unsafe {
        let tramp = if *target == 0xE9 {
            let rel = target.add(1).cast::<i32>().read_unaligned();
            target.add(5).offset(rel as isize)
        } else {
            target.add(6).cast::<*const u8>().read_unaligned()
        };
        for i in 0..512 {
            let p = tramp.add(i);
            if *p == 0x49 && *p.add(1) == 0xB8 && *p.add(10) == 0x48 && *p.add(11) == 0xBA {
                return p.add(12).cast::<*const c_void>().read_unaligned();
            }
        }
    }
    std::ptr::null()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    type F = extern "C" fn(i32, i32) -> i32;
    static ORIG: AtomicUsize = AtomicUsize::new(0);

    #[inline(never)]
    extern "C" fn target(a: i32, b: i32) -> i32 {
        // Long enough for a 14-byte patch, and not foldable.
        std::hint::black_box(a.wrapping_mul(3)).wrapping_add(std::hint::black_box(b))
    }

    extern "C" fn detour(a: i32, b: i32) -> i32 {
        // SAFETY: ORIG holds the trampoline install returned, of type F.
        let orig: F = unsafe { std::mem::transmute(ORIG.load(Ordering::SeqCst)) };
        orig(a, b) + 1000
    }

    #[test]
    fn detour_and_original() {
        let f: F = std::hint::black_box(target);
        assert_eq!(f(2, 1), 7);
        let orig = super::install("test!target", f as *const _, detour as *const _).unwrap();
        assert!(!orig.is_null(), "trampoline not found");
        ORIG.store(orig as usize, Ordering::SeqCst);
        assert_eq!(f(2, 1), 1007, "detour not called or original broken");
    }
}
