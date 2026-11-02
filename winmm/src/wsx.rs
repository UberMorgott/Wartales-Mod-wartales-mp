//! Port of internal/wsx: a minimal RFC6455 server, matching mpman's
//! WSConnection (see decomp/mpman/WSConnection.hx.txt).
//!
//! Deliberately hand rolled: the game's handshake is not quite standard. It
//! authenticates with the extra headers X-Ident / X-Pass (and accepts X-Hash in
//! place of Sec-WebSocket-Key), never negotiates a subprotocol and never sends
//! fragmented frames.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::conn::ConnRef;
use crate::sendq;

pub const OP_TEXT: u8 = 1;
pub const OP_BINARY: u8 = 2;
const OP_CLOSE: u8 = 8;
const OP_PING: u8 = 9;
const OP_PONG: u8 = 10;

/// Mirrors WSConnection.MAX_BUFFER_SIZE order of magnitude; guards against a malicious peer.
pub const MAX_FRAME: usize = 64 << 20;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The accept timeout for publicly reachable listeners: it bounds the upgrade
/// request of a peer that has not authenticated yet.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds the upgrade request; the game's is a few hundred bytes.
const MAX_REQUEST: usize = 16 << 10;

/// How far a peer may fall behind before it is dropped: room for a frame of
/// the largest size queued behind another.
const SEND_BUDGET: usize = 2 * (MAX_FRAME + 10);

/// The reader under the BufReader: every read is bounded by `until`, Go's read deadline.
struct Deadline {
    c: ConnRef,
    until: Option<Instant>,
    timed: bool, // a read timeout is set on c
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.until {
            Some(t) => {
                let left = t.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "i/o timeout"));
                }
                self.c.set_read_timeout(Some(left))?;
                self.timed = true;
            }
            None if self.timed => {
                self.c.set_read_timeout(None)?;
                self.timed = false;
            }
            None => {}
        }
        self.c.read(buf)
    }
}

/// An accepted websocket connection.
pub struct Conn {
    pub ident: String,
    pub headers: BTreeMap<String, String>,
    br: Mutex<BufReader<Deadline>>,
    q: sendq::Queue,    // every write goes through it, so none waits on the peer
    idle_ms: AtomicU64, // see keep_alive; 0 = read waits forever
}

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// WSConnection.onAuth: X-Ident -> the password the client must have used, None = refuse.
pub type Auth<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Performs the server side handshake on an accepted TCP (or TLS) connection.
/// `auth` is WSConnection.onAuth: given the X-Ident value it returns the
/// password the client must have used, or None to refuse; Some("") skips the
/// X-Pass check.
///
/// The peer is not authenticated yet, so the request must arrive whole within
/// timeout (on a TLS conn it also covers the lazy TLS handshake) and MAX_REQUEST bytes.
pub fn accept(c: ConnRef, timeout: Duration, auth: Option<&Auth<'_>>) -> io::Result<Conn> {
    let until = Instant::now() + timeout;
    let mut br = BufReader::new(Deadline {
        c: c.clone(),
        until: Some(until),
        timed: false,
    });
    let headers = read_request(&mut br)?;

    let hash = match headers.get("x-hash").filter(|h| !h.is_empty()) {
        Some(h) => h.clone(),
        None => headers
            .get("sec-websocket-key")
            .cloned()
            .unwrap_or_default(),
    };
    if hash.is_empty() {
        return Err(other("websocket: no Sec-WebSocket-Key"));
    }
    let ident = headers.get("x-ident").cloned().unwrap_or_default();

    if let Some(auth) = auth {
        let Some(pass) = auth(&ident) else {
            return Err(other(format!(
                "websocket: auth refused for ident {ident:?}"
            )));
        };
        if !pass.is_empty() && !check_pass(&hash, &pass, headers.get("x-pass").map_or("", |s| s)) {
            return Err(other(format!(
                "websocket: bad password for ident {ident:?}"
            )));
        }
    }

    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        base64(&sha1(format!("{hash}{WS_GUID}").as_bytes()))
    );
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "i/o timeout"));
    }
    c.set_write_timeout(Some(left))?;
    c.write_all(resp.as_bytes())?;
    c.set_write_timeout(None)?;
    br.get_mut().until = None;
    Ok(Conn {
        ident,
        headers,
        br: Mutex::new(br),
        q: sendq::Queue::new(c, SEND_BUDGET),
        idle_ms: AtomicU64::new(0),
    })
}

