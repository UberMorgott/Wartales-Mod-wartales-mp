//! Lobbies and the lobby/* commands (SERVER-CONTRACT §3.8).

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{anyhow, bail};
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};

use super::{args_of, from_raw, same, text, to_raw, Out, PeerRef, Server, Transport};
use crate::applog::trunc;
use crate::{code, logf, uid};

/// Ends the id of every lobby on our direct relay, so a guest's master can
/// tell the host's verdict (log_host_transport). The host's game asks
/// instance/get either way (patcher start_transport.rs) and start_answer picks
/// the transport. An SDR lobby id is "L" + lowercase hex only.
pub const DIRECT_LOBBY_MARK: &str = "D";

/// How many lobby transport packets pass between two summaries; the payloads
/// themselves are never logged, only counted.
const TRANSPORT_LOG_EVERY: i64 = 64;

/// One player inside a lobby. data is the JSON the client sent (MPLobby
/// encodes user data with haxe.Json.print); None = never sent.
pub(super) struct Member {
    pub id: String,
    pub name: String,
    pub data: Option<Value>,
    pub peer: PeerRef,
}

impl Member {
    /// Whether p speaks for this member. An id alone proves nothing: a guest
    /// names its own Session id in its hello, so it could name the owner's. A
    /// remote member is the link it joined over; the local game is whichever
    /// of its master sockets is talking, so any local session holds a local
    /// member, and no remote peer ever does.
    fn held_by(&self, p: &PeerRef) -> bool {
        same(&self.peer, p) || (!p.remote() && !self.peer.remote())
    }
}

pub(super) struct Lobby {
    pub id: String,
    pub owner: String,
    pub data: Map<String, Value>, // values are JSON strings (encodeData)
    pub is_private: Option<bool>,
    pub max_players: Option<i64>,
    pub users: Vec<Member>,
    /// Decided once, at creation.
    pub transport: Transport,
}

impl Lobby {
    /// The id a member is known by inside this lobby: the id its own game
    /// calls itself (user/login "uid"), on either transport, as the vanilla
    /// master does. The game compares member ids with its own id and with the
    /// player ids in its saves (LoadMultiGame.isHostIn, LobbyState
    /// userCanJoin/getPlayer, MPLobby owner checks). On SDR the authenticated
    /// Steam id is used. "" means the peer never said who it is; such a peer
    /// cannot create or join a lobby.
    pub fn id_of(&self, p: &PeerRef) -> String {
        if self.transport == Transport::Sdr {
            let s = p.steam_id();
            if !s.is_empty() {
                return s;
            }
        }
        p.game_id()
    }

    /// The member p speaks for, if any.
    fn member_of(&self, p: &PeerRef) -> Option<usize> {
        let id = self.id_of(p);
        self.users.iter().position(|u| u.id == id && u.held_by(p))
    }

    /// Every member but those named except.
    fn recipients(&self, except: &str) -> Vec<PeerRef> {
        self.users
            .iter()
            .filter(|u| u.id != except)
            .map(|u| u.peer.clone())
            .collect()
    }

    /// The LobbyInfo the client parses (a copy, marshalled after the lock).
    pub fn info(&self) -> Value {
        let users: Vec<Value> = self
            .users
            .iter()
            .map(|u| json!({"id": u.id, "name": u.name, "data": u.data.clone().unwrap_or_else(|| json!(""))}))
            .collect();
        json!({
            "id": self.id,
            "owner": self.owner,
            "props": {"data": self.data, "isPrivate": self.is_private, "maxPlayers": self.max_players},
            "users": users,
        })
    }
}

#[derive(Default)]
pub(super) struct Lobbies {
    pub by_id: HashMap<String, Lobby>,
    pub codes: HashMap<String, String>, // short code -> lobby id
    /// The latest locally created lobby; never a remote guest's lobby.
    pub invite_lobby: String,
    /// The transport of the latest game (not reconnect) lobby created
    /// locally; the in-game reconnect lobby reuses it (lobby_create).
    pub game_transport: Option<Transport>,
    packets: i64, // lobby transport packets seen (lobby/chat fan-out)
    packet_bytes: i64,
}

fn push_all(to: &[PeerRef], cmd: &str, args: &Value) {
    let raw = to_raw(args);
    for p in to {
        p.push(cmd, &raw);
    }
}

