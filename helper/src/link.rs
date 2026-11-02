//! Port of internal/link: the proxy-link. The guest's master forwards every
//! lobby command to the host's master so that lobby state lives in one place.
//!
//! It is our own protocol, not the game's: newline delimited JSON using the
//! same {uid, cmd, args} envelope as the master websocket, carried over any
//! Conn (the host's public TCP port, an SDR bridge stream, a punch loopback).

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::{to_raw_value, RawValue};

use crate::conn::{ConnRef, Io};
use crate::modver::Info;
use crate::sendq::Queue;
use crate::uid;

/// Shorter than the game's own 20s command timeout so that a dead host
/// surfaces as an error rather than as a client side timeout.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// Bounds the wait for a guest's hello: until it is vetted the guest is
/// nobody, and must not hold a thread.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds the hello line; a real one is a few hundred bytes.
const MAX_HELLO: usize = 64 << 10;
/// Bounds every later line: a lobby/chat carries at most one of the game's
/// websocket frames (64 MiB, wsx MAX_FRAME) plus its envelope.
const MAX_LINE: usize = (64 << 20) + (64 << 10);

/// How far a peer may fall behind before the link is dropped.
const SEND_BUDGET: usize = 2 * MAX_LINE;

/// The request id of link/hello, so a refusal comes back as its reply and
/// never looks like a push.
const HELLO_UID: i64 = 1;

/// The wire frame, identical in shape to the master's JSON frame.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(default, deserialize_with = "null_default")]
    pub uid: i64,
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "null_default")]
    pub cmd: String,
    /// Raw JSON; `null` is kept as Some (Go's json.RawMessage), only absent is None.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "raw_keep_null")]
    pub args: Option<Box<RawValue>>,
}

/// null leaves the zero value, as Go's Unmarshal does.
fn null_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Option::<T>::deserialize(d).map(Option::unwrap_or_default)
}

fn raw_keep_null<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

/// The player behind a link.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct User {
    #[serde(rename = "uid", default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The real Steam id, for a lobby on SDR.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub steam: String,
    /// The id the guest's game calls its own (its user/login uid).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub game: String,
    /// From the join code; a link over SDR must present it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key: u32,
    /// The guest's mod-file fingerprint; None from an older wartales-mp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#mod: Option<Info>,
}

fn is_zero(k: &u32) -> bool {
    *k == 0
}

/// Sends are queued, never waited on: a push to one stalled guest must not
/// hold up the command that caused it, nor the pushes to everyone else.
fn send(q: &Queue, e: &Envelope) -> io::Result<()> {
    let mut b = serde_json::to_vec(e).map_err(io::Error::other)?;
    b.push(b'\n');
    q.send(&[&b])
}

fn raw_str(s: &str) -> Box<RawValue> {
    to_raw_value(s).expect("a string always marshals")
}

/// An "err" reply's text: the JSON string, or the raw args when not one.
fn err_text(args: Option<&RawValue>) -> String {
    let raw = args.map_or("", |a| a.get());
    match serde_json::from_str::<Option<String>>(raw) {
        Ok(msg) => msg.unwrap_or_default(), // null is "", as in Go
        Err(_) => raw.to_string(),
    }
}

// ---------------------------------------------------------------- host side

/// A guest as seen by the host's master.
pub struct Peer {
    user: User,
    out: Queue,
    sdr: bool,
    push_uid: AtomicI64,
}

impl Peer {
    /// The guest's minted Session id.
    pub fn user_id(&self) -> &str {
        &self.user.id
    }
    /// The guest's real Steam id, or "" when it reported none.
    pub fn steam_id(&self) -> &str {
        &self.user.steam
    }
    /// The id the guest's game calls its own, or "" for an old guest.
    pub fn game_id(&self) -> &str {
        &self.user.game
    }
    /// The guest's mod-file fingerprint, or None for an older guest.
    pub fn r#mod(&self) -> Option<&Info> {
        self.user.r#mod.as_ref()
    }
    /// The guest's display name.
    pub fn name(&self) -> &str {
        &self.user.name
    }
    /// The guest's link rides the SDR bridge (a Steam join), not a direct route.
    pub fn over_sdr(&self) -> bool {
        self.sdr
    }
    /// This peer sits behind a proxy-link.
    pub fn remote(&self) -> bool {
        true
    }
    /// Sends a server initiated command (a lobby event) to the guest, which
    /// relays it to its own game.
    pub fn push(&self, cmd: &str, args: &impl Serialize) {
        let Ok(raw) = to_raw_value(args) else { return };
        let n = self.push_uid.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = send(&self.out, &Envelope { uid: n, cmd: cmd.into(), args: Some(raw) });
    }
}

