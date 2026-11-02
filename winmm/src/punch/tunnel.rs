//! Tunnels (peers) and their streams: one KCP conversation per TCP stream.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kcp::Kcp;

use super::{rand_u32, Socket, State, MAX_GUESTS};
use crate::conn::close_tcp;
use crate::logf;

/// A stream: the peer address and its KCP conversation.
pub(super) type Key = (SocketAddr, u32);

/// Every 20 ticks (10 s) each tunnel gets a punch: it keeps the NAT open and
/// the pong proves the other end alive.
pub(super) const PING_TICKS: u64 = 20;
/// No datagram from a tunnel (or no progress on a stream) this long: dead.
const TIMEOUT: Duration = Duration::from_secs(60);
/// A finished stream answers retransmits of the last FIN this long.
const LINGER: Duration = Duration::from_secs(10);
/// An ended stream's late packets open nothing this long.
const GONE_FOR: Duration = Duration::from_secs(120);
/// One TCP read becomes one KCP message (14 fragments).
const CHUNK: usize = 16 * 1024;
/// Segments queued per stream before its TCP reader waits (two windows).
const SEND_LIMIT: usize = 1024;
/// The first message of a stream: magic + the opener's socket nonce.
const OPEN: &[u8; 8] = b"WMPOPEN!";
const OPEN_LEN: usize = 12;
/// "No such stream": magic + conv, the answer to a segment of an unknown or
/// ended stream.
const RESET: &[u8; 8] = b"WMPRESET";
const RESET_LEN: usize = 12;
const RESET_EVERY: Duration = Duration::from_secs(1);

/// A tunnel to one address, whoever opened it.
pub(super) struct Peer {
    /// the nonce of the other end's socket, once it opened a stream
    pub nonce: Option<u32>,
    /// the last datagram from it
    pub last: Instant,
}

impl Peer {
    pub fn new(nonce: Option<u32>) -> Self {
        Peer {
            nonce,
            last: Instant::now(),
        }
    }
}

pub(super) struct Stream {
    kcp: Kcp<Out>,
    tcp: Option<TcpStream>, // a handle to close the TCP side with
    last_in: Instant,
    ended: Option<Instant>, // our FIN is queued and the TCP side closed
    fin_recv: bool,
}

impl Stream {
    fn new(kcp: Kcp<Out>) -> Self {
        Stream {
            kcp,
            tcp: None,
            last_in: Instant::now(),
            ended: None,
            fin_recv: false,
        }
    }

    /// Ends our side: FIN to the other end, TCP closed (either direction ending
    /// ends both, like Go's splice).
    fn end(&mut self) {
        if self.ended.is_none() {
            self.ended = Some(Instant::now());
            if self.kcp.wait_snd() == 0 {
                self.last_in = Instant::now(); // the FIN's progress is timed from now
            }
            let _ = self.kcp.send(&[]);
            let _ = self.kcp.flush();
        }
        if let Some(c) = self.tcp.take() {
            close_tcp(&c);
        }
    }

    fn finished(&self, now: Instant) -> bool {
        let stuck = now.duration_since(self.last_in) > TIMEOUT;
        match self.ended {
            Some(t) => {
                stuck
                    || (self.fin_recv && self.kcp.wait_snd() == 0 && now.duration_since(t) > LINGER)
            }
            None => stuck && self.kcp.wait_snd() > 0,
        }
    }
}

/// KCP's output: datagrams to the peer on the shared socket.
pub(super) struct Out {
    udp: Arc<UdpSocket>,
    to: SocketAddr,
}

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let _ = self.udp.send_to(b, self.to); // lost like any datagram: KCP resends
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reports whether a datagram is a KCP segment (no crypt, no FEC): conv(4)
/// then a command byte 81..84, 24 bytes of header at least.
pub(super) fn is_kcp(pkt: &[u8]) -> bool {
    pkt.len() >= kcp::KCP_OVERHEAD && (81..=84).contains(&pkt[4])
}

/// Reports whether a KCP segment proves its sender had heard this stream from
/// us: anything but a PUSH, or a PUSH acking our data (una > 0). An opener's
/// first segments (OPEN, maybe reordered behind later ones) are PUSH, una 0.
fn had_state(pkt: &[u8]) -> bool {
    pkt[4] != 81 || pkt[16..20] != [0; 4]
}

/// The conversation a reset datagram names: magic + conv (12 bytes, never
/// STUN, punch or KCP).
pub(super) fn reset_conv(pkt: &[u8]) -> Option<u32> {
    (pkt.len() == RESET_LEN && &pkt[..8] == RESET)
        .then(|| u32::from_le_bytes(pkt[8..].try_into().expect("4 bytes")))
}

