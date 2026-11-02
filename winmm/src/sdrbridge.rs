//! The master-link over SDR: per-peer byte streams (`Conn`) on top of
//! reliable ISteamNetworkingMessages on their own channel, so the
//! master-to-master lobby traffic needs no public endpoint at all.
//!
//! The SDR side is a `Wire`: in the game `crate::steam::Sdr` (direct calls into
//! the in-process transport, channel 100), in tests `switch::Switch`.
//!
//! Inside each SDR message the stream layer adds one byte: 1 = data, 2 = end
//! of stream (the peer closed its side).

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use crate::conn::{Conn, ConnRef};
use crate::{logf, Log};

#[cfg(test)]
pub mod switch;

/// The SDR channel the master-link uses. The game's SteamService only uses
/// channel 0 and the shim serves it channels 0..7, so nothing the game does
/// can reach this one.
pub const CHANNEL: u32 = 100;

const OP_DATA: u8 = 1;
const OP_FIN: u8 = 2;

/// Keeps one SDR message well under Steam's 512 KiB limit.
const MAX_CHUNK: usize = 64 * 1024;
/// A proxy-link never legitimately queues this many frames.
const PEER_QUEUE: usize = 256;

/// Go's ErrNotReady: errors of this kind mean SDR cannot carry streams yet.
pub const NOT_READY: ErrorKind = ErrorKind::NotConnected;

fn not_ready() -> io::Error {
    io::Error::new(NOT_READY, "SDR not ready")
}

/// The transport's verdict on SDR.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// A verdict exists (ok or unavailable); false while pending.
    pub known: bool,
    /// SDR works in the game process.
    pub ok: bool,
    /// Why not, or why still pending.
    pub reason: String,
}

/// What the bridge needs from SDR, on CHANNEL.
pub trait Wire: Send + Sync {
    fn status(&self) -> Status;
    /// One reliable message to peer; Err(text) when Steam refused it.
    fn send(&self, peer: u64, msg: &[u8]) -> Result<(), String>;
    /// What arrived since the last call, with the sender; never blocks.
    fn recv(&self) -> Vec<(u64, Vec<u8>)>;
    /// The desired friends-list lobby invite ("" clears); false if invalid.
    fn set_lobby_invite(&self, invite: &str) -> bool;
}

// ---------------------------------------------------------------- bridge

/// Receives a stream opened by a remote peer (host side), with its SteamID64.
pub type OnPeer = Arc<dyn Fn(ConnRef, u64) + Send + Sync>;

/// Streams to other players over SDR.
pub struct Bridge {
    me: Weak<Bridge>,
    wire: Arc<dyn Wire>,
    log: Log,
    mu: Mutex<State>,
    /// Serializes stream replacement, writes and local closes in wire order.
    wmu: Mutex<()>,
}

struct State {
    peers: HashMap<u64, Arc<Peer>>,
    on_peer: Option<OnPeer>,
}

impl Bridge {
    /// A bridge on wire (in the game: `Arc::new(crate::steam::Sdr)`).
    pub fn new(wire: Arc<dyn Wire>, log: Log) -> Arc<Bridge> {
        Arc::new_cyclic(|me| Bridge {
            me: me.clone(),
            wire,
            log,
            mu: Mutex::new(State {
                peers: HashMap::new(),
                on_peer: None,
            }),
            wmu: Mutex::new(()),
        })
    }

    /// Go's OnPeer: streams opened by remote peers go to f; unset drops them.
    pub fn set_on_peer(&self, f: impl Fn(ConnRef, u64) + Send + Sync + 'static) {
        self.mu.lock().unwrap().on_peer = Some(Arc::new(f));
    }

    /// Publishes the desired native Steam lobby invite (an empty one clears
    /// it); invalid ones are ignored. Never waits for Steam.
    pub fn set_lobby_invite(&self, invite: &str) {
        if invite.len() <= 32 && invite.bytes().all(|c| (33..=126).contains(&c)) {
            self.wire.set_lobby_invite(invite);
        }
    }

    /// The transport's current verdict.
    pub fn status(&self) -> Status {
        self.wire.status()
    }

    /// Whether streams can be opened right now, and if not, why.
    pub fn ready(&self) -> (bool, String) {
        let st = self.status();
        if st.ok {
            (true, String::new())
        } else if st.known {
            (false, format!("SDR unavailable: {}", st.reason))
        } else {
            (false, format!("SDR still initialising: {}", st.reason))
        }
    }