/// Runs one guest connection on the host side until it closes. sdr marks a
/// stream of the SDR bridge (Go read it off RemoteAddr().Network()). accept
/// vets the hello before any command is served; an error closes the link
/// with its text. h answers each forwarded command; on_close runs once a
/// vetted peer's link ends.
pub fn serve(
    c: ConnRef,
    sdr: bool,
    accept: impl FnOnce(&User) -> anyhow::Result<()>,
    h: impl FnMut(&str, Option<&RawValue>, &Arc<Peer>) -> anyhow::Result<Box<RawValue>>,
    on_close: impl FnOnce(&Arc<Peer>),
) {
    let out = Queue::new(c.clone(), SEND_BUDGET);
    serve_link(c, sdr, &out, accept, h, on_close);
    out.drain(); // what is queued (a refusal, say) still goes out
}

fn serve_link(
    c: ConnRef,
    sdr: bool,
    out: &Queue,
    accept: impl FnOnce(&User) -> anyhow::Result<()>,
    mut h: impl FnMut(&str, Option<&RawValue>, &Arc<Peer>) -> anyhow::Result<Box<RawValue>>,
    on_close: impl FnOnce(&Arc<Peer>),
) {
    let mut br = BufReader::new(Io(c.clone()));

    // SDR streams ignore deadlines, so the wait is bounded by closing c.
    let (stop, stopped) = mpsc::channel::<()>();
    let timer = std::thread::spawn({
        let c = c.clone();
        move || {
            let fired = stopped.recv_timeout(HELLO_TIMEOUT) == Err(mpsc::RecvTimeoutError::Timeout);
            if fired {
                c.close();
            }
            fired
        }
    });
    let first = read_envelope(&mut br, MAX_HELLO);
    drop(stop);
    let expired = timer.join().unwrap_or(true);
    let first = match first {
        Ok(e) if !expired && e.cmd == "link/hello" => e,
        _ => return,
    };
    let Ok(mut u) = serde_json::from_str::<User>(first.args.as_ref().map_or("null", |a| a.get())) else {
        return;
    };
    if let Err(e) = accept(&u) {
        let _ = send(out, &Envelope { uid: first.uid.wrapping_neg(), cmd: "err".into(), args: Some(raw_str(&format!("{e:#}"))) });
        return;
    }
    // The guest announces itself, so its ids are not trusted as given: the
    // Session id must be one, or a Steam shaped id would travel into every
    // direct-relay LobbyInfo we serve and flip the lobby onto the Steam path;
    // and the Steam id must be well-formed, or an SDR lobby would hand the
    // host's game something it cannot turn into a SteamID (see uid).
    u.id = uid::ensure(&u.id);
    if !uid::is_steam(&u.steam) {
        u.steam.clear();
    }
    let peer = Arc::new(Peer { user: u, out: out.clone(), sdr, push_uid: AtomicI64::new(0) });

    while let Ok(e) = read_envelope(&mut br, MAX_LINE) {
        if e.uid <= 0 {
            continue; // a reply to one of our pushes; nothing to do
        }
        // A panicking handler fails its command, not the link (Go would crash the process).
        let result = catch_unwind(AssertUnwindSafe(|| h(&e.cmd, e.args.as_deref(), &peer))).unwrap_or_else(|_| {
            crate::applog::append(&format!("link: handler for {} panicked", e.cmd));
            Err(anyhow!("link: {} failed", e.cmd))
        });
        let reply = match result {
            Ok(raw) => Envelope { uid: -e.uid, cmd: String::new(), args: Some(raw) },
            Err(err) => Envelope { uid: -e.uid, cmd: "err".into(), args: Some(raw_str(&format!("{err:#}"))) },
        };
        let _ = send(out, &reply);
    }
    on_close(&peer);
}

