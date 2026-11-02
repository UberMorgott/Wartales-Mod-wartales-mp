//! Port of internal/relay: the server half of mpman's RelayP2P transport.
//!
//! The game's host connects here with an identity starting with "@:" and the
//! host password; every other connection is a guest and uses the slave
//! password. The relay owns the public TCP port and multiplexes it: a
//! connection whose first bytes are "GET " is a websocket (relay traffic),
//! anything else is handed to the proxy-link handler.
//!
//! Wire format, from mpman/net/RelayP2PService.hx (RelayServer): every relay ->
//! host frame starts with H_SIZE bytes [type:u8][cid:u16], and every host ->
//! relay frame starts with [cid:u16]. Client (slave) frames carry no header at
//! all: the relay adds one on the way to the host and strips it on the way
//! back. haxe.io.Bytes.getUInt16/setUInt16 are little endian on HashLink.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::conn::{Conn, ConnRef};
use crate::{applog, logf, rand_bytes, wsx, Log};

const H_SIZE: usize = 3;
const TYPE_CONNECT: u8 = 1; // [1][cid][identLen:u16][ident bytes]
const TYPE_DISCONNECT: u8 = 2; // [2][cid]
const TYPE_DATA: u8 = 3; // [3][cid][payload]

/// The game's own liveness check (hxbit's 60 s clientTimeout) is patched out,
/// because its timeout path crashes the game (see hlpatch), so the relay checks
/// liveness itself: it pings every connection, the game's
/// WSConnection.processData answers with a pong, and a connection silent for
/// IDLE_TIMEOUT - a hung or frozen game behind a socket the OS keeps open - is
/// dropped, which the host sees as an ordinary disconnect. IDLE_TIMEOUT is twice
/// the game's own bound, so a long blocking load is not taken for a hang.
const PING_INTERVAL: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How many frames pass between two relay traffic summaries.
const FRAME_LOG_EVERY: u64 = 512;

struct Client {
    cid: u16,
    ident: String,
    is_host: bool,
    ws: wsx::Conn,
    // bumped by the host's thread
    frames_out: AtomicU64,
    bytes_out: AtomicU64,
}

#[derive(Default)]
struct State {
    clients: HashMap<u16, Arc<Client>>,
    host_cid: u16,
}

/// The relay. It serves exactly one host at a time, which is all the game ever needs.
pub struct Server {
    pub host_pw: String,
    pub slave_pw: String,
    log: Log,
    /// Sees the source of every connection that spoke: it is how the public
    /// endpoint gets verified as reachable.
    pub on_inbound: Option<Box<dyn Fn(SocketAddr) + Send + Sync>>,
    st: Mutex<State>,
}

fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut b = [0u8; 24];
    rand_bytes(&mut b);
    b.iter()
        .map(|&v| ALPHABET[v as usize % ALPHABET.len()] as char)
        .collect()
}

fn addr(c: &ConnRef) -> String {
    c.peer_addr().map_or("pipe".into(), |a| a.to_string())
}

/// Runs f on a thread of its own; a panic is logged, never kills the game.
fn spawn(log: &Log, f: impl FnOnce() + Send + 'static) {
    let log = log.clone();
    std::thread::spawn(move || {
        if catch_unwind(AssertUnwindSafe(f)).is_err() {
            logf!(log, "relay: connection thread panicked");
        }
    });
}

/// Hands the bytes the dispatch already read back to the next reader (Go's bufferedConn).
struct Prefixed {
    c: ConnRef,
    head: Mutex<Vec<u8>>,
}

impl Conn for Prefixed {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut head = self.head.lock().unwrap();
        if head.is_empty() {
            drop(head);
            return self.c.read(buf);
        }
        let n = buf.len().min(head.len());
        buf[..n].copy_from_slice(&head[..n]);
        head.drain(..n);
        Ok(n)
    }
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        self.c.write_all(buf)
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.c.set_read_timeout(d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.c.set_write_timeout(d)
    }
    fn close(&self) {
        self.c.close();
    }
    fn peer_addr(&self) -> Option<SocketAddr> {
        self.c.peer_addr()
    }
}

