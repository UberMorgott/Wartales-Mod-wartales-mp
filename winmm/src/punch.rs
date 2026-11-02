//! Port of internal/punch: join-by-code traffic through NAT without Steam. One
//! UDP socket per helper does STUN (to learn the address peers must aim at),
//! UDP hole punching and KCP. Each TCP stream of the existing protocols
//! (proxy-link, the game's websocket) is one KCP conversation on that socket:
//! the host splices it into its relay port on loopback, the guest takes it from
//! a loopback TCP forwarder whose address is the route the guest joined over.
//! No encryption and no FEC: the link is as open as the TCP port it stands in
//! for.
//!
//! Who sends first: the guest punches toward the host's code address and the
//! host answers every punch, so a host whose NAT lets that through connects at
//! once. Otherwise the guest gets a reply code; the host enters it and punches
//! back, which opens its NAT for the guest's next try. Punching toward one
//! address lasts BUDGET, re-armed by every attempt and every reply code.
//!
//! Design (tunnel.rs): a tunnel is a peer address, kept open by a punch every
//! 10 s and closed after 60 s without a datagram from it. A stream is a KCP
//! conversation (message mode, random conv): its first message is OPEN (magic +
//! the opener's socket nonce, so a peer that restarted on the same address
//! replaces its old streams), an empty message is FIN. One owner thread reads
//! the socket and runs the KCP timers; per stream one thread copies TCP->KCP and
//! one KCP->TCP, all under one mutex. Queues are bounded by the KCP windows.

mod tunnel;

use std::collections::HashMap;
use std::fmt;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};

use crate::conn::close_tcp;
use crate::nat::{self, Mapping};
use crate::{code, logf, Log};

/// How long one side punches toward one address.
pub const BUDGET: Duration = Duration::from_secs(120);
/// How long a guest waits for the host's first answer.
pub const ANSWER_WAIT: Duration = Duration::from_secs(5);
/// Spaces the punch packets.
const PUNCH_EVERY: Duration = Duration::from_millis(500);
/// One STUN binding every 20 s keeps our own mapping (the address in the codes
/// we hand out) from expiring while idle.
const KEEPALIVE_TICKS: u64 = 40;
/// An answer this recent means the path is open.
const HEARD_FRESH: Duration = Duration::from_secs(30);
/// Bounds one STUN round over all servers.
const STUN_WAIT: Duration = Duration::from_millis(3500);
/// Caps the tunnels a host serves at once.
const MAX_GUESTS: usize = 8;

// Punch packets are 8 bytes: never STUN (20+ bytes) nor KCP (24+ bytes).
const PUNCH_MSG: &[u8] = b"WMPPUNCH";
const PONG_MSG: &[u8] = b"WMPPONG!";

/// What STUN said about our socket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StunResult {
    /// our address as the internet sees it
    pub mapped: SocketAddr,
    pub mapping: Mapping,
}

enum Waiter {
    Ask { server: SocketAddr, got: mpsc::SyncSender<SocketAddr> },
    Keep { server: SocketAddr },
}

#[derive(Default)]
struct State {
    fixed: Option<SocketAddr>,
    wait: Option<Duration>, // overrides ANSWER_WAIT (tests)
    closed: bool,
    stun: HashMap<[u8; 12], Waiter>, // pending binding requests by transaction id
    targets: HashMap<SocketAddr, Instant>, // addresses being punched, until
    heard: HashMap<SocketAddr, Instant>, // last punch or pong from an address
    peers: HashMap<SocketAddr, tunnel::Peer>, // tunnels, both roles
    streams: HashMap<tunnel::Key, tunnel::Stream>,
    gone: HashMap<tunnel::Key, Instant>, // ended streams: their late packets open nothing
    fwds: HashMap<SocketAddr, SocketAddr>, // guest role: loopback forwarders by host address
    keep: Option<SocketAddr>,        // STUN server for the keepalive
    keep_tx: Option<[u8; 12]>,       // its pending transaction
    mapped: Option<SocketAddr>,      // last address STUN saw
    threads: Vec<JoinHandle<()>>,    // joined by close
}

