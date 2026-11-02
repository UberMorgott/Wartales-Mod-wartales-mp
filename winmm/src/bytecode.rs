//! Hands the game a patched copy of its HashLink bytecode (port of proxy.c
//! hook_bytecode_open / redirect_bytecode / materialise_copy).
//!
//! The game reads hlboot.dat with _wfopen + fread from ucrtbase, which opens
//! through kernelbase (never kernel32's stubs), so CreateFileW/CreateFileA are
//! hooked in kernelbase. An open of hlboot.dat for reading is answered with
//! %LOCALAPPDATA%\wartales-mp\hlboot.dat: the original with the needle patches
//! (hlpatch) and the patcher passes applied. The copy is handed out only when it equals, byte for byte,
//! what patching the original yields right now; otherwise it is rewritten, and
//! when any stage refuses the original the game gets its own file. The game
//! folder is never written. Installed from DllMain: the game's main opens the
//! file before the worker thread could run.

use std::cell::Cell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use windows_sys::core::{PCSTR, PCWSTR};
use windows_sys::Win32::Foundation::{GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_DATA, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::dll::shim_log;

/// The largest image read into memory (hlboot.dat is ~19 MB).
const MAX_IMAGE: u64 = 256 * 1024 * 1024;
/// The files the hashlink loader reads as its main image.
const BYTECODE_NAMES: [&str; 2] = ["hlboot.dat", "sdlboot.dat"];

type CreateFileWFn = unsafe extern "system" fn(
    PCWSTR,
    u32,
    u32,
    *const SECURITY_ATTRIBUTES,
    u32,
    u32,
    HANDLE,
) -> HANDLE;
type CreateFileAFn = unsafe extern "system" fn(
    PCSTR,
    u32,
    u32,
    *const SECURITY_ATTRIBUTES,
    u32,
    u32,
    HANDLE,
) -> HANDLE;

/// kernelbase!CreateFileW, set before it is hooked; its original is read from
/// the stub until REAL_W is set (install's own log line opens shim.log).
static TARGET_W: AtomicUsize = AtomicUsize::new(0);
static REAL_W: AtomicUsize = AtomicUsize::new(0);
static REAL_A: AtomicUsize = AtomicUsize::new(0);

/// The last bytecode file prepared in this process and whether its copy may
/// be handed out; the lock also serialises (re)generation.
static PREPARED: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);

