//! Forwards every winmm export to the real System32\winmm.dll. The game's
//! libhl.dll imports WINMM.dll and the game folder is searched first, so this
//! DLL is loaded in its place; build.rs generated one thunk per System32
//! export (same name, or same NONAME ordinal) jumping through `SLOTS`.

use std::ffi::{c_void, CStr};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

include!(concat!(env!("OUT_DIR"), "/forwards.rs"));

/// Loads System32\winmm.dll by absolute path (a bare "winmm.dll" would find
/// this DLL again) and points every thunk at the real export. Returns
/// (bound, missing). Called from DllMain, before anything can call a thunk.
pub fn bind() -> Result<(usize, usize), String> {
    let mut buf = [0u16; 260];
    // SAFETY: buf is valid for buf.len() u16 writes.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), 260) } as usize;
    if n == 0 || n >= buf.len() {
        return Err(format!(
            "GetSystemDirectoryW failed ({})",
            std::io::Error::last_os_error()
        ));
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])).join("winmm.dll");
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: wide is NUL-terminated.
    let real = unsafe { LoadLibraryW(wide.as_ptr()) };
    if real.is_null() {
        return Err(format!(
            "LoadLibraryW({}) failed ({})",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut missing = 0;
    for ((name, ordinal), slot) in PROCS.iter().zip(&SLOTS) {
        // An ordinal is passed as the low word of the name pointer (MAKEINTRESOURCEA).
        let key = name.map_or(usize::from(*ordinal) as *const u8, |s| s.as_ptr().cast());
        // SAFETY: real is a loaded module; key is a NUL-terminated name or an ordinal.
        let p = unsafe { GetProcAddress(real, key) };
        missing += usize::from(p.is_none());
        slot.store(
            p.map_or(null_mut(), |f| f as *mut c_void),
            Ordering::Release,
        );
    }
    Ok((SLOTS.len() - missing, missing))
}
