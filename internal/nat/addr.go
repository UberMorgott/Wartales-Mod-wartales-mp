package nat

import (
	"net"
	"net/netip"
)

// Blocks that can never be reached from the internet. Anything inside one of
// them is useless in a join code, however confidently a router reports it.
var nonPublicBlocks = []struct {
	prefix netip.Prefix
	reason string
}{
	{netip.MustParsePrefix("0.0.0.0/8"), "unspecified/this-network"},
	{netip.MustParsePrefix("10.0.0.0/8"), "private (RFC1918)"},
	{netip.MustParsePrefix("100.64.0.0/10"), "carrier-grade NAT (RFC6598)"},
	{netip.MustParsePrefix("127.0.0.0/8"), "loopback"},
	{netip.MustParsePrefix("169.254.0.0/16"), "link-local"},
	{netip.MustParsePrefix("172.16.0.0/12"), "private (RFC1918)"},
	{netip.MustParsePrefix("192.0.0.0/24"), "IETF protocol assignments"},
	{netip.MustParsePrefix("192.0.2.0/24"), "documentation (TEST-NET-1)"},
	{netip.MustParsePrefix("192.168.0.0/16"), "private (RFC1918)"},
	{netip.MustParsePrefix("198.18.0.0/15"), "benchmarking"},
	{netip.MustParsePrefix("198.51.100.0/24"), "documentation (TEST-NET-2)"},
	{netip.MustParsePrefix("203.0.113.0/24"), "documentation (TEST-NET-3)"},
	{netip.MustParsePrefix("224.0.0.0/4"), "multicast"},
	{netip.MustParsePrefix("240.0.0.0/4"), "reserved"},
}

// classify reports whether ip is a public IPv4 address usable in a join code,
// and, when it is not, why it is unusable.
func classify(ip netip.Addr) (bool, string) {
	if !ip.IsValid() {
		return false, "invalid address"
	}
	ip = ip.Unmap()
	if !ip.Is4() {
		return false, "not IPv4"
	}
	for _, b := range nonPublicBlocks {
		if b.prefix.Contains(ip) {
			return false, b.reason
		}
	}
	return true, ""
}

// IsPublicIPv4 reports whether ip is a globally routable IPv4 address.
func IsPublicIPv4(ip net.IP) bool {
	a, ok := netip.AddrFromSlice(ip)
	if !ok {
		return false
	}
	public, _ := classify(a)
	return public
}

// describeIP renders an address for the log, with the reason it is unusable.
func describeIP(ip net.IP) string {
	if ip == nil {
		return "none"
	}
	if IsPublicIPv4(ip) {
		return ip.String()
	}
	a, ok := netip.AddrFromSlice(ip)
	if !ok {
		return ip.String() + " (invalid address)"
	}
	_, reason := classify(a)
	return ip.String() + " (" + reason + ")"
}