// --------------------------------------------------------------- guest side

/// The guest's connection to the host's master.
pub struct Client {
    out: Queue,
    st: Mutex<ClientState>,
}

#[derive(Default)]
struct ClientState {
    call_timeout: Duration, // zero = CALL_TIMEOUT
    uid: i64,
    pending: HashMap<i64, mpsc::SyncSender<Envelope>>,
    closed: bool,
    refused: String,       // the host's answer to our hello, when it said no
    cause: Option<String>, // why the connection ended, when the transport said
}

impl ClientState {
    /// The error a call gets on a closed link.
    fn closed_err(&self) -> anyhow::Error {
        if !self.refused.is_empty() {
            return anyhow!("link: host refused the connection: {}", self.refused);
        }
        match &self.cause {
            Some(c) => anyhow!("link: connection closed: {c}"),
            None => anyhow!("link: connection closed"),
        }
    }
}

/// Runs the guest side of a link over an already open connection (a TCP
/// connection to the host's port, or a stream over the SDR bridge) and
/// announces the local user. Pushes from the host go to on_push; on_close
/// runs once the link ends. The hello is sent with uid 1 so a refusal from
/// the host comes back as a reply to it and reaches the caller instead of a
/// later command.
pub fn dial_conn(
    c: ConnRef,
    u: &User,
    mut on_push: impl FnMut(&str, Option<Box<RawValue>>) + Send + 'static,
    on_close: impl FnOnce() + Send + 'static,
) -> io::Result<Arc<Client>> {
    let st = ClientState { uid: HELLO_UID, ..Default::default() };
    let cl = Arc::new(Client { out: Queue::new(c.clone(), SEND_BUDGET), st: Mutex::new(st) });

    let hello = to_raw_value(u).map_err(io::Error::other)?;
    if let Err(e) = send(&cl.out, &Envelope { uid: HELLO_UID, cmd: "link/hello".into(), args: Some(hello) }) {
        c.close(); // the hello already failed; report that error, not this one
        return Err(e);
    }

    let reader = cl.clone();
    let spawned = std::thread::Builder::new().name("link-client".into()).spawn(move || {
        let cl = reader;
        let read = catch_unwind(AssertUnwindSafe(|| cl.read_loop(c, &mut on_push)));
        if read.is_err() {
            crate::applog::append("link: client reader thread panicked");
        }
        cl.shutdown(); // before on_close, so a call from it already sees the link closed
        on_close();
    });
    if let Err(e) = spawned {
        cl.close();
        return Err(e);
    }
    Ok(cl)
}

impl Client {
    fn read_loop(&self, c: ConnRef, on_push: &mut impl FnMut(&str, Option<Box<RawValue>>)) {
        let mut br = BufReader::new(Io(c));
        loop {
            let e = match read_envelope(&mut br, MAX_LINE) {
                Ok(e) => e,
                Err(err) => {
                    let mut st = self.st.lock().unwrap();
                    // EOF, or our own close (Go's net.ErrClosed), is no cause.
                    if err.kind() != io::ErrorKind::UnexpectedEof && !st.closed {
                        st.cause = Some(err.to_string()); // e.g. the SDR bridge's reason for dropping the stream
                    }
                    return;
                }
            };
            if e.uid < 0 {
                let id = e.uid.wrapping_neg();
            if id == HELLO_UID && e.cmd == "err" {
                    // The host refused our hello: remember why, then end.
                    self.st.lock().unwrap().refused = err_text(e.args.as_deref());
                    return;
                }
                let ch = self.st.lock().unwrap().pending.remove(&id);
                if let Some(ch) = ch {
                    let _ = ch.try_send(e);
                }
                continue;
            }
            if !e.cmd.is_empty() {
                on_push(&e.cmd, e.args);
            }
            // answer the push so the host's side stays symmetric with the game
            let _ = send(&self.out, &Envelope { uid: -e.uid, cmd: String::new(), args: Some(raw_str_true()) });
        }
    }

