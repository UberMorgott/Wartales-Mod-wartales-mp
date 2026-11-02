//! The game's own output into shim.log as "game:" lines (port of proxy.c's
//! hl_sys_print hook). Its stdout goes nowhere; its traces and every
//! Log.logError message are copied, so a co-op softlock leaves evidence.
//!
//! The game thread never touches the file: a line is formatted into a bounded
//! buffer (a line that does not fit is counted and dropped) and the writer
//! thread appends it every FLUSH, sooner once the buffer is half full, and at
//! process exit. LINES_MAX bounds what a spamming trace adds per run.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError, TryLockError};
use std::thread::Thread;
use std::time::Duration;

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::dll::{log_line, shim_append, shim_log};

const LINES_MAX: u32 = 20000;
const BUF_BYTES: usize = 64 * 1024;
const FLUSH: Duration = Duration::from_millis(500);
/// 300 UTF-16 units fit a 1 KB line once encoded.
const CHUNK: usize = 300;
const MSG_SCAN: usize = 64 * 1024;
const MSG_LINES: usize = 400;

struct Pending {
    buf: Vec<u8>,
    dropped: u64,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    buf: Vec::new(),
    dropped: 0,
});
static WRITER: OnceLock<Thread> = OnceLock::new();
static LINES: AtomicU32 = AtomicU32::new(0);
static SYNC_SKIPPED: AtomicU32 = AtomicU32::new(0);
static REAL: AtomicUsize = AtomicUsize::new(0);

type SysPrintFn = unsafe extern "C" fn(*const u16);

