//! The guest's way to a host: the routes of a join code, tried in order
//! (direct TCP, UDP punch, SDR), and the proxy-link that results.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{anyhow, bail};
use serde_json::value::{to_raw_value, RawValue};
use serde_json::{Map, Value};

use super::lobby::is_lobby_info;
use super::{from_raw, PeerRef, Server};
use crate::applog::trunc;
use crate::conn::ConnRef;
use crate::{code, link, logf, nat, punch, uid};

/// Bounds the guest's attempt to connect to the host's endpoint before it
/// falls back: a refused port fails at once, a silently dropped SYN (the
/// common firewall case) must not keep a player waiting. 3 s is one
/// retransmit past the first SYN on Windows, well inside the game's own 20 s
/// command timeout with the SDR attempt still to come.
pub const DEFAULT_DIRECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Bounds the first command over a freshly connected direct link: the host
/// answered TCP, so anything slower is a wrong host or a dead one.
pub(super) const DIRECT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounds commands over the SDR link, so that a dead host surfaces before the
/// game's own 20 s timeout even after a direct attempt.
const SDR_CALL_TIMEOUT: Duration = Duration::from_secs(8);

/// The helper's UDP socket for join codes behind NAT (punch::Socket): the
/// host's code carries its address, a guest that cannot reach the host's TCP
/// port punches through it, and a host enters a guest's reply code to punch
/// back.
pub trait Puncher: Send + Sync {
    /// Whether our UDP address is known (a code can carry it).
    fn ready(&self) -> bool;
    /// Asks STUN for our UDP address now.
    fn endpoint(&self) -> anyhow::Result<punch::StunResult>;
    /// The loopback address that leads to the host at addr.
    fn dial(&self, addr: &str) -> anyhow::Result<String>;
    /// Punches toward a guest's address (a reply code).
    fn toward(&self, addr: &str) -> anyhow::Result<()>;
}

/// The cascade's attempt at one route: (what, relay, dial, key, probe) ->
/// the host's LobbyInfo, or the failure line.
pub(super) type TryRoute<'a> = dyn FnMut(&str, &str, &dyn Fn() -> io::Result<ConnRef>, u32, Duration) -> Result<Value, String>
    + 'a;

fn connect(addr: &str, timeout: Duration) -> io::Result<ConnRef> {
    let a: SocketAddr = addr
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{addr}: {e}")))?;
    let c = TcpStream::connect_timeout(&a, timeout)?;
    let _ = c.set_nodelay(true);
    Ok(Arc::new(c))
}

impl Server {
    /// Tries the code's routes in order: direct first (bounded by
    /// DEFAULT_DIRECT_TIMEOUT, then one probe command), punch, SDR last. A
    /// route counts only when the host's master actually answered
    /// lobby/resolveShortCode over it; a refused hello, a dead port or a
    /// timeout falls through to the next, and every outcome is logged with its
    /// reason. The guest sees nothing of it but a moment's delay.
    pub(super) fn cascade(
        &self,
        c: &code::Code,
        args: Option<&RawValue>,
        p: &PeerRef,
    ) -> anyhow::Result<Value> {
        let log = &self.opt.log;
        let user = link::User {
            id: p.user_id(),
            name: p.name(),
            steam: p.steam_id(),
            game: p.game_id(),
            key: 0,
            r#mod: self.local_mod(),
        };
        let mut failures: Vec<String> = Vec::new();
        let mut try_route = |what: &str,
                             relay: &str,
                             dial: &dyn Fn() -> io::Result<ConnRef>,
                             key: u32,
                             probe: Duration| {
            let u = link::User {
                key,
                ..user.clone()
            };
            self.probe_route(&u, args, what, relay, dial(), probe)
        };
        if let Some(e) = c.endpoint {
            let addr = e.addr().to_string();
            let key = c.steam.map_or(0, |s| s.key);
            match try_route(
                &format!("direct {addr}"),
                &addr,
                &|| self.dial_direct(&addr),
                key,
                DIRECT_PROBE_TIMEOUT,
            ) {
                Ok(v) => return Ok(v),
                Err(f) => failures.push(f),
            }
            if c.steam.is_some() {
                logf!(
                    log,
                    "master: falling back to SDR after the direct route to {addr} failed"
                );
            }
        }
        if c.punch.is_some() {
            if let Some(v) = self.punch_route(c, &mut try_route, &mut failures)? {
                return Ok(v);
            }
        }
        if let Some(st) = c.steam {
            let target = st.steam_id64();
            let what = format!("SDR to SteamID {target}");
            match &self.opt.bridge {
                None => failures.push(format!("{what}: this helper has no SDR bridge")),
                Some(b) => {
                    let (ready, why) = b.ready();
                    if ready {
                        match try_route(&what, "", &|| b.dial(target), st.key, SDR_CALL_TIMEOUT) {
                            Ok(v) => return Ok(v),
                            Err(f) => failures.push(f),
                        }
                    } else {
                        logf!(log, "master: route {what} deferred: {why}");
                        if failures.is_empty() {
                            bail!("Steam relay not ready yet ({why}); try again in a few seconds");
                        }
                        failures.push(format!("{what}: not ready ({why})"));
                    }
                }
            }
        }
        bail!("Cannot reach the host: {}", failures.join("; "))
    }