    /// Bounds each later call; zero means CALL_TIMEOUT.
    pub fn set_call_timeout(&self, d: Duration) {
        self.st.lock().unwrap().call_timeout = d;
    }

    /// Forwards a command to the host's master and waits for its reply.
    pub fn call(&self, cmd: &str, args: Option<Box<RawValue>>) -> anyhow::Result<Option<Box<RawValue>>> {
        let (tx, rx) = mpsc::sync_channel(1);
        let (n, timeout) = {
            let mut st = self.st.lock().unwrap();
            if st.closed {
                return Err(st.closed_err());
            }
            st.uid += 1;
            let n = st.uid;
            st.pending.insert(n, tx);
            (n,if st.call_timeout.is_zero() { CALL_TIMEOUT } else { st.call_timeout })
        };
        if let Err(e) = send(&self.out, &Envelope { uid: n, cmd: cmd.into(), args }) {
            self.st.lock().unwrap().pending.remove(&n);
            return Err(e.into());
        }
        match rx.recv_timeout(timeout) {
            Ok(e) if e.cmd == "err" => Err(anyhow::Error::msg(err_text(e.args.as_deref()))),
            Ok(e) => Ok(e.args),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.st.lock().unwrap().closed_err()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.st.lock().unwrap().pending.remove(&n);
                Err(anyhow!("link: timeout"))
            }
        }
    }

    /// Tears the link down.
    pub fn close(&self) {
        self.out.close();
        self.shutdown();
    }

    fn shutdown(&self) {
        let mut st = self.st.lock().unwrap();
        st.closed = true;
        st.pending.clear(); // dropping the senders wakes every waiting call
    }
}

fn raw_str_true() -> Box<RawValue> {
    RawValue::from_string("true".into()).expect("valid JSON")
}

