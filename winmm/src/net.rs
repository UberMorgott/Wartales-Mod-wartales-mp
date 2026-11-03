//! The co-op network side of the DLL: what the Go helper's `run` did, now on
//! threads of the game process (relay, NAT mapper, punch socket, SDR bridge,
//! master). It lives and dies with the game: no PID watch, no signals.

use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
};

use crate::dll::shim_log;
use crate::master::{self, Puncher};
use crate::{applog, logf, modver, nat, punch, relay, sdrbridge, Log};

/// The public TCP port of the relay and the proxy-link, and the UDP port of
/// the punch socket.
const PORT: u16 = 14250;
const MASTER_ADDR: &str = "127.0.0.1:60442";

/// Starts the network services once per process, in the background. Called
/// from the DLL's worker thread; never blocks it, never panics into it.
pub fn start() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // One runtime per session, whoever loads us: a second game on the same
    // profile must not fight over the ports. The handle is deliberately never
    // closed, so the name stays taken until the process ends.
    let name: Vec<u16> = mutex_name().encode_utf16().chain([0]).collect();
    // SAFETY: name is NUL-terminated and outlives the call; default security.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h.is_null() {
        // SAFETY: reads the calling thread's last error.
        shim_log(&format!("net: CreateMutexW failed ({})", unsafe {
            GetLastError()
        }));
        return;
    }
    // SAFETY: reads the calling thread's last error, set by CreateMutexW.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        shim_log("net: already running in this session, not started");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("wartales-mp-net".into())
        .spawn(|| {
            if let Err(p) = catch_unwind(run) {
                let msg = master::panic_text(&*p);
                applog::append(&format!("FATAL: network start panicked: {msg}"));
                shim_log(&format!("net: start panicked: {msg}"));
            }
        });
    if let Err(e) = spawned {
        shim_log(&format!("net: thread failed to start: {e}"));
    }
}

/// Runs f on a thread of its own; a panic is logged and ends only that service.
fn service(log: &Log, what: &'static str, f: impl FnOnce() + Send + 'static) {
    let tlog = log.clone();
    let r = std::thread::Builder::new()
        .name(what.into())
        .spawn(move || {
            if let Err(p) = catch_unwind(AssertUnwindSafe(f)) {
                logf!(tlog, "{what}: panicked: {}", master::panic_text(&*p));
            }
        });
    if let Err(e) = r {
        logf!(log, "{what}: thread failed to start: {e}");
    }
}

/// The game's folder: the mod files to fingerprint (winmm.dll, res1.pak)
/// live next to its exe.
fn game_dir() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other(format!("{} has no folder", exe.display())))
}