    /// One route of the cascade over an open (or failed) connection: the link,
    /// then the probe command. The host's LobbyInfo, or the failure line.
    fn probe_route(
        &self,
        u: &link::User,
        args: Option<&RawValue>,
        what: &str,
        relay: &str,
        conn: io::Result<ConnRef>,
        probe: Duration,
    ) -> Result<Value, String> {
        let log = &self.opt.log;
        let fail = |e: &dyn std::fmt::Display| {
            logf!(log, "master: route {what} failed: {e}");
            format!("{what}: {e}")
        };
        let conn = conn.map_err(|e| fail(&e))?;
        let cl = self
            .attach_link(conn, u, what, relay, probe)
            .map_err(|e| fail(&e))?;
        let r = cl
            .call("lobby/resolveShortCode", args.map(ToOwned::to_owned))
            .map(|r| from_raw(r.as_deref()))
            .and_then(|v| {
                if is_lobby_info(&v) {
                    Ok(v)
                } else {
                    Err(anyhow!("the host does not know this join code"))
                }
            });
        match r {
            Err(e) => {
                // Connected, but not to a master that takes this code: a stale
                // address now owned by someone else, a refused key, a dead
                // host, or one that answered "no such lobby".
                logf!(
                    log,
                    "master: route {what} answered the probe with an error, dropping it: {e:#}"
                );
                self.drop_link(&cl);
                Err(format!("{what}: {e:#}"))
            }
            Ok(v) => {
                logf!(log, "master: route {what} WORKS; joining over it");
                // Resolving a remote lobby makes this helper a guest: do not
                // leave a previous local lobby advertised while the game's join
                // is in flight.
                {
                    let mut l = self.lobbies();
                    l.invite_lobby.clear();
                    self.sync_invite(&mut l);
                }
                // A route that works is final, whatever the mod check says: a
                // mismatch must not fall through to the next route.
                Ok(self.host_answer(Some(&cl), v))
            }
        }
    }

    /// Connects to the host's endpoint within DEFAULT_DIRECT_TIMEOUT
    /// (Options.direct_timeout), through Options.dial_direct when set.
    fn dial_direct(&self, addr: &str) -> io::Result<ConnRef> {
        let timeout = if self.opt.direct_timeout.is_zero() {
            DEFAULT_DIRECT_TIMEOUT
        } else {
            self.opt.direct_timeout
        };
        match &self.opt.dial_direct {
            Some(f) => f(addr, timeout),
            None => connect(addr, timeout),
        }
    }

