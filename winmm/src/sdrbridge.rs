//! Port of internal/sdrbridge: the helper's end of the shim's SDR bridge, a
//! loopback TCP connection into the game process through which bytes are sent
//! to, and received from, other players over ISteamNetworkingMessages (Steam
//! Datagram Relay) on the bridge's own channel. On top of it, per-peer byte
//! streams (`Conn`) carry the proxy-link, so the master-to-master lobby
//! traffic needs no public endpoint at all.
//!
//! The shim advertises the bridge in %LOCALAPPDATA%\wartales-mp\sdr.status:
//!
//! ```text
//! ok bridge=127.0.0.1:<port> token=<32 hex>
//! pending <why>          the Steam API is still coming up
//! unavailable <why>      SDR cannot work in this game process
//! ```
//!
//! Wire format on the socket, both directions (bridge.c):
//!
//! ```text
//! [type:u8][peer SteamID64:u64 LE][len:u32 LE][payload]
//! 0 AUTH  helper -> shim  the token; must be the first frame
//! 1 SEND  helper -> shim  payload to peer, reliable, on the bridge channel
//! 2 RECV  shim -> helper  payload from peer
//! 3 ERR   shim -> helper  text; a SEND to peer that Steam refused
//! 4 LOBBY helper -> shim  the desired native Steam lobby invite ("" = none)
//! ```
//!
//! Inside each SDR message the stream layer adds one byte: 1 = data, 2 = end
//! of stream (the peer closed its side).

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::conn::{Conn, ConnRef, Io};
use crate::{logf, Log};

#[cfg(test)]
pub mod switch;

/// The SDR channel the bridge uses. The game's SteamService only uses channel
/// 0 and the shim serves it channels 0..7, so nothing the game does can reach
/// this one.
pub const CHANNEL: u32 = 100;

const FR_AUTH: u8 = 0;
const FR_SEND: u8 = 1;
const FR_RECV: u8 = 2;
const FR_ERR: u8 = 3;
const FR_LOBBY: u8 = 4;

const OP_DATA: u8 = 1;
const OP_FIN: u8 = 2;

/// Keeps one SDR message well under Steam's 512 KiB limit.
const MAX_CHUNK: usize = 64 * 1024;
const FRAME_HEAD: usize = 13;
const MAX_FRAME: u32 = 512 * 1024;
/// A proxy-link never legitimately queues this many frames.
const PEER_QUEUE: usize = 256;

/// Go's ErrNotReady: errors of this kind mean the shim's bridge is not connected.
pub const NOT_READY: ErrorKind = ErrorKind::NotConnected;

fn not_ready() -> io::Error {
    io::Error::new(NOT_READY, "SDR bridge not connected")
}

// ---------------------------------------------------------------- status

/// The shim's verdict on the SDR transport.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// A verdict exists (ok or unavailable); false while pending or absent.
    pub known: bool,
    /// SDR works in the game process.
    pub ok: bool,
    /// Why not, or why still pending.
    pub reason: String,
    /// "127.0.0.1:port" when the shim listens for us.
    pub bridge: String,
    /// What the shim expects first.
    pub token: String,
}

/// Parses the status file the shim writes; absent = unknown.
pub fn read_status(path: &std::path::Path) -> Status {
    match std::fs::read(path) {
        Ok(b) => parse_status(&String::from_utf8_lossy(&b)),
        Err(e) => Status { reason: format!("shim has not reported yet ({e})"), ..Status::default() },
    }
}

/// Parses the file's one line.
pub fn parse_status(text: &str) -> Status {
    let text = text.trim();
    if text.is_empty() {
        return Status { reason: "empty status".into(), ..Status::default() };
    }
    let (verdict, rest) = text.split_once(' ').unwrap_or((text, ""));
    match verdict {
        "ok" => {
            let mut st = Status { known: true, ok: true, ..Status::default() };
            for f in rest.split_whitespace() {
                let (k, v) = f.split_once('=').unwrap_or((f, ""));
                match k {
                    "bridge" if v != "none" => st.bridge = v.into(),
                    "token" => st.token = v.into(),
                    _ => {}
                }
            }
            if st.bridge.is_empty() {
                st.reason = "the shim could not open its bridge listener".into();
            }
            st
        }
        "unavailable" => Status { known: true, reason: rest.into(), ..Status::default() },
        "pending" => Status { reason: rest.into(), ..Status::default() },
        _ => Status { known: true, reason: format!("unrecognised status {}", text.replace('\u{FFFD}', "?")), ..Status::default() },
    }
}

