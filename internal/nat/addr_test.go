package nat

import (
	"net"
	"testing"
)

func TestIsPublicIPv4(t *testing.T) {
	cases := []struct {
		name   string
		ip     string
		public bool
	}{
		{"routable", "203.0.114.9", true},
		{"routable google dns", "8.8.8.8", true},
		{"rfc1918 10", "10.0.0.1", false},
		{"rfc1918 172.16", "172.16.5.4", false},
		{"rfc1918 172.31 top", "172.31.255.255", false},
		{"just above rfc1918 172", "172.32.0.1", true},
		{"just below rfc1918 172", "172.15.255.255", true},
		{"rfc1918 192.168 (this user's router)", "192.168.18.187", false},
		{"cgnat low", "100.64.0.1", false},
		{"cgnat high", "100.127.255.255", false},
		{"just below cgnat", "100.63.255.255", true},
		{"just above cgnat", "100.128.0.1", true},
		{"loopback", "127.0.0.1", false},
		{"link-local", "169.254.13.7", false},
		{"unspecified", "0.0.0.0", false},
		{"multicast", "239.1.2.3", false},
		{"reserved", "240.0.0.1", false},
		{"broadcast", "255.255.255.255", false},
		{"test-net-1", "192.0.2.5", false},
		{"benchmark", "198.18.0.1", false},
		{"ipv6", "2001:db8::1", false},
		{"ipv4-mapped private", "::ffff:192.168.1.1", false},
		{"ipv4-mapped public", "::ffff:8.8.4.4", true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			ip := net.ParseIP(c.ip)
			if ip == nil {
				t.Fatalf("bad test address %q", c.ip)
			}
			if got := IsPublicIPv4(ip); got != c.public {
				t.Fatalf("IsPublicIPv4(%s) = %v, want %v", c.ip, got, c.public)
			}
		})
	}
	if got := IsPublicIPv4(nil); got {
		t.Fatalf("IsPublicIPv4(nil) = true, want false")
	}
}

func TestDescribeIP(t *testing.T) {
	cases := []struct{ ip, want string }{
		{"", "none"},
		{"8.8.8.8", "8.8.8.8"},
		{"192.168.18.187", "192.168.18.187 (private (RFC1918))"},
		{"100.70.0.1", "100.70.0.1 (carrier-grade NAT (RFC6598))"},
		{"2001:db8::1", "2001:db8::1 (not IPv4)"},
	}
	for _, c := range cases {
		var ip net.IP
		if c.ip != "" {
			ip = net.ParseIP(c.ip)
		}
		if got := describeIP(ip); got != c.want {
			t.Fatalf("describeIP(%q) = %q, want %q", c.ip, got, c.want)
		}
	}
}