/// The helper's UDP socket.
pub struct Socket {
    me: Weak<Socket>,
    udp: Arc<UdpSocket>,
    relay: String, // host role: the relay port on loopback
    servers: Vec<String>,
    log: Log,
    nonce: u32, // tells our streams from those of an earlier socket on the same address
    epoch: Instant,
    st: Mutex<State>,
    cv: Condvar, // any hear, KCP input or stream end
    dial_mu: Mutex<()>, // one tunnel dial at a time (dial and the forwarders' redials)
}

/// Binds UDP port (any free port when it is taken) and starts serving: KCP
/// streams from guests are spliced into relay ("127.0.0.1:<relay port>").
pub fn open(port: u16, relay: &str, servers: &[&str], log: Log) -> anyhow::Result<Arc<Socket>> {
    let udp = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(u) => u,
        Err(e) => {
            logf!(log, "punch: UDP {port} unavailable ({e}); using any free port");
            UdpSocket::bind("0.0.0.0:0")?
        }
    };
    udp.set_read_timeout(Some(Duration::from_millis(10)))?; // the KCP timer step
    let s = Arc::new_cyclic(|me| Socket {
        me: me.clone(),
        udp: Arc::new(udp),
        relay: relay.to_string(),
        servers: servers.iter().map(|s| s.to_string()).collect(),
        log,
        nonce: rand_u32(),
        epoch: Instant::now(),
        st: Mutex::default(),
        cv: Condvar::new(),
        dial_mu: Mutex::new(()),
    });
    let me = s.clone();
    s.spawn(&mut s.st(), move || me.run());
    logf!(s.log, "punch: UDP socket {}", s.local_addr());
    Ok(s)
}

impl Socket {
    fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, st: MutexGuard<'a, State>, d: Duration) -> MutexGuard<'a, State> {
        self.cv.wait_timeout(st, d).unwrap_or_else(|e| e.into_inner()).0
    }

    fn me(&self) -> Arc<Socket> {
        self.me.upgrade().expect("a method runs on a live socket")
    }

    /// Runs f on a thread joined by close; a panic is logged, never kills the
    /// game. After close nothing starts.
    fn spawn(&self, st: &mut State, f: impl FnOnce() + Send + 'static) {
        if st.closed {
            return;
        }
        let log = self.log.clone();
        st.threads.retain(|h| !h.is_finished());
        st.threads.push(std::thread::spawn(move || {
            if let Err(p) = catch_unwind(AssertUnwindSafe(f)) {
                let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
                logf!(log, "punch: thread panicked: {}", msg.unwrap_or_default());
            }
        }));
    }

    /// Returned by endpoint instead of asking STUN (the loopback harness).
    pub fn set_fixed(&self, a: SocketAddr) {
        self.st().fixed = Some(a);
    }

    /// The socket's local address.
    pub fn local_addr(&self) -> SocketAddr {
        self.udp.local_addr().expect("a bound socket has an address")
    }

    /// Stops everything and joins every thread; tunnels close with it. The
    /// port is free once the last Arc is dropped.
    pub fn close(&self) {
        let (fwds, threads) = {
            let mut st = self.st();
            if st.closed {
                return;
            }
            st.closed = true;
            for a in st.peers.keys().copied().collect::<Vec<_>>() {
                self.drop_peer_locked(&mut st, a);
            }
            st.stun.clear(); // wakes the STUN askers
            (std::mem::take(&mut st.fwds), std::mem::take(&mut st.threads))
        };
        self.cv.notify_all();
        for a in fwds.values() {
            let _ = TcpStream::connect_timeout(a, Duration::from_secs(1)); // wakes its accept
        }
        for h in threads {
            let _ = h.join();
        }
    }

    /// Reports whether a STUN server has answered: codes can carry our UDP address.
    pub fn ready(&self) -> bool {
        let st = self.st();
        st.mapped.is_some() || st.fixed.is_some()
    }

