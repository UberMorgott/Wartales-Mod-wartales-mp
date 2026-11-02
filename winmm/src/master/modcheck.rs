//! The mod-file check on join (see modver).
//!
//! The host's master adds its fingerprint to every LobbyInfo it answers a
//! remote guest's lobby/resolveShortCode or lobby/infoInvite with (MOD_FIELD).
//! The guest's master takes it out again before the game sees the answer and
//! compares it with its own. On a mismatch it drops the link and rewrites the
//! lobby's data so the game refuses to join by itself: Lobby.checkJoinLobby
//! (Lobby.hx:868, the gate of both join by code and Steam invite) shows our
//! text from MOD_ERROR_FIELD (the patcher's mod_version pass), and the vanilla
//! test of data.version, which the marker no longer matches, blocks the join
//! with the game's own version error even where that pass did not apply.
//! Missing information (an older helper on either side, a file that could not
//! be hashed) is logged as a warning and never blocks.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use super::Server;
use crate::modver::{self, describe, Info};
use crate::{link, logf};

/// The LobbyInfo field carrying the host's fingerprint over the proxy-link;
/// the game never sees it.
pub(super) const MOD_FIELD: &str = "mpMod";
/// The lobby data key Lobby.checkJoinLobby shows when set;
/// patcher/src/mod_version.rs reads it under this exact name.
pub(super) const MOD_ERROR_FIELD: &str = "mpModError";
/// Replaces data.version on a mismatch.
pub(super) const MOD_MISMATCH_VERSION: &str = "wartales-mp: mod files differ from the host";
/// Bounds the wait for this helper's own fingerprint (hashed in the
/// background at startup; normally long done by the first join).
const MOD_WAIT: Duration = Duration::from_secs(2);

/// s serialized like haxe.Serializer: "y<len>:<urlEncode(s)>". Every byte
/// outside [A-Za-z0-9] is percent-encoded, which HashLink's url_decode (UTF-8
/// aware, '+' is a space) reads back exactly.
pub(super) fn haxe_serialize(s: &str) -> String {
    use std::fmt::Write as _;
    let mut enc = String::new();
    for c in s.bytes() {
        if c.is_ascii_alphanumeric() {
            enc.push(c as char);
        } else {
            let _ = write!(enc, "%{c:02X}"); // writing to a String cannot fail
        }
    }
    format!("y{}:{enc}", enc.len())
}

impl Server {
    /// This helper's fingerprint, None when it is not known (yet).
    pub(super) fn local_mod(&self) -> Option<Info> {
        self.opt.r#mod.as_ref().and_then(|m| m.get(MOD_WAIT))
    }

    /// Adds this host's fingerprint to a LobbyInfo answered to a remote guest.
    pub(super) fn with_host_mod(&self, info: Option<Value>) -> Option<Value> {
        let mut info = info?;
        if let (Value::Object(m), Some(mine)) = (&mut info, self.local_mod()) {
            m.insert(
                MOD_FIELD.into(),
                serde_json::to_value(mine).expect("an Info always marshals"),
            );
        }
        Some(info)
    }

    /// The host's note on a guest's hello: the guest blocks itself on a
    /// mismatch, the host only learns about it here.
    pub(super) fn note_guest_mod(&self, name: &str, guest: Option<&Info>) {
        let local = self.local_mod();
        let r = modver::compare(local.as_ref(), guest);
        let log = &self.opt.log;
        if guest.is_none() {
            logf!(log, "WARNING: link: guest {name:?} runs an older wartales-mp without version info; its mod files are not checked");
        } else if r.mismatch() {
            logf!(
                log,
                "WARNING: link: guest {name:?} has different mod files ({}): its game will refuse to join and tell the player \
                 to download the archive again; guest: {}; host: {}",
                r.differ.join(", "),
                describe(guest),
                describe(local.as_ref())
            );
        } else if !r.unchecked.is_empty() {
            logf!(
                log,
                "WARNING: link: guest {name:?}: mod files not compared (unknown on one side): {}",
                r.unchecked.join(", ")
            );
        } else {
            logf!(
                log,
                "link: guest {name:?} has the same mod files ({})",
                r.compared.join(", ")
            );
        }
    }

    /// The guest's check of a LobbyInfo that came from the host's master. It
    /// always strips the fingerprint field; on a mismatch it rewrites the
    /// lobby data so the game refuses the join, and reports it so the caller
    /// drops the link. A null or non-object answer is passed through untouched.
    pub(super) fn check_host_mod(&self, raw: Value) -> (Value, bool) {
        let Value::Object(mut info) = raw else {
            return (raw, false);
        };
        let host: Option<Info> = info
            .remove(MOD_FIELD)
            .and_then(|v| serde_json::from_value(v).ok());
        // MPLobby.init@54937 reads the lobby data from info.props.data.
        let props: Option<Map<String, Value>> =
            info.get("props").and_then(Value::as_object).cloned();
        let mut data: Option<Map<String, Value>> = props
            .as_ref()
            .and_then(|p| p.get("data"))
            .and_then(Value::as_object)
            .cloned();
        // Only this master may ask the game to show a mod error.
        if let Some(d) = data.as_mut() {
            d.remove(MOD_ERROR_FIELD);
        }

        let local = self.local_mod();
        let r = modver::compare(local.as_ref(), host.as_ref());
        let log = &self.opt.log;
        if host.is_none() {
            logf!(log, "WARNING: the host runs an older wartales-mp without version info; mod files are not checked");
        } else if r.mismatch() {
            logf!(
                log,
                "MISMATCH: mod files differ from the host's: {}; host: {}; ours: {}; the join is refused",
                r.differ.join(", "),
                describe(host.as_ref()),
                describe(local.as_ref())
            );
        } else if !r.unchecked.is_empty() {
            logf!(
                log,
                "WARNING: mod files not compared with the host (unknown on one side): {}",
                r.unchecked.join(", ")
            );
        } else {
            logf!(
                log,
                "master: mod files match the host's ({})",
                r.compared.join(", ")
            );
        }
        if r.mismatch() {
            let d = data.get_or_insert_default();
            d.insert(
                "version".into(),
                Value::String(haxe_serialize(MOD_MISMATCH_VERSION)),
            );
            d.insert(
                MOD_ERROR_FIELD.into(),
                Value::String(haxe_serialize(&modver::message(&r.differ))),
            );
        }
        if let Some(d) = data {
            let mut p = props.unwrap_or_default();
            p.insert("data".into(), Value::Object(d));
            info.insert("props".into(), Value::Object(p));
        }
        (Value::Object(info), r.mismatch())
    }

    /// Runs check_host_mod on a LobbyInfo the host's master answered over cl
    /// and drops that link on a mismatch: the game will not join, and the next
    /// code goes through the cascade (and this check) again.
    pub(super) fn host_answer(&self, cl: Option<&Arc<link::Client>>, raw: Value) -> Value {
        let (out, mismatch) = self.check_host_mod(raw);
        if let (true, Some(cl)) = (mismatch, cl) {
            self.drop_link(cl);
        }
        out
    }
}