// ---------------------------------------------------------------- bridge

/// Receives a stream opened by a remote peer (host side), with its SteamID64.
pub type OnPeer = Arc<dyn Fn(ConnRef, u64) + Send + Sync>;

/// The helper's connection to the shim's SDR bridge.
pub struct Bridge {
    me: Weak<Bridge>,
    status_path: PathBuf,
    log: Log,
    mu: Mutex<State>,
    /// Serializes stream replacement, writes and local closes in wire order.
    wmu: Mutex<()>,
    lobby_wake: Mutex<bool>,
    lobby_cv: Condvar,
}

struct State {
    conn: Option<ConnRef>, // to the shim, None while disconnected
    status: Status,
    peers: HashMap<u64, Arc<Peer>>,
    on_peer: Option<OnPeer>,
    lobby_invite: String,
    lobby_known: bool,
}

impl Bridge {
    /// Prepares a bridge that will follow status_path.
    pub fn new(status_path: impl Into<PathBuf>, log: Log) -> Arc<Bridge> {
        let status_path = status_path.into();
        Arc::new_cyclic(|me| Bridge {
            me: me.clone(),
            status_path,
            log,
            mu: Mutex::new(State {
                conn: None,
                status: Status::default(),
                peers: HashMap::new(),
                on_peer: None,
                lobby_invite: String::new(),
                lobby_known: false,
            }),
            wmu: Mutex::new(()),
            lobby_wake: Mutex::new(false),
            lobby_cv: Condvar::new(),
        })
    }

    /// Go's OnPeer: streams opened by remote peers go to f; unset drops them.
    pub fn set_on_peer(&self, f: impl Fn(ConnRef, u64) + Send + Sync + 'static) {
        self.mu.lock().unwrap().on_peer = Some(Arc::new(f));
    }

    /// Records the latest desired native Steam lobby without waiting for Steam
    /// readiness or network I/O. An empty invite clears the lobby.
    pub fn set_lobby_invite(&self, invite: &str) {
        if invite.len() > 32 || invite.bytes().any(|c| !(33..=126).contains(&c)) {
            return;
        }
        {
            let mut s = self.mu.lock().unwrap();
            if s.lobby_known && s.lobby_invite == invite {
                return;
            }
            s.lobby_invite = invite.into();
            s.lobby_known = true;
        }
        self.wake_lobby();
    }

    fn wake_lobby(&self) {
        *self.lobby_wake.lock().unwrap() = true;
        self.lobby_cv.notify_one();
    }

