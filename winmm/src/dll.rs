//! DllMain and shim.log. Under the loader lock only: open the log, bind the
//! forwards, hook the bytecode open, start the worker thread. Everything else
//! runs on the worker.

use std::ffi::c_void;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;
/// shim.log is deleted at attach once it grows past this.
const LOG_MAX_BYTES: u64 = 1024 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// This DLL's module handle (window class owner for the splash).
static INST: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(crate) fn instance() -> HMODULE {
    INST.load(Ordering::Relaxed)
}

/// "[pid tid tick] msg\r\n", the shim.log line format.
pub(crate) fn log_line(msg: &str) -> String {
    // SAFETY: both only read the calling thread's / system's state.
    let (tid, tick) = unsafe { (GetCurrentThreadId(), GetTickCount()) };
    format!("[{} {tid} {tick}] {msg}\r\n", std::process::id())
}

/// Appends raw bytes to shim.log in one open/append/close; failures are swallowed.
pub(crate) fn shim_append(bytes: &[u8]) {
    let Some(path) = LOG_PATH.get() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let _ = f.write_all(bytes);
    }
}

/// Appends "[pid tid tick] msg" to %LOCALAPPDATA%\wartales-mp\shim.log: one
/// open/append/close per line, so a crash loses nothing already written.
/// Failures are swallowed: logging never takes the game down.
pub fn shim_log(msg: &str) {
    shim_append(log_line(msg).as_bytes());
}

/// Creates the log directory and drops an oversized log.
fn log_open() {
    let dir = crate::applog::dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("shim.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = LOG_PATH.set(path);
}

/// The worker thread: everything that must not run under the loader lock.
fn worker() {
    shim_log("worker: started");
    crate::gamelog::start();
    crate::steam::start();
}

fn attach(inst: HMODULE) {
    // SAFETY: inst is this DLL's module handle, as DllMain received it.
    unsafe { DisableThreadLibraryCalls(inst) };
    INST.store(inst, Ordering::Relaxed);
    log_open();
    shim_log("attach: winmm.dll proxy loaded");
    crate::splash::console_detach(); // no empty console window next to the game
    match crate::forward::bind() {
        Ok((bound, missing)) => shim_log(&format!(
            "forwards: {bound} exports bound, {missing} missing"
        )),
        Err(e) => shim_log(&format!("forwards: {e}")),
    }
    // Before the game's entry point opens hlboot.dat.
    crate::bytecode::hook_open();
    // The thread starts running once the loader lock is released.
    let spawned = std::thread::Builder::new()
        .name("wartales-mp".into())
        .spawn(|| {
            if std::panic::catch_unwind(worker).is_err() {
                shim_log("worker: panicked");
            }
        });
    if let Err(e) = spawned {
        shim_log(&format!("attach: worker thread failed to start: {e}"));
    }
}

extern "system" fn dll_main(inst: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // An unwind out of DllMain would abort the game: swallow it.
        let _ = std::panic::catch_unwind(|| attach(inst));
    } else if reason == DLL_PROCESS_DETACH {
        let _ = std::panic::catch_unwind(crate::gamelog::detach);
    }
    1
}

// The CRT's DLL entry calls DllMain. A label, not a #[no_mangle] fn: rustc
// would add the latter to winmm.dll's exports.
core::arch::global_asm!(".globl DllMain", "DllMain:", "jmp {f}", f = sym dll_main);