impl Server {
    /// A relay with freshly generated passwords.
    pub fn new(log: Log) -> Self {
        Server {
            host_pw: random_password(),
            slave_pw: random_password(),
            log,
            on_inbound: None,
            st: Mutex::default(),
        }
    }

    /// Owns the public port. Connections that do not look like an HTTP
    /// upgrade are passed to on_link (the proxy-link protocol).
    pub fn listen_and_serve(
        self: &Arc<Self>,
        addr: &str,
        on_link: impl Fn(ConnRef) + Send + Sync + 'static,
    ) -> io::Result<()> {
        let ln = TcpListener::bind(addr)?;
        logf!(self.log, "relay listening on {addr}");
        let on_link: Arc<dyn Fn(ConnRef) + Send + Sync> = Arc::new(on_link);
        loop {
            let (c, _) = ln.accept()?;
            let _ = c.set_nodelay(true); // Go's default: game frames are small and latency-bound
            let (s, on_link) = (self.clone(), on_link.clone());
            spawn(&self.log, move || s.dispatch(Arc::new(c), &*on_link));
        }
    }

    /// Reads the first bytes to tell a websocket from a proxy-link.
    fn dispatch(&self, c: ConnRef, on_link: &dyn Fn(ConnRef)) {
        // A connection that never speaks must not hold a thread forever; the
        // handshake that follows sets its own bounds.
        let until = Instant::now() + wsx::HANDSHAKE_TIMEOUT;
        let mut head = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || c.set_read_timeout(Some(left)).is_err() {
                return c.close();
            }
            match c.read(&mut head[got..]) {
                Ok(0) | Err(_) => return c.close(), // the peer never sent anything; nothing to report to
                Ok(n) => got += n,
            }
        }
        let _ = c.set_read_timeout(None);
        if let (Some(f), Some(a)) = (&self.on_inbound, c.peer_addr()) {
            f(a); // it spoke: the port is reachable from there
        }
        let c: ConnRef = Arc::new(Prefixed {
            c,
            head: Mutex::new(head.to_vec()),
        });
        if &head == b"GET " {
            logf!(self.log, "relay: accepted websocket from {}", addr(&c));
            self.serve(&c);
            return;
        }
        on_link(c);
    }

    /// Runs one relay websocket connection to completion.
    fn serve(&self, c: &ConnRef) {
        let r = catch_unwind(AssertUnwindSafe(|| self.serve_ws(c)));
        c.close(); // connection is finished either way
        if let Err(p) = r {
            std::panic::resume_unwind(p);
        }
    }

    fn serve_ws(&self, c: &ConnRef) {
        let auth = |ident: &str| {
            let st = self.st.lock().unwrap();
            // RelayServer.onClient: the first '@' connection becomes the host.
            if ident.starts_with('@') {
                return (st.host_cid == 0).then(|| self.host_pw.clone());
            }
            // "No host connected, slave connection refused"
            (st.host_cid != 0).then(|| self.slave_pw.clone())
        };
        let ws = match wsx::accept(c.clone(), wsx::HANDSHAKE_TIMEOUT, Some(&auth)) {
            Ok(ws) => ws,
            Err(e) => return logf!(self.log, "relay: handshake from {} refused: {e}", addr(c)),
        };
        // Only an authenticated, fully built client is published, and the role
        // is checked again as it is: two host handshakes may both have passed
        // the check above, and the host may have left while a guest's was running.
        let is_host = ws.ident.starts_with('@');
        let me = {
            let mut st = self.st.lock().unwrap();
            if is_host == (st.host_cid != 0) {
                drop(st);
                return logf!(
                    self.log,
                    "relay: {} lost its role during the handshake (ident {:?}), dropped",
                    addr(c),
                    ws.ident
                );
            }
            let cid = gen_cid(&st.clients);
            let me = Arc::new(Client {
                cid,
                ident: ws.ident.clone(),
                is_host,
                ws,
                frames_out: AtomicU64::new(0),
                bytes_out: AtomicU64::new(0),
            });
            st.clients.insert(cid, me.clone());
            if is_host {
                st.host_cid = cid;
            }
            me
        };

        let _registered = Registered(self, me.clone()); // dropped even on a panic

        let role = if is_host { "host" } else { "client" };
        let _pings = me.ws.keep_alive(PING_INTERVAL, IDLE_TIMEOUT);
        logf!(
            self.log,
            "relay: {role} connected (cid {}, ident {:?}, peer {}, headers {})",
            me.cid,
            me.ident,
            addr(c),
            applog::trunc(&applog::headers(&me.ws.headers))
        );
        if !is_host {
            self.to_host(&pack_connect(me.cid, &me.ident));
        }
        let (frames_in, bytes_in) = self.pump(&me, role);
        logf!(self.log, "relay: {role} cid {} gone after {frames_in} frames in ({bytes_in} B), {} frames out ({} B)",
            me.cid, me.frames_out.load(Ordering::Relaxed), me.bytes_out.load(Ordering::Relaxed));
    }

    /// Forwards me's frames until its connection ends; returns the in counters.
    fn pump(&self, me: &Client, role: &str) -> (u64, u64) {
        let (mut frames_in, mut bytes_in) = (0u64, 0u64);
        loop {
            let (op, payload) = match me.ws.read() {
                Ok(f) => f,
                Err(e) => {
                    logf!(self.log, "relay: {role} cid {} read ended: {e}", me.cid);
                    return (frames_in, bytes_in);
                }
            };
            if op != wsx::OP_BINARY {
                logf!(
                    self.log,
                    "relay: {role} cid {} sent a non-binary frame (opcode {op}), ignored",
                    me.cid
                );
                continue; // the relay carries binary frames only
            }
            if payload.len() < H_SIZE {
                logf!(self.log, "relay: {role} cid {} sent a {} byte frame, shorter than the {H_SIZE} byte header", me.cid, payload.len());
                continue; // RelayServer drops anything shorter than a header
            }
            frames_in += 1;
            bytes_in += payload.len() as u64;
            if frames_in % FRAME_LOG_EVERY == 0 {
                logf!(
                    self.log,
                    "relay: {role} cid {}: {frames_in} frames in ({bytes_in} B)",
                    me.cid
                );
            }
            if !me.is_host {
                self.to_host(&pack_to_host(TYPE_DATA, me.cid, &payload));
                continue;
            }
            let Some((cid, body)) = parse_from_host(&payload) else {
                logf!(
                    self.log,
                    "relay: bad host frame ({} B): relay: frame too short",
                    payload.len()
                );
                continue;
            };
            let Some(to) = self.client(cid) else {
                logf!(
                    self.log,
                    "relay: host addressed unknown client {cid}, dropping {} B",
                    body.len()
                );
                continue;
            };
            if let Err(e) = to.ws.write_binary(body) {
                logf!(self.log, "relay: error with client {cid}: {e}");
                self.close_client(cid);
                continue;
            }
            to.frames_out.fetch_add(1, Ordering::Relaxed);
            to.bytes_out.fetch_add(body.len() as u64, Ordering::Relaxed);
        }
    }

    fn client(&self, cid: u16) -> Option<Arc<Client>> {
        self.st.lock().unwrap().clients.get(&cid).cloned()
    }

    /// Forwards a framed message to the host, if one is connected.
    fn to_host(&self, frame: &[u8]) {
        let host = {
            let st = self.st.lock().unwrap();
            st.clients.get(&st.host_cid).cloned()
        };
        if let Some(host) = host {
            if let Err(e) = host.ws.write_binary(frame) {
                logf!(self.log, "relay: can't send to host: {e}");
            }
        }
    }

    /// Drops one guest and tells the host about it.
    fn close_client(&self, cid: u16) {
        let c = self.st.lock().unwrap().clients.remove(&cid);
        let Some(c) = c.filter(|c| !c.is_host) else {
            return;
        };
        self.to_host(&pack_to_host(TYPE_DISCONNECT, cid, &[]));
        c.ws.close(); // the guest is being dropped
    }

    /// Cleans up when a connection ends. Losing the host closes everyone.
    fn drop_client(&self, c: &Client) {
        if !c.is_host {
            return self.close_client(c.cid);
        }
        logf!(self.log, "relay: host disconnected");
        let others: Vec<_> = {
            let mut st = self.st.lock().unwrap();
            st.host_cid = 0;
            std::mem::take(&mut st.clients)
                .into_iter()
                .filter(|(cid, _)| *cid != c.cid)
                .map(|(_, o)| o)
                .collect()
        };
        for other in others {
            other.ws.close(); // host is gone; every guest is dropped regardless
        }
    }
}

