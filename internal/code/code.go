// Package code implements the short join code: Crockford base32 over a small
// payload whose first byte says what it carries, plus one check symbol.
//
// Two layouts exist, told apart by length (and, as a belt, by bit 7 of the
// flags byte):
//
//	endpoint  [flags:1 bit7=0][ipv4:4][port:2]              7 bytes -> 12 + 1 = 13 symbols
//	steam     [flags:1 bit7=1][account:4 BE][key:4 BE]       9 bytes -> 15 + 1 = 16 symbols
//
// An endpoint code is the direct transport: the guest connects to ip:port.
// A steam code is the SDR transport: the guest reaches the host through
// Valve's relay by SteamID64 (rebuilt from the 32-bit account id: every
// player account is universe 1, type Individual, instance 1), and must
// present the 32-bit key to the host's master. Old endpoint codes decode as
// they always did.
package code

import (
	"encoding/binary"
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
	// Length is the total number of symbols of an endpoint join code.
	Length = bodyLen + 1

	steamPayloadLen = 9  // flags(1) + account(4) + key(4)
	steamBodyLen    = 15 // ceil(9*8/5)
	// SteamLength is the total number of symbols of a steam join code.
	SteamLength = steamBodyLen + 1

	// FlagSteam in the flags byte marks the steam layout.
	FlagSteam = 0x80

	// steamIDBase is SteamID64 of account 0 in universe Public, type
	// Individual, instance Desktop: what every player account is.
	steamIDBase = 0x0110000100000000
)

// Endpoint is what an endpoint join code carries.
type Endpoint struct {
	Flags byte
	IP    net.IP // IPv4
	Port  uint16
}

func (e Endpoint) String() string { return fmt.Sprintf("%s:%d", e.IP, e.Port) }

// Addr returns the endpoint as "ip:port".
func (e Endpoint) Addr() string { return net.JoinHostPort(e.IP.String(), fmt.Sprint(e.Port)) }

// Steam is what a steam join code carries.
type Steam struct {
	Flags     byte   // low 7 bits are free; bit 7 is always set on the wire
	AccountID uint32 // the low 32 bits of the host's SteamID64
	Key       uint32 // the proxy-link key the host's master expects
}

// SteamID64 rebuilds the host's SteamID64.
func (s Steam) SteamID64() uint64 { return steamIDBase | uint64(s.AccountID) }

// AccountID extracts the account id from a SteamID64, and reports whether the
// id is an ordinary player account (the only kind a steam code can carry).
func AccountID(steamID64 uint64) (uint32, bool) {
	return uint32(steamID64 & 0xffffffff), steamID64&^0xffffffff == steamIDBase
}

// Code is a decoded join code of either layout: exactly one field is set.
type Code struct {
	Endpoint *Endpoint
	Steam    *Steam
}

var errBadCode = errors.New("invalid join code")

// Encode turns an endpoint into a 13 symbol join code.
func Encode(e Endpoint) (string, error) {
	ip4 := e.IP.To4()
	if ip4 == nil {
		return "", fmt.Errorf("join code needs an IPv4 address, got %v", e.IP)
	}
	if e.Flags&FlagSteam != 0 {
		return "", fmt.Errorf("endpoint flags 0x%02x: bit 7 is reserved for steam codes", e.Flags)
	}
	payload := []byte{e.Flags, ip4[0], ip4[1], ip4[2], ip4[3], byte(e.Port >> 8 & 0xff), byte(e.Port & 0xff)}
	return encode(payload, bodyLen), nil
}

// EncodeSteam turns a host SteamID + key into a 16 symbol join code.
func EncodeSteam(s Steam) string {
	payload := make([]byte, steamPayloadLen)
	payload[0] = s.Flags | FlagSteam
	binary.BigEndian.PutUint32(payload[1:5], s.AccountID)
	binary.BigEndian.PutUint32(payload[5:9], s.Key)
	return encode(payload, steamBodyLen)
}

