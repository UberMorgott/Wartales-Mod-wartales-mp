package nat

import (
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

// STUNExternalIP asks a STUN server for our public address (RFC 5389 binding
// request). It only tells us the IP; port mapping still needs UPnP or a manual
// forward, since the game speaks TCP.
func STUNExternalIP(server string) (net.IP, error) {
	if server == "" {
		server = DefaultSTUN
	}
	c, err := net.Dial("udp", server)
	if err != nil {
		return nil, err
	}
	defer func() { _ = c.Close() }() // UDP socket, nothing to flush on close

	req := make([]byte, 20)
	binary.BigEndian.PutUint16(req[0:], 0x0001) // binding request
	binary.BigEndian.PutUint16(req[2:], 0)      // no attributes
	binary.BigEndian.PutUint32(req[4:], magicCookie)
	if _, err := rand.Read(req[8:20]); err != nil {
		return nil, err
	}

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

func parseSTUN(resp, txID []byte) (net.IP, error) {
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

	for len(body) >= 4 {
		typ := binary.BigEndian.Uint16(body)
		length := int(binary.BigEndian.Uint16(body[2:]))
		if len(body) < 4+length {
			break
		}
		val := body[4 : 4+length]
		switch typ {
		case 0x0020, 0x0001: // XOR-MAPPED-ADDRESS, MAPPED-ADDRESS
			if len(val) >= 8 && val[1] == 0x01 { // IPv4
				ip := make(net.IP, 4)
				copy(ip, val[4:8])
				if typ == 0x0020 {
					for i := range ip {
						ip[i] ^= resp[4+i]
					}
				}
				return ip, nil
			}
		}
		// attributes are padded to 4 bytes
		body = body[4+(length+3)&^3:]
	}
	return nil, errors.New("stun: no mapped address")
}