    /// Runs the guest side of a link over c and makes it the master's uplink.
    /// probe bounds each command over it. relay is the host endpoint a direct
    /// route dialed ("" over SDR): the game's relay address in the host's
    /// lobby messages is rewritten to it (guest_relay_addr).
    pub(super) fn attach_link(
        &self,
        c: ConnRef,
        u: &link::User,
        what: &str,
        relay: &str,
        probe: Duration,
    ) -> io::Result<Arc<link::Client>> {
        let (me, me2) = (self.me.clone(), self.me.clone());
        let (what1, what2, relay) = (what.to_string(), what.to_string(), relay.to_string());
        // on_close must see the client it belongs to, which exists only once
        // dial_conn returned.
        let (attached, wait) = mpsc::channel::<std::sync::Weak<link::Client>>();
        let cl = link::dial_conn(
            c,
            u,
            move |cmd, args| {
                let Some(s) = me.upgrade() else { return };
                let Some(sess) = s.local_session() else {
                    logf!(
                        s.opt.log,
                        "master: {what1} pushed {cmd} {}, but no game is connected; dropped",
                        trunc(args.as_ref().map_or("", |a| a.get()))
                    );
                    return;
                };
                let args = if !relay.is_empty() && cmd == "lobby/chat" {
                    s.guest_relay_addr(args, &relay)
                } else {
                    args
                };
                let null = RawValue::NULL.to_owned();
                super::Peer::push(&*sess, cmd, args.as_deref().unwrap_or(&null));
            },
            move || {
                let Ok(cl) = wait.recv() else { return };
                let Some(s) = me2.upgrade() else { return };
                {
                    let mut st = s.state();
                    if st
                        .client
                        .as_ref()
                        .is_some_and(|c| std::ptr::eq(Arc::as_ptr(c), cl.as_ptr()))
                    {
                        st.client = None;
                    }
                }
                logf!(s.opt.log, "master: {what2} closed");
            },
        )?;
        cl.set_call_timeout(probe);
        self.state().client = Some(cl.clone());
        let _ = attached.send(Arc::downgrade(&cl));
        // Over SDR this only means the hello left; the first reply (or the
        // probe's timeout) is what proves the host was reached.
        logf!(
            self.opt.log,
            "master: {what}: link open, hello sent; waiting for the host's first reply"
        );
        Ok(cl)
    }

