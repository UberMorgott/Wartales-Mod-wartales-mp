package nat

import (
	"encoding/binary"
	"net"
	"testing"
)

var testTxID = []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12}

// attr builds one STUN attribute, padded to a multiple of 4 bytes.
func attr(typ uint16, val []byte) []byte {
	out := make([]byte, 4)
	binary.BigEndian.PutUint16(out, typ)
	binary.BigEndian.PutUint16(out[2:], uint16(len(val)))
	out = append(out, val...)
	for len(out)%4 != 0 {
		out = append(out, 0)
	}
	return out
}

// addrAttr builds a {reserved, family, port, address} attribute body,
// XOR-ing port and address with the magic cookie when xor is set.
func addrAttr(ip string, port uint16, xor bool) []byte {
	v4 := net.ParseIP(ip).To4()
	var key [4]byte
	binary.BigEndian.PutUint32(key[:], magicCookie)
	if xor {
		port ^= uint16(magicCookie >> 16)
		for i := range v4 {
			v4[i] ^= key[i]
		}
	}
	val := []byte{0x00, familyIPv4, 0, 0}
	binary.BigEndian.PutUint16(val[2:], port)
	return append(val, v4...)
}

// response builds a canned binding success response carrying attrs.
func response(msgType uint16, cookie uint32, txID []byte, attrs ...[]byte) []byte {
	var body []byte
	for _, a := range attrs {
		body = append(body, a...)
	}
	out := make([]byte, 20)
	binary.BigEndian.PutUint16(out, msgType)
	binary.BigEndian.PutUint16(out[2:], uint16(len(body)))
	binary.BigEndian.PutUint32(out[4:], cookie)
	copy(out[8:], txID)
	return append(out, body...)
}

func TestParseSTUN(t *testing.T) {
	xorAttr := attr(attrXORMappedAddress, addrAttr("203.0.114.9", 54321, true))
	plainAttr := attr(attrMappedAddress, addrAttr("198.51.101.7", 54321, false))
	software := attr(0x8022, []byte("test server"))

	cases := []struct {
		name string
		resp []byte
		want string // "" means an error is expected
	}{
		{"xor-mapped-address", response(0x0101, magicCookie, testTxID, xorAttr), "203.0.114.9"},
		{"xor after other attributes", response(0x0101, magicCookie, testTxID, software, xorAttr), "203.0.114.9"},
		{"mapped-address fallback", response(0x0101, magicCookie, testTxID, plainAttr), "198.51.101.7"},
		{"xor preferred over mapped", response(0x0101, magicCookie, testTxID, plainAttr, xorAttr), "203.0.114.9"},
		{"legacy 0x8020 xor", response(0x0101, magicCookie, testTxID,
			attr(attrXORMappedAddressLegacy, addrAttr("203.0.114.9", 1234, true))), "203.0.114.9"},
		{"ipv6 attribute is skipped", response(0x0101, magicCookie, testTxID,
			attr(attrXORMappedAddress, append([]byte{0x00, 0x02, 0x00, 0x00}, make([]byte, 16)...)), plainAttr),
			"198.51.101.7"},
		{"short response", []byte{0x01, 0x01}, ""},
		{"wrong message type", response(0x0111, magicCookie, testTxID, xorAttr), ""},
		{"bad magic cookie", response(0x0101, 0xdeadbeef, testTxID, xorAttr), ""},
		{"transaction id mismatch", response(0x0101, magicCookie,
			[]byte{9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9}, xorAttr), ""},
		{"no address attribute", response(0x0101, magicCookie, testTxID, software), ""},
		{"truncated attribute", response(0x0101, magicCookie, testTxID, attr(attrXORMappedAddress, []byte{0x00, 0x01})), ""},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			ip, err := parseSTUN(c.resp, testTxID)
			if c.want == "" {
				if err == nil {
					t.Fatalf("parseSTUN = %v, want an error", ip)
				}
				return
			}
			if err != nil {
				t.Fatalf("parseSTUN: %v", err)
			}
			if ip.String() != c.want {
				t.Fatalf("parseSTUN = %s, want %s", ip, c.want)
			}
		})
	}
}

// The live bug: a canned "192.168.18.187" answer must not pass as public.
func TestParsedSTUNAddressIsClassified(t *testing.T) {
	resp := response(0x0101, magicCookie, testTxID,
		attr(attrXORMappedAddress, addrAttr("192.168.18.187", 14250, true)))
	ip, err := parseSTUN(resp, testTxID)
	if err != nil {
		t.Fatalf("parseSTUN: %v", err)
	}
	if ip.String() != "192.168.18.187" {
		t.Fatalf("parseSTUN = %s, want 192.168.18.187", ip)
	}
	if IsPublicIPv4(ip) {
		t.Fatalf("192.168.18.187 must not be treated as public")
	}
}