/// Deletes what versions with the helper exe left behind: the exe in `local`
/// (%LOCALAPPDATA%\wartales-mp) and its SDR status file, the self-made CA
/// and master certificate in `program_data`\wartales-mp, and that folder once
/// empty. Errors are logged, never fatal.
fn remove_old_install(local: &std::path::Path, program_data: Option<&std::path::Path>, log: &Log) {
    let certs = program_data.map(|p| p.join("wartales-mp"));
    let files = ["wartales-mp.exe", "wartales-mp.exe.new", "sdr.status"]
        .map(|f| local.join(f))
        .into_iter()
        .chain(
            certs
                .iter()
                .flat_map(|d| ["ca.crt", "ca.key", "master.crt", "master.key"].map(|f| d.join(f))),
        );
    for f in files {
        match std::fs::remove_file(&f) {
            Ok(()) => logf!(log, "old install: removed {}", f.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => logf!(
                log,
                "WARNING: old install: cannot remove {}: {e}",
                f.display()
            ),
        }
    }
    // remove_dir only removes an empty folder: anything else in it stays.
    if let Some(d) = certs.filter(|d| std::fs::remove_dir(d).is_ok()) {
        logf!(log, "old install: removed {}", d.display());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one startup sequence, read top to bottom as run.go was"
)]
fn run() {
    let log = applog::open();
    let (port, master_addr, transport) = harness::config();
    logf!(
        log,
        "wartales-mp starting: port {port}, master {master_addr}"
    );
    remove_old_install(
        &applog::dir(),
        // As the old helper's DataDir: the temp dir when ProgramData is unset or empty.
        Some(
            std::env::var_os("ProgramData")
                .filter(|v| !v.is_empty())
                .map_or_else(std::env::temp_dir, PathBuf::from),
        )
        .as_deref(),
        &log,
    );

    // Fingerprint the mod files now, while the game loads them; a join
    // compares them with the host's (modver). An unknown folder only leaves
    // them unchecked.
    let dir = match game_dir() {
        Ok(d) => {
            logf!(log, "modver: game folder {}", d.display());
            Some(d)
        }
        Err(e) => {
            logf!(log, "WARNING: modver: game folder unknown ({e}); mod files will not be compared with other players");
            None
        }
    };
    let r#mod = modver::Session::new(dir, log.clone());

    let mut rl = relay::Server::new(log.clone());
    let mapper = Arc::new(nat::Mapper::new(port, log.clone()));
    let (endpoint, public_addr, listen_host) = if let Some(ep) = harness::endpoint(port, &log) {
        let pa = ep.clone();
        let public_addr: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync> =
            Arc::new(move || pa().map(|e| e.addr));
        (ep, public_addr, "127.0.0.1") // harness: loopback only, no firewall prompt
    } else {
        let (m1, m2) = (mapper.clone(), mapper.clone());
        let endpoint: Arc<dyn Fn() -> anyhow::Result<nat::Endpoint> + Send + Sync> =
            Arc::new(move || m1.endpoint());
        let public_addr: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync> =
            Arc::new(move || m2.addr());
        (endpoint, public_addr, "0.0.0.0") // every interface: guests come from the internet
    };

    // The SDR bridge follows the in-process Steam side for as long as we run,
    // because the game's Steam API comes up after us.
    let bridge = sdrbridge::Bridge::new(Arc::new(crate::steam::Sdr), log.clone());
    let mut key = [0u8; 4];
    crate::rand_bytes(&mut key);

    // The UDP socket for join codes behind NAT: same port number as the
    // relay, its streams end on the relay port over loopback.
    let servers = if listen_host == "127.0.0.1" {
        &[][..] // harness: loopback only
    } else {
        nat::STUN_SERVERS
    };
    let ps = match punch::open(port, &format!("127.0.0.1:{port}"), servers, log.clone()) {
        Ok(ps) => Some(ps),
        Err(e) => {
            logf!(
                log,
                "punch: no UDP socket ({e:#}); join codes need a reachable TCP port"
            );
            None
        }
    };

    let sdr_bridge = bridge.clone();
    let mut opts = master::Options {
        addr: master_addr.clone(),
        relay_port: port,
        host_pw: rl.host_pw.clone(),
        slave_pw: rl.slave_pw.clone(),
        log: log.clone(),
        public_addr: Some(public_addr),
        transport: transport.into(),
        endpoint: Some(endpoint.clone()),
        sdr_status: Some(Arc::new(move || sdr_bridge.status())),
        bridge: Some(bridge.clone()),
        link_key: u32::from_be_bytes(key),
        r#mod: Some(r#mod),
        punch: ps.clone().map(|p| p as Arc<dyn Puncher>),
        ..master::Options::default()
    };
    harness::punch(ps.as_deref(), &mut opts, &log);
    let ms = master::Server::new(opts);
    {
        let ms = ms.clone();
        bridge.set_on_peer(move |c, peer| ms.serve_sdr_link(&c, peer));
    }
    // A connection arriving on the public port from the internet is the only
    // proof the endpoint is reachable; UPnP and STUN are hints.
    {
        let mapper = mapper.clone();
        rl.on_inbound = Some(Box::new(move |a: SocketAddr| {
            mapper.mark_inbound(a.ip());
        }));
    }
    logf!(log, "transport mode {transport} (direct relay once the public endpoint is verified by inbound traffic, else SDR for everything; \
                manual codes use direct; Steam invitations retain SDR)");

    // Set on a fatal error: what still runs stops serving, as the Go helper exited.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (b, stop) = (bridge.clone(), stop.clone());
        service(&log, "sdr-bridge", move || b.run(&stop));
    }
    let (errs, failed) = mpsc::channel::<String>();
    {
        let (rl, ms, errs, stop) = (Arc::new(rl), ms.clone(), errs.clone(), stop.clone());
        let addr = format!("{listen_host}:{port}");
        service(&log, "relay", move || {
            // shortcut: the relay's listener stays bound after a fatal error (relay has no close);
            // its proxy-links are refused then, and the master is gone, so nobody can host.
            let on_link = move |c: crate::conn::ConnRef| {
                if stop.load(Ordering::SeqCst) {
                    c.close();
                } else {
                    ms.serve_link(&c);
                }
            };
            if let Err(e) = rl.listen_and_serve(&addr, on_link) {
                let _ = errs.send(format!("relay: listen {addr}: {e}"));
            }
        });
    }
    {
        let (ms, errs) = (ms.clone(), errs.clone());
        service(&log, "master", move || {
            if let Err(e) = ms.listen_and_serve() {
                let _ = errs.send(format!("master: listen {master_addr}: {e}"));
            }
        });
    }
    drop(errs);

    // Warm the public endpoint up so the first join code is instant.
    if let Some(ps) = ps.clone() {
        let log2 = log.clone();
        service(&log, "punch warm-up", move || match ps.endpoint() {
            Err(e) => logf!(
                log2,
                "punch: {e:#}; join codes carry no UDP address until STUN answers"
            ),
            Ok(r) => logf!(
                log2,
                "punch: UDP address {}, NAT mapping {}",
                r.mapped,
                r.mapping
            ),
        });
    }
    {
        let log2 = log.clone();
        service(&log, "endpoint warm-up", move || match endpoint() {
            Err(e) => logf!(log2, "public endpoint unknown: {e:#}"),
            Ok(ep) if ep.reachable => {
                logf!(log2, "public endpoint: {} (via {})", ep.addr, ep.source);
            }
            Ok(ep) => {
                // Double NAT and friends: the code still works on the LAN, but
                // nobody on the internet can reach it - say so instead of
                // handing out a silently broken code.
                logf!(log2, "WARNING: {}", ep.warning);
                logf!(log2, "LAN-only endpoint: {} (via {})", ep.addr, ep.source);
            }
        });
    }

    // Only a listener failing ends the services; the rest run until the game exits.
    if let Ok(err) = failed.recv() {
        logf!(log, "FATAL: {err}");
        ms.close();
        if let Some(ps) = &ps {
            ps.close();
        }
        stop.store(true, Ordering::SeqCst);
        mapper.close();
        fatal_notice(&err);
    }
}

