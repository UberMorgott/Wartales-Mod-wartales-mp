package nat

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"net"
	"time"
)

// DefaultSTUN is the server used when none is configured.
const DefaultSTUN = "stun.l.google.com:19302"

const magicCookie = 0x2112A442

// RFC 5389 attribute types and address family (RFC 5780 §7.2 keeps 0x8020 for
// the pre-standard XOR-MAPPED-ADDRESS some servers still send).
const (
	attrMappedAddress          = 0x0001
	attrXORMappedAddress       = 0x0020
	attrXORMappedAddressLegacy = 0x8020
	familyIPv4                 = 0x01
)

// STUNExternalIP asks a STUN server for our public address (RFC 5389 binding
// request). It only tells us the IP; port mapping still needs UPnP or a manual
// forward, since the game speaks TCP.
func STUNExternalIP(server string) (net.IP, error) {
	if server == "" {
		server = DefaultSTUN
	}
	var d net.Dialer
	c, err := d.DialContext(context.Background(), "udp", server)
	if err != nil {
		return nil, err
	}
	defer func() { _ = c.Close() }() // UDP socket, nothing to flush on close

	req := BindingRequest()
	if err := c.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return nil, err
	}
	if _, err := c.Write(req); err != nil {
		return nil, err
	}
	buf := make([]byte, 1500)
	n, err := c.Read(buf)
	if err != nil {
		return nil, err
	}
	return parseSTUN(buf[:n], req[8:20])
}

// BindingRequest builds an RFC 5389 binding request with a random
// transaction id (bytes 8..20).
func BindingRequest() []byte {
	req := make([]byte, 20)
	binary.BigEndian.PutUint16(req[0:], 0x0001) // binding request, no attributes
	binary.BigEndian.PutUint32(req[4:], magicCookie)
	_, _ = rand.Read(req[8:20]) // crypto/rand.Read never returns an error
	return req
}

// IsSTUN reports whether a datagram looks like a STUN message (RFC 5389
// §6: top two bits zero, magic cookie at offset 4). Punch and KCP packets
// never match: KCP has its command byte (81..84) at offset 4.
func IsSTUN(b []byte) bool {
	return len(b) >= 20 && b[0]&0xc0 == 0 && binary.BigEndian.Uint32(b[4:]) == magicCookie
}

func parseSTUN(resp, txID []byte) (net.IP, error) {
	a, err := ParseBinding(resp, txID)
	if err != nil {
		return nil, err
	}
	return a.IP, nil
}

// ParseBinding returns the mapped address (ip and port) of a binding success
// response to the request with transaction id txID.
func ParseBinding(resp, txID []byte) (*net.UDPAddr, error) {
	if len(resp) < 20 {
		return nil, errors.New("stun: short response")
	}
	if binary.BigEndian.Uint16(resp) != 0x0101 {
		return nil, fmt.Errorf("stun: unexpected message type 0x%04x", binary.BigEndian.Uint16(resp))
	}
	if binary.BigEndian.Uint32(resp[4:]) != magicCookie {
		return nil, errors.New("stun: bad magic cookie")
	}
	if string(resp[8:20]) != string(txID) {
		return nil, errors.New("stun: transaction id mismatch")
	}
	n := int(binary.BigEndian.Uint16(resp[2:]))
	if 20+n > len(resp) {
		n = len(resp) - 20
	}
	body := resp[20 : 20+n]

	// XOR-MAPPED-ADDRESS wins over MAPPED-ADDRESS whatever order they arrive
	// in: some NATs rewrite the plain one on the way back.
	var mapped *net.UDPAddr
	for len(body) >= 4 {
		typ := binary.BigEndian.Uint16(body)
		length := int(binary.BigEndian.Uint16(body[2:]))
		if len(body) < 4+length {
			break
		}
		val := body[4 : 4+length]
		switch typ {
		case attrXORMappedAddress, attrXORMappedAddressLegacy:
			if a := ipv4Attr(val); a != nil {
				// Address and port are XOR-ed with the magic cookie.
				var key [4]byte
				binary.BigEndian.PutUint32(key[:], magicCookie)
				for i := range a.IP {
					a.IP[i] ^= key[i]
				}
				a.Port ^= int(magicCookie >> 16)
				return a, nil
			}
		case attrMappedAddress:
			if a := ipv4Attr(val); a != nil && mapped == nil {
				mapped = a
			}
		}
		// attributes are padded to a multiple of 4 bytes
		body = body[4+(length+3)&^3:]
	}
	if mapped != nil {
		return mapped, nil
	}
	return nil, errors.New("stun: no mapped address")
}

// ipv4Attr decodes the {reserved, family, port, address} body shared by
// MAPPED-ADDRESS and XOR-MAPPED-ADDRESS, returning nil unless it is IPv4.
func ipv4Attr(val []byte) *net.UDPAddr {
	if len(val) < 8 || val[1] != familyIPv4 {
		return nil
	}
	ip := make(net.IP, 4)
	copy(ip, val[4:8])
	return &net.UDPAddr{IP: ip, Port: int(binary.BigEndian.Uint16(val[2:4]))}
}