    /// On stop closes the shim connection (Go's context.AfterFunc), which ends
    /// the session's read loop and fails writes blocked on it.
    fn close_on_stop(&self, stop: &AtomicBool) {
        loop {
            let s = self.mu.lock().unwrap();
            if stop.load(Ordering::SeqCst) {
                if let Some(c) = &s.conn {
                    c.close();
                }
                return;
            }
            drop(s);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn publish_lobbies(&self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            let woke = {
                let w = self.lobby_wake.lock().unwrap();
                let (mut w, _) = self.lobby_cv.wait_timeout_while(w, Duration::from_millis(50), |w| !*w).unwrap();
                std::mem::replace(&mut *w, false)
            };
            if woke {
                let _w = self.wmu.lock().unwrap();
                let (c, invite, known) = {
                    let s = self.mu.lock().unwrap();
                    (s.conn.clone(), s.lobby_invite.clone(), s.lobby_known)
                };
                if let (Some(c), true) = (c, known) {
                    if write_frame(&*c, FR_LOBBY, 0, invite.as_bytes()).is_err() {
                        c.close(); // session reconnect will replay the desired state
                    }
                }
            }
        }
    }

    /// The shim's latest verdict, re-read from disk while disconnected.
    pub fn status(&self) -> Status {
        let mut s = self.mu.lock().unwrap();
        if s.conn.is_none() {
            s.status = read_status(&self.status_path);
        }
        s.status.clone()
    }

    /// Whether streams can be opened right now, and if not, why.
    pub fn ready(&self) -> (bool, String) {
        let st = self.status();
        let connected = self.mu.lock().unwrap().conn.is_some();
        if connected {
            (true, String::new())
        } else if st.known && !st.ok {
            (false, format!("SDR unavailable: {}", st.reason))
        } else if st.ok {
            (false, format!("SDR bridge at {} not connected yet", st.bridge))
        } else {
            (false, format!("SDR still initialising: {}", st.reason))
        }
    }

    /// Follows the status file, keeps a connection to the shim while it
    /// advertises one, and reconnects after a drop. Returns once stop is set.
    pub fn run(&self, stop: &AtomicBool) {
        std::thread::scope(|sc| {
            sc.spawn(|| guard(&self.log, "lobby publisher", || self.publish_lobbies(stop)));
            sc.spawn(|| guard(&self.log, "stop watcher", || self.close_on_stop(stop)));
            let mut logged = String::new();
            while !stop.load(Ordering::SeqCst) {
                let st = self.status();
                if !st.ok || st.bridge.is_empty() || st.token.is_empty() {
                    let line = if st.known && !st.ok {
                        format!("SDR UNAVAILABLE in the game process: {}", st.reason)
                    } else {
                        format!("waiting: {}", st.reason)
                    };
                    if line != logged {
                        logf!(self.log, "sdr-bridge: {line}");
                        logged = line;
                    }
                    sleep(stop, Duration::from_millis(500));
                    continue;
                }
                if let Err(e) = self.session(stop, st) {
                    logf!(self.log, "sdr-bridge: {e}");
                }
                logged.clear();
                sleep(stop, Duration::from_secs(1));
            }
        });
    }

    /// Runs one connection to the shim.
    fn session(&self, stop: &AtomicBool, st: Status) -> Result<(), String> {
        let reach = |e: &dyn std::fmt::Display| format!("cannot reach the shim's bridge at {}: {e}", st.bridge);
        let addr: SocketAddr = st.bridge.parse().map_err(|e| reach(&e))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| reach(&e))?;
        let c: ConnRef = Arc::new(tcp);
        if let Err(e) = write_frame(&*c, FR_AUTH, 0, st.token.as_bytes()) {
            c.close();
            return Err(e.to_string());
        }
        {
            let mut s = self.mu.lock().unwrap();
            s.conn = Some(c.clone());
            s.status = st.clone();
            if stop.load(Ordering::SeqCst) {
                c.close(); // stop raced the connect; close_on_stop may have already left
            }
        }
        self.wake_lobby();
        logf!(self.log, "sdr-bridge: connected to the shim at {}, relaying on SDR channel {CHANNEL}", st.bridge);

        let err = self.read_loop(&c);

        let peers = {
            let mut s = self.mu.lock().unwrap();
            s.conn = None;
            std::mem::take(&mut s.peers)
        };
        c.close();
        for p in peers.values() {
            p.remote_closed(End::Err(NOT_READY, not_ready().to_string()));
        }
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(format!("connection to the shim ended: {err} ({} stream(s) dropped)", peers.len()))
    }

    fn read_loop(&self, c: &ConnRef) -> io::Error {
        let mut r = Io(c.clone());
        let mut head = [0u8; FRAME_HEAD];
        loop {
            if let Err(e) = r.read_exact(&mut head) {
                return e;
            }
            let typ = head[0];
            let peer = u64::from_le_bytes(head[1..9].try_into().unwrap());
            let n = u32::from_le_bytes(head[9..13].try_into().unwrap());
            if n > MAX_FRAME {
                return io::Error::other(format!("frame of {n} bytes from the shim"));
            }
            let mut payload = vec![0u8; n as usize];
            if let Err(e) = r.read_exact(&mut payload) {
                return e;
            }
            match typ {
                FR_RECV => self.deliver(peer, payload),
                FR_ERR => {
                    let text = String::from_utf8_lossy(&payload);
                    logf!(self.log, "sdr-bridge: send to {peer} refused by Steam: {text}");
                    let p = self.mu.lock().unwrap().peers.get(&peer).cloned();
                    if let Some(p) = p {
                        p.remote_closed(End::Err(ErrorKind::Other, format!("SDR: {text}")));
                    }
                }
                _ => logf!(self.log, "sdr-bridge: unexpected frame type {typ} from the shim, ignored"),
            }
        }
    }

