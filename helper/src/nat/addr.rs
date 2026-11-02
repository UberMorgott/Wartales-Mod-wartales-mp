//! Which addresses can be handed out in a join code.

use std::net::IpAddr;

/// Blocks that can never be reached from the internet. Anything inside one of
/// them is useless in a join code, however confidently a router reports it.
const NON_PUBLIC_BLOCKS: [([u8; 4], u32, &str); 14] = [
    ([0, 0, 0, 0], 8, "unspecified/this-network"),
    ([10, 0, 0, 0], 8, "private (RFC1918)"),
    ([100, 64, 0, 0], 10, "carrier-grade NAT (RFC6598)"),
    ([127, 0, 0, 0], 8, "loopback"),
    ([169, 254, 0, 0], 16, "link-local"),
    ([172, 16, 0, 0], 12, "private (RFC1918)"),
    ([192, 0, 0, 0], 24, "IETF protocol assignments"),
    ([192, 0, 2, 0], 24, "documentation (TEST-NET-1)"),
    ([192, 168, 0, 0], 16, "private (RFC1918)"),
    ([198, 18, 0, 0], 15, "benchmarking"),
    ([198, 51, 100, 0], 24, "documentation (TEST-NET-2)"),
    ([203, 0, 113, 0], 24, "documentation (TEST-NET-3)"),
    ([224, 0, 0, 0], 4, "multicast"),
    ([240, 0, 0, 0], 4, "reserved"),
];

/// Why ip is not a public IPv4 address usable in a join code; None when it is.
fn unusable_reason(ip: IpAddr) -> Option<&'static str> {
    let IpAddr::V4(v4) = ip.to_canonical() else {
        return Some("not IPv4");
    };
    let a = u32::from(v4);
    NON_PUBLIC_BLOCKS
        .iter()
        .find(|(net, bits, _)| a >> (32 - bits) == u32::from_be_bytes(*net) >> (32 - bits))
        .map(|b| b.2)
}

/// Reports whether ip is a globally routable IPv4 address.
pub(super) fn is_public_ipv4(ip: IpAddr) -> bool {
    unusable_reason(ip).is_none()
}

/// Renders an address for the log, with the reason it is unusable.
pub(super) fn describe_ip(ip: Option<IpAddr>) -> String {
    let Some(ip) = ip.map(|ip| ip.to_canonical()) else {
        return "none".into();
    };
    match unusable_reason(ip) {
        None => ip.to_string(),
        Some(reason) => format!("{ip} ({reason})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_public_ipv4() {
        let cases = [
            ("routable", "203.0.114.9", true),
            ("routable google dns", "8.8.8.8", true),
            ("rfc1918 10", "10.0.0.1", false),
            ("rfc1918 172.16", "172.16.5.4", false),
            ("rfc1918 172.31 top", "172.31.255.255", false),
            ("just above rfc1918 172", "172.32.0.1", true),
            ("just below rfc1918 172", "172.15.255.255", true),
            ("rfc1918 192.168 (this user's router)", "192.168.18.187", false),
            ("cgnat low", "100.64.0.1", false),
            ("cgnat high", "100.127.255.255", false),
            ("just below cgnat", "100.63.255.255", true),
            ("just above cgnat", "100.128.0.1", true),
            ("loopback", "127.0.0.1", false),
            ("link-local", "169.254.13.7", false),
            ("unspecified", "0.0.0.0", false),
            ("multicast", "239.1.2.3", false),
            ("reserved", "240.0.0.1", false),
            ("broadcast", "255.255.255.255", false),
            ("test-net-1", "192.0.2.5", false),
            ("benchmark", "198.18.0.1", false),
            ("ipv6", "2001:db8::1", false),
            ("ipv4-mapped private", "::ffff:192.168.1.1", false),
            ("ipv4-mapped public", "::ffff:8.8.4.4", true),
        ];
        for (name, ip, public) in cases {
            let parsed: IpAddr = ip.parse().unwrap_or_else(|_| panic!("bad test address {ip:?}"));
            assert_eq!(is_public_ipv4(parsed), public, "{name}: is_public_ipv4({ip})");
        }
        // Go's IsPublicIPv4(nil) case: a missing address is not an IpAddr here.
    }

    #[test]
    fn test_describe_ip() {
        let cases = [
            ("", "none"),
            ("8.8.8.8", "8.8.8.8"),
            ("192.168.18.187", "192.168.18.187 (private (RFC1918))"),
            ("100.70.0.1", "100.70.0.1 (carrier-grade NAT (RFC6598))"),
            ("2001:db8::1", "2001:db8::1 (not IPv4)"),
            ("::ffff:192.168.1.1", "192.168.1.1 (private (RFC1918))"),
        ];
        for (ip, want) in cases {
            let ip = (!ip.is_empty()).then(|| ip.parse().unwrap());
            assert_eq!(describe_ip(ip), want);
        }
    }
}