    /// Delivers what arrives to the streams until stop is set (polled at 20
    /// ms: the proxy-link is command traffic, not the game loop). Then every
    /// open stream ends with NOT_READY and the lobby advert is withdrawn.
    pub fn run(&self, stop: &AtomicBool) {
        guard(&self.log, "receiver", || {
            while !stop.load(Ordering::SeqCst) {
                let got = self.wire.recv();
                let idle = got.is_empty();
                for (peer, msg) in got {
                    self.deliver(peer, msg);
                }
                std::thread::sleep(Duration::from_millis(if idle { 20 } else { 1 }));
            }
        });
        let peers = std::mem::take(&mut self.mu.lock().unwrap().peers);
        for p in peers.values() {
            p.remote_closed(End::Err(NOT_READY, not_ready().to_string()));
        }
        self.wire.set_lobby_invite("");
    }

    /// Routes one SDR message to its stream, opening one for a new peer.
    fn deliver(&self, peer: u64, payload: Vec<u8>) {
        if payload.is_empty() {
            return;
        }
        let op = payload[0];
        let mut s = self.mu.lock().unwrap();
        let p = if let Some(p) = s.peers.get(&peer).cloned() {
            drop(s);
            p
        } else {
            let on_peer = if let (Some(f), OP_DATA) = (&s.on_peer, op) {
                f.clone()
            } else {
                drop(s);
                logf!(
                    self.log,
                    "sdr-bridge: {} bytes from unknown peer {peer} dropped",
                    payload.len() - 1
                );
                return;
            };
            let p = self.new_peer(&mut s, peer);
            drop(s);
            logf!(self.log, "sdr-bridge: incoming stream from {peer}");
            let (c, log): (ConnRef, Log) = (p.clone(), self.log.clone());
            std::thread::spawn(move || guard(&log, "OnPeer", || on_peer(c, peer)));
            p
        };
        if op == OP_FIN {
            self.forget(peer, &p);
            p.remote_closed(End::Eof);
            return;
        }
        p.push(payload);
    }

    /// Opens a stream to peer (guest side). Errors of kind NOT_READY name why.
    pub fn dial(&self, peer: u64) -> io::Result<ConnRef> {
        let (ok, why) = self.ready();
        if !ok {
            return Err(io::Error::new(NOT_READY, format!("{}: {why}", not_ready())));
        }
        let _w = self.wmu.lock().unwrap();
        // The remote helper may still hold our previous stream, even when this
        // side has restarted and has no local record of it. End that stream
        // before sending the new hello; otherwise it becomes an ordinary command
        // on the old link and the host rejects it as "Unknown command link/hello".
        // FIN and the subsequent data use the same reliable, ordered channel.
        let old = self.mu.lock().unwrap().peers.remove(&peer);
        if let Some(old) = old {
            old.remote_closed(End::Err(
                ErrorKind::Other,
                "replaced by a new stream".into(),
            ));
        }
        self.send(peer, &[OP_FIN])?;
        let mut s = self.mu.lock().unwrap();
        Ok(self.new_peer(&mut s, peer))
    }

    /// Registers a stream; the caller holds mu (passes its guard).
    fn new_peer(&self, s: &mut State, peer: u64) -> Arc<Peer> {
        let p = Arc::new(Peer {
            b: self.me.upgrade().expect("bridge alive while in use"),
            id: peer,
            st: Mutex::new(PeerState {
                q: VecDeque::new(),
                rest: Vec::new(),
                off: 0,
                end: None,
            }),
            cv: Condvar::new(),
        });
        s.peers.insert(peer, p.clone());
        p
    }

    fn forget(&self, peer: u64, p: &Peer) {
        let mut s = self.mu.lock().unwrap();
        if s.peers
            .get(&peer)
            .is_some_and(|q| std::ptr::eq(&raw const **q, p))
        {
            s.peers.remove(&peer);
        }
    }

