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
		{Flags: 0x3f, IP: net.IPv4(255, 255, 255, 255), Port: 65535},
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

// TestEndpointCodesAreUnchanged pins the endpoint layout to a known code, so
// codes issued by earlier builds keep decoding.
func TestEndpointCodesAreUnchanged(t *testing.T) {
	// legacy is the encoder of the first release, verbatim: 7 bytes into one
	// word, shifted left by 4, 12 symbols MSB first, check = sum mod 32.
	legacy := func(e Endpoint) string {
		ip4 := e.IP.To4()
		payload := []byte{e.Flags, ip4[0], ip4[1], ip4[2], ip4[3], byte(e.Port >> 8 & 0xff), byte(e.Port & 0xff)}
		var acc uint64
		for _, b := range payload {
			acc = acc<<8 | uint64(b)
		}
		acc <<= 4
		var sb strings.Builder
		sum := 0
		for i := bodyLen - 1; i >= 0; i-- {
			v := int(acc >> (uint(i) * 5) & 31)
			sum += v
			sb.WriteByte(Alphabet[v])
		}
		sb.WriteByte(Alphabet[sum%32])
		return sb.String()
	}
	for _, e := range []Endpoint{
		{IP: net.IPv4(88, 12, 200, 3), Port: 14250},
		{Flags: 0x3f, IP: net.IPv4(255, 255, 255, 255), Port: 65535},
		{IP: net.IPv4(0, 0, 0, 0), Port: 0},
	} {
		s, err := Encode(e)
		if err != nil {
			t.Fatal(err)
		}
		if want := legacy(e); s != want {
			t.Fatalf("Encode(%v) = %q, the first release produced %q", e, s, want)
		}
		c, err := DecodeAny(s)
		if err != nil || c.Endpoint == nil || c.Steam != nil || c.Endpoint.Port != e.Port || !c.Endpoint.IP.Equal(e.IP) {
			t.Fatalf("DecodeAny(%q) = %+v, %v", s, c, err)
		}
	}
	if _, err := Encode(Endpoint{Flags: FlagSteam, IP: net.IPv4(1, 2, 3, 4), Port: 1}); err == nil {
		t.Fatal("bit 7 of the endpoint flags is reserved")
	}
}

func TestSteamRoundTrip(t *testing.T) {
	cases := []Steam{
		{AccountID: 0, Key: 0},
		{AccountID: 0xffffffff, Key: 0xffffffff},
		{AccountID: 12345678, Key: 0xdeadbeef},
		{Flags: 0x3f, AccountID: 1, Key: 2}, // bits 7 and 6 belong to the layout
	}
	for _, want := range cases {
		s := EncodeSteam(want)
		if len(s) != SteamLength {
			t.Fatalf("EncodeSteam(%+v) = %q, want %d symbols", want, s, SteamLength)
		}
		got, err := DecodeSteam(s)
		if err != nil {
			t.Fatalf("DecodeSteam(%q): %v", s, err)
		}
		if got != want {
			t.Fatalf("round trip: got %+v, want %+v (code %q)", got, want, s)
		}
		if got.SteamID64() != 0x0110000100000000|uint64(want.AccountID) {
			t.Fatalf("SteamID64 = %x", got.SteamID64())
		}
		c, err := DecodeAny(s)
		if err != nil || c.Steam == nil || c.Endpoint != nil {
			t.Fatalf("DecodeAny(%q) = %+v, %v", s, c, err)
		}
		if _, err := Decode(s); err == nil {
			t.Fatalf("Decode must refuse the steam code %q", s)
		}
	}
	// A known code, so the layout is pinned.
	s := EncodeSteam(Steam{AccountID: 12345678, Key: 0xdeadbeef})
	// [80][00 bc 61 4e][de ad be ef] as a 75-bit stream, computed independently.
	const want = "G00BRRAEVTPVXVRS"
	if s != want {
		t.Fatalf("EncodeSteam = %q, want %q", s, want)
	}
	// Corrupting any symbol breaks the checksum or the padding.
	for i := range SteamLength {
		idx := strings.IndexByte(Alphabet, s[i])
		bad := s[:i] + string(Alphabet[(idx+1)%32]) + s[i+1:]
		if _, err := DecodeSteam(bad); err == nil {
			t.Fatalf("DecodeSteam(%q) accepted a corrupted symbol %d", bad, i)
		}
	}
}

func TestCombinedRoundTrip(t *testing.T) {
	ep := Endpoint{IP: net.IPv4(45, 154, 88, 66), Port: 14250}
	st := Steam{AccountID: 12345678, Key: 0xdeadbeef}
	s, err := EncodeCombined(ep, st)
	if err != nil {
		t.Fatal(err)
	}
	if len(s) != CombinedLength {
		t.Fatalf("EncodeCombined = %q, want %d symbols", s, CombinedLength)
	}
	// [c0][2d 9a 58 42][37 aa][00 bc 61 4e][de ad be ef] as a 120-bit stream,
	// computed independently.
	const want = "R0PSMP226YN01F319VFAVFQFF"
	if s != want {
		t.Fatalf("EncodeCombined = %q, want %q", s, want)
	}
	c, err := DecodeAny(s)
	if err != nil || c.Endpoint == nil || c.Steam == nil {
		t.Fatalf("DecodeAny(%q) = %+v, %v", s, c, err)
	}
	if !c.Endpoint.IP.Equal(ep.IP) || c.Endpoint.Port != ep.Port || *c.Steam != st {
		t.Fatalf("round trip: %+v / %+v", c.Endpoint, c.Steam)
	}
	if c.Steam.SteamID64() != 0x0110000100BC614E {
		t.Fatalf("SteamID64 = %x", c.Steam.SteamID64())
	}
	// The single-route decoders refuse it: a caller that ignores one route
	// would silently lose the cascade.
	if _, err := Decode(s); err == nil {
		t.Fatal("Decode accepted a combined code")
	}
	if _, err := DecodeSteam(s); err == nil {
		t.Fatal("DecodeSteam accepted a combined code")
	}
	// Corrupting any symbol is caught.
	for i := range CombinedLength {
		idx := strings.IndexByte(Alphabet, s[i])
		bad := s[:i] + string(Alphabet[(idx+1)%32]) + s[i+1:]
		if _, err := DecodeAny(bad); err == nil {
			t.Fatalf("DecodeAny(%q) accepted a corrupted symbol %d", bad, i)
		}
	}
	// Reserved bits in the callers' flags are refused.
	if _, err := EncodeCombined(Endpoint{Flags: FlagBoth, IP: ep.IP, Port: 1}, st); err == nil {
		t.Fatal("bit 6 of the flags is reserved")
	}
	// The three layouts never collide: distinct lengths.
	if Length == SteamLength || SteamLength == CombinedLength || Length == CombinedLength {
		t.Fatal("layouts must differ in length")
	}
}

func TestAccountID(t *testing.T) {
	if id, ok := AccountID(0x0110000100BC614E); !ok || id != 12345678 {
		t.Fatalf("AccountID = %d, %v", id, ok)
	}
	if _, ok := AccountID(0x0170000000000001); ok {
		t.Fatal("a clan id is not a player account")
	}
}

func TestDecodeRejectsBadShape(t *testing.T) {
	for _, s := range []string{"", "ABC", strings.Repeat("Z", Length+1), strings.Repeat("!", Length), strings.Repeat("!", SteamLength)} {
		if _, err := Decode(s); err == nil {
			t.Fatalf("Decode(%q) should have failed", s)
		}
	}
}