/// The session mutex: one runtime per Windows session.
fn mutex_name() -> String {
    let base = r"Local\wartales-mp-running".to_string();
    match harness::instance() {
        Some(n) => format!("{base}-{n}"),
        None => base,
    }
}

/// Tells the player that co-op stopped: nobody reads the log on their own.
fn fatal_notice(err: &str) {
    let text = format!(
        "The co-op helper (wartales-mp) stopped:\n\n{err}\n\nCo-op will not work until the game is restarted. Details are in:\n{}",
        applog::dir().join(applog::FILE_NAME).display()
    );
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (t, c) = (wide(&text), wide("Wartales Co-op Fix"));
    // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            t.as_ptr(),
            c.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
}

/// The local co-op harness seam (feature `harness`, marker WMP_TEST_SEAM).
/// With WARTALES_MP_TEST_INSTANCE=N (1..9) two games run side by side: each
/// gets its own session mutex, master on 127.0.0.N:60442, relay port 1425N,
/// transport direct, and join codes advertising loopback (no UPnP/STUN).
#[cfg(feature = "harness")]
mod harness {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Instant;

    use super::{MASTER_ADDR, PORT};
    use crate::{logf, master, nat, punch, Log};

    pub fn instance() -> Option<u8> {
        let v = std::env::var("WARTALES_MP_TEST_INSTANCE").ok()?;
        v.parse::<u8>().ok().filter(|n| (1..=9).contains(n))
    }