/// An id whose first character is a valid UserID platform char ('L' =
/// Lobby); the client rejects anything else.
fn new_lobby_id(t: Transport) -> String {
    let mut b = [0u8; 8];
    crate::rand_bytes(&mut b);
    let mark = if t == Transport::Direct {
        DIRECT_LOBBY_MARK
    } else {
        ""
    };
    format!("L{}{mark}", crate::wsx::hex(&b))
}

/// Whether a lobby data value is the boolean true. Lobby data values are JSON
/// strings holding haxe-serialized values ("t" = true); a bare JSON true is
/// accepted too.
pub(super) fn haxe_true(v: &Value) -> bool {
    match v {
        Value::String(s) => s == "t",
        Value::Bool(b) => *b,
        _ => false,
    }
}

/// Whether a host's answer is a lobby, not the null that means "no such lobby".
pub(super) fn is_lobby_info(v: &Value) -> bool {
    v.get("id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
}

impl Server {
    /// Publishes only the current local owner's lobby as the native Steam
    /// lobby. The setter queues desired state, so holding the store lock never
    /// waits for Steam or I/O.
    pub(super) fn sync_invite(&self, l: &mut Lobbies) {
        let Some(b) = &self.opt.bridge else { return };
        let mut invite = String::new();
        if let Some(lb) = l.by_id.get(&l.invite_lobby) {
            if let Some(u) = lb
                .users
                .iter()
                .find(|u| u.id == lb.owner && !u.peer.remote())
            {
                if let Some(id64) = uid::steam_id64(&u.peer.steam_id()) {
                    if let (account_id, true) = code::account_id(id64) {
                        invite = code::encode_steam(code::Steam {
                            flags: 0,
                            account_id,
                            key: self.opt.link_key,
                        });
                        l.codes.insert(invite.clone(), lb.id.clone());
                    }
                }
            }
        }
        b.set_lobby_invite(&invite);
    }

    /// Drops a disconnected player from every lobby it was in.
    pub(super) fn peer_gone(&self, p: &PeerRef) {
        if p.user_id().is_empty() {
            return;
        }
        // (lobby, the leaver as this lobby knew it, owner, remaining, recipients)
        let mut affected = Vec::new();
        {
            let mut l = self.lobbies();
            let mut emptied = Vec::new();
            for lb in l.by_id.values_mut() {
                let id = lb.id_of(p);
                let Some(i) = lb.users.iter().position(|u| u.id == id && same(&u.peer, p)) else {
                    continue;
                };
                lb.users.remove(i);
                if lb.users.is_empty() {
                    emptied.push(lb.id.clone());
                    continue;
                }
                if lb.owner == id {
                    lb.owner = lb.users[0].id.clone();
                }
                let to = lb.recipients(&id);
                affected.push((lb.id.clone(), id, lb.owner.clone(), lb.users.len(), to));
            }
            for id in emptied {
                l.by_id.remove(&id);
            }
            self.sync_invite(&mut l);
        }
        for (lid, id, owner, remain, to) in affected {
            logf!(
                self.opt.log,
                "master: {id} left lobby {lid}, {remain} member(s) remain, owner {owner}"
            );
            push_all(&to, "lobby/leave", &json!({"id": lid, "uid": id}));
            if owner != id {
                push_all(&to, "lobby/transfer", &json!({"id": lid, "uid": owner}));
            }
        }
    }

    /// A guest forwards every lobby command to the host's master, which holds
    /// the authoritative state.
    pub(super) fn lobby_command(&self, cmd: &str, args: Option<&RawValue>, p: &PeerRef) -> Out {
        if !p.remote() && cmd != "lobby/resolveShortCode" {
            if let Some(cl) = self.uplink() {
                let r = cl
                    .call(cmd, args.map(ToOwned::to_owned))
                    .map(|r| from_raw(r.as_deref()));
                match r {
                    // Not this host's invitation: it leads somewhere else, so
                    // the link goes and the invitation is resolved afresh below.
                    Ok(v) if cmd == "lobby/infoInvite" && !is_lobby_info(&v) => self.drop_link(&cl),
                    Ok(v) if cmd == "lobby/infoInvite" => {
                        return Ok(Some(self.host_answer(Some(&cl), v)))
                    }
                    r => {
                        if let (Ok(v), "lobby/join") = (&r, cmd) {
                            self.log_host_transport(v);
                        }
                        return r.map(Some);
                    }
                }
            }
        }
        let a = args_of(args);
        match cmd {
            "lobby/create" => self.lobby_create(&a, p),
            "lobby/join" => self.lobby_join(&a, p),
            "lobby/leave" => {
                self.peer_gone(p);
                Ok(None)
            }
            "lobby/list" => Ok(Some(self.lobby_list())),
            "lobby/info" => Ok(self.lobby_info(&text(&a, "id"))),
            "lobby/infoInvite" => self.lobby_info_invite(&a, p),
            "lobby/setData" => self.lobby_set_data(&a, p).map(|()| None),
            "lobby/setUserData" => self.lobby_set_user_data(&a, p).map(|()| None),
            "lobby/chat" => self.lobby_chat(&a, p).map(|()| None),
            "lobby/transfer" => self.lobby_transfer(&a, p).map(|()| None),
            "lobby/report" => Ok(None),
            "lobby/makeShortCode" => self.lobby_make_short_code(&a),
            "lobby/initInvite" => {
                let id = text(&a, "id");
                let short = self.issue_invite(&id, p)?;
                logf!(
                    self.opt.log,
                    "master: Steam invite for {id} carries the join code"
                );
                Ok(Some(json!(short)))
            }
            "lobby/resolveShortCode" => self.lobby_resolve_short_code(&a, args, p),
            _ => Err(anyhow!("Unknown command {cmd}")),
        }
    }

    /// The guest's view of the host's verdict, which travels in the lobby id
    /// the host's master minted (DIRECT_LOBBY_MARK); this only makes it visible.
    fn log_host_transport(&self, v: &Value) {
        let id = text(v, "id");
        if id.is_empty() {
            return;
        }
        let t = if id.ends_with(DIRECT_LOBBY_MARK) {
            Transport::Direct
        } else {
            Transport::Sdr
        };
        logf!(
            self.opt.log,
            "master: joined lobby {id}; the host's master chose {t} (owner id {})",
            text(v, "owner")
        );
    }

    /// Runs the transport choice for a new lobby with what is known right now.
    fn decide_transport(&self) -> anyhow::Result<(Transport, String)> {
        let (ep, ep_err) = match &self.opt.endpoint {
            Some(f) => match f() {
                Ok(e) => (e, None),
                Err(e) => (crate::nat::Endpoint::default(), Some(format!("{e:#}"))),
            },
            None => (
                crate::nat::Endpoint::default(),
                Some("no endpoint resolver".into()),
            ),
        };
        let sdr = self
            .opt
            .sdr_status
            .as_ref()
            .map(|f| f())
            .unwrap_or_default();
        let punch = self.opt.punch.as_ref().is_some_and(|p| p.ready());
        super::transport::choose_transport(&self.opt.transport, &ep, ep_err.as_deref(), &sdr, punch)
    }

    /// Replies with the new lobby id only; the client builds the LobbyInfo
    /// itself. The transport is decided here, once per lobby.
    ///
    /// The in-game reconnect lobby (Game.createLobby, data isReconnect) is the
    /// drop-in door of a game already running on some transport: its member
    /// ids must be rendered the same way as the game's players were (Game.hx
    /// getServerID compares them with state.players[].user), so it reuses the
    /// transport of the host's game lobby instead of deciding afresh.
    pub(super) fn lobby_create(&self, a: &Value, p: &PeerRef) -> Out {
        let log = &self.opt.log;
        let props = a.get("props").filter(|v| v.is_object());
        let reconnect = props
            .and_then(|pr| pr.get("data"))
            .and_then(|d| d.get("isReconnect"))
            .is_some_and(haxe_true);
        let pinned = self.lobbies().game_transport;
        let (transport, reason) = match pinned {
            Some(t) if reconnect && !p.remote() => (
                t,
                "reconnect lobby keeps the running game's transport".to_string(),
            ),
            _ => self.decide_transport().map_err(|e| {
                logf!(
                    log,
                    "master: lobby creation by {} ({}) REFUSED: {e:#}",
                    p.name(),
                    p.user_id()
                );
                anyhow!("Cannot host: {e:#}")
            })?,
        };
        let mut lb = Lobby {
            id: new_lobby_id(transport),
            owner: String::new(),
            data: Map::new(),
            is_private: None,
            max_players: None,
            users: Vec::new(),
            transport,
        };
        lb.owner = lb.id_of(p);
        if lb.owner.is_empty() {
            logf!(
                log,
                "master: lobby creation by {} ({}) REFUSED: the game reported no player id",
                p.name(),
                p.user_id()
            );
            bail!("Cannot host: the game reported no player id");
        }
        let mut my_data = None;
        if let Some(pr) = props {
            if let Some(Value::Object(d)) = pr.get("data") {
                lb.data.clone_from(d);
            }
            lb.is_private = pr.get("isPrivate").and_then(Value::as_bool);
            lb.max_players = pr.get("maxPlayers").and_then(Value::as_i64);
            my_data = pr.get("myData").cloned();
        }
        lb.users.push(super::lobby::Member {
            id: lb.owner.clone(),
            name: p.name(),
            data: my_data,
            peer: p.clone(),
        });
        let (id, owner) = (lb.id.clone(), lb.owner.clone());
        {
            let mut l = self.lobbies();
            l.by_id.insert(id.clone(), lb);
            if !p.remote() {
                l.invite_lobby.clone_from(&id);
                self.sync_invite(&mut l);
                if !reconnect {
                    l.game_transport = Some(transport);
                }
            }
        }
        logf!(
            log,
            "master: lobby {id} created by {} ({owner}), owner {owner}, props {}",
            p.name(),
            trunc(&props.map_or("null".into(), Value::to_string))
        );
        logf!(
            log,
            "master: lobby {id} game transport: {transport} ({reason})"
        );
        if transport == Transport::Sdr && p.steam_id().is_empty() {
            logf!(
                log,
                "master: WARNING: lobby {id} is on SDR but the host reported no Steam id; \
                 its game id {owner} is used and must be its Steam id for SDR to reach it"
            );
        }
        logf!(
            log,
            "master: lobby {id} lobby-phase transport is lobby/chat on this connection \
             (mpman LobbyService, no second socket)"
        );
        Ok(Some(json!(id)))
    }

    /// A guest that joins the same lobby a second time without having left is
    /// simply refreshed ("rejoined"). The game itself cannot survive that (the
    /// new Lobby's constructor leaves Lobby.inst first, and the HostWT.connect
    /// that follows throws "Lobby not found <id>"); it only happens when the
    /// first join hung and the player clicked Join again.
    pub(super) fn lobby_join(&self, a: &Value, p: &PeerRef) -> Out {
        let log = &self.opt.log;
        let lid = text(a, "id");
        let data = a.get("data").cloned();
        let mut l = self.lobbies();
        let Some(lb) = l.by_id.get_mut(&lid) else {
            bail!("Unknown lobby {lid}")
        };
        let id = lb.id_of(p);
        if id.is_empty() {
            // An old helper's link hello carries no game id; a fallback id
            // would match no save and no LobbyState player (see id_of).
            drop(l);
            logf!(
                log,
                "master: {} ({}) cannot join lobby {lid}: no player id (different wartales-mp version?)",
                p.name(),
                p.user_id()
            );
            bail!("Cannot join: your wartales-mp version does not send a player id; update it");
        }
        let mut found = false;
        if let Some(u) = lb.users.iter_mut().find(|u| u.id == id) {
            // A guest reconnecting over a new link takes its slot over; the
            // local game's slot is never taken by a remote peer, whatever id
            // that peer claims.
            if p.remote() && !u.peer.remote() {
                drop(l);
                logf!(
                    log,
                    "master: {} claimed the local player's id {id} in lobby {lid}; refused",
                    p.name()
                );
                bail!("Already in lobby");
            }
            u.data.clone_from(&data);
            u.peer = p.clone();
            u.name = p.name();
            found = true;
        }
        if !found {
            if lb
                .max_players
                .is_some_and(|m| i64::try_from(lb.users.len()).unwrap_or(i64::MAX) >= m)
            {
                bail!("Lobby is full");
            }
            lb.users.push(Member {
                id: id.clone(),
                name: p.name(),
                data: data.clone(),
                peer: p.clone(),
            });
        }
        let info = lb.info();
        let transport = lb.transport;
        let to = if found {
            Vec::new()
        } else {
            lb.recipients(&id)
        };
        drop(l);

        if transport == Transport::Sdr && p.steam_id().is_empty() {
            logf!(
                log,
                "master: WARNING: {} joined SDR lobby {lid} without a Steam id (as {id}); \
                 its game id is used and must be its Steam id for SDR to reach it",
                p.name()
            );
        }
        if found {
            logf!(log, "master: {} ({id}) rejoined lobby {lid}", p.name());
        } else {
            // MPLobby.onCommand@54985 runs haxe.Unserializer over args.data and
            // answers false - dropping the joiner - when it yields null, so the
            // push always carries something that unserializes: "z" is the haxe
            // encoding of Int 0.
            let data = data.unwrap_or_else(|| json!("z"));
            push_all(
                &to,
                "lobby/join",
                &json!({"id": lid, "uid": id, "name": p.name(), "data": data}),
            );
            logf!(
                log,
                "master: {} ({id}) joined lobby {lid} ({transport}), told {} peer(s)",
                p.name(),
                to.len()
            );
        }
        Ok(Some(info))
    }

    pub(super) fn lobby_list(&self) -> Value {
        let l = self.lobbies();
        Value::Array(
            l.by_id
                .values()
                .filter(|lb| lb.is_private != Some(true))
                .map(Lobby::info)
                .collect(),
        )
    }

    /// None (no args) is what the client treats as "not found".
    pub(super) fn lobby_info(&self, id: &str) -> Option<Value> {
        self.lobbies().by_id.get(id).map(Lobby::info)
    }

    pub(super) fn lobby_set_data(&self, a: &Value, p: &PeerRef) -> anyhow::Result<()> {
        let lid = text(a, "id");
        let mut l = self.lobbies();
        let Some(lb) = l.by_id.get_mut(&lid) else {
            bail!("Unknown lobby {lid}")
        };
        let data = match a.get("data") {
            Some(Value::Object(m)) => Some(m.clone()),
            Some(Value::Null) => None,
            _ => bail!("Invalid data"),
        };
        if lb.member_of(p).is_none_or(|i| lb.users[i].id != lb.owner) {
            bail!("Cannot set data if not owner");
        }
        if let Some(d) = &data {
            lb.data.extend(d.clone());
        }
        let to = lb.recipients(&lb.id_of(p));
        drop(l);
        push_all(
            &to,
            "lobby/setData",
            &json!({"id": lid, "data": data.map_or(Value::Null, Value::Object)}),
        );
        Ok(())
    }

    pub(super) fn lobby_set_user_data(&self, a: &Value, p: &PeerRef) -> anyhow::Result<()> {
        let lid = text(a, "id");
        let mut l = self.lobbies();
        let Some(lb) = l.by_id.get_mut(&lid) else {
            bail!("Unknown lobby {lid}")
        };
        let Some(i) = lb.member_of(p) else {
            bail!("Not in lobby {lid}")
        };
        let data = a.get("data").cloned();
        lb.users[i].data.clone_from(&data);
        let id = lb.users[i].id.clone();
        let to = lb.recipients(&lb.id_of(p));
        drop(l);
        push_all(
            &to,
            "lobby/setUserData",
            &json!({"id": lid, "uid": id, "data": data}),
        );
        Ok(())
    }

    /// lobby/chat is also the lobby transport: mpman's LobbyService has no
    /// socket of its own and tunnels every hxbit packet through lobby/chat as
    /// a haxe.Serializer string of LobbyMessageData.Packet(Bytes, targetUid)
    /// (SERVER-CONTRACT §7). The fan-out reaches every other member verbatim,
    /// which then filters on the target uid embedded in the payload
    /// (LobbyService.onMessage@54929): member ids are exactly the ids the
    /// games reported (id_of), as the vanilla server sent them.
    pub(super) fn lobby_chat(&self, a: &Value, p: &PeerRef) -> anyhow::Result<()> {
        let lid = text(a, "id");
        let mut l = self.lobbies();
        let Some(lb) = l.by_id.get(&lid) else {
            bail!("Unknown lobby {lid}")
        };
        let Some(i) = lb.member_of(p) else {
            bail!("Not in lobby {lid}")
        };
        let id = lb.users[i].id.clone();
        let to = lb.recipients(&id);
        let msg = a.get("msg").cloned().unwrap_or(Value::Null);
        let args = to_raw(&json!({"id": lid, "uid": id, "msg": msg}));
        let size = a.get("msg").map_or(0, |m| m.to_string().len());
        l.packets += 1;
        l.packet_bytes += i64::try_from(size).unwrap_or(i64::MAX);
        let (n, total) = (l.packets, l.packet_bytes);
        drop(l);
        for t in &to {
            t.push("lobby/chat", &args);
        }
        if n == 1 || n % TRANSPORT_LOG_EVERY == 0 {
            logf!(
                self.opt.log,
                "master: lobby transport {lid}: packet #{n} from {id}, {size} B to {} peer(s), {total} B total",
                to.len()
            );
        }
        Ok(())
    }

    /// The new owner comes from the client, so it is only ever accepted when it
    /// names a member of this lobby: member ids are rendered by us (id_of).
    pub(super) fn lobby_transfer(&self, a: &Value, p: &PeerRef) -> anyhow::Result<()> {
        let (lid, to_uid) = (text(a, "id"), text(a, "uid"));
        let mut l = self.lobbies();
        let Some(lb) = l.by_id.get_mut(&lid) else {
            bail!("Unknown lobby {lid}")
        };
        if lb.member_of(p).is_none_or(|i| lb.users[i].id != lb.owner) {
            bail!("Cannot transfer if not owner");
        }
        if !lb.users.iter().any(|u| u.id == to_uid) {
            bail!("Unknown user {to_uid}");
        }
        lb.owner.clone_from(&to_uid);
        let to = lb.recipients(&lb.id_of(p));
        self.sync_invite(&mut l);
        drop(l);
        push_all(&to, "lobby/transfer", &json!({"id": lid, "uid": to_uid}));
        Ok(())
    }

    /// Issues a direct-only manual code. Steam invitations carry their own
    /// routes independently. The endpoint is resolved for every request
    /// because the WAN address can change between lobbies.
    fn lobby_make_short_code(&self, a: &Value) -> Out {
        let lid = text(a, "id");
        if !self.lobbies().by_id.contains_key(&lid) {
            bail!("Unknown lobby {lid}");
        }
        let ep = self.endpoint_route();
        let short = match self.punch_code(ep.as_ref().ok()) {
            Some(s) => s,
            None => match &ep {
                Err(e) => bail!("Cannot create direct join code: {e:#}; use a Steam invitation"),
                Ok(e) => code::encode_direct(*e)?,
            },
        };
        self.lobbies().codes.insert(short.clone(), lid.clone());
        match code::decode_any(&short) {
            Ok(code::Code {
                endpoint: Some(e),
                punch: Some(u),
                ..
            }) => logf!(
                self.opt.log,
                "master: manual join code for {lid} issued (direct {}, punch {})",
                e.addr(),
                u.addr()
            ),
            _ => logf!(
                self.opt.log,
                "master: manual join code for {lid} issued (direct {})",
                ep.as_ref().map_or("?".into(), |e| e.addr().to_string())
            ),
        }
        Ok(Some(json!({"shortCode": short})))
    }

    /// The Steam "invite friends" path. The game asks for it once per lobby,
    /// before it creates a friends-only Steam lobby, and writes the string we
    /// answer into that lobby's "invite" data. A friend who clicks "Join Game"
    /// hands the same string to their own helper as lobby/infoInvite, so it
    /// must reach our lobby on its own: it is the join code. The SDR route is
    /// always the owner's: a guest asking for the code (inviting its own Steam
    /// friends) must still send them to the host.
    fn issue_invite(&self, lid: &str, p: &PeerRef) -> anyhow::Result<String> {
        let log = &self.opt.log;
        let (transport, owner) = {
            let l = self.lobbies();
            let Some(lb) = l.by_id.get(lid) else {
                bail!("Unknown lobby {lid}")
            };
            let owner = lb
                .users
                .iter()
                .rfind(|u| u.id == lb.owner)
                .map_or_else(|| p.clone(), |u| u.peer.clone());
            (lb.transport, owner)
        };
        let ep = self.endpoint_route();
        let (st, sdr_why) = self.steam_route(&owner);
        let short = match (&ep, st) {
            (Ok(e), Some(s)) => code::encode_combined(*e, s)?,
            (Err(_), Some(s)) => code::encode_steam(s),
            (Ok(e), None) => {
                // No SDR route. A pending bridge, when only SDR reaches us from
                // the internet (the endpoint is unverified), is a "not yet",
                // not a code that could not work.
                let (verified, _) = self.endpoint_verified();
                if transport == Transport::Sdr || !verified {
                    if let Some(b) = &self.opt.bridge {
                        let (ready, why) = b.ready();
                        if !ready && !b.status().known {
                            logf!(log, "master: join code for {lid} not issued yet: {why}");
                            bail!("Steam relay not ready yet ({why}); ask for the code again in a few seconds");
                        }
                    }
                }
                code::encode_endpoint(*e)?
            }
            (Err(ep_err), None) => {
                logf!(
                    log,
                    "master: join code for {lid} cannot be issued: endpoint: {ep_err:#}; SDR: {sdr_why}"
                );
                if self.opt.bridge.as_ref().is_some_and(|b| !b.status().known) {
                    bail!("Steam relay not ready yet ({sdr_why}); ask for the code again in a few seconds");
                }
                bail!("No route to offer: {ep_err:#}; {sdr_why}");
            }
        };
        self.lobbies().codes.insert(short.clone(), lid.into());
        let mut routes = String::new();
        if let Ok(e) = &ep {
            routes = format!("direct {}", e.addr());
            if !self.endpoint_verified().0 {
                routes += " (unverified)";
            }
        }
        match st {
            Some(s) => {
                if !routes.is_empty() {
                    routes += ", then ";
                }
                let _ = write!(routes, "SDR to SteamID {}", s.steam_id64());
            }
            None => {
                let _ = write!(routes, "; no SDR route: {sdr_why}");
            }
        }
        logf!(log, "master: join code for {lid} issued (routes: {routes})");
        Ok(short)
    }

    /// Serves a Steam "Join Game": the value is what lobby/initInvite answered
    /// on the host, so it resolves like a typed join code and the game's
    /// lobby/join that follows is forwarded to the host as usual. A bare lobby
    /// id, which is what the vanilla master used, still works locally.
    fn lobby_info_invite(&self, a: &Value, p: &PeerRef) -> Out {
        let invite = text(a, "invite");
        if let Some(info) = self.lobby_info(&invite) {
            return Ok(if p.remote() {
                self.with_host_mod(Some(info))
            } else {
                Some(info)
            });
        }
        if p.remote() {
            let id = self.lobbies().codes.get(&invite).cloned();
            return Ok(id.and_then(|id| self.with_host_mod(self.lobby_info(&id))));
        }
        let resolve = json!({"shortCode": invite, "filters": {}});
        let raw = to_raw(&resolve);
        self.lobby_resolve_short_code(&resolve, Some(&raw), p)
    }

    /// Where a guest becomes a guest: it decodes the code, opens the
    /// proxy-link to that host and asks the host's master for the lobby.
    pub(super) fn lobby_resolve_short_code(
        &self,
        a: &Value,
        args: Option<&RawValue>,
        p: &PeerRef,
    ) -> Out {
        let short = text(a, "shortCode");
        let mapped = self.lobbies().codes.get(&short).cloned();
        if p.remote() {
            return Ok(mapped.and_then(|id| self.with_host_mod(self.lobby_info(&id))));
        }
        // Our own code (host pasting its own code) resolves locally.
        if let Some(id) = mapped {
            return Ok(self.lobby_info(&id));
        }
        // A guest's reply code: the host punches back toward it.
        if let Ok(code::Code { reply: Some(r), .. }) = code::decode_any(&short) {
            return Err(self.punch_reply(r));
        }
        if let Some(cl) = self.uplink() {
            let v = from_raw(
                cl.call("lobby/resolveShortCode", args.map(ToOwned::to_owned))?
                    .as_deref(),
            );
            if is_lobby_info(&v) {
                return Ok(Some(self.host_answer(Some(&cl), v)));
            }
            // The host we are linked to does not know this code: it is another
            // host's, so the code's own routes are tried instead.
            self.drop_link(&cl);
        }
        let c = code::decode_any(&short).map_err(|_| anyhow!("Invalid join code"))?;
        self.cascade(&c, args, p).map(Some)
    }
}