/// Reads one line of at most limit bytes (newline included). EOF, even after
/// a partial line, is UnexpectedEof.
fn read_envelope(br: &mut BufReader<Io>, limit: usize) -> io::Result<Envelope> {
    let mut line = Vec::new();
    loop {
        let buf = br.fill_buf()?;
        if buf.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (n, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        if line.len() + n > limit {
            return Err(io::Error::other("link: line too long"));
        }
        line.extend_from_slice(&buf[..n]);
        br.consume(n);
        if done {
            break;
        }
    }
    serde_json::from_slice(&line).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::pipe;
    use std::time::Instant;

    fn no_handler(_: &str, _: Option<&RawValue>, _: &Arc<Peer>) -> anyhow::Result<Box<RawValue>> {
        Ok(RawValue::from_string("null".into()).unwrap())
    }

    /// Runs serve against a peer that writes req and then stays silent, and
    /// returns how long serve took to give up on it.
    fn run_serve(req: &str) -> Duration {
        let (srv, cli) = pipe();
        cli.write_all(req.as_bytes()).unwrap();
        let start = Instant::now();
        let (done_tx, done) = mpsc::channel();
        let t = std::thread::spawn(move || {
            serve(srv, false, |_| Ok(()), no_handler, |_| {});
            let _ = done_tx.send(());
        });
        done.recv_timeout(HELLO_TIMEOUT + Duration::from_secs(5))
            .expect("serve still waits on a guest that never said hello");
        t.join().unwrap();
        cli.close();
        start.elapsed()
    }

    #[test]
    fn serve_refuses_an_oversized_hello() {
        let took = run_serve(&"a".repeat(2 * MAX_HELLO));
        assert!(took < HELLO_TIMEOUT, "took {took:?}: the oversized hello was not refused as it arrived");
    }

    #[test]
    fn serve_drops_a_silent_guest() {
        let took = run_serve(r#"{"uid":1,"cmd":"link/hel"#);
        assert!(took >= HELLO_TIMEOUT, "gave up after {took:?}, before HELLO_TIMEOUT");
    }

    #[test]
    fn read_envelope_limit() {
        let (srv, cli) = pipe();
        let big = format!("{{\"uid\":2,\"args\":\"{}\"}}\n", "x".repeat(10000));
        cli.write_all(format!("{big}{big}").as_bytes()).unwrap();
        let mut br = BufReader::new(Io(srv));
        read_envelope(&mut br, big.len()).expect("a line at the limit");
        assert!(read_envelope(&mut br, big.len() - 1).is_err(), "a line over the limit was read");
    }

    // Not in Go: a silent host times a call out; close wakes a waiting call; null err text.
    #[test]
    fn call_timeout_and_close() {
        let (_host, guest) = pipe();
        let cl = dial_conn(guest, &User::default(), |_, _| {}, || {}).unwrap();
        cl.set_call_timeout(Duration::from_millis(50));
        assert_eq!(cl.call("x", None).unwrap_err().to_string(), "link: timeout");
        cl.set_call_timeout(Duration::from_secs(10));
        let waiter = std::thread::spawn({
            let cl = cl.clone();
            move || cl.call("y", None).unwrap_err().to_string()
        });
        std::thread::sleep(Duration::from_millis(50));
        cl.close();
        assert_eq!(waiter.join().unwrap(), "link: connection closed");
        assert_eq!(err_text(Some(&RawValue::from_string("null".into()).unwrap())), "");
        let e: Envelope = serde_json::from_str(r#"{"uid":null,"cmd":null,"args":null}"#).unwrap();
        assert_eq!((e.uid, e.cmd.as_str(), e.args.unwrap().get()), (0, "", "null"));
    }

    // Not in Go: hello, call/reply, err reply, push, refusal, and the threads ending.
    #[test]
    fn link_roundtrip() {
        let (host, guest) = pipe();
        let pushes = Arc::new(Mutex::new(Vec::new()));
        let (closed_tx, closed) = mpsc::channel();
        let srv = std::thread::spawn(move || {
            serve(
                host,
                true,
                |u| if u.key == 7 { Ok(()) } else { Err(anyhow!("Invalid join code")) },
                |cmd, args, p| {
                    assert!(p.over_sdr() && uid::is_session(p.user_id()) && p.steam_id().is_empty());
                    p.push("lobby/chat", &"hi");
                    match cmd {
                        "echo" => Ok(args.unwrap().to_owned()),
                        _ => Err(anyhow!("no such cmd")),
                    }
                },
                |p| assert_eq!(p.name(), "G"),
            )
        });
        let u = User { id: "S123".into(), name: "G".into(), steam: "bad".into(), key: 7, ..Default::default() };
        let got = pushes.clone();
        let cl = dial_conn(guest.clone(), &u, move |c, a| got.lock().unwrap().push(format!("{c} {}", a.unwrap())), move || {
            let _ = closed_tx.send(());
        })
        .unwrap();
        let r = cl.call("echo", Some(RawValue::from_string(r#"{"a":1}"#.into()).unwrap())).unwrap();
        assert_eq!(r.unwrap().get(), r#"{"a":1}"#);
        assert_eq!(cl.call("nope", None).unwrap_err().to_string(), "no such cmd");
        assert_eq!(*pushes.lock().unwrap(), vec![r#"lobby/chat "hi""#, r#"lobby/chat "hi""#]);
        cl.close();
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
        srv.join().unwrap();
        assert_eq!(cl.call("x", None).unwrap_err().to_string(), "link: connection closed");

        // a wrong key: the refusal reaches the guest's next call
        let (host, guest) = pipe();
        let srv = std::thread::spawn(move || serve(host, false, |_| Err(anyhow!("Invalid join code")), no_handler, |_| {}));
        let (closed_tx, closed) = mpsc::channel();
        let cl = dial_conn(guest, &u, |_, _| {}, move || {
            let _ = closed_tx.send(());
        })
        .unwrap();
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
        srv.join().unwrap();
        assert_eq!(cl.call("x", None).unwrap_err().to_string(), "link: host refused the connection: Invalid join code");
    }
}