/// Hooks libhl!hl_sys_print and starts the writer (worker thread).
pub fn start() {
    // SAFETY: plain module/export lookups; libhl.dll imports us, so it is mapped.
    let p = unsafe {
        let libhl = GetModuleHandleW(windows_sys::w!("libhl.dll"));
        if libhl.is_null() {
            shim_log("hook libhl!hl_sys_print: libhl.dll not loaded");
            return;
        }
        GetProcAddress(libhl, windows_sys::s!("hl_sys_print"))
    };
    let p = p.map_or(std::ptr::null(), |f| f as *const c_void);
    // SAFETY: hl_sys_print is a libhl function entry; the detour has its signature.
    let Some(orig) =
        (unsafe { crate::hook::install("libhl!hl_sys_print", p, detour as *const c_void) })
    else {
        return;
    };
    REAL.store(orig as usize, Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("wartales-mp-gamelog".into())
        .spawn(|| {
            if std::panic::catch_unwind(writer).is_err() {
                shim_log("game: writer panicked, lines flushed at exit only");
            }
        });
    match spawned {
        Ok(t) => {
            let _ = WRITER.set(t.thread().clone());
        }
        Err(e) => shim_log(&format!(
            "game: writer thread failed ({e}), lines flushed at exit only"
        )),
    }
}

fn writer() {
    loop {
        std::thread::park_timeout(FLUSH);
        flush(false);
    }
}

/// Appends whatever is pending. `try_only` (process exit) never waits for the
/// lock: a thread killed while holding it must not hang the exit.
fn flush(try_only: bool) {
    let mut p = if try_only {
        match PENDING.try_lock() {
            Ok(p) => p,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        }
    } else {
        PENDING.lock().unwrap_or_else(PoisonError::into_inner)
    };
    let out = std::mem::take(&mut p.buf);
    let dropped = std::mem::take(&mut p.dropped);
    drop(p);
    if !out.is_empty() {
        shim_append(&out);
    }
    if dropped > 0 {
        shim_log(&format!("game: {dropped} lines dropped (log buffer full)"));
    }
}

/// Queues one line; only copies memory.
fn line(msg: &str) {
    let l = log_line(msg);
    let mut p = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    if p.buf.len() + l.len() <= BUF_BYTES {
        p.buf.extend_from_slice(l.as_bytes());
    } else {
        p.dropped += 1;
    }
    let wake = p.buf.len() >= BUF_BYTES / 2;
    drop(p);
    if wake {
        if let Some(t) = WRITER.get() {
            t.unpark();
        }
    }
}

/// The last buffered lines and the SYNC count (DllMain PROCESS_DETACH).
pub fn detach() {
    flush(true);
    let n = SYNC_SKIPPED.load(Ordering::Relaxed);
    if n > 0 {
        shim_log(&format!("game: {n} network SYNC trace lines not logged"));
    }
}

fn emit(w: &[u16]) -> bool {
    let k = LINES.fetch_add(1, Ordering::Relaxed) + 1;
    if k > LINES_MAX {
        if k == LINES_MAX + 1 {
            line(&format!(
                "game: further output not logged ({LINES_MAX} lines)"
            ));
        }
        return false;
    }
    line(&format!("game: {}", String::from_utf16_lossy(w)));
    true
}

/// hxbit's network trace (prefs networkLog, always in the lobby) prints
/// "<secs> [S] SYNC > <object> <fields>" for every replicated field change of
/// every frame; that noise filled LINES_MAX in ~15 min, so these lines are
/// counted, not logged. Its RPC and register lines are kept.
fn is_net_sync(w: &[u16]) -> bool {
    let i = w
        .iter()
        .take_while(|&&c| c == u16::from(b'.') || (u16::from(b'0')..=u16::from(b'9')).contains(&c))
        .count();
    let head = String::from_utf16_lossy(&w[i..w.len().min(i + 10)]);
    i > 0 && (head == " [S] SYNC " || head == " [C] SYNC ")
}

/// One shim.log line per printed line: a multi-line message (an exception and
/// its "Called from" stack) is split at '\n', and a line longer than CHUNK
/// units continues on the next line, so nothing is cut. MSG_LINES bounds one
/// message.
fn print(msg: &[u16]) {
    let mut lines = 0;

    let end = msg
        .iter()
        .rposition(|&c| c != u16::from(b'\n') && c != u16::from(b'\r'))
        .map_or(0, |i| i + 1);
    for raw in msg[..end].split(|&c| c == u16::from(b'\n')) {
        if lines >= MSG_LINES {
            return;
        }
        let mut l = raw.strip_suffix(&[u16::from(b'\r')]).unwrap_or(raw);
        if is_net_sync(l) {
            if SYNC_SKIPPED.fetch_add(1, Ordering::Relaxed) == 0 {
                line("game: network SYNC trace lines (prefs networkLog) are counted, not logged");
            }
            continue;
        }
        while !l.is_empty() && lines < MSG_LINES {
            let mut take = l.len().min(CHUNK);
            // Never split a UTF-16 surrogate pair.
            if take < l.len() && take > 1 && (0xD800..=0xDBFF).contains(&l[take - 1]) {
                take -= 1;
            }
            if !emit(&l[..take]) {
                return;
            }
            lines += 1;
            l = &l[take..];
        }
    }
}

unsafe extern "C" fn detour(msg: *const u16) {
    if !msg.is_null() {
        let mut n = 0;
        // SAFETY: msg is a NUL-terminated HashLink String's UTF-16 bytes; the scan is bounded.
        while n < MSG_SCAN && unsafe { *msg.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: the first n units were just read.
        let w = unsafe { std::slice::from_raw_parts(msg, n) };
        let _ = std::panic::catch_unwind(|| print(w));
    }
    let real = REAL.load(Ordering::Acquire);
    if real != 0 {
        // SAFETY: real is the callable original of hl_sys_print.
        unsafe { std::mem::transmute::<usize, SysPrintFn>(real)(msg) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn sync_trace_lines_are_recognised() {
        assert!(is_net_sync(&w("12.5 [S] SYNC > ent.Roaming aggroTime")));
        assert!(is_net_sync(&w("3 [C] SYNC > x")));
        assert!(!is_net_sync(&w("12.5 [S] RPC > x")));
        assert!(!is_net_sync(&w(" [S] SYNC > x")));
        assert!(!is_net_sync(&w("12")));
    }

    #[test]
    fn print_splits_lines_and_chunks() {
        let before = PENDING.lock().unwrap().buf.len();
        let long = "x".repeat(CHUNK + 5);
        print(&w(&format!("Error\r\nCalled from a\n\n{long}\n\n")));
        let p = PENDING.lock().unwrap();
        let text = String::from_utf8_lossy(&p.buf[before..]).into_owned();
        drop(p);
        let got: Vec<&str> = text
            .lines()
            .map(|l| l.split_once("] ").unwrap().1)
            .collect();
        assert_eq!(
            got,
            [
                "game: Error".to_string(),
                "game: Called from a".into(),
                format!("game: {}", "x".repeat(CHUNK)),
                "game: xxxxx".into(),
            ]
        );
    }
}
