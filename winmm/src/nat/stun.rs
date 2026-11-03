//! RFC 5389 binding requests: our public address as a STUN server sees it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use anyhow::{anyhow, bail};

/// The server used for the Mapper's external address check.
pub(super) const DEFAULT_STUN: &str = "stun.l.google.com:19302";

pub(super) const MAGIC_COOKIE: u32 = 0x2112_A442;

// RFC 5389 attribute types and address family (RFC 5780 §7.2 keeps 0x8020 for
// the pre-standard XOR-MAPPED-ADDRESS some servers still send).
pub(super) const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
pub(super) const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
pub(super) const ATTR_XOR_MAPPED_ADDRESS_LEGACY: u16 = 0x8020;
pub(super) const FAMILY_IPV4: u8 = 0x01;

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Asks a STUN server for our public address. It only tells us the IP; port
/// mapping still needs UPnP or a manual forward, since the game speaks TCP.
pub(super) fn stun_external_ip(server: &str) -> anyhow::Result<IpAddr> {
    let server = if server.is_empty() {
        DEFAULT_STUN
    } else {
        server
    };
    // IPv4 socket: connect skips the server's IPv6 addresses, we only want a v4 answer.
    let c = UdpSocket::bind("0.0.0.0:0")?;
    c.connect(server)?;
    c.set_read_timeout(Some(Duration::from_secs(5)))?;
    c.set_write_timeout(Some(Duration::from_secs(5)))?;
    let req = binding_request();
    c.send(&req)?;
    let mut buf = [0u8; 1500];
    let n = c.recv(&mut buf)?;
    parse_stun(&buf[..n], &req[8..20])
}

/// Builds an RFC 5389 binding request with a random transaction id (bytes 8..20).
pub fn binding_request() -> [u8; 20] {
    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&0x0001u16.to_be_bytes()); // binding request, no attributes
    req[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    crate::rand_bytes(&mut req[8..20]);
    req
}

/// Reports whether a datagram looks like a STUN message (RFC 5389 §6: top two
/// bits zero, magic cookie at offset 4). Punch and KCP packets never match: KCP
/// has its command byte (81..84) at offset 4.
pub fn is_stun(b: &[u8]) -> bool {
    b.len() >= 20 && b[0] & 0xc0 == 0 && be32(&b[4..]) == MAGIC_COOKIE
}

pub(super) fn parse_stun(resp: &[u8], tx_id: &[u8]) -> anyhow::Result<IpAddr> {
    parse_binding(resp, tx_id).map(|a| a.ip())
}

/// Returns the mapped address (ip and port) of a binding success response to
/// the request with transaction id tx_id.
pub fn parse_binding(resp: &[u8], tx_id: &[u8]) -> anyhow::Result<SocketAddr> {
    if resp.len() < 20 {
        bail!("stun: short response");
    }
    if be16(resp) != 0x0101 {
        bail!("stun: unexpected message type {:#06x}", be16(resp));
    }
    if be32(&resp[4..]) != MAGIC_COOKIE {
        bail!("stun: bad magic cookie");
    }
    if &resp[8..20] != tx_id {
        bail!("stun: transaction id mismatch");
    }
    let n = (be16(&resp[2..]) as usize).min(resp.len() - 20);
    let mut body = &resp[20..20 + n];

    // XOR-MAPPED-ADDRESS wins over MAPPED-ADDRESS whatever order they arrive
    // in: some NATs rewrite the plain one on the way back.
    let mut mapped = None;
    while body.len() >= 4 {
        let typ = be16(body);
        let length = be16(&body[2..]) as usize;
        if body.len() < 4 + length {
            break;
        }
        let val = &body[4..4 + length];
        match typ {
            ATTR_XOR_MAPPED_ADDRESS | ATTR_XOR_MAPPED_ADDRESS_LEGACY => {
                if let Some(a) = ipv4_attr(val) {
                    // Address and port are XOR-ed with the magic cookie.
                    let ip = Ipv4Addr::from(u32::from(*a.ip()) ^ MAGIC_COOKIE);
                    return Ok(
                        SocketAddrV4::new(ip, a.port() ^ (MAGIC_COOKIE >> 16) as u16).into(),
                    );
                }
            }
            ATTR_MAPPED_ADDRESS if mapped.is_none() => mapped = ipv4_attr(val),
            _ => {}
        }
        // attributes are padded to a multiple of 4 bytes (the last one may not be)
        body = &body[(4 + ((length + 3) & !3)).min(body.len())..];
    }
    mapped
        .map(SocketAddr::V4)
        .ok_or_else(|| anyhow!("stun: no mapped address"))
}

