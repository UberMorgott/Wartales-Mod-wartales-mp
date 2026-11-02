//! Port of internal/master: the local stand-in for master.shirogames.com.
//!
//! It speaks the JSON envelope of mpman/net/WSConnection over a plain
//! websocket on 127.0.0.1:60442 (the bytecode's master_config pass points the
//! game there with ssl:false) and answers the commands listed in
//! decomp/SERVER-CONTRACT.md.
//!
//! Replies and pushes are built as serde_json Values: object keys come out
//! sorted, as Go's map marshalling wrote them.

// why: a port of Go code that names args a, peers p, the lobby store l.
#![allow(clippy::many_single_char_names)]

mod lobby;
mod modcheck;
mod route;
mod transport;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use serde_json::value::{to_raw_value, RawValue};
use serde_json::{json, Value};

use crate::applog::{headers, trunc};
use crate::conn::ConnRef;
use crate::sdrbridge::{Bridge, Status};
use crate::{link, logf, modver, nat, punch, uid, wsx, Log};

pub use lobby::DIRECT_LOBBY_MARK;
pub use route::{Puncher, DEFAULT_DIRECT_TIMEOUT};
pub use transport::{Transport, MODE_AUTO, MODE_DIRECT, MODE_SDR};

/// Whoever issued a command: the local game, or a guest behind a proxy-link.
pub trait Peer: Send + Sync {
    /// The minted Session ('X') id: logs and the user/login sid only.
    fn user_id(&self) -> String;
    /// The real Steam id the player's game reported, or "" when it reported
    /// none (or one that is not well-formed). An SDR lobby emits it.
    fn steam_id(&self) -> String;
    /// The id the player's game calls its own: the "uid" of its user/login,
    /// verbatim, and the id every lobby names it by (idOf):
    /// LobbyService.onMessage@54929 drops every lobby packet whose target is
    /// not exactly this string (LobbyService.hx:101). "" when unknown.
    fn game_id(&self) -> String;
    fn name(&self) -> String;
    fn remote(&self) -> bool;
    /// The peer's link rides the SDR bridge (a Steam join).
    fn over_sdr(&self) -> bool {
        false
    }
    /// Sends a server initiated command; never waits on the peer.
    fn push(&self, cmd: &str, args: &RawValue);
}

pub type PeerRef = Arc<dyn Peer>;

/// Whether a and b are the same peer (Go's interface ==).
fn same(a: &PeerRef, b: &PeerRef) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

impl Peer for link::Peer {
    fn user_id(&self) -> String {
        link::Peer::user_id(self).into()
    }
    fn steam_id(&self) -> String {
        link::Peer::steam_id(self).into()
    }
    fn game_id(&self) -> String {
        link::Peer::game_id(self).into()
    }
    fn name(&self) -> String {
        link::Peer::name(self).into()
    }
    fn remote(&self) -> bool {
        true
    }
    fn over_sdr(&self) -> bool {
        link::Peer::over_sdr(self)
    }
    fn push(&self, cmd: &str, args: &RawValue) {
        link::Peer::push(self, cmd, &args);
    }
}

type EndpointFn = dyn Fn() -> anyhow::Result<nat::Endpoint> + Send + Sync;
type DialFn = dyn Fn(&str, Duration) -> io::Result<ConnRef> + Send + Sync;

