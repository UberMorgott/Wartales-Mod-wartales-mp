//! Port of internal/master's tests: the master driven the way the game drives
//! it (a masked websocket client), guests and hosts linked over real TCP and
//! over a fake SDR switch, and the pieces on their own.

// why: table-driven test code; Option/by-value helpers keep call sites short.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::items_after_statements
)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::lobby::{haxe_true, Lobby, Member};
use super::modcheck::{haxe_serialize, MOD_ERROR_FIELD, MOD_FIELD, MOD_MISMATCH_VERSION};
use super::route::{rewrite_wserver, TryRoute};
use super::transport::choose_transport;
use super::*;
use crate::conn::{pipe, ConnRef};
use crate::modver::Info;
use crate::sdrbridge::switch::Switch;
use crate::{code, link, nat, uid, wsx};

// ------------------------------------------------------------ the game

/// The minimum of a masked RFC6455 client needed to drive the master the way
/// the game does.
struct Ws {
    c: TcpStream,
    br: BufReader<TcpStream>,
    uid: i64,
}

fn dial(addr: &str) -> Ws {
    let c = (0..100)
        .find_map(|_| {
            TcpStream::connect(addr)
                .map_err(|_| std::thread::sleep(Duration::from_millis(20)))
                .ok()
        })
        .expect("the master never listened");
    let mut key = [0u8; 16];
    crate::rand_bytes(&mut key);
    let req = format!(
        "GET / HTTP/1.1\r\nHost: master.shirogames.com\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\
         X-Ident: wartales\r\nX-Pass: whatever\r\n\r\n",
        wsx::base64(&key)
    );
    (&c).write_all(req.as_bytes()).unwrap();
    let mut br = BufReader::new(c.try_clone().unwrap());
    loop {
        let mut line = String::new();
        br.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        assert!(
            !line.starts_with("HTTP/") || line.contains("101"),
            "handshake refused: {line}"
        );
    }
    Ws { c, br, uid: 0 }
}

impl Ws {
    fn write_masked(&mut self, payload: &[u8]) {
        let n = u16::try_from(payload.len()).expect("a test frame needs no 64 bit length");
        let mut f = vec![0x81];
        if n < 126 {
            f.push(0x80 | u8::try_from(n).unwrap());
        } else {
            f.push(0x80 | 0x7e);
            f.extend(n.to_be_bytes());
        }
        let mut mask = [0u8; 4];
        crate::rand_bytes(&mut mask);
        f.extend(mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        (&self.c).write_all(&f).unwrap();
    }

    fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut h = [0u8; 2];
        self.br.read_exact(&mut h)?;
        let mut n = u64::from(h[1] & 127);
        if n == 126 {
            let mut b = [0u8; 2];
            self.br.read_exact(&mut b)?;
            n = u64::from(u16::from_be_bytes(b));
        } else if n == 127 {
            let mut b = [0u8; 8];
            self.br.read_exact(&mut b)?;
            n = u64::from_be_bytes(b);
        }
        let mut buf = vec![0u8; usize::try_from(n).unwrap()];
        self.br.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn frame(&mut self) -> Value {
        serde_json::from_slice(&self.read_frame().expect("the master closed the socket")).unwrap()
    }

    /// Fires a command without waiting for its reply and returns its uid.
    fn send(&mut self, cmd: &str, args: Value) -> i64 {
        self.uid += 1;
        let f = json!({"uid": self.uid, "cmd": cmd, "args": args});
        self.write_masked(f.to_string().as_bytes());
        self.uid
    }

    /// Reads frames until the reply to uid arrives, answering pushes on the
    /// way: its args (null when absent), or the error text.
    fn await_reply(&mut self, uid: i64) -> Result<Value, String> {
        loop {
            let e = self.frame();
            let n = e["uid"].as_i64().unwrap_or(0);
            if n > 0 {
                self.write_masked(json!({"uid": -n, "args": true}).to_string().as_bytes());
                continue;
            }
            if n != -uid {
                continue;
            }
            let args = e.get("args").cloned().unwrap_or(Value::Null);
            if e["cmd"] == "err" {
                return Err(args.as_str().unwrap_or_default().into());
            }
            return Ok(args);
        }
    }

    fn call(&mut self, cmd: &str, args: Value) -> Value {
        let n = self.send(cmd, args);
        self.await_reply(n)
            .unwrap_or_else(|e| panic!("{cmd} failed: {e}"))
    }

    fn call_err(&mut self, cmd: &str, args: Value) -> String {
        let n = self.send(cmd, args);
        match self.await_reply(n) {
            Ok(v) => panic!("{cmd} succeeded ({v}), want an error"),
            Err(e) => e,
        }
    }

    /// Waits for a server initiated command and answers it the way the game
    /// does ({"uid": -n, "args": true}).
    fn read_push(&mut self, cmd: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no {cmd} push arrived");
            self.br.get_ref().set_read_timeout(Some(left)).unwrap();
            let Ok(f) = self.read_frame() else {
                panic!("no {cmd} push arrived")
            };
            let e: Value = serde_json::from_slice(&f).unwrap();
            let n = e["uid"].as_i64().unwrap_or(0);
            let got = e["cmd"].as_str().unwrap_or("");
            if n <= 0 || got.is_empty() {
                continue; // a reply to one of our own calls
            }
            self.write_masked(json!({"uid": -n, "args": true}).to_string().as_bytes());
            if got == cmd {
                self.br.get_ref().set_read_timeout(None).unwrap();
                return e.get("args").cloned().unwrap_or(Value::Null);
            }
        }
    }

    /// Fails if anything at all is pushed within d.
    fn expect_no_push(&mut self, d: Duration) {
        self.br.get_ref().set_read_timeout(Some(d)).unwrap();
        let mut h = [0u8; 2];
        let got = self.br.read_exact(&mut h);
        self.br.get_ref().set_read_timeout(None).unwrap();
        assert!(
            got.is_err(),
            "a frame arrived where none was due (a push to the wrong peer, or a second reply to one request)"
        );
    }

    fn close(&self) {
        crate::conn::close_tcp(&self.c);
    }

    fn login(&mut self, name: &str, game_id: &str) -> Value {
        self.call(
            "user/login",
            json!({"name": name, "uid": game_id, "version": 2}),
        )
    }
}

// ------------------------------------------------------------ fixtures

fn fixed_endpoint(ep: nat::Endpoint) -> Option<Arc<EndpointFn>> {
    Some(Arc::new(move || Ok(ep.clone())))
}

fn fixed_sdr(st: Status) -> Option<Arc<dyn Fn() -> Status + Send + Sync>> {
    Some(Arc::new(move || st.clone()))
}

fn ip4(a: u8, b: u8, c: u8, d: u8) -> Option<IpAddr> {
    Some(IpAddr::V4(Ipv4Addr::new(a, b, c, d)))
}

/// The verified-reachable host: the direct relay is chosen.
fn public_endpoint() -> nat::Endpoint {
    nat::Endpoint {
        addr: "203.0.113.7:14250".into(),
        ip: ip4(203, 0, 113, 7),
        source: "UPnP",
        reachable: true,
        verified: true,
        ..nat::Endpoint::default()
    }
}

/// A host nobody on the internet can reach.
fn lan_endpoint() -> nat::Endpoint {
    nat::Endpoint {
        addr: "192.168.1.5:14250".into(),
        ip: ip4(192, 168, 1, 5),
        source: "LAN",
        warning: "no public address found".into(),
        ..nat::Endpoint::default()
    }
}

fn sdr_ok() -> Status {
    Status {
        known: true,
        ok: true,
        ..Status::default()
    }
}

/// A master on a free port, closed when dropped.
struct Master {
    srv: Arc<Server>,
    addr: String,
}

impl Drop for Master {
    fn drop(&mut self) {
        self.srv.close();
    }
}

/// A master with the public endpoint verified reachable and SDR ready; m
/// adjusts the options. public_addr follows endpoint unless m set it.
fn start_master(m: impl FnOnce(&mut Options)) -> Master {
    let mut o = Options {
        relay_port: 14250,
        host_pw: "hpw".into(),
        slave_pw: "spw".into(),
        endpoint: fixed_endpoint(public_endpoint()),
        sdr_status: fixed_sdr(sdr_ok()),
        ..Options::default()
    };
    m(&mut o);
    if o.public_addr.is_none() {
        let ep = o.endpoint.clone();
        o.public_addr = Some(Arc::new(move || match &ep {
            Some(f) => f().map(|e| e.addr),
            None => Err(anyhow!("no endpoint resolver")),
        }));
    }
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap().to_string();
    o.addr.clone_from(&addr);
    let srv = Server::new(o);
    let s = srv.clone();
    std::thread::spawn(move || s.serve(&ln));
    Master { srv, addr }
}

fn start_master_with(ep: nat::Endpoint, sdr: Status) -> Master {
    start_master(|o| {
        o.endpoint = fixed_endpoint(ep);
        o.sdr_status = fixed_sdr(sdr);
    })
}

/// A running bridge; dropping it stops and joins it.
struct Br {
    b: Arc<Bridge>,
    stop: Arc<AtomicBool>,
    h: Option<JoinHandle<()>>,
}

impl Drop for Br {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.h.take().unwrap().join();
    }
}

fn run_bridge(b: Arc<Bridge>) -> Br {
    let stop = Arc::new(AtomicBool::new(false));
    let (b2, s) = (b.clone(), stop.clone());
    Br {
        b,
        stop,
        h: Some(std::thread::spawn(move || b2.run(&s))),
    }
}