    /// The owner thread, the only reader of the socket: STUN answers and punch
    /// packets are handled here, KCP goes to its stream; it also runs the timers.
    fn run(&self) {
        let mut buf = [0u8; 2048];
        let mut next_tick = Instant::now() + PUNCH_EVERY;
        let mut n = 0u64;
        let mut next_update = Instant::now();
        while !self.st().closed {
            // An error is a timeout or e.g. an ICMP unreachable reported on a later read.
            if let Ok((len, from)) = self.udp.recv_from(&mut buf) {
                let pkt = &buf[..len];
                if nat::is_stun(pkt) {
                    self.on_stun(pkt, from);
                } else if pkt == PUNCH_MSG {
                    self.hear(from);
                    self.send(PONG_MSG, from);
                } else if pkt == PONG_MSG {
                    self.hear(from);
                } else if tunnel::is_kcp(pkt) {
                    self.on_kcp(pkt, from);
                }
            }
            if Instant::now() >= next_update {
                next_update = Instant::now() + Duration::from_millis(10);
                self.update_streams();
            }
            if Instant::now() >= next_tick {
                next_tick += PUNCH_EVERY;
                n += 1;
                self.tick(n);
            }
        }
    }

    fn send(&self, b: &[u8], to: SocketAddr) {
        let _ = self.udp.send_to(b, to); // UDP: a lost packet is the next tick's job
    }

