//! Fingerprints the mod files a co-op session depends on, so a guest whose
//! files differ from the host's is stopped at the join instead of crashing
//! later on "Missing <sheet>.<id>" (port of internal/modver).
//!
//! The hxbit signature the game exchanges covers code schemas only; data.cdb
//! (inside the Remastered res1.pak) is neither exchanged nor hashed, and two
//! different wartales-mp builds patch the bytecode differently. So each helper
//! hashes, once per session, the two files players get from the mod archive:
//! the game folder's winmm.dll (this helper is linked into it, and so is the
//! bytecode patcher) and res1.pak.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{logf, Log};

/// The hash of a file that does not exist: a player without the Remastered
/// mod has no res1.pak, which differs from one who has it.
pub const NONE: &str = "none";

/// The bytecode patcher and this helper.
pub const FILE_DLL: &str = "winmm.dll";
/// The Remastered mod's data pack.
pub const FILE_RES1: &str = "res1.pak";

/// One player's fingerprint. An empty hash is unknown and is never compared;
/// build is for logs only (the DLL hash already covers it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub build: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dll: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub res1: String,
}

/// The fingerprint for the log; None is an older wartales-mp that sent none.
pub fn describe(i: Option<&Info>) -> String {
    let Some(i) = i else {
        return "none (older wartales-mp)".into();
    };
    let short = |s: &str| {
        if s.is_empty() {
            "unknown".to_string()
        } else {
            s.chars().take(12).collect()
        }
    };
    let build = if i.build.is_empty() {
        "unknown"
    } else {
        &i.build
    };
    format!(
        "build {build}, {FILE_DLL} {}, {FILE_RES1} {}",
        short(&i.dll),
        short(&i.res1)
    )
}

/// The outcome of comparing two fingerprints (Go's `Result`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Files whose hashes differ.
    pub differ: Vec<&'static str>,
    /// Files known on both sides.
    pub compared: Vec<&'static str>,
    /// Files unknown on either side, or every file when the other side sent
    /// no fingerprint at all (an older wartales-mp).
    pub unchecked: Vec<&'static str>,
}

impl Comparison {
    /// At least one compared file differs.
    pub fn mismatch(&self) -> bool {
        !self.differ.is_empty()
    }
}

/// Checks remote against local. A missing fingerprint, or a file unknown on
/// either side, is unchecked: never a mismatch.
pub fn compare(local: Option<&Info>, remote: Option<&Info>) -> Comparison {
    let empty = Info::default();
    let (l, r) = (local.unwrap_or(&empty), remote.unwrap_or(&empty));
    let mut c = Comparison::default();
    for (name, a, b) in [(FILE_DLL, &l.dll, &r.dll), (FILE_RES1, &l.res1, &r.res1)] {
        if a.is_empty() || b.is_empty() {
            c.unchecked.push(name);
            continue;
        }
        c.compared.push(name);
        if a != b {
            c.differ.push(name);
        }
    }
    c
}

/// The text a guest's game shows when its files differ from the host's. One
/// line, no markup: the game's Confirm window renders it as is.
pub fn message(differ: &[&str]) -> String {
    let files = differ.join(", ");
    format!(
        "Версия мода отличается от хоста: скачай архив заново (отличается: {files}). \
         Mod version differs from the host: download the archive again (differs: {files})."
    )
}

/// Hashes the mod files in game_dir. None (unknown) leaves both hashes
/// unknown; a file that cannot be read is unknown too, and its error is
/// returned alongside the partial fingerprint.
fn compute(game_dir: Option<&Path>) -> (Info, Option<String>) {
    let mut info = Info {
        build: build().to_string(),
        ..Info::default()
    };
    let Some(dir) = game_dir else {
        return (info, Some("game folder unknown".into()));
    };
    let mut errs = Vec::new();
    for (name, slot) in [(FILE_DLL, &mut info.dll), (FILE_RES1, &mut info.res1)] {
        match hash_file(&dir.join(name)) {
            Ok(h) => *slot = h,
            Err(e) => errs.push(e),
        }
    }
    (info, (!errs.is_empty()).then(|| errs.join("\n")))
}

