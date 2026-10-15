package code

import (
	"net"
	"testing"
)

func TestPunchRoundTrip(t *testing.T) {
	for _, c := range []struct {
		tcp Endpoint
		udp uint16
	}{
		{Endpoint{IP: net.IPv4(45, 154, 88, 66), Port: DefaultPort}, 8104},
		{Endpoint{IP: net.IPv4(255, 255, 255, 255), Port: 65535}, 65535},
		{Endpoint{IP: net.IPv4(1, 2, 3, 4), Port: 1}, 0},
	} {
		s, err := EncodePunch(c.tcp, c.udp)
		if err != nil {
			t.Fatal(err)
		}
		if len(s) != SteamLength {
			t.Fatalf("EncodePunch = %q, want %d symbols", s, SteamLength)
		}
		got, err := DecodeAny(s)
		if err != nil {
			t.Fatalf("DecodeAny(%q): %v", s, err)
		}
		if got.Steam != nil || got.Reply != nil || got.Endpoint == nil || got.Punch == nil {
			t.Fatalf("DecodeAny(%q) = %+v, want endpoint + punch", s, got)
		}
		if got.Endpoint.Addr() != c.tcp.Addr() || !got.Punch.IP.Equal(c.tcp.IP) || got.Punch.Port != c.udp {
			t.Fatalf("DecodeAny(%q) = %s / %s, want %s / udp %d", s, got.Endpoint, got.Punch, c.tcp, c.udp)
		}
		// Decode (endpoint only) still gives the TCP route.
		if e, err := Decode(s); err != nil || e.Addr() != c.tcp.Addr() {
			t.Fatalf("Decode(%q) = %s, %v", s, e, err)
		}
	}
	// A steam code of the same length is still a steam code.
	st := Steam{AccountID: 0x12345678, Key: 0x9abcdef0}
	got, err := DecodeAny(EncodeSteam(st))
	if err != nil || got.Steam == nil || got.Punch != nil || got.Steam.AccountID != st.AccountID {
		t.Fatalf("steam code decodes as %+v, %v", got, err)
	}
}

func TestReplyRoundTrip(t *testing.T) {
	want := Endpoint{IP: net.IPv4(100, 64, 7, 9), Port: 50123}
	s, err := EncodeReply(want)
	if err != nil {
		t.Fatal(err)
	}
	if len(s) != Length {
		t.Fatalf("EncodeReply = %q, want %d symbols", s, Length)
	}
	got, err := DecodeAny(s)
	if err != nil || got.Reply == nil || got.Endpoint != nil || got.Punch != nil || got.Steam != nil {
		t.Fatalf("DecodeAny(%q) = %+v, %v; want a reply only", s, got, err)
	}
	if got.Reply.Addr() != want.Addr() {
		t.Fatalf("reply %s, want %s", got.Reply, want)
	}
	if _, err := Decode(s); err == nil {
		t.Fatal("a reply code must not decode as an endpoint")
	}
	// Old 13-symbol endpoint codes, every legacy flag bit included, still decode.
	for _, flags := range []byte{0, 1, 0x20, 0x3f} {
		old := Endpoint{Flags: flags, IP: net.IPv4(88, 12, 200, 3), Port: 14250}
		s, err := Encode(old)
		if err != nil {
			t.Fatal(err)
		}
		got, err := DecodeAny(s)
		if err != nil || got.Reply != nil || got.Endpoint == nil || got.Endpoint.Flags != flags {
			t.Fatalf("legacy endpoint flags %#x: %+v, %v", flags, got, err)
		}
	}
	// The 8-symbol direct layout is untouched.
	d, _ := EncodeDirect(Endpoint{IP: net.IPv4(127, 0, 0, 1), Port: DefaultPort})
	if got, err := DecodeAny(d); err != nil || got.Endpoint == nil || got.Punch != nil {
		t.Fatalf("direct code %q: %+v, %v", d, got, err)
	}
}