/// Configures the master.
pub struct Options {
    /// Listen address, normally 127.0.0.1:60442.
    pub addr: String,
    /// Local relay port, used for the host's serverID.
    pub relay_port: u16,
    pub host_pw: String,
    pub slave_pw: String,
    pub log: Log,
    /// The "ip:port" other players must reach us on: guests' serverID and codes.
    pub public_addr: Option<Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>>,
    /// MODE_AUTO, MODE_DIRECT or MODE_SDR ("" = auto).
    pub transport: String,
    /// The full public endpoint verdict, for the transport choice; None = unknown.
    pub endpoint: Option<Arc<EndpointFn>>,
    /// The verdict on the SDR transport; None = unknown.
    pub sdr_status: Option<Arc<dyn Fn() -> Status + Send + Sync>>,
    /// Carries the proxy-link over SDR; None = the lobby phase only travels over TCP.
    pub bridge: Option<Arc<Bridge>>,
    /// Embedded in join codes that carry an SDR route; a proxy-link over SDR
    /// must present it, and one over TCP must when it presents any.
    pub link_key: u32,
    /// This player's mod-file fingerprint; None = unknown, never a mismatch.
    pub r#mod: Option<Arc<modver::Session>>,
    /// Bounds a guest's connect to the host's endpoint (zero = DEFAULT_DIRECT_TIMEOUT).
    pub direct_timeout: Duration,
    /// Replaces the TCP dialer of the direct route (tests, the harness).
    pub dial_direct: Option<Arc<DialFn>>,
    /// The UDP socket for join codes behind NAT; None = TCP only.
    pub punch: Option<Arc<dyn Puncher>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            addr: String::new(),
            relay_port: 0,
            host_pw: String::new(),
            slave_pw: String::new(),
            log: Log::discard(),
            public_addr: None,
            transport: String::new(),
            endpoint: None,
            sdr_status: None,
            bridge: None,
            link_key: 0,
            r#mod: None,
            direct_timeout: Duration::ZERO,
            dial_direct: None,
            punch: None,
        }
    }
}

impl Puncher for punch::Socket {
    fn ready(&self) -> bool {
        punch::Socket::ready(self)
    }
    fn endpoint(&self) -> anyhow::Result<punch::StunResult> {
        punch::Socket::endpoint(self)
    }
    fn dial(&self, addr: &str) -> anyhow::Result<String> {
        punch::Socket::dial(self, addr)
    }
    fn toward(&self, addr: &str) -> anyhow::Result<()> {
        punch::Socket::toward(self, addr)
    }
}

/// A command's answer: None = no args at all, Some(Null) = "null".
type Out = anyhow::Result<Option<Value>>;

#[derive(Default)]
struct State {
    local: Option<Arc<Session>>,       // the local game's connection
    client: Option<Arc<link::Client>>, // set once we joined a remote lobby as a guest
    conns: HashMap<u64, ConnRef>,      // the game's sockets, closed by close
    next_conn: u64,
    ln: Option<SocketAddr>, // the listener, woken by close
    closed: bool,
}

/// The master.
pub struct Server {
    opt: Options,
    me: Weak<Server>,
    st: Mutex<State>,
    lob: Mutex<lobby::Lobbies>,
}

/// Bounds the commands waiting on a blocked one; the game waits for every
/// reply and sends a handful at most, so this is never reached.
const QUEUE_DEPTH: usize = 64;

/// Bounds the game's websocket upgrade on the loopback listener; a slow PC
/// needed ~10 s for the old TLS handshake + upgrade (#5).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// Runs f on a thread of its own; a panic is logged, never kills the game.
fn spawn(log: &Log, what: &'static str, f: impl FnOnce() + Send + 'static) {
    let tlog = log.clone();
    let r = std::thread::Builder::new()
        .name(what.into())
        .spawn(move || {
            if let Err(p) = catch_unwind(AssertUnwindSafe(f)) {
                logf!(tlog, "master: {what} thread panicked: {}", panic_text(&*p));
            }
        });
    if let Err(e) = r {
        logf!(log, "master: cannot start a {what} thread: {e}");
    }
}

/// The message of a caught panic.
pub fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(ToString::to_string)
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

fn to_raw(v: &Value) -> Box<RawValue> {
    to_raw_value(v).expect("a Value always marshals")
}

/// A reply from the host's master as a Value; absent is null, as Go's
/// json.RawMessage(nil) marshalled.
fn from_raw(r: Option<&RawValue>) -> Value {
    r.and_then(|r| serde_json::from_str(r.get()).ok())
        .unwrap_or(Value::Null)
}

