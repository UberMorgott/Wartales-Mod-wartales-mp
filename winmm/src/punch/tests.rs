use super::*;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};

const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Closes the socket when a test ends, even a failing one.
struct S(Arc<Socket>);

impl Drop for S {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl std::ops::Deref for S {
    type Target = Socket;
    fn deref(&self) -> &Socket {
        &self.0
    }
}

fn open_s(port: u16, relay: &str, servers: &[&str]) -> S {
    S(open(port, relay, servers, Log::discard()).unwrap())
}

/// Answers binding requests with the sender's address, its port moved by delta
/// (a symmetric NAT shows each server another port).
fn fake_stun(delta: u16) -> String {
    let c = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let addr = c.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((n, SocketAddr::V4(from))) = c.recv_from(&mut buf) else {
                continue;
            };
            if !nat::is_stun(&buf[..n]) {
                continue;
            }
            let mut resp = [0u8; 32];
            resp[0..2].copy_from_slice(&0x0101u16.to_be_bytes());
            resp[2..4].copy_from_slice(&12u16.to_be_bytes());
            resp[4..20].copy_from_slice(&buf[4..20]); // cookie + transaction id
            resp[20..22].copy_from_slice(&0x0020u16.to_be_bytes());
            resp[22..24].copy_from_slice(&8u16.to_be_bytes());
            resp[25] = 0x01;
            resp[26..28].copy_from_slice(&((from.port() + delta) ^ 0x2112).to_be_bytes());
            for (i, k) in [0x21, 0x12, 0xa4, 0x42].iter().enumerate() {
                resp[28 + i] = from.ip().octets()[i] ^ k;
            }
            let _ = c.send_to(&resp, from);
        }
    });
    addr
}

/// A UDP address that never answers.
fn silent() -> String {
    let c = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let addr = c.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        loop {
            let _ = c.recv_from(&mut buf);
        }
    });
    addr
}

/// The socket's loopback address.
fn local(s: &Socket) -> String {
    SocketAddrV4::new(LOOPBACK, s.local_addr().port()).to_string()
}

/// A TCP server that echoes every connection: the host's relay port.
fn echo() -> String {
    let ln = TcpListener::bind((LOOPBACK, 0)).unwrap();
    let addr = ln.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for c in ln.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = c.try_clone().unwrap();
                let mut w = c;
                let _ = std::io::copy(&mut r, &mut w);
                let _ = w.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    addr
}

fn round_trip(addr: &str, msg: &[u8]) {
    let mut c = TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(3)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    c.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    c.write_all(msg).unwrap();
    let mut got = vec![0u8; msg.len()];
    c.read_exact(&mut got).unwrap();
    assert!(got == msg, "echo differs ({} bytes)", msg.len());
}

#[test]
fn test_endpoint_falls_back_across_servers() {
    // The first server never answers; the other two do, with the same address.
    let (quiet, stun_a, stun_b) = (silent(), fake_stun(0), fake_stun(0));
    let s = open_s(0, "", &[&quiet, &stun_a, &stun_b]);
    let start = Instant::now();
    let r = s.endpoint().unwrap();
    assert_eq!(
        r.mapped,
        SocketAddr::from((LOOPBACK, s.local_addr().port()))
    );
    assert_eq!(r.mapping, Mapping::Stable);
    let d = start.elapsed();
    assert!(
        d < Duration::from_secs(2),
        "two answers took {d:?}: the silent server held the round up"
    );
    assert!(s.ready(), "ready false after a STUN answer");
}

#[test]
fn test_endpoint_symmetric() {
    let (a, b) = (fake_stun(0), fake_stun(7));
    let s = open_s(0, "", &[&a, &b]);
    assert_eq!(s.endpoint().unwrap().mapping, Mapping::Symmetric);
}

#[test]
fn test_endpoint_no_server() {
    let a = silent();
    let s = open_s(0, "", &[&a]);
    let r = s.endpoint();
    assert!(
        r.is_err() && !s.ready(),
        "endpoint = {r:?}; want an error and no mapping"
    );
    assert_eq!(r.unwrap_err().to_string(), "no STUN server answered");
}