/// Runs a bridge on a fake SDR endpoint and waits for it to be ready (or not,
/// when the endpoint says SDR is unusable).
fn start_bridge(wire: Arc<crate::sdrbridge::switch::Endpoint>, want_ready: bool) -> Br {
    let br = run_bridge(Bridge::new(wire, Log::discard()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while want_ready && !br.b.ready().0 {
        let why = br.b.ready().1;
        assert!(
            Instant::now() < deadline,
            "bridge never became ready: {why}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    br
}

fn sdr_opts(o: &mut Options, b: &Br) {
    let b2 = b.b.clone();
    o.sdr_status = Some(Arc::new(move || b2.status()));
    o.bridge = Some(b.b.clone());
}

/// Streams opened by remote peers go to srv's SDR link.
fn on_peer(b: &Br, srv: &Arc<Server>) {
    let w = Arc::downgrade(srv);
    b.b.set_on_peer(move |c, peer| {
        if let Some(s) = w.upgrade() {
            s.serve_sdr_link(&c, peer);
        }
    });
}

/// Serves proxy-links on a fresh loopback port (what the relay's dispatch
/// hands serve_link) and returns its address.
fn link_listener(srv: &Arc<Server>) -> String {
    let ln = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap().to_string();
    let w = Arc::downgrade(srv);
    std::thread::spawn(move || {
        for c in ln.incoming() {
            let (Ok(c), Some(s)) = (c, w.upgrade()) else {
                return;
            };
            std::thread::spawn(move || s.serve_link(&(Arc::new(c) as ConnRef)));
        }
    });
    addr
}

const CASCADE_HOST_ID: u64 = 0x0110_0001_00BC_614E;

/// A host master with both routes on offer: an SDR identity on the switch
/// and, with listen, a TCP proxy-link listener.
fn cascade_host(sw: &Switch, host_id64: u64, listen: bool) -> (Master, Br, String) {
    let br = start_bridge(sw.add(host_id64), true);
    let m = start_master(|o| {
        sdr_opts(o, &br);
        o.link_key = 0xdead_beef;
    });
    on_peer(&br, &m.srv);
    let addr = if listen {
        link_listener(&m.srv)
    } else {
        String::new()
    };
    (m, br, addr)
}

/// The code a host at addr (or none) with the given key would issue.
fn combined_code(addr: &str, host_id64: u64, key: u32) -> String {
    let (account_id, _) = code::account_id(host_id64);
    let st = code::Steam {
        flags: 0,
        account_id,
        key,
    };
    if addr.is_empty() {
        return code::encode_steam(st);
    }
    let a: std::net::SocketAddr = addr.parse().unwrap();
    code::encode_combined(code::Endpoint::new(a.ip(), a.port()), st).unwrap()
}

/// A guest master on the switch with its own bridge (or an unusable one), its
/// game logged in.
fn guest_for(
    sw: &Switch,
    guest_id64: u64,
    sdr_usable: bool,
    m: impl FnOnce(&mut Options),
) -> (Ws, Master, Br) {
    let status = if sdr_usable {
        sw.add(guest_id64)
    } else {
        sw.unavailable("steam_api64.dll is not loaded in this process")
    };
    let br = start_bridge(status, sdr_usable);
    let ms = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        sdr_opts(o, &br);
        m(o);
    });
    let mut g = dial(&ms.addr);
    g.login("Guest", &uid::from_steam_id64(guest_id64));
    (g, ms, br)
}

/// Logs the host's game in and creates a lobby.
fn create_lobby(m: &Master, host_id64: u64) -> (Ws, String) {
    let mut h = dial(&m.addr);
    h.login("Host", &uid::from_steam_id64(host_id64));
    let id = h.call("lobby/create", json!({"props": {"maxPlayers": 4}}));
    (h, id.as_str().unwrap().into())
}

fn map_code(m: &Master, short: &str, id: &str) {
    m.srv.lobbies().codes.insert(short.into(), id.into());
}

fn resolve_ok(g: &mut Ws, short: &str, want: &str) {
    let v = g.call(
        "lobby/resolveShortCode",
        json!({"shortCode": short, "filters": {}}),
    );
    assert_eq!(v["id"], want, "resolveShortCode = {v}");
}

/// haxe.Serializer's encoding of a plain string.
fn haxe_string(s: &str) -> String {
    format!("y{}:{s}", s.len())
}

fn decode(s: &str) -> code::Code {
    code::decode_any(s).unwrap_or_else(|e| panic!("{s}: {e:#}"))
}

fn ids(info: &Value) -> Vec<String> {
    info["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_str().unwrap().to_string())
        .collect()
}

// ------------------------------------------------------------ fake peers

#[derive(Default)]
struct Fake {
    uid: String,
    game: String,
    steam: String,
    name: String,
    remote: bool,
}

impl Peer for Fake {
    fn user_id(&self) -> String {
        self.uid.clone()
    }
    fn steam_id(&self) -> String {
        self.steam.clone()
    }
    fn game_id(&self) -> String {
        self.game.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn remote(&self) -> bool {
        self.remote
    }
    fn push(&self, _: &str, _: &RawValue) {}
}

fn fake(uid: &str, game: &str, name: &str, remote: bool) -> PeerRef {
    Arc::new(Fake {
        uid: uid.into(),
        game: game.into(),
        name: name.into(),
        remote,
        ..Fake::default()
    })
}

fn quiet_server(m: Option<Info>) -> Arc<Server> {
    Server::new(Options {
        r#mod: m.map(modver::Session::fixed),
        ..Options::default()
    })
}

fn lobby_of(id: &str, owner: &PeerRef, t: Transport) -> Lobby {
    Lobby {
        id: id.into(),
        owner: owner.game_id(),
        data: Map::new(),
        is_private: None,
        max_players: None,
        users: vec![Member {
            id: owner.game_id(),
            name: owner.name(),
            data: None,
            peer: owner.clone(),
        }],
        transport: t,
    }
}

fn insert(s: &Server, l: Lobby) {
    s.lobbies().by_id.insert(l.id.clone(), l);
}

// ------------------------------------------------------------ over SDR

/// The whole point of the SDR bridge: a host with NO public endpoint issues a
/// join code that carries its SteamID, and a guest's master reaches the
/// host's master through the (fake) SDR bridge, with no address anywhere.
#[test]
fn join_over_sdr() {
    let sw = Switch::new();
    let (host_id64, guest_id64) = (CASCADE_HOST_ID, 0x0110_0001_0000_0007);
    let (host_steam, guest_steam) = (
        uid::from_steam_id64(host_id64),
        uid::from_steam_id64(guest_id64),
    );
    let hb = start_bridge(sw.add(host_id64), true);
    let hm = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        sdr_opts(o, &hb);
        o.link_key = 0xdead_beef;
    });
    on_peer(&hb, &hm.srv);
    let gb = start_bridge(sw.add(guest_id64), true);
    let gm = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        sdr_opts(o, &gb);
        o.link_key = 0x0102_0304; // its own key; irrelevant for joining
    });

    let mut host = dial(&hm.addr);
    host.login("Host", &host_steam);
    let id = host.call("lobby/create", json!({"props": {"maxPlayers": 4}}));
    let id = id.as_str().unwrap();
    let short = host.call("lobby/initInvite", json!({"id": id}));
    let short = short.as_str().unwrap();
    // The host's endpoint is LAN-only, unverified: the code still offers it
    // first (a LAN guest would take it) and the SDR route after.
    let sc = decode(short);
    let st = sc.steam.expect("an SDR route");
    assert!(
        st.steam_id64() == host_id64
            && st.key == 0xdead_beef
            && sc.endpoint.map(|e| e.addr().to_string()) == Some(lan_endpoint().addr),
        "join code {short} = {sc:?}"
    );

    let mut guest = dial(&gm.addr);
    guest.login("Guest", &guest_steam);
    let info = guest.call(
        "lobby/resolveShortCode",
        json!({"shortCode": short, "filters": {}}),
    );
    assert!(
        info["id"] == id && info["owner"] == host_steam.as_str(),
        "resolve over SDR = {info}"
    );
    let info = guest.call(
        "lobby/join",
        json!({"id": id, "data": r#"{"ready":false}"#}),
    );
    assert_eq!(
        ids(&info),
        [host_steam.clone(), guest_steam.clone()],
        "join over SDR = {info}"
    );
    let join = host.read_push("lobby/join");
    assert!(
        join.to_string().contains(&guest_steam),
        "lobby/join push = {join}"
    );
    guest.call("lobby/chat", json!({"id": id, "msg": "y1:x"}));
    let chat = host.read_push("lobby/chat");
    assert!(
        chat.to_string().contains(&guest_steam),
        "lobby/chat push = {chat}"
    );

    // A code with the wrong key is refused by the host's master, not served.
    let ib = start_bridge(sw.add(0x0110_0001_0000_0099), true);
    let im = start_master(|o| sdr_opts(o, &ib));
    let mut intruder = dial(&im.addr);
    intruder.login("Intruder", &uid::from_steam_id64(0x0110_0001_0000_0099));
    let tampered = code::encode_steam(code::Steam {
        flags: 0,
        account_id: 0x00BC_614E,
        key: 0xdead_beef ^ 1,
    });
    let msg = intruder.call_err(
        "lobby/resolveShortCode",
        json!({"shortCode": tampered, "filters": {}}),
    );
    assert!(msg.contains("Invalid join code"), "tampered code: {msg:?}");
}

/// The endpoint answers, so the guest joins over TCP even though its own SDR
/// is unusable.
#[test]
fn cascade_direct_wins() {
    let sw = Switch::new();
    let (hm, _hb, link) = cascade_host(&sw, CASCADE_HOST_ID, true);
    let (_h, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = combined_code(&link, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0007, false, |_| {});
    resolve_ok(&mut g, &short, &id);
}

const PACKET: &str = "wy26:mpman.net.LobbyMessageDatay6:Packet:2s12:/v8AAQIDBAUGBw==";

/// The lobby transport with the guest behind a proxy-link (direct TCP): the
/// guest's packet reaches the host's game as a lobby/chat push, and back.
#[test]
fn lobby_transport_over_link() {
    let sw = Switch::new();
    let (hm, _hb, link) = cascade_host(&sw, CASCADE_HOST_ID, true);
    let (mut host, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let a: std::net::SocketAddr = link.parse().unwrap();
    let short = code::encode_direct(code::Endpoint::new(a.ip(), a.port())).unwrap();
    map_code(&hm, &short, &id);
    let (mut guest, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0007, false, |_| {});
    resolve_ok(&mut guest, &short, &id);
    let info = guest.call("lobby/join", json!({"id": id, "data": "og"}));
    let users = ids(&info);
    assert_eq!(users.len(), 2, "join over the link = {info}");
    let (host_uid, guest_uid) = (
        info["owner"].as_str().unwrap().to_string(),
        users[1].clone(),
    );
    let join = host.read_push("lobby/join");
    assert!(
        join.to_string().contains(&guest_uid),
        "lobby/join push = {join}"
    );

    // Each game addresses the packet to the member id it knows, and gets it
    // back addressed to the id it calls its own (LobbyService.hx:101).
    let (host_game, guest_game) = (
        uid::from_steam_id64(CASCADE_HOST_ID),
        uid::from_steam_id64(0x0110_0001_0000_0007),
    );
    guest.call(
        "lobby/chat",
        json!({"id": id, "msg": PACKET.to_string() + &haxe_string(&host_uid)}),
    );
    let chat = host.read_push("lobby/chat");
    assert!(
        chat["id"] == id.as_str()
            && chat["uid"] == guest_uid.as_str()
            && chat["msg"] == PACKET.to_string() + &haxe_string(&host_game),
        "lobby/chat push on the host = {chat}"
    );
    host.call(
        "lobby/chat",
        json!({"id": id, "msg": PACKET.to_string() + &haxe_string(&guest_uid)}),
    );
    let chat = guest.read_push("lobby/chat");
    assert!(
        chat["id"] == id.as_str()
            && chat["uid"] == host_uid.as_str()
            && chat["msg"] == PACKET.to_string() + &haxe_string(&guest_game),
        "lobby/chat push on the guest = {chat}"
    );
    host.expect_no_push(Duration::from_millis(250));
}

/// The guest of the 2026-09-18 session: the game opens a second master socket
/// and drops it at once, and sends user/login again before every command on
/// the socket it keeps. The host's pushes must still reach that socket.
#[test]
fn guest_push_survives_second_socket_and_relogin() {
    let sw = Switch::new();
    let (hm, _hb, link) = cascade_host(&sw, CASCADE_HOST_ID, true);
    let (mut host, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = combined_code(&link, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let (mut guest, gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0007, false, |_| {});
    let second = dial(&gm.addr);
    second.close(); // master2: connected, never logs in, EOF at once
    std::thread::sleep(Duration::from_millis(100));

    let me = uid::from_steam_id64(0x0110_0001_0000_0007);
    guest.login("Guest", &me);
    resolve_ok(&mut guest, &short, &id);
    guest.login("Guest", &me);
    let info = guest.call("lobby/join", json!({"id": id, "data": "og"}));
    host.read_push("lobby/join");
    guest.login("Guest", &me);
    host.call("lobby/chat", json!({"id": id, "msg": format!("wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::P8{}", haxe_string(&me))}));
    let chat = guest.read_push("lobby/chat");
    assert!(
        chat["id"] == id.as_str() && chat["uid"] == info["owner"],
        "lobby/chat push on the guest = {chat}"
    );
}

/// The player clicks Join again on a lobby the game is already in: the game
/// fires lobby/leave, user/login and lobby/join back to back; the host must
/// end up with the guest as a member exactly once, which needs the commands
/// forwarded in the order they came.
#[test]
fn guest_rejoin_burst_keeps_membership() {
    let sw = Switch::new();
    let (hm, _hb, link) = cascade_host(&sw, CASCADE_HOST_ID, true);
    let (mut host, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = combined_code(&link, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let gid = 0x0110_0001_0000_0007;
    let (mut guest, _gm, _gb) = guest_for(&sw, gid, false, |_| {});
    resolve_ok(&mut guest, &short, &id);
    guest.call("lobby/join", json!({"id": id, "data": "og"}));
    host.read_push("lobby/join");

    let login = json!({"name": "Guest", "uid": uid::from_steam_id64(gid), "version": 2});
    let mut guest_uid = String::new();
    for _ in 0..5 {
        let leave = guest.send("lobby/leave", json!({"id": id}));
        guest.send("user/login", login.clone());
        let join = guest.send("lobby/join", json!({"id": id, "data": "og"}));
        guest.await_reply(leave).expect("lobby/leave");
        let info = guest.await_reply(join).expect("lobby/join");
        let users = ids(&info);
        assert_eq!(users.len(), 2, "join after leave = {info}");
        guest_uid.clone_from(&users[1]);
    }
    // Every request got exactly one reply: the game raises "Missing callb
    // #<uid>" (Connection.hx:129) on a second one.
    guest.expect_no_push(Duration::from_millis(250));
    let l = hm.srv.lobbies();
    let members: Vec<&str> = l.by_id[&id].users.iter().map(|u| u.id.as_str()).collect();
    assert!(
        members.len() == 2 && members[1] == guest_uid,
        "host lobby members = {members:?}"
    );
}

/// The endpoint in the code refuses the connection, so the guest falls
/// through to SDR, fast.
#[test]
fn cascade_direct_refused_falls_back_to_sdr() {
    let sw = Switch::new();
    let (hm, _hb, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let (_h, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let short = combined_code(&dead, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0008, true, |_| {});
    let start = Instant::now();
    resolve_ok(&mut g, &short, &id);
    // Windows retries a refused loopback SYN for ~2 s; fast = before the direct timeout.
    assert!(
        start.elapsed() < DEFAULT_DIRECT_TIMEOUT,
        "a refused port must fail fast, took {:?}",
        start.elapsed()
    );
}

/// The endpoint swallows the SYN (a firewall), the bounded attempt expires,
/// and the guest joins over SDR.
#[test]
fn cascade_direct_times_out_falls_back_to_sdr() {
    let sw = Switch::new();
    let (hm, _hb, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let (_h, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = combined_code("203.0.113.9:14250", CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let timeout = Duration::from_millis(300);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0009, true, |o| {
        o.direct_timeout = timeout;
        o.dial_direct = Some(Arc::new(|_, t| {
            std::thread::sleep(t); // a black hole: nothing ever answers
            Err(io::ErrorKind::TimedOut.into())
        }));
    });
    let start = Instant::now();
    resolve_ok(&mut g, &short, &id);
    let d = start.elapsed();
    assert!(
        d >= timeout && d <= timeout + Duration::from_secs(2),
        "took {d:?}"
    );
}

/// The address in the code now answers as somebody else's helper. Its master
/// refuses the key, the cascade drops that link and reaches the real host
/// over SDR.
#[test]
fn cascade_wrong_host_behind_the_endpoint() {
    let sw = Switch::new();
    let (hm, _hb, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let (_h, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let stranger = start_master(|o| o.link_key = 0x0102_0304);
    let link = link_listener(&stranger.srv);
    let short = combined_code(&link, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0010, true, |_| {});
    resolve_ok(&mut g, &short, &id);
}

/// Nothing works and the error names both routes.
#[test]
fn cascade_both_fail() {
    let sw = Switch::new();
    let dead = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let short = combined_code(&dead, 0x0110_0001_0000_0999, 0xdead_beef); // nobody on the switch has that id
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0011, true, |_| {});
    let msg = g.call_err(
        "lobby/resolveShortCode",
        json!({"shortCode": short, "filters": {}}),
    );
    assert!(
        msg.contains(&format!("direct {dead}"))
            && msg.contains("SDR to SteamID")
            && msg.contains("EResult 3"),
        "both-fail error = {msg:?}"
    );
}

/// A host with an unverified endpoint and a ready bridge issues a direct
/// manual code and a combined Steam invite; the lobby is direct-marked.
#[test]
fn manual_code_and_steam_invite_routes() {
    let sw = Switch::new();
    let hinted = nat::Endpoint {
        addr: "45.154.88.66:14250".into(),
        ip: ip4(45, 154, 88, 66),
        source: "UPnP+STUN",
        reachable: true,
        ..nat::Endpoint::default()
    };
    let br = start_bridge(sw.add(CASCADE_HOST_ID), true);
    let m = start_master(|o| {
        o.endpoint = fixed_endpoint(hinted.clone());
        sdr_opts(o, &br);
        o.link_key = 0xdead_beef;
    });
    let (mut host, id) = create_lobby(&m, CASCADE_HOST_ID);
    assert_eq!(
        m.srv.lobbies().by_id[&id].transport,
        Transport::Direct,
        "every auto lobby is direct-marked"
    );
    let short = host.call("lobby/makeShortCode", json!({"id": id}));
    let short = short["shortCode"].as_str().unwrap().to_string();
    let c = decode(&short);
    assert!(
        c.steam.is_none() && short.len() == 8,
        "manual code {short} = {c:?}"
    );
    assert_eq!(c.endpoint.unwrap().addr().to_string(), hinted.addr);
    resolve_ok(&mut host, &short, &id);
    let invite = host.call("lobby/initInvite", json!({"id": id}));
    let c = decode(invite.as_str().unwrap());
    let st = c.steam.expect("both routes");
    assert_eq!(c.endpoint.unwrap().addr().to_string(), hinted.addr);
    assert!(
        st.steam_id64() == CASCADE_HOST_ID && st.key == 0xdead_beef,
        "{st:?}"
    );
}

/// Manual codes fail directly rather than silently using the host's Steam
/// route; the separate invitation still connects after that failed attempt.
#[test]
fn manual_code_does_not_fall_back_to_steam() {
    let sw = Switch::new();
    let (hm, _hb, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let (mut host, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = host.call("lobby/makeShortCode", json!({"id": id}));
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0012, true, |o| {
        o.dial_direct = Some(Arc::new(
            |_, _| Err(io::ErrorKind::ConnectionRefused.into()),
        ));
    });
    let msg = g.call_err(
        "lobby/resolveShortCode",
        json!({"shortCode": short["shortCode"]}),
    );
    assert!(
        msg.contains("Cannot reach the host: direct ") && !msg.contains("SDR"),
        "manual direct failure = {msg:?}"
    );
    let invite = host.call("lobby/initInvite", json!({"id": id}));
    resolve_ok(&mut g, invite.as_str().unwrap(), &id);
}

#[test]
fn steam_invite_without_endpoint() {
    let sw = Switch::new();
    let br = start_bridge(sw.add(CASCADE_HOST_ID), true);
    let m = start_master(|o| {
        o.public_addr = Some(Arc::new(|| {
            Err(anyhow!("use of closed network connection"))
        }));
        o.endpoint = Some(Arc::new(|| {
            Err(anyhow!("use of closed network connection"))
        }));
        sdr_opts(o, &br);
        o.link_key = 0xdead_beef;
    });
    on_peer(&br, &m.srv);
    let (mut host, id) = create_lobby(&m, CASCADE_HOST_ID);
    let msg = host.call_err("lobby/makeShortCode", json!({"id": id}));
    assert!(
        msg.contains("use a Steam invitation"),
        "manual code without endpoint = {msg:?}"
    );
    let invite = host.call("lobby/initInvite", json!({"id": id}));
    let c = decode(invite.as_str().unwrap());
    assert!(
        c.endpoint.is_none() && c.steam.is_some(),
        "Steam-only invite = {c:?}"
    );
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0013, true, |_| {});
    resolve_ok(&mut g, invite.as_str().unwrap(), &id);
}

/// A Steam invitation request or a join attempt made while SDR is still
/// coming up is answered with a clear, retryable error.
#[test]
fn sdr_join_before_the_bridge_is_ready() {
    let sw = Switch::new();
    let pending = sw.pending(CASCADE_HOST_ID, "Steam API not initialised yet");
    let br = start_bridge(pending.clone(), false);
    let m = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        sdr_opts(o, &br);
    });
    let mut w = dial(&m.addr);
    w.login("Host", "S4e61bc0000000000");
    let id = w.call("lobby/create", json!({"props": {"maxPlayers": 4}}));
    let id = id
        .as_str()
        .expect("lobby/create must succeed with SDR pending");
    // Manual codes depend only on the endpoint, even while Steam starts.
    let manual = w.call("lobby/makeShortCode", json!({"id": id}));
    let c = decode(manual["shortCode"].as_str().unwrap());
    assert!(c.endpoint.is_some() && c.steam.is_none(), "{c:?}");
    let msg = w.call_err("lobby/initInvite", json!({"id": id}));
    assert!(
        msg.contains("Steam relay not ready yet") && msg.contains("not initialised"),
        "initInvite while pending = {msg:?}"
    );
    let steam_code = code::encode_steam(code::Steam {
        flags: 0,
        account_id: 1,
        key: 2,
    });
    let msg = w.call_err(
        "lobby/resolveShortCode",
        json!({"shortCode": steam_code, "filters": {}}),
    );
    assert!(
        msg.contains("Steam relay not ready yet"),
        "resolveShortCode while pending = {msg:?}"
    );

    // Once SDR is reported, the same lobby gets its code.
    pending.set_status(Status {
        known: true,
        ok: true,
        reason: String::new(),
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !br.b.ready().0 {
        assert!(
            Instant::now() < deadline,
            "bridge did not pick the new status up"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let invite = w.call("lobby/initInvite", json!({"id": id}));
    assert!(
        decode(invite.as_str().unwrap()).steam.is_some(),
        "initInvite once ready = {invite}"
    );
}

#[test]
fn login_and_instance_get() {
    let m = start_master(|_| {});
    let mut w = dial(&m.addr);
    let login = w.call(
        "user/login",
        json!({"name": "Tester", "token": "1527950@x", "uid": "S0011223344556677", "version": 2}),
    );
    let sid = login["sid"].as_str().unwrap_or("");
    assert!(sid.starts_with('X'), "sid = {sid:?}, want a Session id");
    assert!(
        login["version"] == 2 && login["time"].as_f64().unwrap_or(0.0) > 0.0,
        "login reply = {login}"
    );
    assert_eq!(login["perm"], "");
    // instance/get must route the host onto our local relay; host:port:pass -
    // the client's RelayP2P parser pops the pass unconditionally.
    let inst = w.call(
        "instance/get",
        json!({"game": "p2p", "version": "*", "zone": "eu", "startParams": "{}", "clientVersion": 1}),
    );
    assert_eq!(inst["serverID"], "R127.0.0.1:14250:hpw");
    assert_eq!(
        inst["serverStartAnswer"],
        json!({"hostpw": "hpw", "slavepw": "spw"})
    );
    // The fixed answers keep their shapes.
    assert_eq!(w.call("code/redeem", json!({})), json!({"code": 0}));
    assert_eq!(
        w.call("instance/ingame", json!({})),
        json!({"inGame": false})
    );
    assert_eq!(w.call("insight/event", json!({})), Value::Null);
    let s = w.call("user/session", json!({"uid": "S0011223344556677"}));
    assert!(s["accept"] == true && s["version"] == 2, "{s}");
    assert_eq!(w.call_err("nope/x", json!({})), "Unknown command nope/x");
}

/// The wire shape of a reply: an answer without args has no "args" field at
/// all, an error carries its text, a push its own uid.
#[test]
fn reply_frames_are_the_games() {
    let m = start_master(|_| {});
    let mut w = dial(&m.addr);
    let n = w.send("insight/event", json!({}));
    let f = w.read_frame().unwrap();
    assert_eq!(String::from_utf8(f).unwrap(), format!(r#"{{"uid":-{n}}}"#));
    let n = w.send("lobby/info", json!({"id": "Lnope"}));
    assert_eq!(
        String::from_utf8(w.read_frame().unwrap()).unwrap(),
        format!(r#"{{"uid":-{n}}}"#)
    );
    let n = w.send("lobby/setData", json!({"id": "Lnope"}));
    assert_eq!(
        String::from_utf8(w.read_frame().unwrap()).unwrap(),
        format!(r#"{{"uid":-{n},"cmd":"err","args":"Unknown lobby Lnope"}}"#)
    );
}

/// A lobby on the direct relay names every member by the id its own game
/// calls itself, exactly as an SDR lobby and the players' saves do.
#[test]
fn direct_lobby_names_players_by_game_ids() {
    let m = start_master(|_| {});
    let (host_id, guest_id) = ("S0011223344556677", "S7766554433221100");
    let mut host = dial(&m.addr);
    let login = host.login("Host", host_id);
    assert!(uid::is_session(login["sid"].as_str().unwrap()), "{login}");
    let id = host.call(
        "lobby/create",
        json!({"props": {"data": {}, "isPrivate": false, "maxPlayers": 4}}),
    );
    let id = id.as_str().unwrap();
    assert!(
        id.starts_with('L') && id.ends_with(DIRECT_LOBBY_MARK),
        "direct lobby id = {id:?}"
    );
    let mut guest = dial(&m.addr);
    guest.call(
        "user/session",
        json!({"name": "Guest", "uid": guest_id, "sid": login["sid"], "version": 2}),
    );
    let info = guest.call(
        "lobby/join",
        json!({"id": id, "data": r#"{"ready":false}"#}),
    );
    assert!(
        info["owner"] == host_id && ids(&info) == [host_id, guest_id],
        "lobby/join = {info}"
    );
    let join = host.read_push("lobby/join");
    assert!(
        join.to_string().contains(&format!(r#""uid":"{guest_id}""#)),
        "lobby/join push = {join}"
    );
}

/// A member that never reported its game id cannot join.
#[test]
fn join_without_game_id_is_refused() {
    let s = quiet_server(None);
    let owner = fake("Xowner", "Sowner", "Owner", false);
    insert(&s, lobby_of("L1D", &owner, Transport::Direct));
    let old = fake("Xold", "", "Old", true);
    let err = s.lobby_join(&json!({"id": "L1D"}), &old).unwrap_err();
    assert!(err.to_string().contains("player id"), "{err}");
    assert_eq!(
        s.lobbies().by_id["L1D"].users.len(),
        1,
        "the refused peer was added"
    );
}

/// A lobby on SDR emits every member by its REAL Steam id, so isSteamOnly is
/// true on every game.
#[test]
fn sdr_lobby_emits_real_steam_ids() {
    let m = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        o.transport = MODE_SDR.into();
    });
    let (host_steam, guest_steam) = ("S0011223344556677", "S7766554433221100");
    let mut host = dial(&m.addr);
    let login = host.login("Host", host_steam);
    assert!(
        uid::is_session(login["sid"].as_str().unwrap()),
        "the session id is never Steam shaped"
    );
    let id = host.call(
        "lobby/create",
        json!({"props": {"data": {}, "isPrivate": false, "maxPlayers": 4}}),
    );
    let id = id.as_str().unwrap();
    assert!(!id.ends_with(DIRECT_LOBBY_MARK), "{id}");
    let mut guest = dial(&m.addr);
    guest.login("Guest", guest_steam);
    let info = guest.call(
        "lobby/join",
        json!({"id": id, "data": r#"{"ready":false}"#}),
    );
    assert!(
        info["owner"] == host_steam && ids(&info) == [host_steam, guest_steam],
        "SDR lobby/join = {info}"
    );
    assert_eq!(host.read_push("lobby/join")["uid"], guest_steam);
    guest.call("lobby/chat", json!({"id": id, "msg": "y1:x"}));
    assert_eq!(host.read_push("lobby/chat")["uid"], guest_steam);
    // Ownership checks use the rendered id too.
    host.call("lobby/setData", json!({"id": id, "data": {"k": r#""v""#}}));
    host.call("lobby/transfer", json!({"id": id, "uid": guest_steam}));
    let info = host.call("lobby/info", json!({"id": id}));
    assert_eq!(info["owner"], guest_steam);
}

/// A member that reported no (well-formed) Steam id is still named by its
/// game id; an auto lobby is direct-marked.
#[test]
fn sdr_lobby_without_steam_id_keeps_game_id() {
    let m = start_master_with(lan_endpoint(), Status::default());
    let mut host = dial(&m.addr);
    host.login("Host", "S0011223344556677");
    let id = host.call("lobby/create", json!({"props": {"maxPlayers": 4}}));
    let id = id.as_str().unwrap();
    assert!(id.ends_with(DIRECT_LOBBY_MARK), "{id}");
    let mut guest = dial(&m.addr);
    guest.login("Guest", "Snot-a-steam-id");
    let info = guest.call("lobby/join", json!({"id": id}));
    assert_eq!(ids(&info), ["S0011223344556677", "Snot-a-steam-id"]);
    // A join without data shows "" in the info, and "z" (haxe Int 0) in the push.
    assert_eq!(info["users"][1]["data"], "");
    assert_eq!(host.read_push("lobby/join")["data"], "z");
}

/// No verified endpoint and SDR cannot work: the lobby is refused with the
/// reasons instead of being created on a path that cannot connect.
#[test]
fn no_transport_is_refused() {
    let m = start_master_with(
        lan_endpoint(),
        Status {
            known: true,
            reason: "steam_api64.dll is not loaded in this process".into(),
            ..Status::default()
        },
    );
    let mut host = dial(&m.addr);
    host.login("Host", "S0011223344556677");
    let msg = host.call_err("lobby/create", json!({"props": {"maxPlayers": 4}}));
    assert!(
        msg.contains("no usable transport")
            && msg.contains("steam_api64.dll is not loaded")
            && msg.contains("not internet-reachable"),
        "refusal = {msg:?}"
    );
}

/// The lobby transport exactly the way mpman drives it: every lobby/chat goes
/// to the other members verbatim, with the sender's uid, never back to the
/// sender.
#[test]
fn lobby_transport() {
    let m = start_master(|_| {});
    let mut host = dial(&m.addr);
    host.login("Host", "S00");
    let id = host.call(
        "lobby/create",
        json!({"props": {"data": {}, "isPrivate": false, "maxPlayers": 4}}),
    );
    let id = id.as_str().unwrap();
    let (host_uid, guest_uid) = ("S00", "S01");
    let mut guest = dial(&m.addr);
    guest.login("Guest", "S01");
    guest.call(
        "lobby/join",
        json!({"id": id, "data": r#"{"ready":false}"#}),
    );
    let join = host.read_push("lobby/join");
    assert_eq!(
        join,
        json!({"id": id, "uid": guest_uid, "name": "Guest", "data": r#"{"ready":false}"#})
    );

    let up = PACKET.to_string() + &haxe_string(host_uid);
    guest.call("lobby/chat", json!({"id": id, "msg": up}));
    assert_eq!(
        host.read_push("lobby/chat"),
        json!({"id": id, "uid": guest_uid, "msg": up})
    );
    let answer = "wy26:mpman.net.LobbyMessageDatay6:Packet:2s8:AAECAwQFBgc=".to_string()
        + &haxe_string(guest_uid);
    host.call("lobby/chat", json!({"id": id, "msg": answer}));
    assert_eq!(
        guest.read_push("lobby/chat"),
        json!({"id": id, "uid": host_uid, "msg": answer})
    );
    // A packet for somebody else, and a plain LobbyMessage, travel verbatim.
    for msg in [
        PACKET.to_string() + &haxe_string("Xnobody"),
        "wy12:LobbyMessagey4:Join:1".to_string() + &haxe_string(guest_uid),
    ] {
        host.call("lobby/chat", json!({"id": id, "msg": msg}));
        assert_eq!(guest.read_push("lobby/chat")["msg"], msg.as_str());
    }
    // The sender never sees its own packet.
    host.expect_no_push(Duration::from_millis(250));
}

#[test]
fn lobby_lifecycle() {
    let m = start_master(|_| {});
    let mut w = dial(&m.addr);
    w.login("Host", "S00");
    let id = w.call(
        "lobby/create",
        json!({"props": {"data": {"version": r#""1.2.3""#}, "isPrivate": false, "maxPlayers": 4, "myData": r#"{"ready":false}"#}}),
    );
    let id = id.as_str().unwrap();
    assert!(
        id.starts_with('L'),
        "lobby id = {id:?}, first char must be a platform char"
    );
    let info = w.call("lobby/info", json!({"id": id}));
    assert_eq!(
        info,
        json!({
            "id": id, "owner": "S00",
            "props": {"data": {"version": r#""1.2.3""#}, "isPrivate": false, "maxPlayers": 4},
            "users": [{"id": "S00", "name": "Host", "data": r#"{"ready":false}"#}],
        })
    );
    assert!(id.ends_with(DIRECT_LOBBY_MARK));
    assert_eq!(w.call("lobby/list", json!({})), json!([info]));
    let short = w.call("lobby/makeShortCode", json!({"id": id}));
    let short = short["shortCode"].as_str().unwrap();
    assert_eq!(short.len(), 8, "{short}");
    // our own code resolves locally, back to the same lobby
    resolve_ok(&mut w, short, id);
    w.call("lobby/leave", json!({"id": id}));
    assert_eq!(w.call("lobby/list", json!({})), json!([]));
}

/// lobby/initInvite answers the join code itself, and lobby/infoInvite takes
/// that code, a bare lobby id, or garbage.
#[test]
fn steam_invite_local() {
    let m = start_master(|_| {});
    let mut w = dial(&m.addr);
    w.login("Host", "S00");
    let id = w.call("lobby/create", json!({"props": {"maxPlayers": 4}}));
    let id = id.as_str().unwrap();
    let invite = w.call("lobby/initInvite", json!({"id": id}));
    let invite = invite
        .as_str()
        .expect("lobby/initInvite answers a JSON string");
    assert!(decode(invite).endpoint.is_some(), "{invite}");
    for v in [invite, id] {
        assert_eq!(
            w.call("lobby/infoInvite", json!({"invite": v}))["id"],
            id,
            "{v}"
        );
    }
    let msg = w.call_err("lobby/infoInvite", json!({"invite": "not-a-code"}));
    assert!(msg.contains("Invalid join code"), "{msg:?}");
    let msg = w.call_err("lobby/initInvite", json!({"id": "Lnope"}));
    assert!(msg.contains("Unknown lobby"), "{msg:?}");
}

/// The Steam friends-list "Join Game": the invite value read from the host's
/// Steam lobby resolves and joins exactly like a typed code, here over SDR.
#[test]
fn steam_join_game_over_sdr() {
    let sw = Switch::new();
    let (host_id64, guest_id64) = (CASCADE_HOST_ID, 0x0110_0001_0000_0007);
    let (host_steam, guest_steam) = (
        uid::from_steam_id64(host_id64),
        uid::from_steam_id64(guest_id64),
    );
    let hb = start_bridge(sw.add(host_id64), true);
    let hm = start_master(|o| {
        o.endpoint = fixed_endpoint(lan_endpoint());
        sdr_opts(o, &hb);
        o.link_key = 0xdead_beef;
    });
    on_peer(&hb, &hm.srv);
    let (mut host, id) = create_lobby(&hm, host_id64);
    let invite = host.call("lobby/initInvite", json!({"id": id}));
    let invite = invite.as_str().unwrap();
    assert_eq!(
        decode(invite).steam.map(|s| s.steam_id64()),
        Some(host_id64)
    );
    let (mut guest, _gm, _gb) = guest_for(&sw, guest_id64, true, |_| {});
    let info = guest.call("lobby/infoInvite", json!({"invite": invite}));
    assert!(
        info["id"] == id.as_str() && info["owner"] == host_steam.as_str(),
        "infoInvite over SDR = {info}"
    );
    let info = guest.call(
        "lobby/join",
        json!({"id": id, "data": r#"{"ready":false}"#}),
    );
    assert_eq!(ids(&info), [host_steam, guest_steam.clone()]);
    assert!(host
        .read_push("lobby/join")
        .to_string()
        .contains(&guest_steam));
    // A guest asking for the code still hands out the host's route.
    let again = guest.call("lobby/initInvite", json!({"id": id}));
    assert_eq!(
        decode(again.as_str().unwrap())
            .steam
            .map(|s| s.steam_id64()),
        Some(host_id64)
    );
}

#[test]
fn rejoin_keeps_replacement_peer() {
    for full in [false, true] {
        let s = quiet_server(None);
        let old = fake("X12345678", "S12345678", "Guest", false);
        let fresh = fake("X12345678", "S12345678", "Guest", false);
        let mut l = lobby_of("Lrejoin", &old, Transport::Direct);
        if full {
            l.max_players = Some(1);
        }
        insert(&s, l);
        s.lobby_join(&json!({"id": "Lrejoin"}), &fresh).unwrap();
        s.peer_gone(&old);
        let l = s.lobbies();
        let lb = &l.by_id["Lrejoin"];
        assert!(
            lb.users.len() == 1 && same(&lb.users[0].peer, &fresh),
            "full={full}: old disconnect removed the replacement member"
        );
    }
}

#[test]
fn old_link_close_preserves_replacement() {
    let (log, lines) = Log::capture();
    let m = start_master(|o| o.log = log);
    let attach = |name: &str| {
        let (local, remote) = pipe();
        let r = remote.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while matches!(r.read(&mut b), Ok(n) if n > 0) {}
        });
        let u = link::User {
            id: "S12345678".into(),
            ..link::User::default()
        };
        let cl = m
            .srv
            .attach_link(local, &u, name, "", Duration::from_secs(1))
            .unwrap();
        (cl, remote)
    };
    let (_old, old_remote) = attach("old");
    let (replacement, _r) = attach("new");
    old_remote.close();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !lines
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("master: old closed"))
    {
        assert!(Instant::now() < deadline, "old close callback did not run");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        m.srv
            .state()
            .client
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &replacement)),
        "old close callback cleared replacement link"
    );
}

/// The commands that carry secrets are logged by name only.
#[test]
fn secret_payloads_not_logged() {
    for cmd in SECRET_CMDS {
        assert_eq!(
            payload(cmd, r#"{"token":"SECRET"}"#),
            "(not logged)",
            "{cmd}"
        );
    }
    assert_eq!(payload("lobby/list", r#"{"a":1}"#), r#"{"a":1}"#);
}

// ------------------------------------------------------------ auto invite

fn lobby_invite(sw: &Switch) -> String {
    sw.lobby_invites
        .lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(2))
        .expect("native lobby state was not published")
        .invite
}

#[test]
fn host_automatically_publishes_steam_lobby() {
    for ending in ["leave", "disconnect", "close"] {
        let sw = Switch::new();
        let (m, _b, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
        let (mut host, id) = create_lobby(&m, CASCADE_HOST_ID);
        let invite = lobby_invite(&sw);
        let c = decode(&invite);
        assert!(
            c.endpoint.is_none() && c.steam.map(|s| s.steam_id64()) == Some(CASCADE_HOST_ID),
            "{ending}: {c:?}"
        );
        assert_eq!(
            host.call("lobby/infoInvite", json!({"invite": invite}))["id"],
            id.as_str()
        );
        match ending {
            "leave" => {
                host.call("lobby/leave", json!({"id": id}));
            }
            "disconnect" => host.close(),
            _ => m.srv.close(),
        }
        assert_eq!(
            lobby_invite(&sw),
            "",
            "{ending}: owner departure did not clear native lobby"
        );
    }
}

#[test]
fn auto_invite_before_bridge_ready_without_endpoint() {
    let sw = Switch::new();
    let b = Bridge::new(sw.add(CASCADE_HOST_ID), Log::discard());
    let b2 = b.clone();
    let m = start_master(|o| {
        o.endpoint = None;
        o.sdr_status = Some(Arc::new(move || b2.status()));
        o.bridge = Some(b.clone());
        o.link_key = 0xdead_beef;
    });
    let (_host, id) = create_lobby(&m, CASCADE_HOST_ID);
    let _running = run_bridge(b);
    let invite = lobby_invite(&sw);
    assert_eq!(
        m.srv.lobbies().codes.get(&invite),
        Some(&id),
        "delayed invite did not resolve to the created lobby"
    );
}

#[test]
fn auto_invite_transfer_and_stale_peer() {
    let sw = Switch::new();
    let (m, _b, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let s = &m.srv;
    let (host, id) = create_lobby(&m, CASCADE_HOST_ID);
    let invite = lobby_invite(&sw);
    let old: PeerRef = s.local_session().unwrap();
    let guest: PeerRef = Arc::new(Fake {
        uid: "Xguest".into(),
        game: "S76561197960265799".into(),
        steam: "S76561197960265799".into(),
        name: "Guest".into(),
        remote: true,
    });
    // A remote guest creating its own lobby cannot replace our publication.
    s.lobby_create(&json!({}), &guest).unwrap();
    let guest_id = {
        let mut l = s.lobbies();
        assert_eq!(
            l.invite_lobby, id,
            "remote lobby replaced local publication"
        );
        let lb = l.by_id.get_mut(&id).unwrap();
        let gid = lb.id_of(&guest);
        lb.users.push(Member {
            id: gid.clone(),
            name: "Guest".into(),
            data: None,
            peer: guest.clone(),
        });
        gid
    };
    s.lobby_transfer(&json!({"id": id, "uid": guest_id}), &old)
        .unwrap();
    assert_eq!(
        lobby_invite(&sw),
        "",
        "transfer to remote owner kept publishing"
    );
    s.lobby_transfer(&json!({"id": id, "uid": old.game_id()}), &guest)
        .unwrap();
    assert_eq!(
        lobby_invite(&sw),
        invite,
        "transfer back did not restore local publication"
    );
    let fresh: PeerRef = Arc::new(Fake {
        uid: old.user_id(),
        game: old.game_id(),
        steam: old.steam_id(),
        name: old.name(),
        remote: false,
    });
    for u in &mut s.lobbies().by_id.get_mut(&id).unwrap().users {
        if same(&u.peer, &old) {
            u.peer = fresh.clone();
        }
    }
    s.peer_gone(&old);
    {
        let l = s.lobbies();
        let lb = &l.by_id[&id];
        assert!(
            lb.users.len() == 2 && lb.owner == lb.id_of(&fresh) && l.invite_lobby == id,
            "stale disconnect changed publication owner"
        );
    }
    // Creating a newer lobby updates the same stable invite's resolution.
    let newer = s.lobby_create(&json!({}), &fresh).unwrap().unwrap();
    assert_eq!(
        s.lobbies().codes.get(&invite).map(String::as_str),
        newer.as_str(),
        "invite did not follow latest local lobby"
    );
    host.close();
}

#[test]
fn becoming_guest_clears_auto_invite() {
    let sw = Switch::new();
    let (remote, _rb, _) = cascade_host(&sw, CASCADE_HOST_ID, false);
    let (_rh, remote_id) = create_lobby(&remote, CASCADE_HOST_ID);
    let remote_invite = lobby_invite(&sw);
    let local_id = 76_561_197_960_265_728 + 477;
    let (local, _lb, _) = cascade_host(&sw, local_id, false);
    let (mut game, _) = create_lobby(&local, local_id);
    let _ = lobby_invite(&sw);
    resolve_ok(&mut game, &remote_invite, &remote_id);
    assert_eq!(
        lobby_invite(&sw),
        "",
        "guest kept advertising its previous host lobby"
    );
    assert_eq!(
        local.srv.lobbies().invite_lobby,
        "",
        "guest kept a publication owner"
    );
}

// ------------------------------------------------------------ identity

/// A guest names its own ids in its hello, so naming the owner's must not make
/// it the owner: not by joining, not by owner-only commands, not by chat.
#[test]
fn remote_peer_cannot_impersonate_local_owner() {
    let s = quiet_server(None);
    let owner = fake(&uid::mint("owner"), "Sowner", "Owner", false);
    insert(&s, lobby_of("L1", &owner, Transport::Direct));
    let evil = fake(&owner.user_id(), "Sowner", "Mallory", true);
    assert!(
        s.lobby_join(&json!({"id": "L1"}), &evil).is_err(),
        "a remote peer took the local owner's slot"
    );
    {
        let l = s.lobbies();
        let lb = &l.by_id["L1"];
        assert!(
            lb.users.len() == 1 && same(&lb.users[0].peer, &owner),
            "the owner's member was replaced"
        );
    }
    assert!(s
        .lobby_set_data(&json!({"id": "L1", "data": {"k": "t"}}), &evil)
        .is_err());
    assert!(s
        .lobby_transfer(&json!({"id": "L1", "uid": "Sowner"}), &evil)
        .is_err());
    assert!(s
        .lobby_set_user_data(&json!({"id": "L1", "data": "x"}), &evil)
        .is_err());
    assert!(s
        .lobby_chat(&json!({"id": "L1", "msg": "m"}), &evil)
        .is_err());
    // The local game itself, on another of its sockets, still is the owner.
    let again = fake(&owner.user_id(), "Sowner", "Owner", false);
    s.lobby_set_data(&json!({"id": "L1", "data": {"k": "t"}}), &again)
        .expect("the local game lost its ownership");
}

/// Steam authenticates an SDR stream's peer; a hello claiming another Steam id
/// is refused, the peer's own one is served.
#[test]
fn sdr_hello_must_carry_the_peers_steam_id() {
    let (peer, other) = (76_561_197_960_265_799_u64, 76_561_197_960_265_800_u64);
    let s = Server::new(Options {
        link_key: 0x1234,
        ..Options::default()
    });
    let call = |steam: String, refused: bool| {
        let (host_end, guest_end) = pipe();
        let s2 = s.clone();
        let srv = std::thread::spawn(move || s2.serve_sdr_link(&host_end, peer));
        let (closed_tx, closed) = std::sync::mpsc::channel();
        let u = link::User {
            id: uid::mint("g"),
            name: "G".into(),
            steam,
            key: 0x1234,
            ..link::User::default()
        };
        let cl = link::dial_conn(
            guest_end,
            &u,
            |_, _| {},
            move || {
                let _ = closed_tx.send(());
            },
        )
        .unwrap();
        if refused {
            closed.recv_timeout(Duration::from_secs(5)).unwrap(); // the refusal is the hello's reply
        }
        let r = cl.call("lobby/list", None);
        cl.close();
        srv.join().unwrap();
        r.map(|_| ())
    };
    let err = call(uid::from_steam_id64(other), true).unwrap_err();
    assert!(
        err.to_string().contains("Steam id does not match"),
        "foreign Steam id: {err}"
    );
    call(uid::from_steam_id64(peer), false).expect("own Steam id refused");
    call(String::new(), false).expect("no Steam id refused");
}

// ------------------------------------------------------------ modcheck

/// haxe.Unserializer for one "y<len>:<urlencoded>" string, decoding like
/// HashLink's url_decode ('+' is a space, %XX are UTF-8 bytes).
fn unhaxe(s: &str) -> String {
    let (n, body) = s
        .strip_prefix('y')
        .and_then(|r| r.split_once(':'))
        .expect("a haxe string");
    assert_eq!(n.parse::<usize>().unwrap(), body.len(), "{s}");
    let b = body.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            b'%' => {
                out.push(u8::from_str_radix(&body[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn haxe_serialize_round_trip() {
    for s in [
        "",
        "1.0.48274",
        "a+b %41 c",
        &modver::message(&["res1.pak", "winmm.dll"]),
    ] {
        let enc = haxe_serialize(s);
        let body = &enc[enc.find(':').unwrap() + 1..];
        assert!(body.bytes().all(|c| c < 128 && c != b'+'), "{enc}");
        assert_eq!(unhaxe(&enc), s);
    }
    assert_eq!(haxe_serialize("ab1"), "y3:ab1");
}

fn mod_a() -> Info {
    Info {
        build: "aaaa".into(),
        dll: "dll-a".into(),
        res1: "res-a".into(),
    }
}

fn mod_b() -> Info {
    Info {
        build: "bbbb".into(),
        dll: "dll-a".into(),
        res1: "res-b".into(),
    }
}

fn lobby_raw(host: Option<&Info>, data: &Value) -> Value {
    let mut info = json!({"id": "L1", "owner": "X1", "props": {"data": data, "maxPlayers": 4}});
    if let Some(h) = host {
        info[MOD_FIELD] = serde_json::to_value(h).unwrap();
    }
    info
}

#[test]
fn check_host_mod() {
    let data = json!({"version": "y9:1.0.48274", "beta": "f"});
    // match strips the field
    let (out, bad) = quiet_server(Some(mod_a())).check_host_mod(lobby_raw(Some(&mod_a()), &data));
    assert!(
        !bad && out.get(MOD_FIELD).is_none() && out["props"]["data"] == data,
        "match: {out}"
    );
    // mismatch blocks with the message
    let (out, bad) = quiet_server(Some(mod_a())).check_host_mod(lobby_raw(Some(&mod_b()), &data));
    let d = &out["props"]["data"];
    assert!(bad && out.get(MOD_FIELD).is_none(), "mismatch: {out}");
    assert_eq!(unhaxe(d["version"].as_str().unwrap()), MOD_MISMATCH_VERSION);
    assert_eq!(
        unhaxe(d[MOD_ERROR_FIELD].as_str().unwrap()),
        modver::message(&[modver::FILE_RES1])
    );
    assert!(
        d["beta"] == "f" && out["id"] == "L1" && out["props"]["maxPlayers"] == 4,
        "other fields changed: {out}"
    );
    // older host passes through
    let raw = lobby_raw(None, &data);
    let (out, bad) = quiet_server(Some(mod_a())).check_host_mod(raw.clone());
    assert!(!bad && out == raw, "older host: {out}");
    // own fingerprint unknown never blocks
    let (out, bad) = quiet_server(None).check_host_mod(lobby_raw(Some(&mod_b()), &data));
    assert!(!bad && out.get(MOD_FIELD).is_none(), "unknown: {out}");
    // a host cannot inject the error
    let evil = json!({"version": "y9:1.0.48274", MOD_ERROR_FIELD: "y4:evil"});
    let (out, bad) = quiet_server(None).check_host_mod(lobby_raw(None, &evil));
    assert!(
        !bad && out["props"]["data"].get(MOD_ERROR_FIELD).is_none(),
        "foreign error kept: {out}"
    );
    // null answer
    assert_eq!(
        quiet_server(Some(mod_a())).check_host_mod(Value::Null),
        (Value::Null, false)
    );
}

/// Resolves a code from a guest with guest_mod to a host with host_mod over
/// the real cascade: the LobbyInfo the game got, and whether its lobby/join
/// then reached the host's lobby.
fn join_with_mods(host_mod: Option<Info>, guest_mod: Option<Info>) -> (Value, bool) {
    let sw = Switch::new();
    let br = start_bridge(sw.add(CASCADE_HOST_ID), true);
    let hm = start_master(|o| {
        sdr_opts(o, &br);
        o.link_key = 0xdead_beef;
        o.r#mod = host_mod.map(modver::Session::fixed);
    });
    on_peer(&br, &hm.srv);
    let link = link_listener(&hm.srv);
    let (_h, id) = create_lobby(&hm, CASCADE_HOST_ID);
    let short = combined_code(&link, CASCADE_HOST_ID, 0xdead_beef);
    map_code(&hm, &short, &id);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0007, false, |o| {
        o.r#mod = guest_mod.map(modver::Session::fixed);
    });
    let raw = g.call(
        "lobby/resolveShortCode",
        json!({"shortCode": short, "filters": {}}),
    );
    let n = g.send("lobby/join", json!({"id": id}));
    (raw, g.await_reply(n).is_ok())
}

#[test]
fn join_with_same_mod_files() {
    let (raw, joined) = join_with_mods(Some(mod_a()), Some(mod_a()));
    assert!(
        raw.get(MOD_FIELD).is_none() && joined,
        "same files: joined={joined} {raw}"
    );
}

#[test]
fn join_with_different_mod_files_is_refused() {
    let (raw, joined) = join_with_mods(Some(mod_a()), Some(mod_b()));
    let err = raw["props"]["data"][MOD_ERROR_FIELD].as_str().unwrap();
    assert_eq!(unhaxe(err), modver::message(&[modver::FILE_RES1]));
    assert!(
        !joined,
        "the link was kept: lobby/join still reached the host"
    );
}

#[test]
fn join_with_older_host_is_not_blocked() {
    let (raw, joined) = join_with_mods(None, Some(mod_b()));
    assert!(
        raw.get(MOD_FIELD).is_none()
            && raw["props"]["data"].get(MOD_ERROR_FIELD).is_none()
            && joined,
        "older host: joined={joined} {raw}"
    );
}

// ------------------------------------------------------------ punch

#[derive(Default)]
struct FakePuncher {
    mapped: Option<std::net::SocketAddr>,
    no_answer: Option<String>, // dial fails with this reply code
    fwd: String,
    toward: Mutex<Vec<String>>,
}

impl Puncher for FakePuncher {
    fn ready(&self) -> bool {
        self.mapped.is_some()
    }
    fn endpoint(&self) -> anyhow::Result<crate::punch::StunResult> {
        let mapped = self
            .mapped
            .ok_or_else(|| anyhow!("no STUN server answered"))?;
        Ok(crate::punch::StunResult {
            mapped,
            mapping: nat::Mapping::Stable,
        })
    }
    fn dial(&self, addr: &str) -> anyhow::Result<String> {
        match &self.no_answer {
            Some(reply) => Err(crate::punch::NoAnswerError {
                host: addr.into(),
                self_: None,
                reply: reply.clone(),
            }
            .into()),
            None => Ok(self.fwd.clone()),
        }
    }
    fn toward(&self, addr: &str) -> anyhow::Result<()> {
        self.toward.lock().unwrap().push(addr.into());
        Ok(())
    }
}

fn punch_server(p: Arc<FakePuncher>) -> Arc<Server> {
    Server::new(Options {
        punch: Some(p),
        ..Options::default()
    })
}

#[test]
fn punch_code_carries_udp_address() {
    let f = Arc::new(FakePuncher {
        mapped: Some("45.154.88.66:8104".parse().unwrap()),
        ..FakePuncher::default()
    });
    let s = punch_server(f);
    let short = s
        .punch_code(Some(&code::Endpoint::new([45, 154, 88, 66], 14251)))
        .expect("a punch code");
    let c = decode(&short);
    assert_eq!(c.endpoint.unwrap().addr().to_string(), "45.154.88.66:14251");
    assert_eq!(c.punch.unwrap().addr().to_string(), "45.154.88.66:8104");
    // A TCP endpoint on another IP is kept: no punch code mixes the two.
    assert!(s
        .punch_code(Some(&code::Endpoint::new([198, 51, 100, 2], 14251)))
        .is_none());
    // No TCP endpoint at all: the default port on the UDP IP.
    let c = decode(&s.punch_code(None).unwrap());
    assert_eq!(c.endpoint.unwrap().addr().to_string(), "45.154.88.66:14250");
    assert!(
        punch_server(Arc::default()).punch_code(None).is_none(),
        "a punch code without a STUN answer"
    );
}

#[test]
fn punch_reply_code_on_host() {
    let f = Arc::new(FakePuncher::default());
    let s = punch_server(f.clone());
    let reply = code::encode_reply(code::Endpoint::new([100, 64, 7, 9], 50123)).unwrap();
    let a = json!({"shortCode": reply});
    let err = s
        .lobby_resolve_short_code(&a, Some(&to_raw(&a)), &fake("X1", "S1", "Host", false))
        .unwrap_err();
    assert!(
        err.to_string().contains("100.64.7.9:50123"),
        "reply code answered {err}"
    );
    assert_eq!(*f.toward.lock().unwrap(), ["100.64.7.9:50123"]);
}

#[test]
fn punch_route_failure_is_the_reply_text() {
    let c = code::Code {
        endpoint: Some(code::Endpoint::new([45, 154, 88, 66], 14250)),
        punch: Some(code::Endpoint::new([45, 154, 88, 66], 8104)),
        ..code::Code::default()
    };
    let s = punch_server(Arc::new(FakePuncher {
        no_answer: Some("81ZG0001TMVGJ".into()),
        ..FakePuncher::default()
    }));
    let mut never =
        |_: &str,
         _: &str,
         _: &dyn Fn() -> io::Result<ConnRef>,
         _: u32,
         _: Duration|
         -> Result<Value, String> { panic!("no link may be tried without an answer") };
    let err = s
        .punch_route(&c, &mut never as &mut TryRoute<'_>, &mut Vec::new())
        .unwrap_err();
    assert!(
        err.to_string().contains("81ZG0001TMVGJ"),
        "punch_route error {err}"
    );

    // An answer: the link is tried over the forwarder, which is also the
    // relay address the game is pointed at.
    let s = punch_server(Arc::new(FakePuncher {
        fwd: "127.0.0.1:50000".into(),
        ..FakePuncher::default()
    }));
    let mut relay = String::new();
    let mut ok = |_: &str,
                  r: &str,
                  _: &dyn Fn() -> io::Result<ConnRef>,
                  _: u32,
                  _: Duration|
     -> Result<Value, String> {
        relay = r.into();
        Ok(json!({"id": "L1D"}))
    };
    let v = s
        .punch_route(&c, &mut ok as &mut TryRoute<'_>, &mut Vec::new())
        .unwrap();
    assert!(
        v.is_some() && relay == "127.0.0.1:50000",
        "punch_route = {v:?}; relay {relay:?}"
    );
}

/// No SDR and no reachable TCP endpoint: a punch address still makes the
/// lobby direct instead of refusing it.
#[test]
fn choose_transport_punch() {
    let lan = nat::Endpoint {
        addr: "192.168.1.5:14250".into(),
        ip: ip4(192, 168, 1, 5),
        source: "LAN",
        ..nat::Endpoint::default()
    };
    let bad = Status {
        known: true,
        reason: "no Steam".into(),
        ..Status::default()
    };
    assert!(
        choose_transport(MODE_AUTO, &lan, None, &bad, false).is_err(),
        "without punch the lobby must be refused"
    );
    let (t, reason) = choose_transport(MODE_AUTO, &lan, None, &bad, true).unwrap();
    assert!(
        t == Transport::Direct && reason.contains("punch"),
        "with punch: {t} {reason}"
    );
}

#[test]
fn choose_transport_cases() {
    let public = nat::Endpoint {
        addr: "198.51.100.7:14250".into(),
        ip: ip4(198, 51, 100, 7),
        source: "UPnP",
        reachable: true,
        verified: true,
        ..nat::Endpoint::default()
    };
    // The live case that started this: UPnP mapped, STUN public, port refused
    // from outside. Reachable, never verified.
    let hinted = nat::Endpoint {
        addr: "45.154.88.66:14250".into(),
        ip: ip4(45, 154, 88, 66),
        source: "UPnP+STUN",
        reachable: true,
        ..nat::Endpoint::default()
    };
    let lan = nat::Endpoint {
        addr: "192.168.1.5:14250".into(),
        ip: ip4(192, 168, 1, 5),
        source: "LAN",
        warning: "no public address found: UPnP=192.168.0.1 (private (RFC1918)), STUN=none; the join code works on the LAN only".into(),
        ..nat::Endpoint::default()
    };
    let ok = sdr_ok();
    let bad = Status {
        known: true,
        reason: "SteamNetworkingMessages002 unavailable".into(),
        ..Status::default()
    };
    let unknown = Status::default();
    let none = nat::Endpoint::default();
    use Transport::{Direct, Sdr};
    // (mode, endpoint, endpoint error, sdr, want: None = refused, reason substring)
    let cases: &[(
        &str,
        &nat::Endpoint,
        Option<&str>,
        &Status,
        Option<Transport>,
        &str,
    )] = &[
        (
            MODE_AUTO,
            &public,
            None,
            &ok,
            Some(Direct),
            "verified reachable",
        ),
        ("", &public, None, &unknown, Some(Direct), "verified"),
        (MODE_AUTO, &public, None, &bad, Some(Direct), "verified"),
        (MODE_AUTO, &hinted, None, &ok, Some(Direct), "game start"),
        (
            MODE_AUTO,
            &hinted,
            None,
            &unknown,
            Some(Direct),
            "UNVERIFIED",
        ),
        (
            MODE_AUTO,
            &hinted,
            None,
            &bad,
            Some(Direct),
            "unverified endpoint",
        ),
        (MODE_AUTO, &lan, None, &ok, Some(Direct), "SDR ready"),
        (
            MODE_AUTO,
            &lan,
            None,
            &unknown,
            Some(Direct),
            "SDR status unknown",
        ),
        (
            MODE_AUTO,
            &none,
            Some("could not determine any address"),
            &ok,
            Some(Direct),
            "public endpoint unknown",
        ),
        (MODE_AUTO, &lan, None, &bad, None, ""),
        (MODE_AUTO, &none, Some("nope"), &bad, None, ""),
        (MODE_DIRECT, &lan, None, &ok, Some(Direct), "forced"),
        (MODE_SDR, &public, None, &ok, Some(Sdr), "forced"),
        (MODE_SDR, &public, None, &unknown, Some(Sdr), "forced"),
        (MODE_SDR, &public, None, &bad, None, ""),
        ("legacy", &public, None, &ok, None, ""),
    ];
    for &(mode, ep, ep_err, sdr, want, reason) in cases {
        match (choose_transport(mode, ep, ep_err, sdr, false), want) {
            (Err(e), None) => {
                let e = e.to_string();
                if mode != "legacy" {
                    assert!(
                        e.starts_with("no usable transport") && e.contains("SDR unavailable"),
                        "{mode} {}: {e}",
                        ep.addr
                    );
                }
            }
            (Ok((t, why)), Some(w)) => assert!(
                t == w && why.contains(reason),
                "{mode} {}: {t} ({why}), want {w} / {reason}",
                ep.addr
            ),
            (got, _) => panic!("{mode} {}: {got:?}, want {want:?}", ep.addr),
        }
    }
}

// ------------------------------------------------------------ relay address

/// The StartClients message exactly as a host's game sent it (harness run
/// 2026-10-15, password replaced).
const START_CLIENTS: &str =
    "wy12:LobbyMessagey12:StartClients:1y45:W127.0.0.1%3A14251%3ALWAmcF78Bvk7d8jnJmGg8vX2";

#[test]
fn rewrite_wserver_cases() {
    let cases = [
        ("StartClients", START_CLIENTS, "wy12:LobbyMessagey12:StartClients:1y50:W192.168.77.108%3A14250%3ALWAmcF78Bvk7d8jnJmGg8vX2", 1),
        ("two ids", "y20:W127.0.0.1%3A1%3Apwxy20:W127.0.0.1%3A2%3Apwx", "y29:W192.168.77.108%3A14250%3Apwxy29:W192.168.77.108%3A14250%3Apwx", 2),
        ("other host", "y21:W10.0.0.1%3A14250%3Apw", "y21:W10.0.0.1%3A14250%3Apw", 0),
        ("length past the end", "y99:W127.0.0.1%3A14251%3Apw", "y99:W127.0.0.1%3A14251%3Apw", 0),
        ("lobby packet", "wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::v8y10:X29f792402", "wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::v8y10:X29f792402", 0),
    ];
    for (name, msg, want, n) in cases {
        assert_eq!(
            rewrite_wserver(msg, "192.168.77.108", "14250"),
            (want.to_string(), n),
            "{name}"
        );
    }
}

#[test]
fn guest_relay_addr_keeps_other_fields() {
    let s = quiet_server(None);
    let input = to_raw(&json!({"id": "L1D", "uid": "S1", "msg": START_CLIENTS}));
    let out = s
        .guest_relay_addr(Some(input.clone()), "192.168.77.108:14250")
        .unwrap();
    let got: Value = serde_json::from_str(out.get()).unwrap();
    assert_eq!(
        got,
        json!({"id": "L1D", "uid": "S1", "msg": "wy12:LobbyMessagey12:StartClients:1y50:W192.168.77.108%3A14250%3ALWAmcF78Bvk7d8jnJmGg8vX2"})
    );
    let same_out = s
        .guest_relay_addr(Some(input.clone()), "not an address")
        .unwrap();
    assert_eq!(
        same_out.get(),
        input.get(),
        "a bad route must leave the message alone"
    );
}

// ------------------------------------------------------------ sessions

/// A link to a host that reads everything and answers nothing, so a
/// forwarded command blocks for the link's whole call timeout.
fn silent_host(timeout: Duration) -> (Arc<link::Client>, ConnRef) {
    let (host_end, guest_end) = pipe();
    let h = host_end.clone();
    std::thread::spawn(move || {
        let mut b = [0u8; 4096];
        while matches!(h.read(&mut b), Ok(n) if n > 0) {}
    });
    let u = link::User {
        id: "Xguest".into(),
        name: "Guest".into(),
        ..link::User::default()
    };
    let cl = link::dial_conn(guest_end, &u, |_, _| {}, || {}).unwrap();
    cl.set_call_timeout(timeout);
    (cl, host_end)
}

/// Commands the game queued behind a slow one must not run after its socket
/// is gone: a late user/login would make the dead session the game's
/// connection again.
#[test]
fn no_command_runs_after_the_game_disconnects() {
    let block = Duration::from_millis(300);
    let m = start_master(|_| {});
    let (cl, host_end) = silent_host(block);
    m.srv.state().client = Some(cl.clone());

    let mut game = dial(&m.addr);
    game.send("lobby/list", json!({})); // forwarded to the silent host: blocks
    game.send("user/login", json!({"uid": "S1234", "name": "P"}));
    std::thread::sleep(Duration::from_millis(50)); // both are read before the socket ends
    game.close();

    let deadline = Instant::now() + block + Duration::from_secs(1);
    while !m.srv.state().conns.is_empty() {
        assert!(Instant::now() < deadline, "the game's session never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(block); // anything still running has had its chance by now
    assert!(
        m.srv.local_session().is_none(),
        "a command queued before the disconnect re-adopted the dead session"
    );
    cl.close();
    host_end.close();
}

/// A host that answers a code with "no such lobby" is not a route, and the
/// link to it must not capture the guest's next code.
#[test]
fn next_code_reaches_its_own_host() {
    let other_host = 0x0110_0001_00BC_6150;
    let sw = Switch::new();
    let (a, _ab, addr_a) = cascade_host(&sw, CASCADE_HOST_ID, true);
    let (_ah, id_a) = create_lobby(&a, CASCADE_HOST_ID);
    let (b, _bb, addr_b) = cascade_host(&sw, other_host, true);
    let (_bh, id_b) = create_lobby(&b, other_host);
    let code_a = combined_code(&addr_a, CASCADE_HOST_ID, 0xdead_beef);
    let code_b = combined_code(&addr_b, other_host, 0xdead_beef);
    map_code(&b, &code_b, &id_b);
    let (mut g, _gm, _gb) = guest_for(&sw, 0x0110_0001_0000_0020, false, |_| {});
    // A does not know code_a (its lobby closed, say): an error, not a route.
    let msg = g.call_err(
        "lobby/resolveShortCode",
        json!({"shortCode": code_a, "filters": {}}),
    );
    assert!(
        msg.contains("does not know this join code"),
        "unknown code = {msg:?}"
    );
    resolve_ok(&mut g, &code_b, &id_b);
    // Linked to B by a code that works, a code of A's still reaches A.
    map_code(&a, &code_a, &id_a);
    resolve_ok(&mut g, &code_a, &id_a);
    // And an invitation of B's, while linked to A, reaches B.
    assert_eq!(
        g.call("lobby/infoInvite", json!({"invite": code_b}))["id"],
        id_b.as_str()
    );
}

/// The in-game reconnect lobby (isReconnect) reuses the transport of the
/// host's game lobby even when the verdict for a fresh lobby has changed.
#[test]
fn reconnect_lobby_keeps_game_transport() {
    let host_steam = "S0011223344556677";
    for (first, later) in [
        (lan_endpoint(), public_endpoint()),
        (public_endpoint(), lan_endpoint()),
    ] {
        let ep = Arc::new(Mutex::new(first));
        let e2 = ep.clone();
        let m = start_master(|o| {
            o.public_addr = Some(Arc::new(|| Ok(public_endpoint().addr)));
            o.endpoint = Some(Arc::new(move || Ok(e2.lock().unwrap().clone())));
        });
        let mut host = dial(&m.addr);
        host.login("Host", host_steam);
        let mut create = |reconnect: bool| {
            let data = if reconnect {
                json!({"isReconnect": "t"})
            } else {
                json!({})
            };
            let id = host.call(
                "lobby/create",
                json!({"props": {"data": data, "maxPlayers": 4}}),
            );
            let id = id.as_str().unwrap().to_string();
            // The transport is in the lobby id; members keep their game ids.
            let owner = host.call("lobby/info", json!({"id": id}))["owner"].clone();
            assert!(
                id.ends_with(DIRECT_LOBBY_MARK) && owner == host_steam,
                "{id} owner {owner}"
            );
            id
        };
        create(false);
        // the game lobby is left (lobby/leave), then the verdict changes
        drop(create);
        host.call("lobby/leave", json!({}));
        *ep.lock().unwrap() = later;
        let mut create = |reconnect: bool| {
            let data = if reconnect {
                json!({"isReconnect": "t"})
            } else {
                json!({})
            };
            let id = host.call(
                "lobby/create",
                json!({"props": {"data": data, "maxPlayers": 4}}),
            );
            id.as_str().unwrap().to_string()
        };
        for id in [create(true), create(false)] {
            assert!(id.ends_with(DIRECT_LOBBY_MARK), "{id}");
        }
    }
}

#[test]
fn haxe_true_values() {
    for (v, want) in [
        (json!("t"), true),
        (json!(true), true),
        (json!("f"), false),
        (json!(false), false),
        (Value::Null, false),
        (json!("x"), false),
    ] {
        assert_eq!(haxe_true(&v), want, "{v}");
    }
}

/// A LobbyInfo is marshalled after the store lock is released, while the
/// owner keeps setting data.
#[test]
fn lobby_info_is_a_snapshot() {
    let s = quiet_server(None);
    let owner = fake("Xowner", "Sowner", "Owner", false);
    insert(&s, lobby_of("L1", &owner, Transport::Direct));
    let (s2, o2) = (s.clone(), owner.clone());
    let writer = std::thread::spawn(move || {
        for i in 0..300 {
            s2.lobby_set_data(&json!({"id": "L1", "data": {format!("k{i}"): "t"}}), &o2)
                .unwrap();
        }
    });
    for _ in 0..300 {
        let info = s.lobby_info("L1").unwrap();
        assert!(serde_json::to_string(&info).is_ok() && s.lobby_list().is_array());
    }
    writer.join().unwrap();
    assert_eq!(
        s.lobby_info("L1").unwrap()["props"]["data"]
            .as_object()
            .unwrap()
            .len(),
        300
    );
}

// ------------------------------------------------------------ game start

/// A real proxy-link over a pipe: the host's view of the guest, and the link
/// that keeps it alive.
fn link_peer(name: &str, sdr: bool) -> (Arc<link::Peer>, Arc<link::Client>) {
    let (host_end, guest_end) = pipe();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        link::serve(
            &host_end,
            sdr,
            |_| Ok(()),
            |_, _, p| {
                let _ = tx.send(p.clone());
                Ok(RawValue::NULL.to_owned())
            },
            |_| {},
        );
    });
    let u = link::User {
        id: "X1".into(),
        name: name.into(),
        game: "S1".into(),
        ..link::User::default()
    };
    let cl = link::dial_conn(guest_end, &u, |_, _| {}, || {}).unwrap();
    cl.call("ping", None).unwrap();
    (rx.recv().unwrap(), cl)
}

#[test]
fn start_answer_follows_the_guests_links() {
    // (name, guests: name -> over SDR, serverID prefix, lobby made under transport sdr, host id)
    let cases: &[(&str, &[(&str, bool)], &str, bool, &str)] = &[
        ("alone", &[], "R127.0.0.1:14250:", false, "S5d6de506"),
        (
            "all direct (#7: LAN code join)",
            &[("loftyrogue", false)],
            "R127.0.0.1:14250:",
            false,
            "S5d6de506",
        ),
        (
            "all over SDR",
            &[("a", true), ("b", true)],
            "S5d6de506",
            false,
            "S5d6de506",
        ),
        (
            "mixed",
            &[("steamy", true), ("coded", false)],
            "Игроки steamy вошли через Steam",
            false,
            "S5d6de506",
        ),
        ("SDR lobby alone", &[], "S5d6de506", true, "S5d6de506"),
        (
            "SDR lobby, Steam ids over mixed links",
            &[("ab", false), ("cd", true)],
            "S5d6de506",
            true,
            "S5d6de506",
        ),
        (
            "SDR lobby, a non-Steam id",
            &[("coded", false)],
            "R127.0.0.1:14250:",
            true,
            "S5d6de506",
        ),
        (
            "SDR lobby, non-Steam host alone",
            &[],
            "R127.0.0.1:14250:",
            true,
            "X1",
        ),
    ];
    for &(name, guests, want, sdr, host_id) in cases {
        let s = Server::new(Options {
            relay_port: 14250,
            host_pw: "hpw".into(),
            slave_pw: "spw".into(),
            transport: MODE_AUTO.into(),
            ..Options::default()
        });
        let host = fake("X0", host_id, "host", false);
        let mut l = lobby_of("L1D", &host, Transport::Direct);
        if sdr {
            l.id = "L1".into();
            l.transport = Transport::Sdr;
        }
        let mut links = Vec::new();
        for &(n, over_sdr) in guests {
            let (p, cl) = link_peer(n, over_sdr);
            l.users.push(Member {
                id: format!("S{n}"),
                name: n.into(),
                data: None,
                peer: p,
            });
            links.push(cl);
        }
        {
            let mut lb = s.lobbies();
            lb.invite_lobby.clone_from(&l.id);
            lb.by_id.insert(l.id.clone(), l);
        }
        let got = s.start_answer();
        let got = got["serverID"].as_str().unwrap();
        assert!(
            got.starts_with(want),
            "{name}: serverID = {got:?}, want prefix {want:?}"
        );
        for cl in links {
            cl.close();
        }
    }
}
