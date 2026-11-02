//! Port of internal/sdrbridge/sdrbridgetest: a stand-in for the shim's bridge,
//! for tests. One loopback listener speaking bridge.c's frame protocol, where
//! each connected helper is a Steam identity and SEND frames are switched to
//! the helper that owns the target SteamID64, arriving there as RECV frames
//! from the sender. A SEND to nobody is answered with an ERR frame, like a
//! refused send.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const FRAME_HEAD: usize = 13;

/// A helper's desired native Steam lobby state, as published to the shim.
#[derive(Debug, Clone, PartialEq)]
pub struct LobbyInvite {
    pub peer: u64,
    pub invite: String,
}

/// The fake shim; stops (listener, clients, temp dir) when dropped.
pub struct Switch {
    /// Every LOBBY frame received, in order.
    pub lobby_invites: Mutex<Receiver<LobbyInvite>>,
    inner: Arc<Inner>,
    accept: Option<JoinHandle<()>>,
}

struct Inner {
    addr: SocketAddr,
    dir: PathBuf,
    stopped: AtomicBool,
    lobby_tx: Mutex<Sender<LobbyInvite>>,
    mu: Mutex<Clients>,
}

#[derive(Default)]
struct Clients {
    tokens: HashMap<String, u64>,         // token -> identity
    clients: HashMap<u64, Arc<TcpStream>>, // identity -> its helper
}

impl Switch {
    /// Starts a switch.
    pub fn new() -> Switch {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut r = [0u8; 8];
        crate::rand_bytes(&mut r);
        let dir = std::env::temp_dir().join(format!("sdrbridgetest-{}", hex(&r)));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = channel();
        let inner = Arc::new(Inner {
            addr: ln.local_addr().unwrap(),
            dir,
            stopped: AtomicBool::new(false),
            lobby_tx: Mutex::new(tx),
            mu: Mutex::new(Clients::default()),
        });
        let i = inner.clone();
        let accept = std::thread::spawn(move || {
            for c in ln.incoming() {
                if i.stopped.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(c) = c {
                    let i = i.clone();
                    std::thread::spawn(move || i.serve(Arc::new(c)));
                }
            }
        });
        Switch { lobby_invites: Mutex::new(rx), inner, accept: Some(accept) }
    }

    /// Registers a Steam identity and returns the path of a status file that
    /// advertises this switch to it, exactly as the shim would.
    pub fn add(&self, steam_id: u64) -> PathBuf {
        let mut b = [0u8; 16];
        crate::rand_bytes(&mut b);
        let token = hex(&b);
        let line = format!("ok bridge={} token={token}\n", self.inner.addr);
        self.inner.mu.lock().unwrap().tokens.insert(token, steam_id);
        let path = self.inner.dir.join(format!("sdr-{steam_id}.status"));
        std::fs::write(&path, line).unwrap();
        path
    }

    /// Returns a status file saying SDR cannot work.
    pub fn unavailable(&self, why: &str) -> PathBuf {
        let path = self.inner.dir.join("sdr-unavailable.status");
        std::fs::write(&path, format!("unavailable {why}\n")).unwrap();
        path
    }

    /// Whether steam_id's helper is authenticated and switched to (a bridge is
    /// ready once AUTH is written, a little before the switch registers it).
    pub fn connected(&self, steam_id: u64) -> bool {
        self.inner.mu.lock().unwrap().clients.contains_key(&steam_id)
    }

    /// Simulates the shim dropping its authenticated helper connection.
    pub fn disconnect(&self, peer: u64) {
        let c = self.inner.mu.lock().unwrap().clients.get(&peer).cloned();
        if let Some(c) = c {
            crate::conn::close_tcp(&c);
        }
    }
}

impl Drop for Switch {
    fn drop(&mut self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.inner.addr); // wakes accept
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        for c in self.inner.mu.lock().unwrap().clients.values() {
            crate::conn::close_tcp(&c);
        }
        let _ = std::fs::remove_dir_all(&self.inner.dir);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn read_frame(mut c: &TcpStream) -> io::Result<(u8, u64, Vec<u8>)> {
    let mut head = [0u8; FRAME_HEAD];
    c.read_exact(&mut head)?;
    let n = u32::from_le_bytes(head[9..13].try_into().unwrap());
    let mut payload = vec![0u8; n as usize];
    c.read_exact(&mut payload)?;
    Ok((head[0], u64::from_le_bytes(head[1..9].try_into().unwrap()), payload))
}

fn write_frame(mut c: &TcpStream, typ: u8, peer: u64, payload: &[u8]) -> io::Result<()> {
    let mut buf = vec![typ];
    buf.extend_from_slice(&peer.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    c.write_all(&buf)
}

impl Inner {
    fn serve(&self, c: Arc<TcpStream>) {
        self.serve_authed(&c);
        crate::conn::close_tcp(&c);
    }

    fn serve_authed(&self, c: &Arc<TcpStream>) {
        let tok = match read_frame(c) {
            Ok((0, _, tok)) => String::from_utf8_lossy(&tok).into_owned(),
            _ => return,
        };
        let me = {
            let mut s = self.mu.lock().unwrap();
            let Some(&me) = s.tokens.get(&tok) else {
                return; // a wrong token: closed without a word, like the shim
            };
            s.clients.insert(me, c.clone());
            me
        };
        while let Ok((typ, peer, payload)) = read_frame(c) {
            if typ != 1 {
                if typ == 4 && peer == 0 {
                    let invite = String::from_utf8_lossy(&payload).into_owned();
                    let _ = self.lobby_tx.lock().unwrap().send(LobbyInvite { peer: me, invite });
                }
                continue;
            }
            let s = self.mu.lock().unwrap(); // also serialises writes to every client
            match s.clients.get(&peer) {
                None => _ = write_frame(c, 3, peer, b"SendMessageToUser failed: EResult 3"),
                Some(dst) => _ = write_frame(dst, 2, me, &payload),
            }
        }
        let mut s = self.mu.lock().unwrap();
        if s.clients.get(&me).is_some_and(|x| Arc::ptr_eq(x, c)) {
            s.clients.remove(&me);
        }
    }
}
