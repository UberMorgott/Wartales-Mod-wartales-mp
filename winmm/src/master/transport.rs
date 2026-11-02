//! How a lobby's game traffic travels, decided at lobby creation.

use std::fmt;

use anyhow::bail;

use crate::nat::Endpoint;
use crate::sdrbridge::Status;

/// There are exactly two: our direct relay on the host's public port, or
/// Steam Datagram Relay through the in-process SDR transport. The game's
/// legacy Steam P2P path is gone by construction (it is diverted for good),
/// so nothing here can select it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Marks the lobby id (DIRECT_LOBBY_MARK); at start the host's game asks
    /// instance/get and start_answer points it at our relay, or at its Steam
    /// path when every guest joined over SDR.
    Direct,
    /// An unmarked lobby id and the players' real Steam ids.
    Sdr,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Transport::Sdr => "SDR",
            Transport::Direct => "direct",
        })
    }
}

/// Modes accepted by Options.transport.
pub const MODE_AUTO: &str = "auto";
pub const MODE_DIRECT: &str = "direct";
pub const MODE_SDR: &str = "sdr";

/// The honest failure: nothing we offer can carry this lobby.
const NO_TRANSPORT: &str = "no usable transport";

/// Decides a lobby's transport at creation:
///
///   - a VERIFIED public endpoint (inbound from the internet has actually
///     arrived on the port this run) keeps the direct relay, which needs no
///     third party at all. A UPnP mapping plus a STUN address is only a hint
///     and never enough: it was seen to be wrong on a real machine;
///   - otherwise also direct-marked, but the game's own transport is decided
///     at game start (start_answer, issue #7) from how the guests joined. The
///     join code still offers the hinted endpoint first, then our SteamID;
///   - if SDR is known not to work in this game process, a hinted endpoint is
///     used unverified (and logged as such) because it is the only route left;
///     with no endpoint at all the lobby is refused with the reasons. "Not
///     known yet" is not a refusal: the join code request is what fails, with
///     a retry hint, until SDR is ready.
///
/// punch (the UDP socket knows its address) makes join codes work without a
/// reachable TCP endpoint, so with no SDR it also keeps the lobby direct.
/// mode forces either transport; forcing SDR against a known-bad SDR is still
/// refused. The returned string is the reason, for the log.
pub fn choose_transport(
    mode: &str,
    ep: &Endpoint,
    ep_err: Option<&str>,
    sdr: &Status,
    punch: bool,
) -> anyhow::Result<(Transport, String)> {
    let endpoint = || {
        if let Some(e) = ep_err {
            return format!("public endpoint unknown ({e})");
        }
        if ep.verified {
            return format!(
                "public endpoint {} verified reachable (inbound seen; via {})",
                ep.addr, ep.source
            );
        }
        if ep.reachable {
            return format!(
                "public endpoint {} hinted by {} but UNVERIFIED (no inbound seen yet)",
                ep.addr, ep.source
            );
        }
        if !ep.warning.is_empty() {
            return format!(
                "endpoint {} not internet-reachable: {}",
                ep.addr, ep.warning
            );
        }
        format!("endpoint {} not internet-reachable", ep.addr)
    };
    let sdr_state = || {
        if !sdr.known && !sdr.reason.is_empty() {
            format!("SDR pending ({})", sdr.reason)
        } else if !sdr.known {
            "SDR status unknown (shim has not reported yet)".into()
        } else if sdr.ok {
            "SDR ready".into()
        } else {
            format!("SDR unavailable ({})", sdr.reason)
        }
    };
    let verified = ep_err.is_none() && ep.verified;

    match mode {
        MODE_DIRECT => {
            return Ok((
                Transport::Direct,
                format!("forced by -transport direct; {}", endpoint()),
            ))
        }
        MODE_SDR => {
            if sdr.known && !sdr.ok {
                bail!("{NO_TRANSPORT}: -transport sdr, but {}", sdr_state());
            }
            return Ok((
                Transport::Sdr,
                format!("forced by -transport sdr; {}", sdr_state()),
            ));
        }
        MODE_AUTO | "" => {}
        _ => bail!("unknown transport mode {mode:?}"),
    }

    if verified {
        return Ok((Transport::Direct, endpoint()));
    }
    if sdr.known && !sdr.ok {
        if ep_err.is_none() && ep.reachable {
            // The only route left is the unproven one: take it, and say so.
            return Ok((
                Transport::Direct,
                format!(
                    "no SDR, falling back to the unverified endpoint: {}; {}",
                    endpoint(),
                    sdr_state()
                ),
            ));
        }
        if punch {
            return Ok((
                Transport::Direct,
                format!(
                    "no SDR; join codes punch through UDP: {}; {}",
                    endpoint(),
                    sdr_state()
                ),
            ));
        }
        bail!("{NO_TRANSPORT}: {}; {}", endpoint(), sdr_state());
    }
    // Unverified endpoint, SDR not known bad: the lobby is marked direct all
    // the same, and the game's transport is chosen when it starts.
    Ok((
        Transport::Direct,
        format!(
            "chosen at game start by how the guests joined; {}; {}",
            endpoint(),
            sdr_state()
        ),
    ))
}
