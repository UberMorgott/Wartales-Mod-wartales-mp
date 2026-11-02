//! hook::install on a real function: kernelbase!CreateFileW, the bytecode
//! hook's own target (its own process, so no other test races the patch).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

type CreateFileWFn =
    unsafe extern "system" fn(PCWSTR, u32, u32, *const c_void, u32, u32, HANDLE) -> HANDLE;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn detour(
    name: PCWSTR,
    access: u32,
    share: u32,
    sa: *const c_void,
    disp: u32,
    flags: u32,
    tmpl: HANDLE,
) -> HANDLE {
    CALLS.fetch_add(1, Ordering::SeqCst);
    // SAFETY: ORIGINAL is set before any open in this test; same arguments.
    unsafe {
        let real = std::mem::transmute::<usize, CreateFileWFn>(ORIGINAL.load(Ordering::SeqCst));
        real(name, access, share, sa, disp, flags, tmpl)
    }
}

#[test]
fn detour_runs_and_the_original_still_opens_files() {
    // SAFETY: export lookup in a loaded system module.
    let target = unsafe {
        GetProcAddress(
            GetModuleHandleW(windows_sys::w!("kernelbase.dll")),
            windows_sys::s!("CreateFileW"),
        )
    }
    .expect("kernelbase!CreateFileW") as *const c_void;
    // SAFETY: a function entry; the detour has its signature. install's own
    // log line goes nowhere here (no shim.log in a test process).
    let orig = unsafe { winmm::hook::install("test!CreateFileW", target, detour as *const c_void) }
        .expect("hooked");
    ORIGINAL.store(orig as usize, Ordering::SeqCst);
    assert_ne!(orig, target);

    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read via the original");
    assert!(text.contains("ilhook"));
    assert!(CALLS.load(Ordering::SeqCst) >= 1, "detour not called");
    assert!(std::fs::File::open("Z:\\definitely\\missing").is_err());
}

#[test]
fn null_target_is_refused() {
    // SAFETY: a null target is checked before anything is patched.
    let got = unsafe { winmm::hook::install("test!none", std::ptr::null(), std::ptr::null()) };
    assert!(got.is_none());
}