    fn hear(&self, from: SocketAddr) {
        let mut st = self.st();
        if st.heard.insert(from, Instant::now()).is_none() {
            logf!(self.log, "punch: {from} answers");
        }
        if let Some(p) = st.peers.get_mut(&from) {
            p.last = Instant::now();
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Sends the punch packets, the tunnel keepalives and the STUN keepalive.
    fn tick(&self, n: u64) {
        let now = Instant::now();
        let mut to = Vec::new();
        let keep = {
            let mut st = self.st();
            st.targets.retain(|a, until| {
                if now > *until {
                    logf!(self.log, "punch: stopped punching {a} ({} spent)", go_dur(BUDGET));
                    return false;
                }
                to.push(*a);
                true
            });
            st.heard.retain(|_, at| now.duration_since(*at) <= HEARD_FRESH);
            if n % tunnel::PING_TICKS == 0 {
                to.extend(st.peers.keys());
            }
            self.expire_locked(&mut st, now);
            st.keep
        };
        self.cv.notify_all();
        for a in to {
            self.send(PUNCH_MSG, a);
        }
        if let Some(k) = keep.filter(|_| n % KEEPALIVE_TICKS == 0) {
            self.keepalive(k);
        }
    }

    /// Refreshes our mapping and notices when it moves.
    fn keepalive(&self, server: SocketAddr) {
        let req = nat::binding_request();
        let tx = tx_id(&req);
        let mut st = self.st();
        if let Some(old) = st.keep_tx.replace(tx) {
            st.stun.remove(&old);
        }
        st.stun.insert(tx, Waiter::Keep { server });
        drop(st);
        self.send(&req, server);
    }

    fn on_stun(&self, pkt: &[u8], from: SocketAddr) {
        let tx = tx_id(pkt);
        let mut st = self.st();
        let keep = match st.stun.get(&tx) {
            Some(Waiter::Ask { server, .. }) | Some(Waiter::Keep { server }) if *server != from => return,
            Some(Waiter::Ask { got, .. }) => {
                if let Ok(m) = nat::parse_binding(pkt, &tx) {
                    let _ = got.try_send(m); // a duplicate answer is dropped
                }
                return;
            }
            Some(Waiter::Keep { .. }) => true,
            None => false,
        };
        let Ok(m) = nat::parse_binding(pkt, &tx) else { return };
        if keep {
            if let Some(old) = st.mapped.filter(|old| *old != m) {
                logf!(self.log, "punch: our address moved {old} -> {m}; codes handed out before carry the old one");
            }
            st.mapped = Some(m);
            st.stun.remove(&tx);
        }
    }

    /// Asks every STUN server for our address over this socket, with retries,
    /// and stops at two answers (enough to compare) or after STUN_WAIT.
    pub fn endpoint(&self) -> anyhow::Result<StunResult> {
        if let Some(f) = self.st().fixed {
            return Ok(StunResult { mapped: f, mapping: Mapping::Stable });
        }
        let deadline = Instant::now() + STUN_WAIT;
        let (tx, rx) = mpsc::channel();
        for srv in &self.servers {
            let (me, srv, tx, log) = (self.me.clone(), srv.clone(), tx.clone(), self.log.clone());
            // Not joined: it holds no Arc, and close wakes its wait.
            std::thread::spawn(move || match catch_unwind(AssertUnwindSafe(|| ask(&me, &srv, deadline))) {
                Ok(Some(a)) => drop(tx.send(a)),
                Ok(None) => {}
                Err(_) => logf!(log, "punch: STUN ask {srv} panicked"),
            });
        }
        drop(tx);
        let mut mapped = HashMap::new();
        let mut first = None;
        while mapped.len() < 2 {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok((server, m)) = rx.recv_timeout(left) else { break };
            first.get_or_insert((server, m));
            mapped.insert(server.to_string(), m);
        }
        let mapping = nat::classify(&mapped);
        let Some((server, m)) = first else { bail!("no STUN server answered") };
        let mut st = self.st();
        st.mapped = Some(m);
        st.keep = Some(server);
        Ok(StunResult { mapped: m, mapping })
    }

    /// Punches toward a guest's address for BUDGET (re-armed if running): the
    /// host side of a reply code.
    pub fn toward(&self, addr: &str) -> anyhow::Result<()> {
        let a = resolve4(addr)?;
        self.arm(a);
        logf!(self.log, "punch: punching toward {a} for {} (reply code)", go_dur(BUDGET));
        Ok(())
    }

    fn arm(&self, a: SocketAddr) {
        self.st().targets.insert(a, Instant::now() + BUDGET);
        self.send(PUNCH_MSG, a);
    }

    /// Reaches the host's punch address and returns a loopback TCP address that
    /// leads to the host's relay port: the route to join over. A live tunnel to
    /// the same address is reused, whoever opened it. Without an answer within
    /// ANSWER_WAIT it fails with a NoAnswerError and keeps punching for BUDGET,
    /// so the host's reply-code punches find the way open.
    ///
    /// The loopback address stays the same for that host for the socket's life:
    /// a tunnel that dies is redialed by the next connection to it, so the
    /// game's saved server id keeps working when it reconnects.
    pub fn dial(&self, host: &str) -> anyhow::Result<String> {
        let ha = resolve4(host)?;
        self.tunnel(ha)?;
        self.forward(ha)
    }

    /// Makes sure a live tunnel to ha exists, punching for a new one when not.
    fn tunnel(&self, ha: SocketAddr) -> anyhow::Result<()> {
        let _one = self.dial_mu.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut st = self.st();
            if let Some(p) = st.peers.get(&ha) {
                if p.last.elapsed() < HEARD_FRESH {
                    return Ok(());
                }
                self.drop_peer_locked(&mut st, ha); // dead but not noticed yet: redial
            }
        }
        self.arm(ha);
        let (tx, rx) = mpsc::channel();
        let me = self.me();
        self.spawn(&mut self.st(), move || {
            let _ = tx.send(me.endpoint().ok());
        });
        let wait = self.st().wait.unwrap_or(ANSWER_WAIT);
        if !self.wait_heard(ha, wait) {
            let r = rx.recv().ok().flatten();
            let reply = r
                .and_then(|r| code::encode_reply(code::Endpoint::new(r.mapped.ip(), r.mapped.port())).ok())
                .unwrap_or_default();
            logf!(
                self.log,
                "punch: no answer from {ha} within {}; our mapping {} ({})",
                go_dur(wait),
                r.map_or("<nil>".to_string(), |r| r.mapped.to_string()),
                r.map_or(Mapping::None, |r| r.mapping)
            );
            return Err(NoAnswerError { host: ha.to_string(), self_: r, reply }.into());
        }
        let mut st = self.st();
        if st.closed {
            bail!("use of closed network connection");
        }
        st.peers.entry(ha).or_insert_with(|| tunnel::Peer::new(None)); // or the one an OPEN made meanwhile
        st.targets.remove(&ha); // the tunnel's own keepalives keep the way open now
        logf!(self.log, "punch: tunnel to {ha} open");
        Ok(())
    }

    /// Waits until key answered recently, or for d.
    fn wait_heard(&self, key: SocketAddr, d: Duration) -> bool {
        let end = Instant::now() + d;
        let mut st = self.st();
        loop {
            if st.heard.get(&key).is_some_and(|t| t.elapsed() < HEARD_FRESH) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if st.closed || left.is_zero() {
                return false;
            }
            st = self.wait(st, left);
        }
    }

    /// Returns the loopback listener for host ha, opening it on first use:
    /// every connection to it is a new stream of the live tunnel, redialed first
    /// when the tunnel is gone. A failed redial closes that connection only;
    /// the game sees a closed connect and retries.
    fn forward(&self, ha: SocketAddr) -> anyhow::Result<String> {
        let mut st = self.st();
        if st.closed {
            bail!("use of closed network connection");
        }
        if let Some(a) = st.fwds.get(&ha) {
            return Ok(a.to_string());
        }
        let ln = TcpListener::bind("127.0.0.1:0")?;
        let addr = ln.local_addr()?;
        st.fwds.insert(ha, addr);
        let me = self.me();
        self.spawn(&mut st, move || {
            for c in ln.incoming() {
                let Ok(c) = c else { return };
                let me2 = me.clone();
                me.spawn(&mut me.st(), move || me2.stream(c, ha)); // refused once closed
                if me.st().closed {
                    return;
                }
            }
        });
        logf!(self.log, "punch: joining over {addr}");
        Ok(addr.to_string())
    }

    /// Splices one forwarder connection into the tunnel to ha.
    fn stream(&self, c: TcpStream, ha: SocketAddr) {
        if let Err(e) = self.tunnel(ha) {
            logf!(self.log, "punch: redial {ha} failed: {e:#}");
            close_tcp(&c);
            return;
        }
        self.open_stream(ha, c);
    }
}

/// Sends one binding request to server, retransmitting it (same transaction)
/// after 0.5 s and 1 s more: lossy networks drop single tries.
fn ask(me: &Weak<Socket>, server: &str, deadline: Instant) -> Option<(SocketAddr, SocketAddr)> {
    let ra = resolve4(server).ok()?;
    let req = nat::binding_request();
    let tx = tx_id(&req);
    let (got, rx) = mpsc::sync_channel(1);
    {
        let s = me.upgrade()?;
        let mut st = s.st();
        if st.closed {
            return None;
        }
        st.stun.insert(tx, Waiter::Ask { server: ra, got });
    }
    let mut res = None;
    for wait in [500, 1000, 2000].map(Duration::from_millis) {
        match me.upgrade() {
            Some(s) => s.send(&req, ra),
            None => break,
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait.min(left)) {
            Ok(m) => {
                res = Some((ra, m));
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => {}
            Err(_) => break, // the deadline passed, or close dropped the waiter
        }
    }
    if let Some(s) = me.upgrade() {
        s.st().stun.remove(&tx);
    }
    res
}

/// A guest's failed punch. Its text is for the player.
#[derive(Debug)]
pub struct NoAnswerError {
    pub host: String,
    /// our own STUN result; None if STUN failed
    pub self_: Option<StunResult>,
    /// the reply code for the host, "" without a mapped address
    pub reply: String,
}

impl fmt::Display for NoAnswerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.reply.is_empty() {
            return write!(
                f,
                "No answer from the host at {}, and no STUN server answered this PC (UDP blocked?). \
                 Use a Steam invitation, or the host forwards port {} (TCP and UDP).",
                self.host,
                code::DEFAULT_PORT
            );
        }
        write!(
            f,
            "No answer from the host at {} yet. Send the host this reply code: {}. \
             The host enters it in Join by code, then join again within 2 minutes.",
            self.host, self.reply
        )?;
        if self.self_.is_some_and(|r| r.mapping == Mapping::Symmetric) {
            f.write_str(
                " Your network changes its port for every destination (symmetric NAT), so this may still fail: \
                 a Steam invitation works on any network.",
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for NoAnswerError {}

fn tx_id(pkt: &[u8]) -> [u8; 12] {
    pkt[8..20].try_into().expect("a STUN message has 20+ bytes")
}

fn resolve4(addr: &str) -> anyhow::Result<SocketAddr> {
    addr.to_socket_addrs()?.find(SocketAddr::is_ipv4).ok_or_else(|| anyhow!("no IPv4 address for {addr}"))
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    crate::rand_bytes(&mut b);
    u32::from_le_bytes(b)
}

/// Go's Duration.String for the values logged here (300ms, 5s, 2m0s).
fn go_dur(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        0..=999 => format!("{ms}ms"),
        1000..=59_999 => format!("{}s", ms as f64 / 1e3),
        _ => format!("{}m{}s", ms / 60_000, (ms % 60_000) as f64 / 1e3),
    }
}

#[cfg(test)]
mod tests;
