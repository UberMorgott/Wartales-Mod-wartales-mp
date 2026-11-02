//! DllMain and shim.log. Under the loader lock only: open the log, bind the
//! forwards, start the worker thread. Everything else runs on the worker.

use std::ffi::c_void;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const DLL_PROCESS_ATTACH: u32 = 1;
/// shim.log is deleted at attach once it grows past this.
const LOG_MAX_BYTES: u64 = 1024 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Appends "[pid tid tick] msg" to %LOCALAPPDATA%\wartales-mp\shim.log: one
/// open/append/close per line, so a crash loses nothing already written.
/// Failures are swallowed: logging never takes the game down.
pub fn shim_log(msg: &str) {
    let Some(path) = LOG_PATH.get() else { return };
    // SAFETY: both only read the calling thread's / system's state.
    let (tid, tick) = unsafe { (GetCurrentThreadId(), GetTickCount()) };
    let line = format!("[{} {tid} {tick}] {msg}\r\n", std::process::id());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
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
}

fn attach(inst: HMODULE) {
    // SAFETY: inst is this DLL's module handle, as DllMain received it.
    unsafe { DisableThreadLibraryCalls(inst) };
    log_open();
    shim_log("attach: winmm.dll proxy loaded");
    match crate::forward::bind() {
        Ok((bound, missing)) => shim_log(&format!(
            "forwards: {bound} exports bound, {missing} missing"
        )),
        Err(e) => shim_log(&format!("forwards: {e}")),
    }
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
    }
    1
}

// The CRT's DLL entry calls DllMain. A label, not a #[no_mangle] fn: rustc
// would add the latter to winmm.dll's exports.
core::arch::global_asm!(".globl DllMain", "DllMain:", "jmp {f}", f = sym dll_main);
