use super::switch::Switch;
use super::*;
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::thread::JoinHandle;

/// Runs a bridge; dropping it stops Run and joins it (Go's goleak check).
struct Running {
    stop: Arc<AtomicBool>,
    h: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.h.take().unwrap().join().unwrap();
    }
}

fn run(b: &Arc<Bridge>) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let (b, s) = (b.clone(), stop.clone());
    Running { stop, h: Some(std::thread::spawn(move || b.run(&s))) }
}

fn start(sw: &Switch, id: u64) -> (Arc<Bridge>, Running) {
    let b = Bridge::new(sw.add(id), Log::discard());
    let r = run(&b);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (ok, why) = b.ready();
        if ok && sw.connected(id) {
            return (b, r);
        }
        assert!(Instant::now() < deadline, "bridge never became ready: {why}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_line(c: &ConnRef) -> io::Result<String> {
    let mut s = String::new();
    BufReader::new(Io(c.clone())).read_line(&mut s)?;
    Ok(s)
}

#[test]
fn parse_status_cases() {
    let st = parse_status("ok bridge=127.0.0.1:52669 token=50e80f126cce8178c774a2e18fbad573\n");
    assert!(st.known && st.ok && st.bridge == "127.0.0.1:52669" && st.token == "50e80f126cce8178c774a2e18fbad573", "{st:?}");
    let st = parse_status("ok bridge=none");
    assert!(st.ok && st.bridge.is_empty() && !st.reason.is_empty(), "{st:?}");
    let st = parse_status("unavailable steam_api64.dll is not loaded in this process");
    assert!(st.known && !st.ok && st.reason == "steam_api64.dll is not loaded in this process", "{st:?}");
    let st = parse_status("pending Steam API not initialised yet");
    assert!(!st.known && !st.ok && st.reason == "Steam API not initialised yet", "{st:?}");
    let st = read_status(&std::env::temp_dir().join("sdrbridge-missing-status"));
    assert!(!st.known && !st.ok, "{st:?}");
}

/// Two helpers, each behind its own fake shim identity; a stream dialed by one
/// arrives at the other's OnPeer, bytes flow both ways in order, and closing
/// one side ends the other's reads.
#[test]
fn streams_over_the_switch() {
    let sw = Switch::new();
    const HOST: u64 = 76561197960265728 + 111;
    const GUEST: u64 = 76561197960265728 + 222;
    let (hb, _hr) = start(&sw, HOST);
    let (tx, incoming) = mpsc::channel();
    hb.set_on_peer(move |c, peer| {
        let _ = tx.send((c, peer));
    });
    let (gb, _gr) = start(&sw, GUEST);

    let gc = gb.dial(HOST).unwrap();
    gc.write_all(b"hello host\n").unwrap();
    let (hc, peer) = incoming.recv_timeout(Duration::from_secs(5)).expect("the host never saw the stream");
    assert_eq!(peer, GUEST, "OnPeer from the wrong peer");
    let mut hr = BufReader::new(Io(hc.clone()));
    let mut line = String::new();
    hr.read_line(&mut line).unwrap();
    assert_eq!(line, "hello host\n");
    // A big write is chunked into several SDR messages and reassembled.
    let big = "x".repeat(200 * 1024) + "\n";
    let w = {
        let (hc, big) = (hc.clone(), big.clone());
        std::thread::spawn(move || hc.write_all(big.as_bytes()))
    };
    assert_eq!(read_line(&gc).unwrap(), big);
    w.join().unwrap().unwrap();
    // FIN from the guest ends the host's reads with EOF.
    gc.close();
    line.clear();
    assert_eq!(hr.read_line(&mut line).unwrap(), 0, "after the guest closed, host read {line:?}, want EOF");
    // And the guest's own conn is closed for good.
    assert!(gc.write_all(b"x").is_err(), "write on a closed stream succeeded");
}

/// A send to nobody comes back as ERR and ends the stream with the reason.
#[test]
fn dial_unknown_peer_is_reported() {
    let sw = Switch::new();
    let (b, _r) = start(&sw, 76561197960265728 + 1);
    let c = b.dial(76561197960265728 + 999).unwrap();
    // The ERR answering Dial's own FIN may already have ended the stream.
    let err = match c.write_all(b"anyone?") {
        Ok(()) => c.read(&mut [0u8; 8]).unwrap_err(),
        Err(e) => e,
    };
    assert!(err.to_string().contains("EResult 3"), "read err = {err}, want the ERR text");
}

/// Without a usable status nothing can be dialed, and the reason names the
/// shim's verdict.
#[test]
fn not_ready() {
    let sw = Switch::new();
    let b = Bridge::new(sw.unavailable("steam_api64.dll is not loaded in this process"), Log::discard());
    let _r = run(&b);
    let err = b.dial(1).err().expect("dial succeeded");
    assert!(err.kind() == NOT_READY && err.to_string().contains("steam_api64.dll is not loaded"), "dial err = {err}");
    let b2 = Bridge::new(std::env::temp_dir().join("sdrbridge-absent-status"), Log::discard());
    let (ok, why) = b2.ready();
    assert!(!ok && why.contains("initialising"), "ready = {ok}, {why:?}");
}

/// Reconnecting without a delivered close, including a helper restart while
/// the host still holds its old stream.
#[test]
fn redial_resets_remote_stream() {
    let sw = Switch::new();
    const HOST: u64 = 76561197960265728 + 311;
    const GUEST: u64 = 76561197960265728 + 322;
    let (hb, _hr) = start(&sw, HOST);
    let (tx, incoming) = mpsc::channel();
    hb.set_on_peer(move |c, _| {
        let _ = tx.send(c);
    });
    let (gb, _gr) = start(&sw, GUEST);
    let first = gb.dial(HOST).unwrap();
    first.write_all(b"first\n").unwrap();
    let old = incoming.recv_timeout(Duration::from_secs(1)).expect("first stream missing");
    assert_eq!(read_line(&old).unwrap(), "first\n");
    let second = gb.dial(HOST).unwrap();
    second.write_all(b"second\n").unwrap();
    let fresh = incoming.recv_timeout(Duration::from_secs(1)).expect("redial reused the stale remote stream");
    assert_eq!(read_line(&fresh).unwrap(), "second\n");
    assert_eq!(old.read(&mut [0u8; 1]).unwrap(), 0, "old stream read, want EOF");
    // Closing the stale handler must not close the replacement stream.
    old.close();
    fresh.write_all(b"reply\n").unwrap();
    assert_eq!(read_line(&second).unwrap(), "reply\n");
    second.close();
}

#[test]
fn lobby_invite_replays_latest_after_connect_and_reconnect() {
    let sw = Switch::new();
    const HOST: u64 = 76561197960265728 + 455;
    let b = Bridge::new(sw.add(HOST), Log::discard());
    b.set_lobby_invite("OLD");
    b.set_lobby_invite("LATEST");
    let _r = run(&b);
    let expect = |want: &str| {
        let ev = sw.lobby_invites.lock().unwrap().recv_timeout(Duration::from_secs(3)).expect("lobby state was not delivered");
        assert!(ev.peer == HOST && ev.invite == want, "published = {ev:?}, want {want:?}");
    };
    expect("LATEST");
    sw.disconnect(HOST);
    expect("LATEST");
    b.set_lobby_invite("");
    expect("");
    sw.disconnect(HOST);
    expect("");
}

/// Losing the shim ends every open stream with NOT_READY.
#[test]
fn shim_drop_ends_streams() {
    let sw = Switch::new();
    const HOST: u64 = 76561197960265728 + 511;
    const GUEST: u64 = 76561197960265728 + 522;
    let (_hb, _hr) = start(&sw, HOST);
    let (gb, _gr) = start(&sw, GUEST);
    let gc = gb.dial(HOST).unwrap();
    sw.disconnect(GUEST);
    let err = gc.read(&mut [0u8; 1]).unwrap_err();
    assert_eq!(err.kind(), NOT_READY, "{err}");
}

/// A runaway peer overflows its queue: what was queued still drains, then the
/// reads fail.
#[test]
fn overflow_drains_then_fails() {
    let b = Bridge::new("", Log::discard());
    let p = b.new_peer(&mut b.mu.lock().unwrap(), 7);
    for _ in 0..=PEER_QUEUE {
        p.push(vec![OP_DATA, b'x']);
    }
    assert!(!b.mu.lock().unwrap().peers.contains_key(&7), "overflowed stream still registered");
    let mut got = Vec::new();
    let err = Io(p).read_to_end(&mut got).unwrap_err();
    assert_eq!(got.len(), PEER_QUEUE);
    assert!(err.to_string().contains("stream overflow"), "{err}");
}

/// Fake shim socket: a FIN frame blocks until released.
struct ResetWire {
    fin: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
    data_frames: Mutex<usize>,
}

impl Conn for ResetWire {
    fn read(&self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn write_all(&self, p: &[u8]) -> io::Result<()> {
        if p[FRAME_HEAD] == OP_FIN {
            drop(self.fin.lock().unwrap().take());
            let _ = self.release.lock().unwrap().recv();
        } else {
            *self.data_frames.lock().unwrap() += 1;
        }
        Ok(())
    }
    fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
    fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
    fn close(&self) {}
}

#[test]
fn dial_retires_old_stream_before_reset() {
    let (fin_tx, fin_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let wire = Arc::new(ResetWire { fin: Mutex::new(Some(fin_tx)), release: Mutex::new(release_rx), data_frames: Mutex::new(0) });
    let b = Bridge::new("", Log::discard());
    let old = {
        let mut s = b.mu.lock().unwrap();
        s.conn = Some(wire.clone());
        b.new_peer(&mut s, 123)
    };
    let dialed = {
        let b = b.clone();
        std::thread::spawn(move || b.dial(123).map(|_| ()))
    };
    let _ = fin_rx.recv(); // FIN is on the wire
    assert!(old.st.lock().unwrap().end.is_some(), "old stream remains writable while reset is on the wire");
    let written = {
        let old = old.clone();
        std::thread::spawn(move || old.write_all(b"stale"))
    };
    let closed = {
        let old = old.clone();
        std::thread::spawn(move || old.close())
    };
    drop(release_tx);
    dialed.join().unwrap().unwrap();
    assert!(written.join().unwrap().is_err(), "old write succeeded after reset");
    closed.join().unwrap();
    assert_eq!(*wire.data_frames.lock().unwrap(), 0, "stale data followed reset");
}

#[test]
fn set_lobby_invite_does_not_wait_for_wire() {
    let b = Bridge::new("", Log::discard());
    let _w = b.wmu.lock().unwrap();
    let (tx, done) = mpsc::channel();
    let b2 = b.clone();
    std::thread::spawn(move || {
        b2.set_lobby_invite("FIRST");
        b2.set_lobby_invite("LATEST");
        let _ = tx.send(());
    });
    done.recv_timeout(Duration::from_secs(1)).expect("setter blocked on the wire");
    b.set_lobby_invite(&"x".repeat(33));
    b.set_lobby_invite("bad\nframe");
    b.set_lobby_invite("bad frame");
    assert_eq!(b.mu.lock().unwrap().lobby_invite, "LATEST", "invalid payload changed desired invite");
}