// encode packs payload big-endian into body symbols (zero padded at the low
// end) and appends the check symbol, the sum of the body symbols mod 32.
func encode(payload []byte, body int) string {
	// A bit stream rather than one machine word: the steam payload is 72 bits.
	bits := len(payload) * 8
	var sb strings.Builder
	sum := 0
	for i := range body {
		v := 0
		for b := range 5 {
			bit := i*5 + b // position in the padded stream, MSB first
			if bit < bits {
				v = v<<1 | int(payload[bit/8]>>(7-uint(bit%8))&1)
			} else {
				v <<= 1 // padding
			}
		}
		sum += v
		sb.WriteByte(Alphabet[v])
	}
	sb.WriteByte(Alphabet[sum%32])
	return sb.String()
}

// decode reverses encode for a code of body symbols carrying n payload bytes.
func decode(norm string, body, n int) ([]byte, error) {
	vals := make([]int, body+1)
	for i := range body + 1 {
		v := strings.IndexByte(Alphabet, norm[i])
		if v < 0 {
			return nil, errBadCode
		}
		vals[i] = v & 31
	}
	sum := 0
	for _, v := range vals[:body] {
		sum += v
	}
	if sum%32 != vals[body] {
		return nil, fmt.Errorf("%w: checksum mismatch", errBadCode)
	}
	payload := make([]byte, n)
	for bit := range n * 8 {
		v := vals[bit/5] >> (4 - uint(bit%5)) & 1
		payload[bit/8] |= byte(v) << (7 - uint(bit%8))
	}
	// Padding bits must be zero: a non-zero tail is not one of our codes.
	for bit := n * 8; bit < body*5; bit++ {
		if vals[bit/5]>>(4-uint(bit%5))&1 != 0 {
			return nil, fmt.Errorf("%w: non-zero padding", errBadCode)
		}
	}
	return payload, nil
}

// Decode parses an endpoint join code. Input is case insensitive and may
// contain dashes or spaces; the Crockford look-alikes I/L (-> 1) and O (-> 0)
// are accepted.
func Decode(s string) (Endpoint, error) {
	c, err := DecodeAny(s)
	if err != nil {
		return Endpoint{}, err
	}
	if c.Endpoint == nil {
		return Endpoint{}, fmt.Errorf("%w: a steam code, not an endpoint code", errBadCode)
	}
	return *c.Endpoint, nil
}

// DecodeSteam parses a steam join code.
func DecodeSteam(s string) (Steam, error) {
	c, err := DecodeAny(s)
	if err != nil {
		return Steam{}, err
	}
	if c.Steam == nil {
		return Steam{}, fmt.Errorf("%w: an endpoint code, not a steam code", errBadCode)
	}
	return *c.Steam, nil
}

// DecodeAny parses a join code of either layout.
func DecodeAny(s string) (Code, error) {
	norm := normalize(s)
	switch len(norm) {
	case Length:
		p, err := decode(norm, bodyLen, payloadLen)
		if err != nil {
			return Code{}, err
		}
		if p[0]&FlagSteam != 0 {
			return Code{}, fmt.Errorf("%w: steam flag on an endpoint-length code", errBadCode)
		}
		return Code{Endpoint: &Endpoint{
			Flags: p[0],
			IP:    net.IPv4(p[1], p[2], p[3], p[4]),
			Port:  uint16(p[5])<<8 | uint16(p[6]),
		}}, nil
	case SteamLength:
		p, err := decode(norm, steamBodyLen, steamPayloadLen)
		if err != nil {
			return Code{}, err
		}
		if p[0]&FlagSteam == 0 {
			return Code{}, fmt.Errorf("%w: no steam flag on a steam-length code", errBadCode)
		}
		return Code{Steam: &Steam{
			Flags:     p[0] &^ FlagSteam,
			AccountID: binary.BigEndian.Uint32(p[1:5]),
			Key:       binary.BigEndian.Uint32(p[5:9]),
		}}, nil
	}
	return Code{}, errBadCode
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
