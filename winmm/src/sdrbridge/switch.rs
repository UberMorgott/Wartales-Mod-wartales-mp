//! A stand-in for SDR, for tests (port of internal/sdrbridge/sdrbridgetest):
//! every endpoint is a Steam identity, and a message sent to a SteamID64 is
//! switched to the endpoint that owns it, arriving there from the sender. A
//! send to nobody is refused with EResult 3, like Steam's.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::{Status, Wire};

/// A desired native Steam lobby state, as an endpoint published it.
#[derive(Debug, Clone, PartialEq)]
pub struct LobbyInvite {
    pub peer: u64,
    pub invite: String,
}

type Inboxes = Arc<Mutex<HashMap<u64, VecDeque<(u64, Vec<u8>)>>>>;

/// The fake SDR network.
pub struct Switch {
    /// Every lobby publication, in order.
    pub lobby_invites: Mutex<Receiver<LobbyInvite>>,
    lobby_tx: Sender<LobbyInvite>,
    inboxes: Inboxes,
}

/// One identity on the switch; its status can be changed at any time.
pub struct Endpoint {
    id: u64,
    status: Mutex<Status>,
    inboxes: Inboxes,
    lobby_tx: Mutex<Sender<LobbyInvite>>,
}

impl Default for Switch {
    fn default() -> Self {
        Self::new()
    }
}

impl Switch {
    pub fn new() -> Switch {
        let (lobby_tx, rx) = channel();
        Switch {
            lobby_invites: Mutex::new(rx),
            lobby_tx,
            inboxes: Arc::default(),
        }
    }

    fn endpoint(&self, id: u64, status: Status) -> Arc<Endpoint> {
        self.inboxes.lock().unwrap().entry(id).or_default();
        Arc::new(Endpoint {
            id,
            status: Mutex::new(status),
            inboxes: self.inboxes.clone(),
            lobby_tx: Mutex::new(self.lobby_tx.clone()),
        })
    }

    /// A ready identity.
    pub fn add(&self, steam_id: u64) -> Arc<Endpoint> {
        self.endpoint(
            steam_id,
            Status {
                known: true,
                ok: true,
                reason: String::new(),
            },
        )
    }

    /// An identity whose SDR is still coming up (see Endpoint::set_status).
    pub fn pending(&self, steam_id: u64, why: &str) -> Arc<Endpoint> {
        self.endpoint(
            steam_id,
            Status {
                reason: why.into(),
                ..Status::default()
            },
        )
    }

    /// A process where SDR cannot work; reachable by nobody.
    pub fn unavailable(&self, why: &str) -> Arc<Endpoint> {
        Arc::new(Endpoint {
            id: 0,
            status: Mutex::new(Status {
                known: true,
                ok: false,
                reason: why.into(),
            }),
            inboxes: Arc::default(),
            lobby_tx: Mutex::new(self.lobby_tx.clone()),
        })
    }
}

impl Endpoint {
    pub fn set_status(&self, st: Status) {
        *self.status.lock().unwrap() = st;
    }
}

impl Wire for Endpoint {
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn send(&self, peer: u64, msg: &[u8]) -> Result<(), String> {
        if !self.status().ok {
            return Err("SDR not ready".into());
        }
        match self.inboxes.lock().unwrap().get_mut(&peer) {
            Some(q) => {
                q.push_back((self.id, msg.to_vec()));
                Ok(())
            }
            None => Err("SendMessageToUser failed: EResult 3".into()),
        }
    }

    fn recv(&self) -> Vec<(u64, Vec<u8>)> {
        self.inboxes
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }

    fn set_lobby_invite(&self, invite: &str) -> bool {
        let _ = self.lobby_tx.lock().unwrap().send(LobbyInvite {
            peer: self.id,
            invite: invite.into(),
        });
        true
    }
}