thread_local! {
    /// Set while this thread prepares the copy: its own opens pass straight through.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

fn real_w() -> CreateFileWFn {
    let mut p = REAL_W.load(Ordering::Acquire);
    if p == 0 {
        // SAFETY: TARGET_W is the function being hooked; the detour only runs once it is.
        p = unsafe { crate::hook::original(TARGET_W.load(Ordering::Acquire) as *const c_void) }
            .map_or(0, |o| o as usize);
    }
    assert!(p != 0, "CreateFileW original unknown");
    // SAFETY: p is the callable original of kernelbase!CreateFileW.
    unsafe { std::mem::transmute::<usize, CreateFileWFn>(p) }
}

fn real_a() -> CreateFileAFn {
    let p = REAL_A.load(Ordering::Acquire);
    assert!(p != 0, "CreateFileA original unknown");
    // SAFETY: p is the callable original of kernelbase!CreateFileA, set before its hook could be reached by name.
    unsafe { std::mem::transmute::<usize, CreateFileAFn>(p) }
}

/// Installs the CreateFileW/CreateFileA hooks (DllMain).
pub fn hook_open() {
    // SAFETY: plain module/export lookups of system DLLs mapped before us.
    let (kb, k32) = unsafe {
        (
            GetModuleHandleW(windows_sys::w!("kernelbase.dll")),
            GetModuleHandleW(windows_sys::w!("kernel32.dll")),
        )
    };
    let module = if kb.is_null() {
        shim_log("hook: kernelbase.dll not loaded, hooking kernel32 instead");
        k32
    } else {
        kb
    };
    if module.is_null() {
        shim_log("hook: no kernel module, bytecode patch disabled");
        return;
    }
    // SAFETY: module is a loaded module handle; the names are NUL-terminated.
    let (w, a) = unsafe {
        (
            GetProcAddress(module, windows_sys::s!("CreateFileW")),
            GetProcAddress(module, windows_sys::s!("CreateFileA")),
        )
    };
    let w = w.map_or(std::ptr::null(), |f| f as *const c_void);
    let a = a.map_or(std::ptr::null(), |f| f as *const c_void);
    TARGET_W.store(w as usize, Ordering::Release);
    // SAFETY: both are kernelbase function entries; the detours have their exact signatures.
    unsafe {
        if let Some(o) =
            crate::hook::install("kernelbase!CreateFileW", w, detour_w as *const c_void)
        {
            REAL_W.store(o as usize, Ordering::Release);
        }
        if let Some(o) =
            crate::hook::install("kernelbase!CreateFileA", a, detour_a as *const c_void)
        {
            REAL_A.store(o as usize, Ordering::Release);
        }
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn is_bytecode_name(path: &str) -> bool {
    let base = base_name(path);
    BYTECODE_NAMES.iter().any(|n| base.eq_ignore_ascii_case(n))
}

/// Only plain reads are the loader's; anything else is not ours.
fn is_plain_read(access: u32, disp: u32) -> bool {
    access & (GENERIC_WRITE | FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE) == 0
        && disp == OPEN_EXISTING
}

/// The path of the patched copy of `name`, when this open is the loader's
/// read of a bytecode image and the copy may be handed out.
fn redirect(name: &str, access: u32, disp: u32) -> Option<Vec<u16>> {
    if !is_bytecode_name(name) || !is_plain_read(access, disp) || BUSY.get() {
        return None;
    }
    let full = std::path::absolute(name).ok()?;
    let copy = crate::applog::dir().join(base_name(&full.to_string_lossy()));
    if full
        .to_string_lossy()
        .eq_ignore_ascii_case(&copy.to_string_lossy())
    {
        return None; // our own copy being opened: pass through
    }
    BUSY.set(true);
    let ok = std::panic::catch_unwind(|| prepare(&full, &copy)).unwrap_or_else(|_| {
        shim_log("bytecode: patching panicked");
        false
    });
    BUSY.set(false);
    ok.then(|| wide(&copy))
}

fn prepare(full: &Path, copy: &Path) -> bool {
    let mut prepared = PREPARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shim_log(&format!("bytecode: open {}", full.display()));
    let ok = match &*prepared {
        // The game opens hlboot.dat more than once at start; the copy was built
        // (or refused) for this very file a moment ago, so the seconds-long
        // patch is not run again.
        Some((p, ok))
            if p.to_string_lossy()
                .eq_ignore_ascii_case(&full.to_string_lossy()) =>
        {
            shim_log(&format!(
                "bytecode: already {} in this process",
                if *ok { "prepared" } else { "refused" }
            ));
            *ok
        }
        _ => {
            crate::splash::begin(); // patching takes seconds before the game has a window
            let ok = materialise(full, copy);
            crate::splash::patched(ok);
            *prepared = Some((full.to_path_buf(), ok));
            ok
        }
    };
    if ok {
        shim_log(&format!("bytecode: redirected to {}", copy.display()));
    } else {
        shim_log("bytecode: handing out the untouched original");
    }
    ok
}

/// fnv1a only identifies images in the log.
fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &c| {
        (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn read_image(path: &Path) -> std::io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len == 0 || len > MAX_IMAGE {
        return Err(std::io::Error::other(format!("{len} bytes")));
    }
    std::fs::read(path)
}

/// Makes `copy` hold exactly passes(needles(source)); false means: use the
/// original. Both stages are required: the needle patches without the passes'
/// co-op fixes are a half-patched game.
fn materialise(source: &Path, copy: &Path) -> bool {
    let mut img = match read_image(source) {
        Ok(b) => b,
        Err(e) => {
            shim_log(&format!("bytecode: cannot read the original ({e})"));
            return false;
        }
    };
    shim_log(&format!(
        "bytecode: original {} bytes, fnv1a {:016x}",
        img.len(),
        fnv1a(&img)
    ));
    match crate::hlpatch::apply(&mut img) {
        Ok(at) => {
            for (i, (p, pos)) in crate::hlpatch::PATCHES.iter().zip(at).enumerate() {
                shim_log(&format!(
                    "patch {i}: offset {pos} 0x{:02x} -> 0x{:02x}",
                    p.from, p.to
                ));
            }
        }
        Err(e) => {
            shim_log(&format!("{e}, image left untouched"));
            return false;
        }
    }
    shim_log(&format!(
        "bytecode: patched image fnv1a {:016x}",
        fnv1a(&img)
    ));
    let tips = capture_stderr(|| wartales_tips::patch_image(&img));
    let img = match tips {
        Ok(out) => {
            shim_log(&format!(
                "tips: patched image {} -> {} bytes",
                img.len(),
                out.len()
            ));
            out
        }
        Err(e) => {
            shim_log(&format!("tips: refused the image, nothing patched: {e:#}"));
            return false;
        }
    };
    match std::fs::read(copy) {
        Ok(cur) if cur == img => {
            shim_log("bytecode: existing copy verified byte-for-byte, reused");
            return true;
        }
        Ok(_) => shim_log("bytecode: copy stale, regenerating"),
        Err(_) => shim_log("bytecode: copy missing, regenerating"),
    }
    // Temp file + rename: a partial copy can never be opened as the real thing.
    let tmp = copy.with_extension("dat.new");
    match std::fs::write(&tmp, &img).and_then(|()| std::fs::rename(&tmp, copy)) {
        Ok(()) => {
            shim_log(&format!("bytecode: copy written, {} bytes", img.len()));
            true
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            shim_log(&format!("bytecode: writing the copy failed ({e})"));
            false
        }
    }
}

/// The passes report on stderr ("patched ..." / "... skipped: why"), which the
/// game process does not have: for the call, stderr is a temporary file (Rust
/// looks the handle up on every write), then its lines go to shim.log as
/// "tips: <line>".
fn capture_stderr<T>(f: impl FnOnce() -> T) -> T {
    use std::io::{Read, Seek};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .attributes(FILE_ATTRIBUTE_TEMPORARY)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(crate::applog::dir().join("tips-stderr.tmp"))
    else {
        return f();
    };
    // SAFETY: reads the process stderr handle, restored below.
    let prev = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    // SAFETY: file's handle stays open until after it is swapped back.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) } == 0 {
        return f();
    }
    // A panic still restores stderr before it goes on unwinding.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    // SAFETY: restores the handle saved above.
    unsafe { SetStdHandle(STD_ERROR_HANDLE, prev) };
    let out = out.unwrap_or_else(|p| std::panic::resume_unwind(p));
    let mut text = String::new();
    if file.rewind().is_ok() {
        let _ = file.take(1024 * 1024).read_to_string(&mut text);
    }
    for line in text.lines().filter(|l| !l.is_empty()) {
        shim_log(&format!("tips: {line}"));
    }
    out
}

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain([0]).collect()
}

/// The caller's NUL-terminated path, when its base name is a bytecode image
/// (checked without allocating: every CreateFile in the process comes here).
///
/// # Safety
/// `p` must be null or a NUL-terminated UTF-16 string.
unsafe fn bytecode_path(p: PCWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: p is NUL-terminated; the scan stops at the NUL or the bound.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
        if n > 32 * 1024 {
            return None;
        }
    }
    // SAFETY: the first n units were just read.
    let s = unsafe { std::slice::from_raw_parts(p, n) };
    let base = s
        .rsplit(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
        .next()?;
    let hit = BYTECODE_NAMES.iter().any(|name| {
        name.len() == base.len()
            && name
                .bytes()
                .zip(base)
                .all(|(a, &b)| u8::try_from(b).is_ok_and(|b| b.eq_ignore_ascii_case(&a)))
    });
    hit.then(|| String::from_utf16_lossy(s))
}

/// Opens the patched copy with the caller's arguments; None (logged) falls
/// back to the original.
fn open_copy(
    copy: &[u16],
    access: u32,
    share: u32,
    sa: *const SECURITY_ATTRIBUTES,
    disp: u32,
    flags: u32,
    tmpl: HANDLE,
) -> Option<HANDLE> {
    // SAFETY: the caller's own arguments with our NUL-terminated path.
    let h = unsafe { real_w()(copy.as_ptr(), access, share, sa, disp, flags, tmpl) };
    if h != INVALID_HANDLE_VALUE {
        return Some(h);
    }
    shim_log(&format!(
        "bytecode: opening the copy failed ({}), falling back to the original",
        std::io::Error::last_os_error()
    ));
    None
}

unsafe extern "system" fn detour_w(
    name: PCWSTR,
    access: u32,
    share: u32,
    sa: *const SECURITY_ATTRIBUTES,
    disp: u32,
    flags: u32,
    tmpl: HANDLE,
) -> HANDLE {
    // SAFETY: name is the caller's CreateFileW path argument.
    if let Some(copy) = unsafe { bytecode_path(name) }.and_then(|n| redirect(&n, access, disp)) {
        if let Some(h) = open_copy(&copy, access, share, sa, disp, flags, tmpl) {
            return h;
        }
    }
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { real_w()(name, access, share, sa, disp, flags, tmpl) }
}

unsafe extern "system" fn detour_a(
    name: PCSTR,
    access: u32,
    share: u32,
    sa: *const SECURITY_ATTRIBUTES,
    disp: u32,
    flags: u32,
    tmpl: HANDLE,
) -> HANDLE {
    if !name.is_null() {
        let mut w = [0u16; 1024];
        // SAFETY: name is NUL-terminated (-1 length); w is writable for its stated size.
        let n = unsafe { MultiByteToWideChar(CP_ACP, 0, name, -1, w.as_mut_ptr(), 1024) };
        let path = if n > 0 {
            // SAFETY: on success w holds a NUL-terminated wide copy of name.
            unsafe { bytecode_path(w.as_ptr()) }
        } else {
            None
        };
        if let Some(copy) = path.and_then(|p| redirect(&p, access, disp)) {
            if let Some(h) = open_copy(&copy, access, share, sa, disp, flags, tmpl) {
                return h;
            }
        }
    }
    // SAFETY: forwards the caller's arguments unchanged.
    unsafe { real_a()(name, access, share, sa, disp, flags, tmpl) }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_names_and_access() {
        assert!(is_bytecode_name(r"D:\Steam\Wartales\hlboot.dat"));
        assert!(is_bytecode_name("HLBOOT.DAT"));
        assert!(is_bytecode_name("x/sdlboot.dat"));
        assert!(!is_bytecode_name(r"C:\hlboot.dat.new"));
        assert!(is_plain_read(0x8000_0000, OPEN_EXISTING));
        assert!(!is_plain_read(GENERIC_WRITE, OPEN_EXISTING));
        assert!(!is_plain_read(0x8000_0000, 4));
    }

    /// Read-only use of the installed game's bytecode; absent skips.
    #[test]
    fn materialise_patches_rewrites_stale_and_refuses_bad() {
        let src = Path::new(r"D:\Steam\steamapps\common\Wartales\hlboot.dat");
        if !src.exists() {
            eprintln!("skip: no {}", src.display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("wmp-bytecode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("hlboot.dat");
        let mut want = std::fs::read(src).unwrap();
        crate::hlpatch::apply(&mut want).unwrap();
        let want = wartales_tips::patch_image(&want).unwrap();

        assert!(materialise(src, &copy), "missing copy");
        assert_eq!(fnv1a(&std::fs::read(&copy).unwrap()), fnv1a(&want));
        assert!(materialise(src, &copy), "reused copy");
        std::fs::write(&copy, b"stale").unwrap();
        assert!(materialise(src, &copy), "stale copy");
        assert_eq!(fnv1a(&std::fs::read(&copy).unwrap()), fnv1a(&want));

        let bad = dir.join("bad.dat");
        std::fs::write(&bad, b"HLB\x09").unwrap();
        assert!(!materialise(&bad, &copy), "not a supported image");
        assert_eq!(
            fnv1a(&std::fs::read(&copy).unwrap()),
            fnv1a(&want),
            "a refusal touched the copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fnv1a_matches_the_c_shim() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