    /// Routes one SDR message to its stream, opening one for a new peer.
    fn deliver(&self, peer: u64, payload: Vec<u8>) {
        if payload.is_empty() {
            return;
        }
        let op = payload[0];
        let mut s = self.mu.lock().unwrap();
        let p = match s.peers.get(&peer).cloned() {
            Some(p) => {
                drop(s);
                p
            }
            None => {
                let on_peer = match (&s.on_peer, op) {
                    (Some(f), OP_DATA) => f.clone(),
                    _ => {
                        drop(s);
                        logf!(self.log, "sdr-bridge: {} bytes from unknown peer {peer} dropped", payload.len() - 1);
                        return;
                    }
                };
                let p = self.new_peer(&mut s, peer);
                drop(s);
                logf!(self.log, "sdr-bridge: incoming stream from {peer}");
                let (c, log): (ConnRef, Log) = (p.clone(), self.log.clone());
                std::thread::spawn(move || guard(&log, "OnPeer", || on_peer(c, peer)));
                p
            }
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
        // helper has restarted and has no local record of it. End that stream
        // before sending the new hello; otherwise it becomes an ordinary command
        // on the old link and the host rejects it as "Unknown command link/hello".
        // FIN and the subsequent data use the same reliable, ordered channel.
        let old = self.mu.lock().unwrap().peers.remove(&peer);
        if let Some(old) = old {
            old.remote_closed(End::Err(ErrorKind::Other, "replaced by a new stream".into()));
        }
        self.send(peer, &[OP_FIN])?;
        let mut s = self.mu.lock().unwrap();
        Ok(self.new_peer(&mut s, peer))
    }

    /// Registers a stream; the caller holds mu (passes its guard).
    fn new_peer(&self, s: &mut State, peer: u64) -> Arc<Peer> {
        let p = Arc::new(Peer {
            b: self.me.upgrade().expect("bridge alive while in use"),
            peer,
            st: Mutex::new(PeerState { q: VecDeque::new(), rest: Vec::new(), off: 0, end: None }),
            cv: Condvar::new(),
        });
        s.peers.insert(peer, p.clone());
        p
    }

    fn forget(&self, peer: u64, p: &Peer) {
        let mut s = self.mu.lock().unwrap();
        if s.peers.get(&peer).is_some_and(|q| std::ptr::eq(&**q, p)) {
            s.peers.remove(&peer);
        }
    }

    /// Writes one SEND frame to the shim; the caller holds wmu.
    fn send(&self, peer: u64, payload: &[u8]) -> io::Result<()> {
        let c = self.mu.lock().unwrap().conn.clone().ok_or_else(not_ready)?;
        write_frame(&*c, FR_SEND, peer, payload)
    }
}

fn write_frame(w: &dyn Conn, typ: u8, peer: u64, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(FRAME_HEAD + payload.len());
    buf.push(typ);
    buf.extend_from_slice(&peer.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // bounded by MAX_FRAME at every call site
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

fn sleep(stop: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs a thread body; a panic is logged instead of killing the game.
fn guard(log: &Log, what: &str, f: impl FnOnce()) {
    if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
        logf!(log, "sdr-bridge: {what} panicked: {}", msg.unwrap_or_default());
    }
}

// ---------------------------------------------------------------- streams

/// One byte stream to one peer over SDR: reliable and ordered per channel, so
/// the payloads simply concatenate.
struct Peer {
    // shortcut: Bridge->peers->Peer->Bridge cycle; harmless for the one
    // process-lifetime bridge, use Weak if bridges are ever created per session.
    b: Arc<Bridge>,
    peer: u64,
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
        logf!(self.b.log, "sdr-bridge: stream from {} overflowed, closing it", self.peer);
        self.b.forget(self.peer, self);
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
            return Err(io::Error::new(ErrorKind::BrokenPipe, format!("sdr stream to {} closed: {why}", self.peer)));
        }
        for chunk in data.chunks(MAX_CHUNK) {
            let mut msg = Vec::with_capacity(1 + chunk.len());
            msg.push(OP_DATA);
            msg.extend_from_slice(chunk);
            self.b.send(self.peer, &msg)?;
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
        self.b.forget(self.peer, self);
        if self.remote_closed(End::Err(ErrorKind::NotConnected, "use of closed network connection".into())) {
            let _ = self.b.send(self.peer, &[OP_FIN]);
        }
    }
}

#[cfg(test)]
mod tests;
