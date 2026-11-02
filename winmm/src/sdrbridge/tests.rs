use super::switch::Switch;
use super::*;
use crate::conn::Io;
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

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
    Running {
        stop,
        h: Some(std::thread::spawn(move || b.run(&s))),
    }
}

fn start(sw: &Switch, id: u64) -> (Arc<Bridge>, Running) {
    let b = Bridge::new(sw.add(id), Log::discard());
    let r = run(&b);
    assert!(b.ready().0);
    (b, r)
}

fn read_line(c: &ConnRef) -> io::Result<String> {
    let mut s = String::new();
    BufReader::new(Io(c.clone())).read_line(&mut s)?;
    Ok(s)
}

/// Two bridges, each on its own identity; a stream dialed by one arrives at
/// the other's OnPeer, bytes flow both ways in order, and closing one side
/// ends the other's reads.
#[test]
fn streams_over_the_switch() {
    const HOST: u64 = 76_561_197_960_265_728 + 111;
    const GUEST: u64 = 76_561_197_960_265_728 + 222;
    let sw = Switch::new();
    let (hb, _hr) = start(&sw, HOST);
    let (tx, incoming) = mpsc::channel();
    hb.set_on_peer(move |c, peer| {
        let _ = tx.send((c, peer));
    });
    let (gb, _gr) = start(&sw, GUEST);

    let gc = gb.dial(HOST).unwrap();
    gc.write_all(b"hello host\n").unwrap();
    let (hc, peer) = incoming
        .recv_timeout(Duration::from_secs(5))
        .expect("the host never saw the stream");
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
    assert_eq!(
        hr.read_line(&mut line).unwrap(),
        0,
        "after the guest closed, host read {line:?}, want EOF"
    );
    // And the guest's own conn is closed for good.
    assert!(
        gc.write_all(b"x").is_err(),
        "write on a closed stream succeeded"
    );
}

/// A send Steam refuses fails with its EResult (the ERR of the old bridge).
#[test]
fn dial_unknown_peer_is_reported() {
    let sw = Switch::new();
    let (b, _r) = start(&sw, 76_561_197_960_265_728 + 1);
    let err = b
        .dial(76_561_197_960_265_728 + 999)
        .err()
        .expect("dial to nobody succeeded");
    assert!(err.to_string().contains("EResult 3"), "dial err = {err}");
}

/// A refused write ends the stream with the reason, for later reads too.
#[test]
fn refused_write_ends_the_stream() {
    const HOST: u64 = 76_561_197_960_265_728 + 611;
    let sw = Switch::new();
    let _host = sw.add(HOST);
    let guest = sw.add(76_561_197_960_265_728 + 622);
    let b = Bridge::new(guest.clone(), Log::discard());
    let c = b.dial(HOST).unwrap();
    guest.set_status(Status::default()); // Steam now refuses every send
    let err = c.write_all(b"x").unwrap_err();
    assert!(
        err.to_string().contains("SDR: SDR not ready"),
        "write err = {err}"
    );
    let err = c.read(&mut [0u8; 1]).unwrap_err();
    assert!(
        err.to_string().contains("SDR: SDR not ready"),
        "read err = {err}"
    );
}

/// Without a ready transport nothing can be dialed, and the reason names its
/// verdict.
#[test]
fn not_ready() {
    let sw = Switch::new();
    let b = Bridge::new(
        sw.unavailable("steam_api64.dll is not loaded in this process"),
        Log::discard(),
    );
    let _r = run(&b);
    let err = b.dial(1).err().expect("dial succeeded");
    assert!(
        err.kind() == NOT_READY && err.to_string().contains("steam_api64.dll is not loaded"),
        "dial err = {err}"
    );
    let ep = sw.pending(5, "Steam API not initialised yet");
    let b2 = Bridge::new(ep.clone(), Log::discard());
    let (ok, why) = b2.ready();
    assert!(
        !ok && why.contains("initialising") && why.contains("not initialised"),
        "ready = {ok}, {why:?}"
    );
    assert!(!b2.status().known);
    ep.set_status(Status {
        known: true,
        ok: true,
        reason: String::new(),
    });
    assert!(b2.ready().0, "pending -> ok not seen");
}

/// Reconnecting without a delivered close, including a restart while the
/// host still holds its old stream.
#[test]
fn redial_resets_remote_stream() {
    const HOST: u64 = 76_561_197_960_265_728 + 311;
    const GUEST: u64 = 76_561_197_960_265_728 + 322;
    let sw = Switch::new();
    let (hb, _hr) = start(&sw, HOST);
    let (tx, incoming) = mpsc::channel();
    hb.set_on_peer(move |c, _| {
        let _ = tx.send(c);
    });
    let (gb, _gr) = start(&sw, GUEST);
    let first = gb.dial(HOST).unwrap();
    first.write_all(b"first\n").unwrap();
    let old = incoming
        .recv_timeout(Duration::from_secs(1))
        .expect("first stream missing");
    assert_eq!(read_line(&old).unwrap(), "first\n");
    let second = gb.dial(HOST).unwrap();
    second.write_all(b"second\n").unwrap();
    let fresh = incoming
        .recv_timeout(Duration::from_secs(1))
        .expect("redial reused the stale remote stream");
    assert_eq!(read_line(&fresh).unwrap(), "second\n");
    assert_eq!(
        old.read(&mut [0u8; 1]).unwrap(),
        0,
        "old stream read, want EOF"
    );
    // Closing the stale handler must not close the replacement stream.
    old.close();
    fresh.write_all(b"reply\n").unwrap();
    assert_eq!(read_line(&second).unwrap(), "reply\n");
    second.close();
}

