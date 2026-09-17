// Package code implements the short join code: a Crockford base32 encoding of
// [flags:1][ipv4:4][port:2] plus one check symbol.
package code

import (
	"errors"
	"fmt"
	"net"
	"strings"
)

// Alphabet is Crockford base32: digits plus letters without I, L, O and U.
const Alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

const (
	payloadLen = 7  // flags(1) + ipv4(4) + port(2)
	bodyLen    = 12 // ceil(7*8/5)
	// Length is the total number of symbols of a join code.
	Length = bodyLen + 1
)

// Endpoint is what a join code carries.
type Endpoint struct {
	Flags byte
	IP    net.IP // IPv4
	Port  uint16
}

func (e Endpoint) String() string { return fmt.Sprintf("%s:%d", e.IP, e.Port) }

// Addr returns the endpoint as "ip:port".
func (e Endpoint) Addr() string { return net.JoinHostPort(e.IP.String(), fmt.Sprint(e.Port)) }

var errBadCode = errors.New("invalid join code")

// Encode turns an endpoint into a 13 symbol join code.
func Encode(e Endpoint) (string, error) {
	ip4 := e.IP.To4()
	if ip4 == nil {
		return "", fmt.Errorf("join code needs an IPv4 address, got %v", e.IP)
	}
	payload := []byte{e.Flags, ip4[0], ip4[1], ip4[2], ip4[3], byte(e.Port >> 8 & 0xff), byte(e.Port & 0xff)}

	// 7 bytes = 56 bits; pad to 60 bits (12 symbols) with 4 low zero bits.
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
	return sb.String(), nil
}

// Decode parses a join code. Input is case insensitive and may contain dashes
// or spaces; the Crockford look-alikes I/L (-> 1) and O (-> 0) are accepted.
func Decode(s string) (Endpoint, error) {
	norm := normalize(s)
	if len(norm) != Length {
		return Endpoint{}, errBadCode
	}

	vals := make([]uint64, Length)
	for i := range Length {
		v := strings.IndexByte(Alphabet, norm[i])
		if v < 0 {
			return Endpoint{}, errBadCode
		}
		vals[i] = uint64(v & 31)
	}

	var sum uint64
	for _, v := range vals[:bodyLen] {
		sum += v
	}
	if sum%32 != vals[bodyLen] {
		return Endpoint{}, fmt.Errorf("%w: checksum mismatch", errBadCode)
	}

	var acc uint64
	for _, v := range vals[:bodyLen] {
		acc = acc<<5 | v
	}
	acc >>= 4

	payload := make([]byte, payloadLen)
	for i := payloadLen - 1; i >= 0; i-- {
		payload[i] = byte(acc)
		acc >>= 8
	}
	return Endpoint{
		Flags: payload[0],
		IP:    net.IPv4(payload[1], payload[2], payload[3], payload[4]),
		Port:  uint16(payload[5])<<8 | uint16(payload[6]),
	}, nil
}

func normalize(s string) string {
	var sb strings.Builder
	for _, r := range strings.ToUpper(s) {
		switch r {
		case '-', ' ', '\t':
			// separators are cosmetic
		case 'I', 'L':
			sb.WriteByte('1')
		case 'O':
			sb.WriteByte('0')
		default:
			sb.WriteRune(r)
		}
	}
	return sb.String()
}
