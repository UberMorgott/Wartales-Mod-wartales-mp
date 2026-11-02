//! STUN servers and what their answers say about our NAT.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;

/// STUN servers answering binding requests on UDP. They were each probed live on
/// 2026-10-15; several are asked at once because any one of them may be
/// unreachable from a given network (issue #7: Google's timed out on a Russian
/// carrier). stun1.l.google.com is left out: it is the same IP as stun.l.
pub const STUN_SERVERS: &[&str] = &[
    "stun.cloudflare.com:3478",
    "stun.l.google.com:19302",
    "global.stun.twilio.com:3478",
    "stun.nextcloud.com:3478",
    "stun.voip.blackberry.com:3478",
    "stun.sipnet.ru:3478",
    "stun.zadarma.com:3478",
];

/// What the STUN answers say about our NAT's port mapping: a diagnosis for the
/// log and the error text, never a reason to refuse a join.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mapping {
    /// No STUN server answered (STUN or UDP blocked).
    #[default]
    None,
    /// One server answered, nothing to compare with.
    Unknown,
    /// Two servers saw the same ip:port (endpoint-independent mapping): the
    /// address in our codes is the one peers can punch.
    Stable,
    /// Two servers saw different ports: the NAT maps every destination anew, so
    /// a peer punching the address STUN saw usually fails.
    Symmetric,
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mapping::Unknown => "unknown (one STUN answer)",
            Mapping::Stable => "endpoint-independent",
            Mapping::Symmetric => "symmetric",
            Mapping::None => "no STUN answer",
        })
    }
}

/// Compares the mapped addresses that different STUN servers (keyed by server
/// address) reported for one socket.
pub fn classify<S: std::hash::BuildHasher>(mapped: &HashMap<String, SocketAddr, S>) -> Mapping {
    let mut all = mapped.values();
    if let Some(first) = all.next() {
        if all.any(|a| a != first) {
            return Mapping::Symmetric;
        }
    }
    match mapped.len() {
        0 => Mapping::None,
        1 => Mapping::Unknown,
        _ => Mapping::Stable,
    }
}

#[cfg(test)]
mod tests {
    use super::super::stun::tests::{addr_attr, attr, response, TEST_TX_ID};
    use super::super::stun::*;
    use super::*;

    #[test]
    fn test_classify() {
        let m = |pairs: &[(&str, &str)]| -> HashMap<String, SocketAddr> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.parse().unwrap()))
                .collect()
        };
        let cases = [
            ("none", m(&[]), Mapping::None),
            ("one", m(&[("s1", "1.2.3.4:5000")]), Mapping::Unknown),
            (
                "same",
                m(&[("s1", "1.2.3.4:5000"), ("s2", "1.2.3.4:5000")]),
                Mapping::Stable,
            ),
            (
                "ports differ",
                m(&[("s1", "1.2.3.4:5000"), ("s2", "1.2.3.4:5001")]),
                Mapping::Symmetric,
            ),
            (
                "ips differ",
                m(&[("s1", "1.2.3.4:5000"), ("s2", "1.2.3.5:5000")]),
                Mapping::Symmetric,
            ),
        ];
        for (name, input, want) in cases {
            let got = classify(&input);
            assert_eq!(got, want, "{name}: classify = {got}, want {want}");
        }
    }

    #[test]
    fn test_parse_binding_port() {
        for xor in [true, false] {
            let typ = if xor {
                ATTR_XOR_MAPPED_ADDRESS
            } else {
                ATTR_MAPPED_ADDRESS
            };
            let resp = response(
                0x0101,
                MAGIC_COOKIE,
                &TEST_TX_ID,
                &[&attr(typ, &addr_attr("203.0.113.7", 40123, xor))],
            );
            let got = parse_binding(&resp, &TEST_TX_ID).unwrap();
            assert_eq!(got.to_string(), "203.0.113.7:40123", "xor={xor}");
        }
        assert!(
            !is_stun(b"WMPPUNCH") && is_stun(&binding_request()),
            "is_stun misclassifies a punch packet or a binding request"
        );
    }
}
