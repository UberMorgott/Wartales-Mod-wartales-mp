package code

import (
	"net"
	"strings"
	"testing"
)

func TestDirectCodesRoundTrip(t *testing.T) {
	for _, tc := range []struct {
		ip     string
		port   uint16
		length int
	}{
		{"45.154.88.66", 14250, 8},
		{"127.0.0.1", 14250, 8},
		{"255.255.255.255", 65535, 11},
		{"0.0.0.0", 0, 11},
		{"192.168.1.7", 4242, 11},
	} {
		want := Endpoint{IP: net.ParseIP(tc.ip), Port: tc.port}
		s, err := EncodeDirect(want)
		if err != nil {
			t.Fatal(err)
		}
		if len(s) != tc.length {
			t.Fatalf("%s: got %q, want %d symbols", want, s, tc.length)
		}
		for _, input := range []string{s, strings.ToLower(s[:4] + "-" + s[4:])} {
			got, err := DecodeAny(input)
			if err != nil {
				t.Fatal(err)
			}
			if got.Steam != nil || got.Endpoint == nil || !got.Endpoint.IP.Equal(want.IP) || got.Endpoint.Port != want.Port {
				t.Fatalf("%q decoded to %+v, want direct %s", input, got, want)
			}
		}
		for i := range s {
			bad := []byte(s)
			pos := strings.IndexByte(Alphabet, bad[i])
			bad[i] = Alphabet[(pos+1)%len(Alphabet)]
			if _, err := DecodeAny(string(bad)); err == nil {
				t.Fatalf("accepted typo at %d in %q", i, s)
			}
		}
	}
}

func TestDirectCodePreservesFlags(t *testing.T) {
	want := Endpoint{IP: net.IPv4(192, 0, 2, 1), Port: 14250, Flags: 3}
	s, err := EncodeDirect(want)
	if err != nil {
		t.Fatal(err)
	}
	got, err := Decode(s)
	if err != nil || got.Flags != want.Flags || got.Port != want.Port || !got.IP.Equal(want.IP) {
		t.Fatalf("flagged endpoint round trip: %+v, %v", got, err)
	}
}

func TestDirectCodeRejectsIPv6(t *testing.T) {
	if _, err := EncodeDirect(Endpoint{IP: net.ParseIP("::1"), Port: 14250}); err == nil {
		t.Fatal("accepted IPv6")
	}
}