impl Socket {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "KCP's clock is a wrapping u32 of ms"
    )]
    fn now_ms(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    /// Go's KCP tuning: fast retransmits, a window large enough for save
    /// transfers, and a datagram that fits any path.
    fn new_kcp(&self, conv: u32, to: SocketAddr) -> Kcp<Out> {
        let mut k = Kcp::new(
            conv,
            Out {
                udp: self.udp.clone(),
                to,
            },
        );
        k.set_nodelay(true, 20, 2, true);
        k.set_wndsize(512, 512);
        k.set_mtu(1200).expect("1200 is a valid MTU");
        let _ = k.update(self.now_ms());
        k
    }

    /// Runs the KCP timers (every 10 ms, from the owner thread).
    pub(super) fn update_streams(&self) {
        let now = self.now_ms();
        for s in self.st().streams.values_mut() {
            let _ = s.kcp.update(now);
        }
    }

    pub(super) fn on_kcp(&self, pkt: &[u8], from: SocketAddr) {
        let key = (from, kcp::get_conv(pkt));
        let mut st = self.st();
        if let Some(p) = st.peers.get_mut(&from) {
            p.last = Instant::now();
        }
        if let Some(s) = st.streams.get_mut(&key) {
            let _ = s.kcp.input(pkt);
            let _ = s.kcp.flush(); // ACK at once
            s.last_in = Instant::now();
            drop(st);
            self.cv.notify_all();
            return;
        }
        if st.closed {
            return;
        }
        if st.gone.contains_key(&key) || had_state(pkt) {
            self.reset_locked(&mut st, key);
        } else {
            self.accept_locked(&mut st, key, pkt);
        }
    }

    /// Tells the other end we have no such stream, so it ends its side now
    /// instead of after TIMEOUT. One reset per stream per second: a flood of
    /// segments gets no flood back (and a reset is smaller than any segment).
    fn reset_locked(&self, st: &mut State, key: Key) {
        let now = Instant::now();
        if st
            .resets
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < RESET_EVERY)
        {
            return;
        }
        st.resets.insert(key, now);
        let mut m = [0u8; RESET_LEN];
        m[..8].copy_from_slice(RESET);
        m[8..].copy_from_slice(&key.1.to_le_bytes());
        let _ = self.udp.send_to(&m, key.0);
    }

    /// Ends a stream the other end reset. Only that exact (address, conv) is
    /// touched: a newer tunnel's streams have other random convs.
    pub(super) fn on_reset(&self, conv: u32, from: SocketAddr) {
        let key = (from, conv);
        let mut st = self.st();
        if st.streams.contains_key(&key) {
            Self::remove_locked(&mut st, key);
            logf!(self.log, "punch: stream {conv:08x} reset by {from}");
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Opens our side of a stream the other end opened. Only an OPEN message
    /// opens one; anything else of an unknown conversation is dropped (its
    /// sender resends). At most MAX_GUESTS tunnels run at once.
    fn accept_locked(&self, st: &mut State, key: Key, pkt: &[u8]) {
        let (from, conv) = key;
        let mut k = self.new_kcp(conv, from);
        let mut m = [0u8; OPEN_LEN];
        if k.input(pkt).is_err()
            || k.peeksize().ok() != Some(OPEN_LEN)
            || k.recv(&mut m).is_err()
            || &m[..8] != OPEN
        {
            return;
        }
        let nonce = u32::from_le_bytes(m[8..].try_into().expect("4 bytes"));
        match st.peers.get(&from).map(|p| p.nonce) {
            None if st.peers.len() >= MAX_GUESTS => return,
            None => logf!(self.log, "punch: tunnel from {from}"),
            Some(Some(old)) if old != nonce => {
                // The other end restarted on the same address: its new
                // streams replace the old tunnel.
                self.drop_peer_locked(st, from);
                logf!(self.log, "punch: tunnel from {from}");
            }
            Some(_) => {}
        }
        st.peers
            .entry(from)
            .or_insert_with(|| Peer::new(None))
            .nonce = Some(nonce);
        st.targets.remove(&from); // the tunnel's own keepalives keep the way open now
        let _ = k.flush(); // ACKs the OPEN
        st.streams.insert(key, Stream::new(k));
        let me = self.me();
        self.spawn(st, move || {
            let c = me
                .relay
                .parse::<SocketAddr>()
                .map_err(anyhow::Error::from)
                .and_then(|a| {
                    TcpStream::connect_timeout(&a, Duration::from_secs(3))
                        .map_err(anyhow::Error::from)
                });
            match &c {
                Ok(c) => {
                    let _ = c.set_nodelay(true); // Go's default
                }
                Err(e) => logf!(me.log, "punch: relay {} for {from}: {e:#}", me.relay),
            }
            me.splice(key, c.ok());
        });
    }

    /// Opens a stream of the tunnel to ha for a forwarder connection.
    pub(super) fn open_stream(&self, ha: SocketAddr, c: TcpStream) {
        let key = {
            let mut st = self.st();
            if st.closed || !st.peers.contains_key(&ha) {
                close_tcp(&c);
                return;
            }
            let mut key = (ha, rand_u32());
            while st.streams.contains_key(&key) || st.gone.contains_key(&key) {
                key.1 = rand_u32();
            }
            let mut k = self.new_kcp(key.1, ha);
            let mut m = [0u8; OPEN_LEN];
            m[..8].copy_from_slice(OPEN);
            m[8..].copy_from_slice(&self.nonce.to_le_bytes());
            let _ = k.send(&m);
            let _ = k.flush();
            st.streams.insert(key, Stream::new(k));
            key
        };
        self.splice(key, Some(c));
    }

    /// Copies both ways between the stream and c (None: the relay was not
    /// reached, so the stream only ends) until either side ends.
    fn splice(&self, key: Key, c: Option<TcpStream>) {
        let c = c.and_then(|c| {
            let r = (|| Some((c.try_clone().ok()?, c.try_clone().ok()?)))();
            if r.is_none() {
                close_tcp(&c);
            }
            r.map(|(keep, w)| (keep, w, c))
        });
        let r = {
            let mut st = self.st();
            let Some(s) = st.streams.get_mut(&key) else {
                if let Some((keep, ..)) = &c {
                    close_tcp(keep);
                }
                return;
            };
            let (keep, w, r) = match c {
                Some((keep, w, r)) => (Some(keep), Some(w), Some(r)),
                None => (None, None, None),
            };
            s.tcp = keep;
            let me = self.me();
            self.spawn(&mut st, move || me.kcp_to_tcp(key, w));
            r
        };
        match r {
            Some(r) => self.tcp_to_kcp(key, r),
            None => self.end(key),
        }
    }

    fn tcp_to_kcp(&self, key: Key, mut c: TcpStream) {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match c.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut st = self.st();
            loop {
                let Some(s) = st.streams.get_mut(&key).filter(|s| s.ended.is_none()) else {
                    return;
                };
                if s.kcp.wait_snd() < SEND_LIMIT {
                    if s.kcp.wait_snd() == 0 {
                        s.last_in = Instant::now(); // progress is timed from when data is pending
                    }
                    let _ = s.kcp.send(&buf[..n]);
                    let _ = s.kcp.flush();
                    break;
                }
                st = self.wait(st, Duration::from_secs(1));
            }
        }
        self.end(key);
    }

    /// Delivers the stream's messages to w until FIN; after a write error (or
    /// without w) it still drains them, so the other end's FIN is seen.
    fn kcp_to_tcp(&self, key: Key, mut w: Option<TcpStream>) {
        let mut buf = Vec::new();
        let mut st = self.st();
        loop {
            let Some(s) = st.streams.get_mut(&key) else {
                return;
            };
            let Ok(n) = s.kcp.peeksize() else {
                st = self.wait(st, Duration::from_secs(1));
                continue;
            };
            buf.resize(n, 0);
            if s.kcp.recv(&mut buf).is_err() {
                continue;
            }
            if n == 0 {
                s.fin_recv = true;
                s.end();
                drop(st);
                self.cv.notify_all();
                return;
            }
            drop(st);
            if w.as_mut().is_some_and(|c| c.write_all(&buf).is_err()) {
                w = None;
                self.end(key);
            }
            st = self.st();
        }
    }

    fn end(&self, key: Key) {
        if let Some(s) = self.st().streams.get_mut(&key) {
            s.end();
        }
        self.cv.notify_all();
    }

    fn remove_locked(st: &mut State, key: Key) {
        if let Some(mut s) = st.streams.remove(&key) {
            if let Some(c) = s.tcp.take() {
                close_tcp(&c);
            }
            st.gone.insert(key, Instant::now());
        }
    }

    /// Closes a tunnel and its streams; a forwarder stays and redials on its
    /// next connection.
    pub(super) fn drop_peer_locked(&self, st: &mut State, a: SocketAddr) {
        st.peers.remove(&a);
        for k in st
            .streams
            .keys()
            .filter(|k| k.0 == a)
            .copied()
            .collect::<Vec<_>>()
        {
            Self::remove_locked(st, k);
        }
        logf!(self.log, "punch: tunnel {a} closed");
    }

    /// Forgets dead tunnels, finished streams and old tombstones (every tick).
    pub(super) fn expire_locked(&self, st: &mut State, now: Instant) {
        let dead: Vec<_> = st
            .peers
            .iter()
            .filter(|(_, p)| now.duration_since(p.last) > TIMEOUT)
            .map(|(a, _)| *a)
            .collect();
        for a in dead {
            self.drop_peer_locked(st, a);
        }
        let done: Vec<_> = st
            .streams
            .iter()
            .filter(|(_, s)| s.finished(now))
            .map(|(k, _)| *k)
            .collect();
        for k in done {
            Self::remove_locked(st, k);
        }
        st.gone.retain(|_, t| now.duration_since(*t) < GONE_FOR);
        st.resets
            .retain(|_, t| now.duration_since(*t) < RESET_EVERY);
    }
}