/// WSConnection.handleRequest: the client sends
/// X-Pass = Sha1.encode(hex(base64decode(hash)) + password), lowercase hex.
/// encode@18770 is haxe.crypto.Sha1.encode (Sha1.hx:27-29: digest + toHex);
/// an md5 here refused every host login with "bad password".
fn check_pass(hash: &str, pass: &str, got: &str) -> bool {
    if got.is_empty() {
        return false;
    }
    let Some(key) = unbase64(hash) else {
        return false;
    };
    got == hex(&sha1(format!("{}{pass}", hex(&key)).as_bytes()))
}

fn read_request(br: &mut impl BufRead) -> io::Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    let mut budget = MAX_REQUEST;
    for i in 0.. {
        let mut line = Vec::new();
        let n = br
            .by_ref()
            .take(budget as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if n > budget {
            return Err(other("websocket: request too large"));
        }
        budget -= n;
        if line.last() != Some(&b'\n') {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF"));
        }
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if i == 0 {
            continue; // request line
        }
        let Some(p) = line.find(':') else { continue };
        headers.insert(
            line[..p].trim().to_lowercase(),
            line[p + 1..].trim().to_string(),
        );
        if headers.len() > 64 {
            return Err(other("websocket: too many headers"));
        }
    }
    Ok(headers)
}

/// Go's io.ReadFull error texts: EOF at a frame boundary, unexpected EOF inside one.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                let msg = if got == 0 { "EOF" } else { "unexpected EOF" };
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, msg));
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Stops the keepalive pings when dropped.
pub struct KeepAlive {
    stop: Option<mpsc::Sender<()>>,
    t: Option<JoinHandle<()>>,
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.stop = None; // disconnects the channel: the ping thread wakes and ends
        if let Some(t) = self.t.take() {
            let _ = t.join();
        }
    }
}

/// One unmasked frame (server frames are never masked), queued on q.
fn write_frame(q: &sendq::Queue, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = [0u8; 10];
    head[0] = 0x80 | opcode;
    let l = payload.len();
    let n = match (u8::try_from(l), u16::try_from(l)) {
        (Ok(b), _) if b < 126 => {
            head[1] = b;
            2
        }
        (_, Ok(w)) => {
            head[1] = 126;
            head[2..4].copy_from_slice(&w.to_be_bytes());
            4
        }
        _ => {
            head[1] = 127;
            head[2..10].copy_from_slice(&(l as u64).to_be_bytes());
            10
        }
    };
    q.send(&[&head[..n], payload])
}

impl Conn {
    /// Pings the peer every interval and makes `read` fail once nothing at all
    /// (data, pong) has arrived for idle: an open socket alone does not prove
    /// the program behind it still runs. Call it before the first read; the
    /// pings stop when the guard is dropped.
    pub fn keep_alive(&self, interval: Duration, idle: Duration) -> KeepAlive {
        self.idle_ms.store(
            u64::try_from(idle.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        let (tx, rx) = mpsc::channel::<()>();
        let q = self.q.clone();
        let t = std::thread::spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| {
                while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(interval) {
                    if write_frame(&q, OP_PING, &[]).is_err() {
                        return;
                    }
                }
            }));
            if r.is_err() {
                crate::applog::append("wsx: keepalive thread panicked");
            }
        });
        KeepAlive {
            stop: Some(tx),
            t: Some(t),
        }
    }

    /// The next data frame (opcode, payload). Control frames are handled internally.
    pub fn read(&self) -> io::Result<(u8, Vec<u8>)> {
        let mut br = self.br.lock().unwrap();
        let br = &mut *br;
        loop {
            let idle = self.idle_ms.load(Ordering::Relaxed);
            if idle > 0 {
                br.get_mut().until = Some(Instant::now() + Duration::from_millis(idle));
            }
            let mut h = [0u8; 2];
            read_full(br, &mut h)?;
            let fin = h[0] & 0x80 != 0;
            let rsv = h[0] >> 4 & 7;
            let op = h[0] & 15;
            let masked = h[1] & 0x80 != 0;
            let mut n = u64::from(h[1] & 127);
            if rsv != 0 {
                return Err(other("websocket: RSV bits set"));
            }
            if !fin {
                return Err(other("websocket: fragmented frames are not supported"));
            }
            if n == 126 {
                let mut b = [0u8; 2];
                read_full(br, &mut b)?;
                n = u64::from(u16::from_be_bytes(b));
            } else if n == 127 {
                let mut b = [0u8; 8];
                read_full(br, &mut b)?;
                n = u64::from_be_bytes(b);
            }
            if n > MAX_FRAME as u64 {
                return Err(other("websocket: frame too large"));
            }
            let mut mask = [0u8; 4];
            if masked {
                read_full(br, &mut mask)?;
            }
            let mut buf =
                vec![0u8; usize::try_from(n).map_err(|_| other("websocket: frame too large"))?];
            read_full(br, &mut buf)?;
            if masked {
                for (i, b) in buf.iter_mut().enumerate() {
                    *b ^= mask[i & 3];
                }
            }
            match op {
                OP_TEXT | OP_BINARY => return Ok((op, buf)),
                OP_PING => write_frame(&self.q, OP_PONG, &buf)?,
                OP_PONG => {}
                OP_CLOSE => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF")),
                _ => return Err(other(format!("websocket: unsupported opcode {op}"))),
            }
        }
    }

    /// Queues one frame and returns without waiting for the peer. An error
    /// means the connection is gone, or was dropped because the peer stopped reading.
    pub fn write(&self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        write_frame(&self.q, opcode, payload)
    }

    /// Drops the frames not sent yet and closes the connection.
    pub fn close(&self) {
        self.q.close();
    }

    /// Sends a text frame.
    pub fn write_text(&self, s: &str) -> io::Result<()> {
        self.write(OP_TEXT, s.as_bytes())
    }

    /// Sends a binary frame.
    pub fn write_binary(&self, b: &[u8]) -> io::Result<()> {
        self.write(OP_BINARY, b)
    }
}