    /// Closes a link that turned out not to lead to the host.
    pub(super) fn drop_link(&self, cl: &Arc<link::Client>) {
        {
            let mut st = self.state();
            if st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, cl)) {
                st.client = None;
            }
        }
        cl.close();
    }

    /// Our endpoint as a code route, if we have an IPv4 one.
    pub(super) fn endpoint_route(&self) -> anyhow::Result<code::Endpoint> {
        let addr = self
            .public_addr()
            .map_err(|e| anyhow!("no public address: {e:#}"))?;
        let a: SocketAddr = addr.parse()?;
        if !a.is_ipv4() {
            bail!("public address {addr:?} is not IPv4");
        }
        Ok(code::Endpoint::new(a.ip(), a.port()))
    }

    /// Whether inbound from the internet has been seen.
    pub(super) fn endpoint_verified(&self) -> (bool, nat::Endpoint) {
        match self.opt.endpoint.as_ref().map(|f| f()) {
            Some(Ok(ep)) => (ep.verified, ep),
            _ => (false, nat::Endpoint::default()),
        }
    }

    /// Our SDR route, if the game gave us a Steam id and the bridge is up;
    /// otherwise the reason.
    pub(super) fn steam_route(&self, p: &PeerRef) -> (Option<code::Steam>, String) {
        let Some(id64) = uid::steam_id64(&p.steam_id()) else {
            return (None, "the game reported no Steam id".into());
        };
        let (account_id, ok) = code::account_id(id64);
        if !ok {
            return (None, format!("{id64} is not a player account"));
        }
        let Some(b) = &self.opt.bridge else {
            return (None, "this helper has no SDR bridge".into());
        };
        let (ready, why) = b.ready();
        if !ready {
            return (None, why);
        }
        (
            Some(code::Steam {
                flags: 0,
                account_id,
                key: self.opt.link_key,
            }),
            String::new(),
        )
    }

    /// The host's code with its UDP address, when STUN knows it. The TCP
    /// route is the one the endpoint resolver found (the default port on the
    /// UDP IP when it found none); on another IP there is no punch code.
    pub(super) fn punch_code(&self, tcp: Option<&code::Endpoint>) -> Option<String> {
        let pu = self.opt.punch.as_ref()?;
        let r = match pu.endpoint() {
            Ok(r) => r,
            Err(e) => {
                logf!(
                    self.opt.log,
                    "master: join code without a punch address: {e:#}"
                );
                return None;
            }
        };
        let udp_ip = r.mapped.ip().to_canonical();
        let mut ep = code::Endpoint::new(udp_ip, code::DEFAULT_PORT);
        if let Some(t) = tcp {
            if t.ip.to_canonical() != udp_ip {
                // Two WAN addresses: the code has room for one, keep the TCP one.
                logf!(
                    self.opt.log,
                    "master: join code without a punch address: UDP leaves by {udp_ip}, TCP is reached at {}",
                    t.ip
                );
                return None;
            }
            ep.port = t.port;
        }
        let short = code::encode_punch(ep, r.mapped.port()).ok()?;
        let note = if r.mapping == nat::Mapping::Symmetric {
            "; guests will likely need a reply code or a Steam invitation"
        } else {
            ""
        };
        logf!(
            self.opt.log,
            "master: punch address {} (NAT mapping {}{note})",
            r.mapped,
            r.mapping
        );
        Some(short)
    }

    /// The cascade's route for a code with a punch address: the host either
    /// answers within punch::ANSWER_WAIT and the link goes through the
    /// tunnel, or the player gets the reply code to send. relay for the link
    /// is the tunnel's loopback address, so the game's relay connection takes
    /// the same way (guest_relay_addr).
    pub(super) fn punch_route(
        &self,
        c: &code::Code,
        try_route: &mut TryRoute<'_>,
        failures: &mut Vec<String>,
    ) -> anyhow::Result<Option<Value>> {
        let (Some(pu), Some(target)) = (&self.opt.punch, c.punch) else {
            return Ok(None);
        };
        let what = format!("punch {}", target.addr());
        let fwd = match pu.dial(&target.addr().to_string()) {
            Ok(f) => f,
            Err(e) => {
                logf!(self.opt.log, "master: route {what} failed: {e:#}");
                bail!("{e:#}");
            }
        };
        // DIRECT_PROBE_TIMEOUT, not SDR_CALL_TIMEOUT: TCP 3 s + its probe 5 s +
        // the answer wait 5 s + this 5 s stays inside the game's 20 s timeout.
        let dial = || connect(&fwd, DEFAULT_DIRECT_TIMEOUT);
        match try_route(&what, &fwd, &dial, 0, DIRECT_PROBE_TIMEOUT) {
            Ok(v) => Ok(Some(v)),
            Err(f) => {
                failures.push(f);
                Ok(None)
            }
        }
    }

    /// Handles a reply code the host entered in its join box; the answer is
    /// always an error text the game shows.
    pub(super) fn punch_reply(&self, r: code::Endpoint) -> anyhow::Error {
        let Some(pu) = &self.opt.punch else {
            return anyhow!("This is a reply code, but this helper has no UDP socket for it");
        };
        if let Err(e) = pu.toward(&r.addr().to_string()) {
            return anyhow!("Reply code: {e:#}");
        }
        anyhow!(
            "Reply code accepted: this PC now opens the way to the player at {} for 2 minutes. \
             Host the game and ask them to join again.",
            r.addr()
        )
    }

    /// The host's game asks its own master for the relay and gets
    /// "R127.0.0.1:<port>:<pw>": it reaches its relay on loopback. The game
    /// builds the guests' WServer id from the same host and port
    /// (Lobby.setupPlatform callback @26421) and sends it to them in a lobby
    /// message (StartClients; a drop-in joiner gets game.options.multi.serverID
    /// the same way), which our masters carry as lobby/chat. On another PC that
    /// address is the guest's own machine, so a guest's master that joined over
    /// a direct route puts the address it dialed (route, "ip:port") in its place
    /// before the message reaches its game; a link over SDR is left alone.
    pub(super) fn guest_relay_addr(
        &self,
        args: Option<Box<RawValue>>,
        route: &str,
    ) -> Option<Box<RawValue>> {
        let Ok(a) = route.parse::<SocketAddr>() else {
            return args;
        };
        let raw = args.as_ref()?;
        let Ok(mut m) = serde_json::from_str::<Map<String, Value>>(raw.get()) else {
            return args;
        };
        let Some(Value::String(msg)) = m.get("msg") else {
            return args;
        };
        let (out, n) = rewrite_wserver(msg, &a.ip().to_string(), &a.port().to_string());
        if n == 0 {
            return args;
        }
        m.insert("msg".into(), Value::String(out));
        let Ok(r) = to_raw_value(&m) else {
            return args;
        };
        logf!(
            self.opt.log,
            "master: host's relay address 127.0.0.1 -> {route} in a lobby message ({n} id(s)); \
             the game connects over the route it joined by"
        );
        Some(r)
    }
}