/// Publications go straight to the transport; invalid ones never do, and a
/// stopped bridge withdraws the advert.
#[test]
fn lobby_invite_published_and_withdrawn() {
    const HOST: u64 = 76_561_197_960_265_728 + 455;
    let sw = Switch::new();
    let b = Bridge::new(sw.add(HOST), Log::discard());
    let next = || {
        sw.lobby_invites
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .expect("lobby state was not delivered")
    };
    b.set_lobby_invite(&"x".repeat(33));
    b.set_lobby_invite("bad\nframe");
    b.set_lobby_invite("bad frame");
    b.set_lobby_invite("LATEST");
    let ev = next();
    assert!(ev.peer == HOST && ev.invite == "LATEST", "{ev:?}");
    let r = run(&b);
    drop(r);
    assert_eq!(next().invite, "", "stop did not withdraw the advert");
}

/// Stopping the bridge ends every open stream with NOT_READY.
#[test]
fn stop_ends_streams() {
    const HOST: u64 = 76_561_197_960_265_728 + 511;
    const GUEST: u64 = 76_561_197_960_265_728 + 522;
    let sw = Switch::new();
    let (_hb, _hr) = start(&sw, HOST);
    let (gb, gr) = start(&sw, GUEST);
    let gc = gb.dial(HOST).unwrap();
    drop(gr);
    let err = gc.read(&mut [0u8; 1]).unwrap_err();
    assert_eq!(err.kind(), NOT_READY, "{err}");
}

/// A runaway peer overflows its queue: what was queued still drains, then the
/// reads fail.
#[test]
fn overflow_drains_then_fails() {
    let sw = Switch::new();
    let b = Bridge::new(sw.add(7), Log::discard());
    let p = b.new_peer(&mut b.mu.lock().unwrap(), 7);
    for _ in 0..=PEER_QUEUE {
        p.push(vec![OP_DATA, b'x']);
    }
    assert!(
        !b.mu.lock().unwrap().peers.contains_key(&7),
        "overflowed stream still registered"
    );
    let mut got = Vec::new();
    let err = Io(p).read_to_end(&mut got).unwrap_err();
    assert_eq!(got.len(), PEER_QUEUE);
    assert!(err.to_string().contains("stream overflow"), "{err}");
}

/// Fake transport: a FIN blocks until released.
struct ResetWire {
    fin: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
    data_frames: Mutex<usize>,
}

impl Wire for ResetWire {
    fn status(&self) -> Status {
        Status {
            known: true,
            ok: true,
            reason: String::new(),
        }
    }
    fn send(&self, _: u64, p: &[u8]) -> Result<(), String> {
        if p[0] == OP_FIN {
            drop(self.fin.lock().unwrap().take());
            let _ = self.release.lock().unwrap().recv();
        } else {
            *self.data_frames.lock().unwrap() += 1;
        }
        Ok(())
    }
    fn recv(&self) -> Vec<(u64, Vec<u8>)> {
        Vec::new()
    }
    fn set_lobby_invite(&self, _: &str) -> bool {
        true
    }
}

#[test]
fn dial_retires_old_stream_before_reset() {
    let (fin_tx, fin_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let wire = Arc::new(ResetWire {
        fin: Mutex::new(Some(fin_tx)),
        release: Mutex::new(release_rx),
        data_frames: Mutex::new(0),
    });
    let b = Bridge::new(wire.clone(), Log::discard());
    let old = b.new_peer(&mut b.mu.lock().unwrap(), 123);
    let dialed = {
        let b = b.clone();
        std::thread::spawn(move || b.dial(123).map(|_| ()))
    };
    let _ = fin_rx.recv(); // FIN is on the wire
    assert!(
        old.st.lock().unwrap().end.is_some(),
        "old stream remains writable while reset is on the wire"
    );
    let written = {
        let old = old.clone();
        std::thread::spawn(move || old.write_all(b"stale"))
    };
    let closed = {
        let old = old.clone();
        std::thread::spawn(move || old.close())
    };
    let t0 = Instant::now();
    drop(release_tx);
    dialed.join().unwrap().unwrap();
    assert!(
        written.join().unwrap().is_err(),
        "old write succeeded after reset"
    );
    closed.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(
        *wire.data_frames.lock().unwrap(),
        0,
        "stale data followed reset"
    );
}