/// Unpublishes a client when its serve ends, Go's deferred drop.
struct Registered<'a>(&'a Server, Arc<Client>);

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.0.drop_client(&self.1);
    }
}

fn gen_cid(clients: &HashMap<u16, Arc<Client>>) -> u16 {
    loop {
        let mut b = [0u8; 2];
        rand_bytes(&mut b);
        let cid = u16::from_le_bytes(b) % 32768 + 1;
        if !clients.contains_key(&cid) {
            return cid;
        }
    }
}

/// A relay -> host frame.
fn pack_to_host(typ: u8, cid: u16, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(H_SIZE + payload.len());
    b.push(typ);
    b.extend_from_slice(&cid.to_le_bytes());
    b.extend_from_slice(payload);
    b
}

/// The SConnect frame: the identity is length prefixed with a u16 (RelayHost
/// reads getUInt16 then getString), cut to what the prefix can describe.
fn pack_connect(cid: u16, ident: &str) -> Vec<u8> {
    let len = u16::try_from(ident.len()).unwrap_or(u16::MAX);
    let ident = &ident.as_bytes()[..usize::from(len)];
    let mut body = len.to_le_bytes().to_vec();
    body.extend_from_slice(ident);
    pack_to_host(TYPE_CONNECT, cid, &body)
}