    /// Sends one message; the caller holds wmu. A refusal ends peer's stream
    /// with Steam's reason.
    fn send(&self, peer: u64, payload: &[u8]) -> io::Result<()> {
        self.wire.send(peer, payload).map_err(|text| {
            logf!(
                self.log,
                "sdr-bridge: send to {peer} refused by Steam: {text}"
            );
            let p = self.mu.lock().unwrap().peers.get(&peer).cloned();
            if let Some(p) = p {
                p.remote_closed(End::Err(ErrorKind::Other, format!("SDR: {text}")));
            }
            io::Error::other(format!("SDR: {text}"))
        })
    }
}

/// Runs a thread body; a panic is logged instead of killing the game.
fn guard(log: &Log, what: &str, f: impl FnOnce()) {
    if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        let msg = p
            .downcast_ref::<&str>()
            .map(std::string::ToString::to_string)
            .or_else(|| p.downcast_ref::<String>().cloned());
        logf!(
            log,
            "sdr-bridge: {what} panicked: {}",
            msg.unwrap_or_default()
        );
    }
}

// ---------------------------------------------------------------- streams

/// One byte stream to one peer over SDR: reliable and ordered per channel, so
/// the payloads simply concatenate.
struct Peer {
    // shortcut: Bridge->peers->Peer->Bridge cycle; harmless for the one
    // process-lifetime bridge, use Weak if bridges are ever created per session.
    b: Arc<Bridge>,
    id: u64,
    st: Mutex<PeerState>,
    cv: Condvar,
}

struct PeerState {
    q: VecDeque<Vec<u8>>, // whole SDR messages, op byte first

    rest: Vec<u8>,
    off: usize,
    end: Option<End>,
}

#[derive(Clone)]
enum End {
    Eof,
    Err(ErrorKind, String),
}

impl Peer {
    fn push(&self, data: Vec<u8>) {
        let mut st = self.st.lock().unwrap();
        if st.end.is_some() {
            return;
        }
        if st.q.len() < PEER_QUEUE {
            st.q.push_back(data);
            self.cv.notify_all();
            return;
        }
        drop(st);
        // The reader is not keeping up; a proxy-link never legitimately
        // queues 256 frames, so this is a runaway peer.
        logf!(
            self.b.log,
            "sdr-bridge: stream from {} overflowed, closing it",
            self.id
        );
        self.b.forget(self.id, self);
        self.remote_closed(End::Err(ErrorKind::Other, "stream overflow".into()));
    }

    /// Ends the stream (first end wins): reads drain then fail.
    fn remote_closed(&self, end: End) -> bool {
        let mut st = self.st.lock().unwrap();
        if st.end.is_some() {
            return false;
        }
        st.end = Some(end);
        self.cv.notify_all();
        true
    }
}

impl Conn for Peer {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut st = self.st.lock().unwrap();
        loop {
            if st.off < st.rest.len() {
                let n = buf.len().min(st.rest.len() - st.off);
                buf[..n].copy_from_slice(&st.rest[st.off..st.off + n]);
                st.off += n;
                return Ok(n);
            }
            if let Some(d) = st.q.pop_front() {
                (st.rest, st.off) = (d, 1); // skip the op byte
                continue;
            }
            match &st.end {
                Some(End::Eof) => return Ok(0),
                Some(End::Err(k, m)) => return Err(io::Error::new(*k, m.clone())),
                None => st = self.cv.wait(st).unwrap(),
            }
        }
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let _w = self.b.wmu.lock().unwrap();
        if let Some(end) = &self.st.lock().unwrap().end {
            let why = match end {
                End::Eof => "EOF".to_string(),
                End::Err(_, m) => m.clone(),
            };
            return Err(io::Error::new(
                ErrorKind::BrokenPipe,
                format!("sdr stream to {} closed: {why}", self.id),
            ));
        }
        for chunk in data.chunks(MAX_CHUNK) {
            let mut msg = Vec::with_capacity(1 + chunk.len());
            msg.push(OP_DATA);
            msg.extend_from_slice(chunk);
            self.b.send(self.id, &msg)?;
        }
        Ok(())
    }

    // Deadlines are no-ops, as on the Go peerConn.
    fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
    fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    /// Ends the stream from our side and tells the peer.
    fn close(&self) {
        let _w = self.b.wmu.lock().unwrap();
        self.b.forget(self.id, self);
        if self.remote_closed(End::Err(
            ErrorKind::NotConnected,
            "use of closed network connection".into(),
        )) {
            let _ = self.b.send(self.id, &[OP_FIN]);
        }
    }
}

#[cfg(test)]
mod tests;