/// The hex sha256 of path, NONE when it does not exist, or the error when it
/// cannot be read.
fn hash_file(path: &Path) -> Result<String, String> {
    let mut f = match File::open(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(NONE.into()),
        Err(e) => return Err(format!("open {}: {e}", path.display())),
        Ok(f) => f,
    };
    let mut ctx = crate::log::Hasher::new(crate::log::Alg::Sha256);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => ctx.update(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(crate::wsx::hex(&ctx.finish()))
}

/// The commit this helper was built from, or "" when the build carries no
/// stamp. shortcut: Go read its VCS stamp; set WARTALES_MP_BUILD at build time
/// once the Rust build script passes one.
pub fn build() -> &'static str {
    option_env!("WARTALES_MP_BUILD").unwrap_or("")
}

/// Go's Duration.Round(time.Millisecond).String() for the log.
fn go_ms(d: Duration) -> String {
    let ms = (d.as_micros() + 500) / 1000;
    if ms == 0 {
        return "0s".into();
    }
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let (m, s) = (ms / 60_000, ms % 60_000);
    let secs = format!("{}.{:03}", s / 1000, s % 1000);
    let secs = secs.trim_end_matches('0').trim_end_matches('.');
    if m > 0 {
        format!("{m}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

/// This helper's fingerprint, computed once in the background: the files are
/// hashed as the game starts, which is what it loaded.
#[derive(Default)]
pub struct Session {
    info: Mutex<Option<Info>>,
    ready: Condvar,
}

impl Session {
    /// Starts hashing game_dir's files in the background; log gets the result.
    pub fn new(game_dir: Option<PathBuf>, log: Log) -> Arc<Session> {
        let s = Arc::new(Session::default());
        let s2 = s.clone();
        std::thread::spawn(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let start = Instant::now();
                let (info, err) = compute(game_dir.as_deref());
                if let Some(err) = err {
                    logf!(log, "modver: fingerprint incomplete ({err}); unknown files are not compared with other players");
                }
                logf!(
                    log,
                    "modver: this player's mod files: {} ({})",
                    describe(Some(&info)),
                    go_ms(start.elapsed())
                );
                *s2.info.lock().unwrap() = Some(info);
                s2.ready.notify_all();
            }));
            if run.is_err() {
                logf!(
                    log,
                    "modver: fingerprint panicked; mod files are not compared"
                );
                // An empty fingerprint is unchecked; waiters stop waiting for one.
                *s2.info
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Info::default());
                s2.ready.notify_all();
            }
        });
        s
    }

    /// A Session whose fingerprint is already known (tests, tools).
    pub fn fixed(info: Info) -> Arc<Session> {
        Arc::new(Session {
            info: Mutex::new(Some(info)),
            ready: Condvar::new(),
        })
    }

    /// The fingerprint, waiting at most wait for it; None when it is not
    /// ready by then (it is then unchecked).
    pub fn get(&self, wait: Duration) -> Option<Info> {
        let g = self.info.lock().unwrap();
        let (g, _) = self
            .ready
            .wait_timeout_while(g, wait, |i| i.is_none())
            .unwrap();
        g.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(build: &str, dll: &str, res1: &str) -> Info {
        Info {
            build: build.into(),
            dll: dll.into(),
            res1: res1.into(),
        }
    }

    #[test]
    fn compare_cases() {
        // (name, local, remote, differ, unchecked, mismatch)
        type Case<'a> = (
            &'a str,
            Option<Info>,
            Option<Info>,
            &'a [&'a str],
            &'a [&'a str],
            bool,
        );
        let a = info("1", "d1", "r1");
        let cases: [Case; 7] = [
            (
                "same",
                Some(a.clone()),
                Some(info("2", "d1", "r1")),
                &[],
                &[],
                false,
            ),
            (
                "res1 differs",
                Some(a.clone()),
                Some(info("", "d1", "r2")),
                &[FILE_RES1],
                &[],
                true,
            ),
            (
                "both differ",
                Some(a.clone()),
                Some(info("", "d2", NONE)),
                &[FILE_DLL, FILE_RES1],
                &[],
                true,
            ),
            (
                "older remote",
                Some(a.clone()),
                None,
                &[],
                &[FILE_DLL, FILE_RES1],
                false,
            ),
            (
                "local unknown",
                None,
                Some(a.clone()),
                &[],
                &[FILE_DLL, FILE_RES1],
                false,
            ),
            (
                "one unknown",
                Some(a.clone()),
                Some(info("", "", "r2")),
                &[FILE_RES1],
                &[FILE_DLL],
                true,
            ),
            (
                "all unknown",
                Some(Info::default()),
                Some(Info::default()),
                &[],
                &[FILE_DLL, FILE_RES1],
                false,
            ),
        ];
        for (name, local, remote, differ, unchecked, mismatch) in cases {
            let r = compare(local.as_ref(), remote.as_ref());
            assert!(
                r.differ == differ && r.unchecked == unchecked && r.mismatch() == mismatch,
                "{name}: {r:?}"
            );
            assert_eq!(r.compared.len() + r.unchecked.len(), 2, "{name}: {r:?}");
        }
    }

    #[test]
    fn message_text() {
        let m = message(&[FILE_RES1, FILE_DLL]);
        for want in [
            "Версия мода отличается от хоста: скачай архив заново",
            "res1.pak, winmm.dll",
            "download the archive again",
        ] {
            assert!(m.contains(want), "{m:?} lacks {want:?}");
        }
        assert!(!m.contains(['\n', '<', '>']), "{m:?} is not one plain line");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wartales-mp-modver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn compute_hashes() {
        let dir = temp_dir("compute");
        std::fs::write(dir.join(FILE_DLL), b"winmm build").unwrap();
        let (info, err) = compute(Some(&dir));
        assert_eq!(err, None);
        // sha256("winmm build"), computed independently.
        assert_eq!(
            info.dll,
            "faf586ef3c75c89e1a8d38d3c4c73d7a5394e75c7b55191c9298aa96e05ee736"
        );
        assert_eq!(info.res1, NONE);
        let (unknown, err) = compute(None);
        assert!(
            err.is_some() && unknown.dll.is_empty() && unknown.res1.is_empty(),
            "{unknown:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wire_shape() {
        let b = serde_json::to_string(&info("abc", "d", NONE)).unwrap();
        assert_eq!(b, r#"{"build":"abc","dll":"d","res1":"none"}"#);
        assert_eq!(serde_json::to_string(&Info::default()).unwrap(), "{}");
        assert_eq!(
            serde_json::from_str::<Info>(r#"{"dll":"d"}"#).unwrap(),
            info("", "d", "")
        );
        assert!(describe(None).contains("older"));
        assert_eq!(
            describe(Some(&info("", "0123456789abcdef", NONE))),
            "build unknown, winmm.dll 0123456789ab, res1.pak none"
        );
        assert!(describe(Some(&info("0123456789ab-dirty", "", "")))
            .starts_with("build 0123456789ab-dirty, "));
    }

    #[test]
    fn session() {
        let want = info("", "x", "");
        assert_eq!(Session::fixed(want.clone()).get(Duration::ZERO), Some(want));
        let dir = temp_dir("session");
        let (log, lines) = Log::capture();
        let s = Session::new(Some(dir.clone()), log);
        let got = s.get(Duration::from_secs(10)).expect("fingerprint");
        assert_eq!((got.dll.as_str(), got.res1.as_str()), (NONE, NONE));
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("modver: this player's mod files: ")));
        let slow = Session::default();
        assert_eq!(
            slow.get(Duration::from_millis(10)),
            None,
            "a pending fingerprint must read as unknown"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn go_durations() {
        let ms = Duration::from_millis;
        assert_eq!(go_ms(Duration::from_micros(400)), "0s");
        assert_eq!(go_ms(ms(12)), "12ms");
        assert_eq!(go_ms(ms(1000)), "1s");
        assert_eq!(go_ms(ms(1234)), "1.234s");
        assert_eq!(go_ms(ms(62_500)), "1m2.5s");
    }
}