    pub fn config() -> (u16, String, &'static str) {
        match instance() {
            Some(n) => {
                crate::dll::shim_log(&format!(
                    "net: WMP_TEST_SEAM instance {n}: master 127.0.0.{n}:60442, port {}, transport direct",
                    PORT + u16::from(n)
                ));
                (
                    PORT + u16::from(n),
                    format!("127.0.0.{n}:60442"),
                    master::MODE_DIRECT,
                )
            }
            None => (PORT, MASTER_ADDR.into(), master::MODE_AUTO),
        }
    }

    type EndpointFn = Arc<dyn Fn() -> anyhow::Result<nat::Endpoint> + Send + Sync>;

    pub fn endpoint(port: u16, log: &Log) -> Option<EndpointFn> {
        let n = instance()?;
        let ep = nat::Endpoint {
            addr: format!("127.0.0.1:{port}"),
            ip: Some([127, 0, 0, 1].into()),
            source: "harness",
            reachable: true,
            verified: true,
            at: Some(Instant::now()),
            ..nat::Endpoint::default()
        };
        logf!(
            log,
            "WMP_TEST_SEAM: harness instance {n}, endpoint {} (no UPnP/STUN)",
            ep.addr
        );
        Some(Arc::new(move || Ok(ep.clone())))
    }

    /// The punch socket's address is its own loopback port (no STUN). With
    /// WARTALES_MP_TEST_PUNCH=1 the guest's direct TCP route is refused, so a
    /// join goes through the UDP punch route and the KCP tunnel end to end.
    pub fn punch(ps: Option<&punch::Socket>, o: &mut master::Options, log: &Log) {
        let (Some(_), Some(ps)) = (instance(), ps) else {
            return;
        };
        let fixed = SocketAddr::from(([127, 0, 0, 1], ps.local_addr().port()));
        ps.set_fixed(fixed);
        logf!(log, "WMP_TEST_SEAM: punch address {fixed} (no STUN)");
        if std::env::var("WARTALES_MP_TEST_PUNCH").as_deref() == Ok("1") {
            logf!(
                log,
                "WMP_TEST_SEAM: direct TCP route disabled, joins go through the punch route"
            );
            o.dial_direct = Some(Arc::new(|_, _| {
                Err(std::io::Error::other(
                    "harness: direct route disabled (WARTALES_MP_TEST_PUNCH)",
                ))
            }));
        }
    }
}

/// The shipped DLL has no harness seam.
#[cfg(not(feature = "harness"))]
mod harness {
    use super::{MASTER_ADDR, PORT};
    use crate::{master, nat, punch, Log};

    pub fn instance() -> Option<u8> {
        None
    }
    pub fn config() -> (u16, String, &'static str) {
        (PORT, MASTER_ADDR.into(), master::MODE_AUTO)
    }
    pub fn endpoint(
        _: u16,
        _: &Log,
    ) -> Option<std::sync::Arc<dyn Fn() -> anyhow::Result<nat::Endpoint> + Send + Sync>> {
        None
    }
    pub fn punch(_: Option<&punch::Socket>, _: &mut master::Options, _: &Log) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_old_install_deletes_only_old_files() {
        let root = std::env::temp_dir().join(format!("wmp-oldinstall-{}", std::process::id()));
        let (local, pd) = (root.join("local"), root.join("pd"));
        let certs = pd.join("wartales-mp");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&certs).unwrap();
        for f in ["wartales-mp.exe", "sdr.status", "shim.log"] {
            std::fs::write(local.join(f), "x").unwrap();
        }
        for f in ["ca.crt", "ca.key", "master.crt", "master.key"] {
            std::fs::write(certs.join(f), "x").unwrap();
        }
        let (log, lines) = Log::capture();
        remove_old_install(&local, Some(&pd), &log);
        assert!(!local.join("wartales-mp.exe").exists() && !local.join("sdr.status").exists());
        assert!(local.join("shim.log").exists(), "current files stay");
        assert!(!certs.exists(), "empty cert folder removed");
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 7, "{got:?}");
        // Second start: nothing left, nothing logged; a non-empty folder stays.
        std::fs::create_dir_all(&certs).unwrap();
        std::fs::write(certs.join("other"), "x").unwrap();
        let (log, lines) = Log::capture();
        remove_old_install(&local, Some(&pd), &log);
        remove_old_install(&local, None, &log);
        assert!(lines.lock().unwrap().is_empty() && certs.join("other").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
