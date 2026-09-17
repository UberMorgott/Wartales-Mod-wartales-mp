package code

import (
	"net"
	"strings"
	"testing"
)

func TestRoundTrip(t *testing.T) {
	cases := []Endpoint{
		{Flags: 0, IP: net.IPv4(127, 0, 0, 1), Port: 14250},
		{Flags: 1, IP: net.IPv4(0, 0, 0, 0), Port: 0},
		{Flags: 31, IP: net.IPv4(255, 255, 255, 255), Port: 65535},
		{Flags: 0, IP: net.IPv4(88, 12, 200, 3), Port: 1},
	}
	for _, want := range cases {
		s, err := Encode(want)
		if err != nil {
			t.Fatalf("Encode(%v): %v", want, err)
		}
		if len(s) != Length {
			t.Fatalf("Encode(%v) = %q, want %d symbols", want, s, Length)
		}
		got, err := Decode(s)
		if err != nil {
			t.Fatalf("Decode(%q): %v", s, err)
		}
		if got.Flags != want.Flags || got.Port != want.Port || !got.IP.Equal(want.IP) {
			t.Fatalf("round trip: got %+v, want %+v (code %q)", got, want, s)
		}
	}
}

func TestDecodeIsForgiving(t *testing.T) {
	s, err := Encode(Endpoint{IP: net.IPv4(10, 0, 0, 7), Port: 14250})
	if err != nil {
		t.Fatal(err)
	}
	// lower case, dashes, and the Crockford look-alikes must all work
	munged := strings.ToLower(s[:4] + "-" + s[4:8] + "-" + s[8:])
	munged = strings.NewReplacer("1", "l", "0", "o").Replace(munged)
	got, err := Decode(munged)
	if err != nil {
		t.Fatalf("Decode(%q): %v", munged, err)
	}
	if !got.IP.Equal(net.IPv4(10, 0, 0, 7)) || got.Port != 14250 {
		t.Fatalf("got %+v", got)
	}
}

func TestDecodeRejectsBadChecksum(t *testing.T) {
	s, err := Encode(Endpoint{IP: net.IPv4(192, 168, 1, 5), Port: 4242})
	if err != nil {
		t.Fatal(err)
	}
	// flip the check symbol
	idx := strings.IndexByte(Alphabet, s[Length-1])
	bad := s[:Length-1] + string(Alphabet[(idx+1)%32])
	if _, err := Decode(bad); err == nil {
		t.Fatalf("Decode(%q) accepted a wrong checksum", bad)
	}
	// flip a body symbol, which also breaks the checksum
	idx = strings.IndexByte(Alphabet, s[0])
	bad = string(Alphabet[(idx+1)%32]) + s[1:]
	if _, err := Decode(bad); err == nil {
		t.Fatalf("Decode(%q) accepted a corrupted body", bad)
	}
}

func TestDecodeRejectsBadShape(t *testing.T) {
	for _, s := range []string{"", "ABC", strings.Repeat("Z", Length+1), strings.Repeat("!", Length)} {
		if _, err := Decode(s); err == nil {
			t.Fatalf("Decode(%q) should have failed", s)
		}
	}
}