/// Decodes the {reserved, family, port, address} body shared by MAPPED-ADDRESS
/// and XOR-MAPPED-ADDRESS, returning None unless it is IPv4.
fn ipv4_attr(val: &[u8]) -> Option<SocketAddrV4> {
    if val.len() < 8 || val[1] != FAMILY_IPV4 {
        return None;
    }
    Some(SocketAddrV4::new(
        Ipv4Addr::new(val[4], val[5], val[6], val[7]),
        be16(&val[2..4]),
    ))
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::addr::is_public_ipv4;
    use super::*;

    pub const TEST_TX_ID: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    /// Builds one STUN attribute, padded to a multiple of 4 bytes.
    pub fn attr(typ: u16, val: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; (4 + val.len() + 3) & !3];
        out[0..2].copy_from_slice(&typ.to_be_bytes());
        out[2..4].copy_from_slice(&u16::try_from(val.len()).unwrap().to_be_bytes());
        out[4..4 + val.len()].copy_from_slice(val);
        out
    }

    /// Builds a {reserved, family, port, address} attribute body, XOR-ing port
    /// and address with the magic cookie when xor is set.
    pub fn addr_attr(ip: &str, mut port: u16, xor: bool) -> Vec<u8> {
        let mut v4 = ip.parse::<Ipv4Addr>().unwrap().octets();
        if xor {
            port ^= (MAGIC_COOKIE >> 16) as u16;
            for (b, k) in v4.iter_mut().zip(MAGIC_COOKIE.to_be_bytes()) {
                *b ^= k;
            }
        }
        let mut val = vec![0x00, FAMILY_IPV4];
        val.extend_from_slice(&port.to_be_bytes());
        val.extend_from_slice(&v4);
        val
    }

    /// Builds a canned binding response carrying attrs.
    pub fn response(msg_type: u16, cookie: u32, tx_id: &[u8], attrs: &[&[u8]]) -> Vec<u8> {
        let body = attrs.concat();
        let mut out = Vec::with_capacity(20 + body.len());
        out.extend_from_slice(&msg_type.to_be_bytes());
        out.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&cookie.to_be_bytes());
        out.extend_from_slice(tx_id);
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn test_parse_stun() {
        let xor_attr = attr(
            ATTR_XOR_MAPPED_ADDRESS,
            &addr_attr("203.0.114.9", 54321, true),
        );
        let plain_attr = attr(
            ATTR_MAPPED_ADDRESS,
            &addr_attr("198.51.101.7", 54321, false),
        );
        let software = attr(0x8022, b"test server");
        let mut v6 = vec![0x00, 0x02, 0x00, 0x00];
        v6.extend_from_slice(&[0u8; 16]);
        let ok = |attrs: &[&[u8]]| response(0x0101, MAGIC_COOKIE, &TEST_TX_ID, attrs);

        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("xor-mapped-address", ok(&[&xor_attr]), "203.0.114.9"),
            (
                "xor after other attributes",
                ok(&[&software, &xor_attr]),
                "203.0.114.9",
            ),
            (
                "mapped-address fallback",
                ok(&[&plain_attr]),
                "198.51.101.7",
            ),
            (
                "xor preferred over mapped",
                ok(&[&plain_attr, &xor_attr]),
                "203.0.114.9",
            ),
            (
                "legacy 0x8020 xor",
                ok(&[&attr(
                    ATTR_XOR_MAPPED_ADDRESS_LEGACY,
                    &addr_attr("203.0.114.9", 1234, true),
                )]),
                "203.0.114.9",
            ),
            (
                "ipv6 attribute is skipped",
                ok(&[&attr(ATTR_XOR_MAPPED_ADDRESS, &v6), &plain_attr]),
                "198.51.101.7",
            ),
            ("short response", vec![0x01, 0x01], ""),
            (
                "wrong message type",
                response(0x0111, MAGIC_COOKIE, &TEST_TX_ID, &[&xor_attr]),
                "",
            ),
            (
                "bad magic cookie",
                response(0x0101, 0xdead_beef, &TEST_TX_ID, &[&xor_attr]),
                "",
            ),
            (
                "transaction id mismatch",
                response(0x0101, MAGIC_COOKIE, &[9; 12], &[&xor_attr]),
                "",
            ),
            ("no address attribute", ok(&[&software]), ""),
            (
                "truncated attribute",
                ok(&[&attr(ATTR_XOR_MAPPED_ADDRESS, &[0x00, 0x01])]),
                "",
            ),
            // Unpadded last attribute: Go sliced past the end and panicked.
            ("unpadded last attribute", ok(&[&software[..15]]), ""),
        ];
        for (name, resp, want) in cases {
            let got = parse_stun(&resp, &TEST_TX_ID);
            if want.is_empty() {
                assert!(got.is_err(), "{name}: parse_stun = {got:?}, want an error");
            } else {
                assert_eq!(got.unwrap().to_string(), want, "{name}");
            }
        }
        let e = parse_stun(
            &response(0x0111, MAGIC_COOKIE, &TEST_TX_ID, &[]),
            &TEST_TX_ID,
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "stun: unexpected message type 0x0111");
    }

    /// The live bug: a canned "192.168.18.187" answer must not pass as public.
    #[test]
    fn test_parsed_stun_address_is_classified() {
        let resp = response(
            0x0101,
            MAGIC_COOKIE,
            &TEST_TX_ID,
            &[&attr(
                ATTR_XOR_MAPPED_ADDRESS,
                &addr_attr("192.168.18.187", 14250, true),
            )],
        );
        let ip = parse_stun(&resp, &TEST_TX_ID).unwrap();
        assert_eq!(ip.to_string(), "192.168.18.187");
        assert!(
            !is_public_ipv4(ip),
            "192.168.18.187 must not be treated as public"
        );
    }
}