pub(crate) fn sha1(b: &[u8]) -> Vec<u8> {
    crate::log::digest(crate::log::Alg::Sha1, b)
}

/// Lowercase hex of b.
pub(crate) fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}"); // writing to a String cannot fail
            s
        })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64(b: &[u8]) -> String {
    let mut s = String::new();
    for c in b.chunks(3) {
        let v = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            s.push(if i <= c.len() {
                B64[(v >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

/// Go's base64.StdEncoding.DecodeString: padded, no stray characters.
fn unbase64(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for (k, c) in s.chunks(4).enumerate() {
        let last = k == s.len() / 4 - 1;
        let pad = c.iter().rev().take_while(|&&x| x == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut v = 0u32;
        for &x in &c[..4 - pad] {
            v = v << 6 | u32::try_from(B64.iter().position(|&y| y == x)?).ok()?;
        }
        v <<= 6 * pad;
        out.extend_from_slice(&v.to_be_bytes()[1..4 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{pipe, Io};
    use std::sync::Arc;

    /// Runs accept against a peer that wrote req (and then stays open).
    fn accept_req(req: &str) -> io::Result<Conn> {
        let (srv, cli) = pipe();
        cli.write_all(req.as_bytes()).unwrap();
        let r = accept(srv.clone(), HANDSHAKE_TIMEOUT, None);
        srv.close();
        r
    }

    #[test]
    fn base64_matches_go() {
        assert_eq!(base64(b"key"), "a2V5");
        assert_eq!(base64(b"ke"), "a2U=");
        assert_eq!(base64(b"k"), "aw==");
        assert_eq!(unbase64("a2U=").unwrap(), b"ke");
        assert_eq!(unbase64("aw==").unwrap(), b"k");
        assert!(
            unbase64("aw=").is_none() && unbase64("a=b=").is_none() && unbase64("a!==").is_none()
        );
        // RFC6455 section 1.3 example
        assert_eq!(
            base64(&sha1(
                format!("dGhlIHNhbXBsZSBub25jZQ=={WS_GUID}").as_bytes()
            )),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn accept_bounds_the_request() {
        let long = format!(
            "GET / HTTP/1.1\r\nX-Junk: {}\r\n\r\n",
            "a".repeat(MAX_REQUEST)
        );
        let err = accept_req(&long).err().expect("longLine accepted");
        assert!(err.to_string().contains("too large"), "longLine: {err}");
        let endless = format!("GET / HTTP/1.1\r\n{}", "junk\r\n".repeat(MAX_REQUEST));
        let err = accept_req(&endless).err().expect("endlessLines accepted");
        assert!(err.to_string().contains("too large"), "endlessLines: {err}");
    }

    /// The frame header bytes Go writes at each length boundary.
    #[test]
    fn frame_headers_match_go() {
        for (len, head) in [
            (125usize, vec![0x82, 125]),
            (126, vec![0x82, 126, 0, 126]),
            (65535, vec![0x82, 126, 0xff, 0xff]),
            (65536, vec![0x82, 127, 0, 0, 0, 0, 0, 1, 0, 0]),
        ] {
            let (a, b) = pipe();
            let q = sendq::Queue::new(a, SEND_BUDGET);
            write_frame(&q, OP_BINARY, &vec![7; len]).unwrap();
            q.drain();
            let mut got = Vec::new();
            Io(b).read_to_end(&mut got).unwrap();
            assert_eq!(got[..head.len()], head[..], "len {len}");
            assert_eq!(got.len(), head.len() + len);
        }
    }

    #[test]
    fn check_pass_is_the_games() {
        let key = b"0123456789abcdef";
        let x = hex(&sha1(format!("{}pw", hex(key)).as_bytes()));
        assert!(check_pass(&base64(key), "pw", &x));
        assert!(!check_pass(&base64(key), "other", &x));
        assert!(!check_pass(&base64(key), "pw", ""));
    }

    /// A complete handshake over a pipe: the server Conn, the client end and its reader.
    fn upgrade() -> (Arc<Conn>, ConnRef, BufReader<Io>) {
        let (srv, cli) = pipe();
        cli.write_all(b"GET / HTTP/1.1\r\nSec-WebSocket-Key: a2V5\r\n\r\n")
            .unwrap();
        let c = accept(srv, HANDSHAKE_TIMEOUT, None).unwrap();
        let mut br = BufReader::new(Io(cli.clone()));
        loop {
            let mut line = String::new();
            br.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        (Arc::new(c), cli, br)
    }

    /// The keepalive pings a live peer, which answering keeps alive, and gives
    /// up on one that stops answering.
    #[test]
    fn keep_alive() {
        let (c, cli, mut br) = upgrade();
        let pings = c.keep_alive(Duration::from_millis(20), Duration::from_millis(150));
        let answering = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let game = std::thread::spawn({
            let (answering, cli) = (answering.clone(), cli.clone());
            move || loop {
                // the game: answer every ping with a (masked) pong while told to
                let mut h = [0u8; 2];
                if read_full(&mut br, &mut h).is_err() || h[0] & 15 != OP_PING || h[1] & 127 != 0 {
                    return;
                }
                if answering.load(Ordering::Relaxed)
                    && cli.write_all(&[0x80 | OP_PONG, 0x80, 0, 0, 0, 0]).is_err()
                {
                    return;
                }
            }
        });
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn({
            let c = c.clone();
            move || tx.send(c.read().map(|_| ())).unwrap()
        });
        if let Ok(r) = rx.recv_timeout(Duration::from_millis(400)) {
            panic!("a peer answering pings was dropped: {r:?}");
        }
        answering.store(false, Ordering::Relaxed);
        let r = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a peer that stopped answering was kept");
        assert!(r.is_err(), "read returned without an error");
        reader.join().unwrap();
        drop(pings);
        c.close();
        cli.close();
        game.join().unwrap();
    }

    /// A peer that never finishes its request is dropped after HANDSHAKE_TIMEOUT.
    #[test]
    fn accept_times_out() {
        let start = Instant::now();
        assert!(
            accept_req("GET / HTTP/1.1\r\n").is_err(),
            "an unfinished request was accepted"
        );
        let took = start.elapsed();
        assert!(
            took >= HANDSHAKE_TIMEOUT && took <= HANDSHAKE_TIMEOUT + Duration::from_secs(5),
            "gave up after {took:?}"
        );
    }

    /// The deadline follows the timeout passed in: a request that takes 300 ms
    /// is dropped under a 100 ms timeout and accepted under a 5 s one (#5).
    #[test]
    fn accept_uses_given_timeout() {
        let slow = |timeout| {
            let (srv, cli) = pipe();
            let t = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                let _ = cli.write_all(b"GET / HTTP/1.1\r\nSec-WebSocket-Key: a2V5\r\n\r\n");
            });
            let r = accept(srv.clone(), timeout, None).map(|_| ());
            srv.close();
            t.join().unwrap();
            r
        };
        assert!(
            slow(Duration::from_millis(100)).is_err(),
            "a slow request beat a 100 ms timeout"
        );
        slow(Duration::from_secs(5)).expect("a slow request failed under a 5 s timeout");
    }
}
