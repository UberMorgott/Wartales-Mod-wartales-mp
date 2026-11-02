//! The helper's debug log: %LOCALAPPDATA%\wartales-mp\wartales-mp.log.
//!
//! Accepted connections and their handshake headers, every master command with
//! its arguments and reply, relay counters, NAT discovery and every internal
//! error. The previous run is kept as wartales-mp.log.1. A debugging tool for a
//! single player, not telemetry: writes are synchronous, nothing leaves the
//! machine, payloads are truncated (`trunc`).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::Log;

pub const FILE_NAME: &str = "wartales-mp.log";

/// How much of any logged payload is kept.
pub const MAX_VALUE: usize = 512;

/// %LOCALAPPDATA%\wartales-mp (the temp dir when LOCALAPPDATA is unset).
pub fn dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("wartales-mp")
}

/// Rotates the previous log to .1 and opens a fresh one. If the file cannot
/// be opened the returned Log drops lines: a read-only profile never stops
/// the helper.
pub fn open() -> Log {
    let dir = dir();
    let path = dir.join(FILE_NAME);
    let file = fs::create_dir_all(&dir).and_then(|_| {
        let old = dir.join(format!("{FILE_NAME}.1"));
        let _ = fs::remove_file(&old); // a rename onto an existing name fails on Windows
        let _ = fs::rename(&path, &old);
        File::create(&path)
    });
    let ok = file.is_ok();
    let file = Mutex::new(file.ok());
    let log = Log::new(move |s| {
        if let Some(f) = file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = f.write_all(format!("{} {}\n", stamp(), s).as_bytes()); // logging never fails the caller
        }
    });
    if ok {
        crate::logf!(log, "log: {}", path.display());
    }
    log
}

/// Appends one line to the log file without holding it open (panic reports
/// from a thread whose Log is gone).
pub fn append(s: &str) {
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(dir().join(FILE_NAME)) {
        let _ = f.write_all(format!("{} {}\n", stamp(), s).as_bytes());
    }
}

/// Go's log.LstdFlags|log.Lmicroseconds: "2006/01/02 15:04:05.000000", local time.
fn stamp() -> String {
    // SAFETY: GetLocalTime only writes the SYSTEMTIME it is given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02}.{:06}",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        u32::from(t.wMilliseconds) * 1000
    )
}

/// A payload for the log, bounded to MAX_VALUE bytes, control characters
/// blanked so one frame stays one line.
pub fn trunc(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".into();
    }
    let mut end = s.len().min(MAX_VALUE);
    while !s.is_char_boundary(end) {
        end -= 1; // never cut a character in half
    }
    let out: String = s[..end].chars().map(|c| if c < ' ' || c == '\x7f' { ' ' } else { c }).collect();
    if s.len() > MAX_VALUE {
        format!("{out}...({}B)", s.len())
    } else {
        out
    }
}

/// The handshake headers in a stable order; x-pass (a password hash) redacted.
pub fn headers(h: &BTreeMap<String, String>) -> String {
    h.iter()
        .map(|(k, v)| if k == "x-pass" { format!("{k}=<redacted>") } else { format!("{k}={v}") })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trunc_bounds_and_blanks() {
        assert_eq!(trunc(""), "\"\"");
        assert_eq!(trunc("a\nb"), "a b");
        let long = "é".repeat(MAX_VALUE);
        let t = trunc(&long);
        assert!(t.ends_with(&format!("...({}B)", long.len())), "{t}");
        assert!(t.len() <= MAX_VALUE + 16);
    }

    #[test]
    fn headers_redact_pass() {
        let mut h = BTreeMap::new();
        h.insert("x-pass".to_string(), "secret".to_string());
        h.insert("a".to_string(), "1".to_string());
        assert_eq!(headers(&h), "a=1 x-pass=<redacted>");
    }

    #[test]
    fn stamp_shape() {
        let s = stamp();
        assert_eq!(s.len(), 26, "{s}");
        assert_eq!(&s[4..5], "/");
    }
}
