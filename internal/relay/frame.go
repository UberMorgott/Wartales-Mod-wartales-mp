package relay

import (
	"encoding/binary"
	"errors"
	"math"
)

// Wire format of the relay, from mpman/net/RelayP2PService.hx (RelayServer):
// every relay -> host frame starts with HSize bytes [type:u8][cid:u16], and
// every host -> relay frame starts with [cid:u16]. Client (slave) frames carry
// no header at all: the relay adds one on the way to the host and strips it on
// the way back.
//
// haxe.io.Bytes.getUInt16/setUInt16 are little endian on HashLink, so cid is
// little endian here.
const HSize = 3

// Frame types (relay -> host).
const (
	TypeConnect    = 1 // [1][cid][identLen:u16][ident bytes]
	TypeDisconnect = 2 // [2][cid]
	TypeData       = 3 // [3][cid][payload]
)

var errShort = errors.New("relay: frame too short")

// packToHost builds a relay -> host frame.
func packToHost(typ byte, cid uint16, payload []byte) []byte {
	b := make([]byte, HSize+len(payload))
	b[0] = typ
	binary.LittleEndian.PutUint16(b[1:], cid)
	copy(b[HSize:], payload)
	return b
}

// packConnect builds the SConnect frame: the identity string is length
// prefixed with a u16 (RelayHost reads getUInt16 then getString).
func packConnect(cid uint16, ident string) []byte {
	n := len(ident)
	if n > math.MaxUint16 { // the u16 prefix cannot describe a longer identity
		n = math.MaxUint16
		ident = ident[:n]
	}
	body := make([]byte, 2+len(ident))
	binary.LittleEndian.PutUint16(body, uint16(n))
	copy(body[2:], ident)
	return packToHost(TypeConnect, cid, body)
}

// parseToHost splits a relay -> host frame. Used by the tests and mirrors what
// the game's RelayHost does on receive.
func parseToHost(b []byte) (typ byte, cid uint16, payload []byte, err error) {
	if len(b) < HSize {
		return 0, 0, nil, errShort
	}
	return b[0], binary.LittleEndian.Uint16(b[1:]), b[HSize:], nil
}

// parseConnect extracts the identity string of a SConnect payload.
func parseConnect(payload []byte) (string, error) {
	if len(payload) < 2 {
		return "", errShort
	}
	n := int(binary.LittleEndian.Uint16(payload))
	if len(payload) < 2+n {
		return "", errShort
	}
	return string(payload[2 : 2+n]), nil
}

// parseFromHost splits a host -> relay frame into the target cid and the
// payload that must be forwarded verbatim (RelayHost.sendTo writes the cid as
// a u16 at offset 0 and blits the payload from offset 2).
func parseFromHost(b []byte) (cid uint16, payload []byte, err error) {
	if len(b) < 2 {
		return 0, nil, errShort
	}
	return binary.LittleEndian.Uint16(b), b[2:], nil
}

// packFromHost is the inverse of parseFromHost (used by tests).
func packFromHost(cid uint16, payload []byte) []byte {
	b := make([]byte, 2+len(payload))
	binary.LittleEndian.PutUint16(b, cid)
	copy(b[2:], payload)
	return b
}