/// A command's arguments; unparsable is null (Go ignored the Unmarshal error).
fn args_of(args: Option<&RawValue>) -> Value {
    from_raw(args)
}

/// A string field, "" when absent or not a string.
fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").into()
}

/// The commands that carry secrets in their arguments or reply (the Steam
/// session token and sid, the relay passwords, join codes); players attach the
/// log to public bug reports, so only the command name is logged for them.
const SECRET_CMDS: &[&str] = &[
    "user/login",
    "user/session",
    "instance/get",
    "lobby/makeShortCode",
    "lobby/initInvite",
    "lobby/infoInvite",
    "lobby/resolveShortCode",
];

/// A command's arguments or reply for the log.
fn payload(cmd: &str, v: &str) -> String {
    if SECRET_CMDS.contains(&cmd) {
        return "(not logged)".into();
    }
    trunc(v)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The epoch clock the client syncs against, in seconds; whole seconds print
/// without a fraction, as Go's float64 did.
fn now_seconds() -> Value {
    let ms = now_ms();
    if ms.is_multiple_of(1000) {
        json!(ms / 1000)
    } else {
        #[allow(clippy::cast_precision_loss, reason = "epoch ms fit in 53 bits")]
        let s = ms as f64 / 1000.0;
        json!(s)
    }
}

/// mpman.Api.getCurrentToken: 16 hour buckets.
fn current_token() -> u64 {
    (now_ms() / 57_600_000) & 0xFFFF
}

/// A Session platform id, "X" + 4 hex digits + signature, as mpman.UserID
/// parses it (getPlatform@25195 case 'X'). The real signature is
/// scrambleToken(token, sign) over a server secret we do not have; a mismatch
/// only makes the client drop its cached $SID and log in again.
fn make_sid(user_id: &str, name: &str) -> String {
    let t = current_token();
    let sum = crate::log::digest(
        crate::log::Alg::Md5,
        format!("wartales-mp{user_id}{t}{name}").as_bytes(),
    );
    let sign = wsx::base64(&sum);
    format!("X{t:04x}{}", sign.trim_end_matches('='))
}

// ---------------------------------------------------------------- session

#[derive(Default)]
struct Ident {
    uid: String,
    steam: String,
    game: String, // the id the game reported in user/login, verbatim
    name: String,
}

/// The local game's master connection.
pub struct Session {
    ws: wsx::Conn,
    log: Log,
    id: Mutex<Ident>,
    push_uid: AtomicI64,
    /// The game's socket ended: the commands still queued are dropped.
    gone: AtomicBool,
}

impl Session {
    fn ident(&self) -> MutexGuard<'_, Ident> {
        self.id.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records who the local game says it is. The game reports its Steam id
    /// here ("S<steamid>"); a lobby on SDR emits exactly it, because the game
    /// derives the peer's SteamID from it.
    fn adopt(&self, a: &Value) {
        let (game, name) = (text(a, "uid"), text(a, "name"));
        let mut id = self.ident();
        if !game.is_empty() {
            id.uid = uid::mint(&game);
            if uid::is_steam(&game) {
                id.steam.clone_from(&game); // only ever emitted for a lobby on SDR
            }
            id.game = game;
        }
        if id.uid.is_empty() {
            let mut b = [0u8; 8];
            crate::rand_bytes(&mut b);
            id.uid = uid::mint(&format!("anonymous-{}", wsx::hex(&b)));
        }
        if !name.is_empty() {
            id.name = name;
        }
        if id.name.is_empty() {
            id.name = "Player".into();
        }
    }
}

impl Peer for Session {
    fn user_id(&self) -> String {
        self.ident().uid.clone()
    }
    fn steam_id(&self) -> String {
        self.ident().steam.clone()
    }
    fn game_id(&self) -> String {
        self.ident().game.clone()
    }
    fn name(&self) -> String {
        self.ident().name.clone()
    }
    fn remote(&self) -> bool {
        false
    }
    /// SERVER-CONTRACT §3.9.
    fn push(&self, cmd: &str, args: &RawValue) {
        let n = self.push_uid.fetch_add(1, Ordering::Relaxed) + 1;
        let e = link::Envelope {
            uid: n,
            cmd: cmd.into(),
            args: Some(args.to_owned()),
        };
        let b = match serde_json::to_string(&e) {
            Ok(b) => b,
            Err(err) => {
                logf!(self.log, "master: cannot marshal the push {cmd}: {err}");
                return;
            }
        };
        logf!(self.log, "master: -> push #{n} {cmd} {}", trunc(args.get()));
        if let Err(err) = self.ws.write_text(&b) {
            logf!(self.log, "master: push {cmd} failed: {err}");
        }
    }
}

// ---------------------------------------------------------------- server

impl Server {
    pub fn new(opt: Options) -> Arc<Server> {
        Arc::new_cyclic(|me| Server {
            opt,
            me: me.clone(),
            st: Mutex::default(),
            lob: Mutex::default(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lobbies(&self) -> MutexGuard<'_, lobby::Lobbies> {
        self.lob.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn arc(&self) -> Arc<Server> {
        self.me.upgrade().expect("a method runs on a live server")
    }

    /// Binds opt.addr and serves until the listener fails or close is called.
    pub fn listen_and_serve(&self) -> io::Result<()> {
        let ln = TcpListener::bind(&self.opt.addr)?;
        logf!(self.opt.log, "master listening on {}", self.opt.addr);
        self.serve(&ln)
    }

    /// Serves the game's connections on ln; a close returns Ok, because a
    /// deliberate shutdown is not an error.
    pub fn serve(&self, ln: &TcpListener) -> io::Result<()> {
        let addr = ln.local_addr()?;
        {
            let mut st = self.state();
            if st.closed {
                return Ok(()); // close raced us; nothing is left listening
            }
            st.ln = Some(addr);
        }
        loop {
            let c = match ln.accept() {
                Ok((c, _)) => c,
                Err(_) if self.state().closed => return Ok(()),
                Err(e) => return Err(e),
            };
            let _ = c.set_nodelay(true); // Go's default; the game's replies are small
            let c: ConnRef = Arc::new(c);
            let n = {
                let mut st = self.state();
                if st.closed {
                    c.close();
                    return Ok(());
                }
                st.next_conn += 1;
                let n = st.next_conn;
                st.conns.insert(n, c.clone());
                n
            };
            let s = self.arc();
            spawn(&self.opt.log, "master-conn", move || {
                let peer = c.peer_addr().map_or("?".into(), |a| a.to_string());
                s.serve_game(&c, &peer);
                c.close(); // the game's connection is finished either way
                s.state().conns.remove(&n);
            });
        }
    }

    fn serve_game(&self, c: &ConnRef, peer: &str) {
        let log = &self.opt.log;
        logf!(log, "master: accepted connection from {peer}");
        // The game authenticates with a password obfuscated inside the
        // bytecode; we cannot check it, and we do not need to.
        let ws = match wsx::accept(c.clone(), HANDSHAKE_TIMEOUT, None) {
            Ok(ws) => ws,
            Err(e) => {
                logf!(log, "master: handshake from {peer} failed: {e}");
                return;
            }
        };
        logf!(
            log,
            "master: game connected from {peer} (ident {:?}, headers {})",
            ws.ident,
            trunc(&headers(&ws.headers))
        );
        let sess = Arc::new(Session {
            ws,
            log: log.clone(),
            id: Mutex::default(),
            push_uid: AtomicI64::new(0),
            gone: AtomicBool::new(false),
        });
        // The queue carries the game's commands to the one thread that answers
        // them, in arrival order. A forwarded command blocks on the host, so
        // the read loop must not answer inline; but the game fires lobby/leave,
        // user/login and lobby/join back to back when it re-joins a lobby, and
        // a join forwarded before the leave ends with the host dropping a
        // member its game believes is in.
        let (tx, rx) = mpsc::sync_channel::<link::Envelope>(QUEUE_DEPTH);
        let (done_tx, done) = mpsc::channel::<()>();
        {
            let (s, sess) = (self.arc(), sess.clone());
            spawn(log, "master-answer", move || {
                let _done = done_tx; // dropped when this thread ends, however it ends
                for e in rx {
                    if !sess.gone.load(Ordering::SeqCst) {
                        s.answer(&sess, &e);
                    }
                }
            });
        }
        // The game opens two sockets at startup (master and master2,
        // getMPManConfig@14965) and drops the second at once, so a socket does
        // not become the game's connection by merely arriving: the first one
        // keeps it, and user/login moves it to whichever socket the game
        // actually speaks on (adopt_local).
        self.state().local.get_or_insert_with(|| sess.clone());

        loop {
            let (op, data) = match sess.ws.read() {
                Ok(f) => f,
                Err(e) => {
                    logf!(log, "master: game disconnected: {e}");
                    break;
                }
            };
            if op != wsx::OP_TEXT {
                continue;
            }
            let e: link::Envelope = match serde_json::from_slice(&data) {
                Ok(e) => e,
                Err(err) => {
                    logf!(
                        log,
                        "master: bad frame from {peer}: {err} ({} B, not logged: it may carry secrets)",
                        data.len()
                    );
                    continue;
                }
            };
            if e.uid < 0 {
                // the game answering one of our pushes
                logf!(
                    log,
                    "master: <- push reply #{} {}",
                    e.uid.wrapping_neg(),
                    trunc(e.args.as_ref().map_or("", |a| a.get()))
                );
                continue;
            }
            if tx.send(e).is_err() {
                break;
            }
        }
        // The commands still queued are dropped and the one being answered is
        // waited for, so none of them can put the session back into a lobby
        // (or back as the game's connection) after it is cleaned up.
        sess.gone.store(true, Ordering::SeqCst);
        drop(tx);
        let _ = done.recv(); // Err once the answer thread has ended
        logf!(
            log,
            "master: game session from {peer} ended (uid {:?})",
            sess.user_id()
        );
        let p: PeerRef = sess.clone();
        self.peer_gone(&p);
        let mut st = self.state();
        if st.local.as_ref().is_some_and(|l| Arc::ptr_eq(l, &sess)) {
            st.local = None;
        }
        drop(st);
        sess.ws.close();
    }

    fn answer(&self, sess: &Arc<Session>, e: &link::Envelope) {
        let log = &self.opt.log;
        let args = e.args.as_deref();
        logf!(
            log,
            "master: <- #{} {} {}",
            e.uid,
            e.cmd,
            payload(&e.cmd, args.map_or("", RawValue::get))
        );
        let p: PeerRef = sess.clone();
        let reply = match self.handle(&e.cmd, args, &p, Some(sess)) {
            Err(err) => {
                let msg = format!("{err:#}");
                logf!(log, "master: -> #{} {} err {}", e.uid, e.cmd, trunc(&msg));
                link::Envelope {
                    uid: e.uid.wrapping_neg(),
                    cmd: "err".into(),
                    args: Some(to_raw(&Value::String(msg))),
                }
            }
            Ok(v) => {
                let raw = v.as_ref().map(to_raw);
                logf!(
                    log,
                    "master: -> #{} {} ok {}",
                    e.uid,
                    e.cmd,
                    payload(&e.cmd, raw.as_ref().map_or("", |r| r.get()))
                );
                link::Envelope {
                    uid: e.uid.wrapping_neg(),
                    cmd: String::new(),
                    args: raw,
                }
            }
        };
        match serde_json::to_string(&reply) {
            Err(err) => logf!(log, "master: cannot marshal the reply to {}: {err}", e.cmd),
            Ok(b) => {
                if let Err(err) = sess.ws.write_text(&b) {
                    logf!(log, "master: reply to {} failed: {err}", e.cmd);
                }
            }
        }
    }

    /// Handles one guest's proxy-link connection over TCP (host role, direct
    /// transport). A guest holding a code with our key presents it and it
    /// must match: a stale address may now be somebody else's host. A guest
    /// with an endpoint-only code (no key) is still served, as it always was.
    pub fn serve_link(&self, c: &ConnRef) {
        let addr = c.peer_addr().map_or("pipe".into(), |a| a.to_string());
        let (log, key, a) = (self.opt.log.clone(), self.opt.link_key, addr.clone());
        self.serve_peer(c, false, "proxy-link", &addr, move |u| {
            if u.key != 0 && u.key != key {
                logf!(
                    log,
                    "proxy-link: connection from {a} refused: wrong join code key"
                );
                anyhow::bail!("Invalid join code");
            }
            Ok(())
        });
    }

    /// Handles one guest's proxy-link stream over the SDR bridge (host role,
    /// SDR transport). Anyone who knows our SteamID can open a stream, so the
    /// hello must carry the key from our join code. Steam authenticated the
    /// stream's peer, and an SDR lobby knows its members by Steam id, so a
    /// hello claiming anybody else's Steam id is refused: it would take that
    /// player's place.
    pub fn serve_sdr_link(&self, c: &ConnRef, peer: u64) {
        let (log, key) = (self.opt.log.clone(), self.opt.link_key);
        self.serve_peer(c, true, "sdr-link", &format!("steam:{peer}"), move |u| {
            if u.key != key {
                logf!(
                    log,
                    "sdr-link: stream from {peer} refused: wrong join code key"
                );
                anyhow::bail!("Invalid join code");
            }
            if !u.steam.is_empty() && uid::steam_id64(&u.steam) != Some(peer) {
                logf!(
                    log,
                    "sdr-link: stream from {peer} refused: its hello claims Steam id {:?}",
                    u.steam
                );
                anyhow::bail!("Steam id does not match the connection");
            }
            Ok(())
        });
    }

    fn serve_peer(
        &self,
        c: &ConnRef,
        sdr: bool,
        kind: &str,
        addr: &str,
        accept: impl FnOnce(&link::User) -> anyhow::Result<()>,
    ) {
        let log = &self.opt.log;
        logf!(log, "link: accepted {kind} from {addr}");
        link::serve(
            c,
            sdr,
            |u| {
                accept(u)?;
                self.note_guest_mod(&u.name, u.r#mod.as_ref());
                Ok(())
            },
            |cmd, args, peer| {
                logf!(
                    log,
                    "link: <- {cmd} from {} ({}) {}",
                    peer.name(),
                    peer.user_id(),
                    payload(cmd, args.map_or("", RawValue::get))
                );
                let p: PeerRef = peer.clone();
                let v = self.handle(cmd, args, &p, None);
                match &v {
                    Err(e) => logf!(log, "link: -> {cmd} err {}", trunc(&format!("{e:#}"))),
                    Ok(v) => logf!(
                        log,
                        "link: -> {cmd} ok {}",
                        payload(cmd, &v.as_ref().map_or("null".into(), Value::to_string))
                    ),
                }
                v.map(|v| to_raw(&v.unwrap_or(Value::Null)))
            },
            |peer| {
                logf!(log, "link: {kind} from {addr} ({}) closed", peer.user_id());
                let p: PeerRef = peer.clone();
                self.peer_gone(&p);
            },
        );
    }

    /// Runs one command. Local lobby commands are forwarded to the host's
    /// master when we are a guest; everything the host answers is served from
    /// the authoritative local state. local is the game's own socket, when it
    /// is the one asking.
    fn handle(
        &self,
        cmd: &str,
        args: Option<&RawValue>,
        p: &PeerRef,
        local: Option<&Arc<Session>>,
    ) -> Out {
        match cmd {
            "user/login" | "user/session" => {
                if let Some(sess) = local {
                    sess.adopt(&args_of(args));
                    // The game logs in on the socket it will use, and does so
                    // again before most commands, so the socket that logs in is
                    // by definition the live one.
                    self.state().local = Some(sess.clone());
                }
                if cmd == "user/session" {
                    // SERVER-CONTRACT §3.2.
                    return Ok(Some(json!({
                        "accept": true, "version": 2, "time": now_seconds(), "perm": "",
                    })));
                }
                // SERVER-CONTRACT §3.1: "sid" must be non-null or the client
                // reports "Login Failure".
                logf!(self.opt.log, "master: login {} ({})", p.name(), p.user_id());
                Ok(Some(json!({
                    "sid": make_sid(&p.user_id(), &p.name()),
                    "perm": "",
                    "time": now_seconds(),
                    "version": 2,
                })))
            }
            "user/time" => Ok(Some(now_seconds())),
            "insight/event" => Ok(None),
            "code/redeem" => Ok(Some(json!({"code": 0}))),
            "instance/get" => self.instance_get(args, p),
            "instance/ingame" => Ok(Some(json!({"inGame": false}))),
            _ if cmd.starts_with("lobby/") => self.lobby_command(cmd, args, p),
            _ => Err(anyhow!("Unknown command {cmd}")),
        }
    }

    /// Proxies a command to the host's master; None when we are not linked to a host.
    fn forward(&self, cmd: &str, args: Option<&RawValue>) -> Option<anyhow::Result<Value>> {
        let cl = self.uplink()?;
        Some(
            cl.call(cmd, args.map(ToOwned::to_owned))
                .map(|r| from_raw(r.as_deref())),
        )
    }

    /// The link to the host's master, None when we are not a guest.
    fn uplink(&self) -> Option<Arc<link::Client>> {
        self.state().client.clone()
    }

    /// The game's connection, if any: where pushes from the host's master go
    /// while we are a guest.
    fn local_session(&self) -> Option<Arc<Session>> {
        self.state().local.clone()
    }

    /// SERVER-CONTRACT §2: the serverID decides the transport.
    /// "R<host>:<port>:<pass>" makes the game's host speak RelayP2P and its
    /// guests WServer, both against our relay.
    fn instance_get(&self, args: Option<&RawValue>, p: &PeerRef) -> Out {
        if p.remote() {
            // A guest asking the host's master: hand out our public endpoint.
            let addr = self
                .public_addr()
                .map_err(|e| anyhow!("no public address: {e:#}"))?;
            return Ok(Some(self.instance_answer(&addr)));
        }
        if let Some(r) = self.forward("instance/get", args) {
            return r.map(Some); // we are a guest: the host's master decides
        }
        Ok(Some(self.start_answer()))
    }

    /// The serverID must carry a password field: the client's RelayP2P parser
    /// (UserID.hx:79-87) splits on ':' and pops the password, then the port,
    /// unconditionally. The client rebuilds both ids with the
    /// serverStartAnswer passwords (@26421), so the field's value here is
    /// only a placeholder; hostpw is used.
    fn instance_answer(&self, addr: &str) -> Value {
        json!({
            "serverID": format!("R{addr}:{}", self.opt.host_pw),
            "serverStartAnswer": {"hostpw": self.opt.host_pw, "slavepw": self.opt.slave_pw},
        })
    }

    fn public_addr(&self) -> anyhow::Result<String> {
        match &self.opt.public_addr {
            Some(f) => f(),
            None => Err(anyhow!("no public address resolver")),
        }
    }

    /// instance/get for the host's own game, asked whenever it starts
    /// (Lobby.setupPlatform, patcher start_transport.rs). The game's transport
    /// is chosen here, from how the guests actually joined (issue #7), not at
    /// lobby creation:
    ///
    ///   - no guest, or every guest over a direct TCP link: our relay
    ///     ("R127.0.0.1:<port>:<pw>"; each guest's master rewrites the address
    ///     in the start message to the route it joined over, guest_relay_addr);
    ///   - every guest over the SDR bridge (a Steam join): the host's Steam id,
    ///     which the game's patched callback turns into its vanilla Steam path;
    ///   - both kinds: a message naming the Steam players. It is not a platform
    ///     id, so the same callback shows it and the start fails cleanly;
    ///   - a lobby made under transport sdr whose members all have Steam ids:
    ///     the host's id, what the game's own isSteamOnly test chose.
    ///
    /// Transport direct keeps the relay whatever the links.
    fn start_answer(&self) -> Value {
        let relay = self.instance_answer(&format!("127.0.0.1:{}", self.opt.relay_port));
        if self.opt.transport == MODE_DIRECT {
            return relay;
        }
        let mut host: Option<PeerRef> = None;
        let (mut steam, mut direct) = (Vec::new(), Vec::new());
        let mut steam_only = String::new(); // the owner of an SDR lobby of Steam ids only
        {
            let l = self.lobbies();
            if let Some(lb) = l.by_id.get(&l.invite_lobby) {
                if lb.transport == Transport::Sdr && !lb.users.is_empty() {
                    steam_only.clone_from(&lb.owner);
                }
                for u in &lb.users {
                    if !uid::is_steam(&u.id) {
                        steam_only.clear();
                    }
                    if !u.peer.remote() {
                        if u.id == lb.owner {
                            host = Some(u.peer.clone());
                        }
                    } else if u.peer.over_sdr() {
                        steam.push(u.name.clone());
                    } else {
                        direct.push(u.name.clone());
                    }
                }
            }
        }
        let log = &self.opt.log;
        let host_game = host.map(|h| h.game_id()).unwrap_or_default();
        if !steam_only.is_empty() {
            logf!(
                log,
                "master: game start: SDR lobby of Steam ids: the game's Steam path ({steam_only})"
            );
            return json!({"serverID": steam_only});
        }
        if steam.is_empty() {
            logf!(
                log,
                "master: game start: {} guest(s), all over a direct link: our relay",
                direct.len()
            );
            return relay;
        }
        if direct.is_empty() && uid::is_steam(&host_game) {
            logf!(
                log,
                "master: game start: {} guest(s), all over SDR: the game's Steam path ({host_game})",
                steam.len()
            );
            return json!({"serverID": host_game});
        }
        if direct.is_empty() {
            logf!(
                log,
                "master: game start REFUSED: the guests joined over SDR, but the host has no Steam id"
            );
            return json!({"serverID": "Игра не может начаться: у хоста нет Steam id. / The game cannot start: the host has no Steam id."});
        }
        let names = steam.join(", ");
        logf!(
            log,
            "master: game start REFUSED: mixed lobby, over SDR: {names}; over a direct link: {}",
            direct.join(", ")
        );
        json!({"serverID": format!(
            "Игроки {names} вошли через Steam и не могут подключиться напрямую: войдите все по коду или все через приглашение Steam.\
             \n\nPlayers {names} joined via Steam and cannot connect directly: everyone join by code, or everyone via a Steam invitation."
        )})
    }

    /// Stops the listener, drops every game connection and releases the link
    /// to the host, if we hold one. Safe to call more than once.
    pub fn close(&self) {
        {
            let mut l = self.lobbies();
            l.invite_lobby.clear();
            self.sync_invite(&mut l);
        }
        let (cl, conns, ln) = {
            let mut st = self.state();
            st.closed = true;
            (
                st.client.take(),
                std::mem::take(&mut st.conns),
                st.ln.take(),
            )
        };
        if let Some(a) = ln {
            // wakes the accept, which then sees closed
            let _ = TcpStream::connect_timeout(&a, Duration::from_secs(1));
        }
        for c in conns.values() {
            c.close(); // unblocks the session's ws.read
        }
        if let Some(cl) = cl {
            cl.close();
        }
    }
}
