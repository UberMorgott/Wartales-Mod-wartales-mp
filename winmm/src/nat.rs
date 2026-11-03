//! Port of internal/nat: discovers the public endpoint other players must
//! connect to, UPnP first (it can also open the port), STUN as a fallback.
//!
//! A router that is itself behind carrier NAT happily answers
//! GetExternalIPAddress with a private address; such an answer is rejected here
//! and STUN is asked instead, while the UPnP port mapping is kept, since the
//! router still needs it to forward traffic inwards.

mod addr;
mod mapping;
mod stun;
mod upnp;

pub use mapping::{classify, Mapping, STUN_SERVERS};
pub use stun::{binding_request, is_stun, parse_binding};

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::{logf, Log};
use addr::{describe_ip, is_public_ipv4};

/// How long the UPnP port mapping is requested for; it is refreshed while we run.
const MAPPING_LEASE: Duration = Duration::from_hours(2);

/// How old a resolved endpoint may be before it is resolved again: the WAN
/// address can change between lobbies (observed alternating on a carrier that
/// hands out two), and the UPnP gateway may answer one time and not the next.
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(90);

/// The address we hand out, plus how honest we can be about it.
///
/// `reachable` is only a HINT: a router that answered UPnP and a STUN server
/// that saw a public address prove nothing about inbound traffic (a firewall
/// rule missing on this machine, a mapping the router silently dropped, a
/// second NAT). `verified` is the proof: a connection from a public address has
/// actually arrived on the port during this run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoint {
    /// "ip:port"
    pub addr: String,
    /// the public (or LAN fallback) address
    pub ip: Option<IpAddr>,
    /// "UPnP", "STUN", "UPnP+STUN" (mapped port + STUN address), "LAN"
    pub source: &'static str,
    /// ip is a public IPv4 (a hint, see above)
    pub reachable: bool,
    /// inbound from the internet has been seen on the port this run
    pub verified: bool,
    /// a UPnP port mapping is in place
    pub mapped: bool,
    /// why the endpoint is not internet-reachable
    pub warning: String,
    /// when it was resolved
    pub at: Option<Instant>,
}

#[derive(Default)]
struct State {
    igd: Option<upnp::Igd>,
    ep: Endpoint,
    done: bool,
    mapped: bool,
    verified_from: Option<IpAddr>,
}

/// Resolves and caches the public "ip:port" for our relay port.
pub struct Mapper {
    port: u16,
    log: Log,
    max_age: Duration,
    st: Mutex<State>,
}

