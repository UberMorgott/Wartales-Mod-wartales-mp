package nat

import (
	"net"
	"testing"
)

func TestClassify(t *testing.T) {
	a := func(s string) *net.UDPAddr {
		u, err := net.ResolveUDPAddr("udp4", s)
		if err != nil {
			t.Fatal(err)
		}
		return u
	}
	cases := []struct {
		name string
		in   map[string]*net.UDPAddr
		want Mapping
	}{
		{"none", map[string]*net.UDPAddr{}, MappingNone},
		{"one", map[string]*net.UDPAddr{"s1": a("1.2.3.4:5000")}, MappingUnknown},
		{"same", map[string]*net.UDPAddr{"s1": a("1.2.3.4:5000"), "s2": a("1.2.3.4:5000")}, MappingStable},
		{"ports differ", map[string]*net.UDPAddr{"s1": a("1.2.3.4:5000"), "s2": a("1.2.3.4:5001")}, MappingSymmetric},
		{"ips differ", map[string]*net.UDPAddr{"s1": a("1.2.3.4:5000"), "s2": a("1.2.3.5:5000")}, MappingSymmetric},
	}
	for _, c := range cases {
		if got := Classify(c.in); got != c.want {
			t.Errorf("%s: Classify = %s, want %s", c.name, got, c.want)
		}
	}
}

func TestParseBindingPort(t *testing.T) {
	for _, xor := range []bool{true, false} {
		typ := uint16(attrMappedAddress)
		if xor {
			typ = attrXORMappedAddress
		}
		resp := response(0x0101, magicCookie, testTxID, attr(typ, addrAttr("203.0.113.7", 40123, xor)))
		got, err := ParseBinding(resp, testTxID)
		if err != nil {
			t.Fatal(err)
		}
		if got.String() != "203.0.113.7:40123" {
			t.Errorf("xor=%v: ParseBinding = %s, want 203.0.113.7:40123", xor, got)
		}
	}
	if IsSTUN([]byte("WMPPUNCH")) || !IsSTUN(BindingRequest()) {
		t.Error("IsSTUN misclassifies a punch packet or a binding request")
	}
}