/// Matches `y(\d+):W127\.0\.0\.1%3A(\d+)%3A` at i: (end of the length
/// digits, end of the match).
fn wserver_at(m: &[u8], i: usize) -> Option<(usize, usize)> {
    const HOST: &[u8] = b"W127.0.0.1%3A";
    let digits = |from: usize| from + m[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    if m.get(i) != Some(&b'y') {
        return None;
    }
    let len_end = digits(i + 1);
    if len_end == i + 1 || m.get(len_end) != Some(&b':') || !m[len_end + 1..].starts_with(HOST) {
        return None;
    }
    let port = len_end + 1 + HOST.len();
    let port_end = digits(port);
    if port_end == port || !m[port_end..].starts_with(b"%3A") {
        return None;
    }
    Some((len_end, port_end + 3))
}

/// Replaces host and port of each "y<len>:W127.0.0.1%3A<port>%3A..." string in
/// msg, fixing its length prefix; the rest of the id (the password) is kept as
/// encoded. In a haxe-serialized string ':' only appears encoded (%3A) inside
/// the content, so the pattern cannot start inside another string; nor inside
/// a Bytes value, whose haxe base64 alphabet (A-Z a-z 0-9 % :) has no '.'.
pub(super) fn rewrite_wserver(msg: &str, host: &str, port: &str) -> (String, usize) {
    let m = msg.as_bytes();
    let (mut out, mut n, mut last, mut i) = (Vec::new(), 0, 0, 0);
    while i < m.len() {
        let Some((len_end, end)) = wserver_at(m, i) else {
            i += 1;
            continue;
        };
        let body = len_end + 1; // after "y<len>:"
        let size = msg[i + 1..len_end].parse::<usize>().ok();
        if let Some(size) = size.filter(|&s| i >= last && body + s <= m.len() && body + s >= end) {
            let mut token = format!("W{host}%3A{port}%3A").into_bytes();
            token.extend_from_slice(&m[end..body + size]);
            out.extend_from_slice(&m[last..i]);
            out.extend_from_slice(format!("y{}:", token.len()).as_bytes());
            out.extend_from_slice(&token);
            last = body + size;
            n += 1;
        }
        i = end; // matches never overlap, as with regexp FindAll
    }
    if n == 0 {
        return (msg.into(), 0);
    }
    out.extend_from_slice(&m[last..]);
    match String::from_utf8(out) {
        Ok(s) => (s, n),
        Err(_) => (msg.into(), 0), // a length that cut a character: not one of ours
    }
}