impl Mapper {
    /// Creates a mapper for the given public TCP port.
    pub fn new(port: u16, log: Log) -> Self {
        Mapper {
            port,
            log,
            max_age: DEFAULT_MAX_AGE,
            st: Mutex::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.st
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that a connection arrived on the port from ip. Only a public
    /// source proves internet reachability; a LAN guest proves nothing about
    /// the router. Reports whether the endpoint is now verified.
    pub fn mark_inbound(&self, ip: IpAddr) -> bool {
        if !is_public_ipv4(ip) {
            return false;
        }
        let mut st = self.state();
        // Before the first resolve there is no address to verify yet.
        if st.done && st.verified_from.is_none() {
            st.verified_from = Some(ip);
            st.ep.verified = true;
            logf!(
                self.log,
                "nat: inbound connection from {}: the public endpoint is now VERIFIED reachable",
                ip.to_canonical()
            );
        }
        true
    }

    /// Returns the cached public endpoint address, resolving it on first use.
    pub fn addr(&self) -> anyhow::Result<String> {
        Ok(self.endpoint()?.addr)
    }

    /// Resolves (at most every 90 s) and returns the full result, including
    /// whether the address is actually reachable from the internet.
    pub fn endpoint(&self) -> anyhow::Result<Endpoint> {
        let mut st = self.state();
        if st.done && st.ep.at.is_some_and(|at| at.elapsed() < self.max_age) {
            return Ok(st.ep.clone());
        }
        let mut ep = match self.resolve(&mut st) {
            Ok(ep) => ep,
            Err(e) if st.done => {
                logf!(
                    self.log,
                    "nat: re-resolving the public endpoint failed ({e:#}), keeping {}",
                    st.ep.addr
                );
                return Ok(st.ep.clone());
            }
            Err(e) => return Err(e),
        };
        let ip = ep.ip.expect("resolve always sets ip");
        ep.addr = SocketAddr::new(ip, self.port).to_string();
        ep.mapped = st.mapped;
        ep.at = Some(Instant::now());
        // Verification is per address: a new WAN address starts unproven.
        if st.done && st.ep.ip != ep.ip {
            logf!(
                self.log,
                "nat: public address changed {} -> {ip}",
                st.ep.ip.map_or("<nil>".into(), |i| i.to_string())
            );
            st.verified_from = None;
        }
        ep.verified = st.verified_from.is_some() && st.done;
        st.ep = ep;
        st.done = true;
        let note = if st.ep.verified {
            ", verified by inbound traffic"
        } else if !st.ep.reachable {
            ", NOT internet-reachable"
        } else {
            ", unverified"
        };
        logf!(
            self.log,
            "nat: public endpoint {} (source {}{note})",
            st.ep.addr,
            st.ep.source
        );
        Ok(st.ep.clone())
    }

    /// Runs UPnP, then STUN, then the LAN fallback; called with the lock held.
    fn resolve(&self, st: &mut State) -> anyhow::Result<Endpoint> {
        let mut upnp_ip = None;
        match upnp::discover_igd(Duration::from_secs(3)) {
            Ok(igd) => {
                if let Err(e) = igd.add_port_mapping(self.port, "wartales-mp", MAPPING_LEASE) {
                    logf!(self.log, "nat: AddPortMapping failed: {e:#}");
                } else {
                    st.mapped = true;
                    logf!(self.log, "nat: UPnP mapped TCP {}", self.port);
                }
                let ext = igd.external_ip();
                st.igd = Some(igd);
                match ext {
                    Ok(ip) if is_public_ipv4(ip) => {
                        logf!(self.log, "nat: external ip {ip} (UPnP)");
                        return Ok(Endpoint {
                            ip: Some(ip),
                            source: "UPnP",
                            reachable: true,
                            ..Default::default()
                        });
                    }
                    Ok(ip) => {
                        upnp_ip = Some(ip);
                        // Double NAT: keep the mapping, distrust the address.
                        logf!(
                            self.log,
                            "nat: UPnP reported external ip {}, not usable; keeping the port mapping and asking STUN",
                            describe_ip(upnp_ip)
                        );
                    }
                    Err(e) => logf!(self.log, "nat: GetExternalIPAddress failed: {e:#}"),
                }
            }
            Err(e) => logf!(self.log, "nat: no UPnP gateway: {e:#}"),
        }

        let mut stun_ip = None;
        match stun::stun_external_ip(stun::DEFAULT_STUN) {
            Ok(ip) if is_public_ipv4(ip) => {
                // Port from the UPnP mapping, address from STUN.
                let source = if st.mapped { "UPnP+STUN" } else { "STUN" };
                logf!(
                    self.log,
                    "nat: external ip {ip} (STUN); UPnP said {}",
                    describe_ip(upnp_ip)
                );
                return Ok(Endpoint {
                    ip: Some(ip),
                    source,
                    reachable: true,
                    ..Default::default()
                });
            }
            Ok(ip) => {
                stun_ip = Some(ip);
                logf!(
                    self.log,
                    "nat: STUN reported external ip {}, not usable",
                    describe_ip(stun_ip)
                );
            }
            Err(e) => logf!(self.log, "nat: STUN failed: {e:#}"),
        }

        let warning = format!(
            "no public address found: UPnP={}, STUN={}; the join code works on the LAN only",
            describe_ip(upnp_ip),
            describe_ip(stun_ip)
        );
        logf!(self.log, "nat: {warning}");
        let Some(ip) = local_ip().or(upnp_ip).or(stun_ip) else {
            bail!("could not determine any address: {warning}");
        };
        logf!(self.log, "nat: falling back to the LAN address {ip}");
        Ok(Endpoint {
            ip: Some(ip),
            source: "LAN",
            reachable: false,
            warning,
            ..Default::default()
        })
    }

    /// Removes the UPnP mapping we added, if any.
    pub fn close(&self) {
        let mut st = self.state();
        if st.mapped {
            if let Some(igd) = &st.igd {
                if let Err(e) = igd.delete_port_mapping(self.port) {
                    logf!(self.log, "nat: DeletePortMapping failed: {e:#}");
                }
                st.mapped = false;
            }
        }
    }
}

/// This machine's primary IPv4 address (the route towards 8.8.8.8; nothing is sent).
fn local_ip() -> Option<IpAddr> {
    let c = UdpSocket::bind("0.0.0.0:0").ok()?;
    c.connect("8.8.8.8:53").ok()?;
    Some(c.local_addr().ok()?.ip()).filter(IpAddr::is_ipv4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a public source verifies the endpoint.
    #[test]
    fn test_mark_inbound() {
        let m = Mapper {
            max_age: Duration::MAX,
            ..Mapper::new(14250, Log::discard())
        };
        {
            let mut st = m.state();
            st.done = true;
            st.ep = Endpoint {
                ip: Some([45, 154, 88, 66].into()),
                addr: "45.154.88.66:14250".into(),
                reachable: true,
                at: Some(Instant::now()),
                ..Default::default()
            };
        }
        assert!(
            !m.mark_inbound([192, 168, 1, 7].into()),
            "a LAN guest must not verify internet reachability"
        );
        assert!(
            !m.endpoint().unwrap().verified,
            "verified without any public inbound"
        );
        assert!(
            m.mark_inbound([8, 8, 8, 8].into()),
            "a public source verifies"
        );
        assert!(
            m.endpoint().unwrap().verified,
            "not verified after a public inbound"
        );
    }
}