/// Both sides punch at once (the reply-code case); the guest's TCP bytes go
/// through KCP to the host's relay and back, on two streams of one tunnel.
#[test]
fn test_punch_tunnel_end_to_end() {
    let host = open_s(0, &echo(), &[]);
    let stun = fake_stun(0);
    let guest = open_s(0, "", &[&stun]);
    host.toward(&local(&guest)).unwrap();
    let fwd = guest.dial(&local(&host)).unwrap();
    assert!(
        fwd.starts_with("127.0.0.1:"),
        "forwarder {fwd} is not on loopback"
    );
    round_trip(&fwd, b"hello over kcp");
    let big = b"0123456789abcdef".repeat(64 * 1024); // 1 MiB: windows and fragments
    let f2 = fwd.clone();
    let t = std::thread::spawn(move || round_trip(&f2, &big));
    round_trip(&fwd, b"second stream");
    t.join().unwrap();

    // A second join to the same host reuses the live tunnel.
    assert_eq!(
        guest.dial(&local(&host)).unwrap(),
        fwd,
        "dial again: want the live tunnel"
    );
}

/// Guest-first: a host that lets the punch through answers it, no reply code.
#[test]
fn test_punch_guest_first() {
    let host = open_s(0, &echo(), &[]);
    let guest = open_s(0, "", &[]);
    let fwd = guest.dial(&local(&host)).unwrap();
    round_trip(&fwd, b"guest first");
}

/// No answer: the error carries a reply code for the guest's own address and,
/// on a symmetric NAT, the Steam invitation hint.
#[test]
fn test_punch_no_answer() {
    let (a, b) = (fake_stun(0), fake_stun(3));
    let guest = open_s(0, "", &[&a, &b]);
    guest.st().wait = Some(Duration::from_millis(300));
    let err = guest.dial(&silent()).unwrap_err();
    let na = err
        .downcast_ref::<NoAnswerError>()
        .expect("a NoAnswerError");
    let c = code::decode_any(&na.reply).unwrap();
    assert_eq!(
        c.reply.unwrap().addr(),
        na.self_.unwrap().mapped,
        "reply code vs STUN"
    );
    for want in [na.reply.as_str(), "Join by code", "Steam invitation"] {
        assert!(err.to_string().contains(want), "error {err} lacks {want:?}");
    }

    // Without STUN there is no reply code to offer.
    let lone = open_s(0, "", &[]);
    lone.st().wait = Some(Duration::from_millis(200));
    let err = lone.dial(&silent()).unwrap_err();
    let na = err
        .downcast_ref::<NoAnswerError>()
        .expect("a NoAnswerError");
    assert!(
        na.reply.is_empty() && err.to_string().contains("Steam invitation"),
        "dial without STUN = {err}"
    );
}

/// A guest that restarts on the same UDP address opens new streams with a new
/// nonce; the host replaces the old tunnel at once.
#[test]
fn test_punch_guest_restart_same_address() {
    let host = open_s(0, &echo(), &[]);
    let first = open_s(0, "", &[]);
    let port = first.local_addr().port();
    let fwd = first.dial(&local(&host)).unwrap();
    round_trip(&fwd, b"before restart");
    drop(first);

    let again = open_s(port, "", &[]);
    if again.local_addr().port() != port {
        eprintln!("port {port} not free again: skipped");
        return;
    }
    let fwd = again.dial(&local(&host)).unwrap();
    round_trip(&fwd, b"after restart");
}

/// A tunnel that dies is redialed by the next connection to the same
/// forwarder: the game's saved route keeps working when it reconnects.
#[test]
fn test_punch_forwarder_redials() {
    let host = open_s(0, &echo(), &[]);
    let guest = open_s(0, "", &[]);
    let ha: SocketAddr = local(&host).parse().unwrap();
    let fwd = guest.dial(&local(&host)).unwrap();
    round_trip(&fwd, b"before the loss");
    let since = guest.st().peers[&ha].last;
    guest.drop_peer_locked(&mut guest.st(), ha);
    round_trip(&fwd, b"after the loss");
    let st = guest.st();
    assert!(
        st.peers.get(&ha).is_some_and(|p| p.last > since),
        "no new tunnel after the redial"
    );
}