/// Splits a host -> relay frame into the target cid and the payload forwarded
/// verbatim (RelayHost.sendTo writes the cid as a u16 at offset 0 and blits
/// the payload from offset 2).
fn parse_from_host(b: &[u8]) -> Option<(u16, &[u8])> {
    (b.len() >= 2).then(|| (u16::from_le_bytes([b[0], b[1]]), &b[2..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{pipe, Io};
    use crate::wsx::{base64, hex, sha1};
    use std::io::{BufRead, BufReader, Read};
    use std::thread::JoinHandle;
    use std::time::Instant;

    /// Splits a relay -> host frame, as the game's RelayHost does on receive.
    fn parse_to_host(b: &[u8]) -> Option<(u8, u16, &[u8])> {
        (b.len() >= H_SIZE).then(|| (b[0], u16::from_le_bytes([b[1], b[2]]), &b[H_SIZE..]))
    }

    /// The identity string of a SConnect payload.
    fn parse_connect(p: &[u8]) -> Option<String> {
        let n = u16::from_le_bytes([*p.first()?, *p.get(1)?]) as usize;
        p.get(2..2 + n)
            .map(|s| String::from_utf8_lossy(s).into_owned())
    }

    fn pack_from_host(cid: u16, payload: &[u8]) -> Vec<u8> {
        let mut b = cid.to_le_bytes().to_vec();
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn pack_to_host_round_trip() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let frame = pack_to_host(TYPE_DATA, 0x1234, &payload);
        assert_eq!(frame.len(), H_SIZE + payload.len());
        assert_eq!(
            frame[..H_SIZE],
            [TYPE_DATA, 0x34, 0x12],
            "cid must be little endian on the wire"
        );
        assert_eq!(
            parse_to_host(&frame),
            Some((TYPE_DATA, 0x1234, &payload[..]))
        );
    }

    #[test]
    fn pack_connect_round_trip() {
        let ident = "S0011223344556677:Player One";
        let frame = pack_connect(7, ident);
        let (typ, cid, payload) = parse_to_host(&frame).unwrap();
        assert_eq!((typ, cid), (TYPE_CONNECT, 7));
        assert_eq!(parse_connect(payload).unwrap(), ident);
    }

    #[test]
    fn disconnect_frame() {
        assert_eq!(
            parse_to_host(&pack_to_host(TYPE_DISCONNECT, 40000, &[])),
            Some((TYPE_DISCONNECT, 40000, &[][..]))
        );
    }

    #[test]
    fn from_host_round_trip() {
        assert_eq!(
            parse_from_host(&pack_from_host(513, b"hello")),
            Some((513, &b"hello"[..]))
        );
    }

    #[test]
    fn short_frames_are_rejected() {
        assert!(
            parse_to_host(&[1, 2]).is_none(),
            "parse_to_host accepted a 2 byte frame"
        );
        assert!(
            parse_from_host(&[1]).is_none(),
            "parse_from_host accepted a 1 byte frame"
        );
        assert!(
            parse_connect(&[5, 0, b'a']).is_none(),
            "parse_connect accepted a truncated identity"
        );
    }

    fn quiet() -> Arc<Server> {
        Arc::new(Server::new(Log::discard()))
    }

    /// Opens a relay connection over a pipe the way the game does and reports
    /// whether the relay switched protocols. The relay side runs on its own
    /// thread, returned to be joined.
    fn handshake(
        s: &Arc<Server>,
        ident: &str,
        pass: &str,
    ) -> (ConnRef, BufReader<Io>, bool, JoinHandle<()>) {
        handshake_over(s, ident, pass, |c| c)
    }

    /// handshake with the relay's end of the pipe wrapped.
    fn handshake_over(
        s: &Arc<Server>,
        ident: &str,
        pass: &str,
        wrap: impl FnOnce(ConnRef) -> ConnRef,
    ) -> (ConnRef, BufReader<Io>, bool, JoinHandle<()>) {
        let (srv, cli) = pipe();
        let srv = wrap(srv);
        let done = std::thread::spawn({
            let s = s.clone();
            move || s.serve(&srv)
        });
        let key = b"0123456789abcdef";
        let x_pass = hex(&sha1(format!("{}{pass}", hex(key)).as_bytes()));
        let req = format!(
            "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nX-Ident: {ident}\r\nX-Pass: {x_pass}\r\n\r\n",
            base64(key)
        );
        cli.write_all(req.as_bytes()).unwrap();
        let mut br = BufReader::new(Io(cli.clone()));
        let mut line = String::new();
        if br.read_line(&mut line).is_err() || !line.contains(" 101 ") {
            return (cli, br, false, done);
        }
        loop {
            line.clear();
            match br.read_line(&mut line) {
                Ok(0) | Err(_) => return (cli, br, false, done),
                Ok(_) if line.trim().is_empty() => return (cli, br, true, done),
                Ok(_) => {}
            }
        }
    }

    /// Waits until the relay has published its host: the handshake's 101
    /// reaches the peer before serve registers the connection.
    fn wait_host(s: &Server) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if s.st.lock().unwrap().host_cid != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the host was never published");
    }

    fn discard(mut br: BufReader<Io>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = io::copy(&mut br, &mut io::sink()); // keep the pipe drained
        })
    }

    /// Two hosts racing through the handshake: one of them is the host, the
    /// other is dropped, and nothing is published before it is authenticated.
    #[test]
    fn one_host_out_of_concurrent_handshakes() {
        let s = quiet();
        let tries: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || {
                    let (c, br, ok, done) = handshake(&s, "@host", &s.host_pw.clone());
                    (c, ok.then(|| discard(br)), done)
                })
            })
            .collect();
        let conns: Vec<_> = tries.into_iter().map(|t| t.join().unwrap()).collect();
        wait_host(&s);
        let hosts =
            s.st.lock()
                .unwrap()
                .clients
                .values()
                .filter(|c| c.is_host)
                .count();
        assert_eq!(hosts, 1, "hosts published");
        for (c, drain, done) in conns {
            c.close();
            done.join().unwrap();
            if let Some(d) = drain {
                d.join().unwrap();
            }
        }
    }

    /// A guest with a wrong password is never published, and a guest needs a host.
    #[test]
    fn unauthenticated_guest_is_never_published() {
        let s = quiet();
        let (c, _, ok, done) = handshake(&s, "guest", &s.slave_pw);
        assert!(!ok, "a guest was accepted without a host");
        c.close();
        done.join().unwrap();
        let (h, hbr, ok, hdone) = handshake(&s, "@host", &s.host_pw);
        assert!(ok, "host refused");
        let hdrain = discard(hbr);
        wait_host(&s);
        let (c, _, ok, done) = handshake(&s, "guest", "wrong");
        assert!(!ok, "a guest with a wrong password was accepted");
        c.close();
        done.join().unwrap();
        assert_eq!(
            s.st.lock().unwrap().clients.len(),
            1,
            "want only the host published"
        );
        h.close();
        hdone.join().unwrap();
        hdrain.join().unwrap();
    }

    /// One unmasked binary frame (the relay takes either).
    fn write_frame(c: &ConnRef, payload: &[u8]) {
        let mut f = vec![0x82, 126];
        f.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        f.extend_from_slice(payload);
        c.write_all(&f).unwrap();
    }

    /// One server frame's payload.
    fn read_frame(br: &mut impl Read) -> io::Result<Vec<u8>> {
        let mut h = [0u8; 2];
        br.read_exact(&mut h)?;
        let mut n = (h[1] & 127) as usize;
        if n == 126 {
            let mut b = [0u8; 2];
            br.read_exact(&mut b)?;
            n = u16::from_be_bytes(b) as usize;
        } else if n == 127 {
            let mut b = [0u8; 8];
            br.read_exact(&mut b)?;
            n = usize::try_from(u64::from_be_bytes(b)).unwrap();
        }
        let mut buf = vec![0u8; n];
        br.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// A peer that stops reading after the handshake: every write after the
    /// first (the 101) blocks until the connection is closed.
    struct Stalled(ConnRef);

    impl Conn for Stalled {
        fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
        fn write_all(&self, buf: &[u8]) -> io::Result<()> {
            if buf.starts_with(b"HTTP/1.1 101") {
                return self.0.write_all(buf);
            }
            while self.0.write_all(&[]).is_ok() {
                std::thread::sleep(Duration::from_millis(5)); // stalled until closed
            }
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(d)
        }
        fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(d)
        }
        fn close(&self) {
            self.0.close();
        }
    }

    /// A guest that stops reading must not hold up the host's frames to the
    /// others: the relay's fan-out never waits on one peer.
    #[test]
    fn stalled_guest_does_not_block_others() {
        let s = quiet();
        let (host, mut hbr, ok, hdone) = handshake(&s, "@host", &s.host_pw);
        assert!(ok, "host refused");
        wait_host(&s);
        let (stalled, _, ok, sdone) =
            handshake_over(&s, "stalled", &s.slave_pw, |c| Arc::new(Stalled(c))); // never read again
        assert!(ok, "guest refused");
        let (live, mut lbr, ok, ldone) = handshake(&s, "live", &s.slave_pw);
        assert!(ok, "guest refused");
        let mut cids = HashMap::new();
        while cids.len() < 2 {
            let f = read_frame(&mut hbr).unwrap();
            let (typ, cid, payload) = parse_to_host(&f).unwrap();
            assert_eq!(typ, TYPE_CONNECT, "want a connect");
            cids.insert(parse_connect(payload).unwrap(), cid);
        }
        let hdrain = discard(hbr);

        for _ in 0..4 {
            write_frame(&host, &pack_from_host(cids["stalled"], &[1; 1000]));
        }
        write_frame(&host, &pack_from_host(cids["live"], b"hello"));
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            if let Ok(f) = read_frame(&mut lbr) {
                let _ = tx.send(f);
            }
        });
        let f = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the live guest's frame is stuck behind the stalled guest");
        assert_eq!(f, b"hello");
        reader.join().unwrap();
        for c in [&host, &stalled, &live] {
            c.close();
        }
        for t in [hdone, sdone, ldone, hdrain] {
            t.join().unwrap();
        }
    }

    /// The dispatch hands a non-GET connection, its first bytes intact, to the link handler.
    #[test]
    fn dispatch_hands_links_their_bytes() {
        let s = quiet();
        let (srv, cli) = pipe();
        cli.write_all(b"LINK hello\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        s.dispatch(srv, &|c: ConnRef| {
            let mut line = String::new();
            BufReader::new(Io(c)).read_line(&mut line).unwrap();
            tx.send(line).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), "LINK hello\n");
    }
}