/// Roles swap: the former host dials the former guest, whose tunnel it serves.
#[test]
fn test_punch_role_swap() {
    let a = open_s(0, &echo(), &[]);
    let b = open_s(0, &echo(), &[]);
    round_trip(&b.dial(&local(&a)).unwrap(), b"b to a");
    round_trip(&a.dial(&local(&b)).unwrap(), b"a to b");
}

/// Go's goleak check: close joins every thread it started and frees the
/// socket, with a stream still open.
#[test]
fn test_close_leaves_nothing() {
    let host = open_s(0, &echo(), &[]);
    let guest = open_s(0, "", &[]);
    let fwd = guest.dial(&local(&host)).unwrap();
    round_trip(&fwd, b"warm");
    let mut c = TcpStream::connect(&fwd).unwrap();
    c.write_all(b"open").unwrap();
    let mut got = [0u8; 4];
    c.read_exact(&mut got).unwrap();
    let (h, g) = (host.0.clone(), guest.0.clone());
    drop(host);
    drop(guest);
    for s in [&h, &g] {
        assert_eq!(Arc::strong_count(s), 1, "a thread outlived close");
        assert!(s.st().threads.is_empty() && s.st().streams.is_empty() && s.st().peers.is_empty());
    }
    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    assert!(
        !matches!(c.read(&mut got), Ok(n) if n > 0),
        "the stream outlived close"
    );
    assert!(
        TcpStream::connect_timeout(&fwd.parse().unwrap(), Duration::from_millis(500)).is_err(),
        "the forwarder outlived close"
    );
}

/// Forwards between a guest and the host, losing, duplicating and reordering
/// datagrams; returns the address the guest dials.
fn lossy_shim(host: SocketAddr) -> String {
    let x = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    x.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    let addr = x.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let (mut guest, mut held, mut rng) = (
            None,
            None::<(Vec<u8>, SocketAddr)>,
            0x9e37_79b9_7f4a_7c15u64,
        );
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = x.recv_from(&mut buf) else {
                if let Some((p, to)) = held.take() {
                    let _ = x.send_to(&p, to);
                }
                continue;
            };
            let to = if from == host {
                let Some(g) = guest else { continue };
                g
            } else {
                guest = Some(from);
                host
            };
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let r = rng % 100;
            let pkt = buf[..n].to_vec();
            if r < 10 {
                continue; // lost
            }
            if r < 20 {
                // reordered: goes out after the next one
                if let Some((p, t)) = held.replace((pkt, to)) {
                    let _ = x.send_to(&p, t);
                }
                continue;
            }
            let _ = x.send_to(&pkt, to);
            if r < 25 {
                let _ = x.send_to(&pkt, to); // duplicated
            }
            if let Some((p, t)) = held.take() {
                let _ = x.send_to(&p, t);
            }
        }
    });
    addr
}

/// A stream survives 10% loss, duplicates and reordering.
#[test]
fn test_punch_lossy_link() {
    let host = open_s(0, &echo(), &[]);
    let guest = open_s(0, "", &[]);
    let shim = lossy_shim(local(&host).parse().unwrap());
    let fwd = guest.dial(&shim).unwrap();
    round_trip(&fwd, b"over a bad line");
    round_trip(&fwd, &b"0123456789abcdef".repeat(64 * 1024)); // 1 MiB
}

#[test]
fn test_go_dur() {
    assert_eq!(go_dur(BUDGET), "2m0s");
    assert_eq!(go_dur(ANSWER_WAIT), "5s");
    assert_eq!(go_dur(Duration::from_millis(300)), "300ms");
    assert_eq!(go_dur(STUN_WAIT), "3.5s");
}
