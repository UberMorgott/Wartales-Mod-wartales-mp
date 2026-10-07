package relay

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"io"
	"net"
	"testing"
	"time"
)

// writeFrame sends one unmasked binary frame (the relay takes either).
func writeFrame(t *testing.T, c net.Conn, payload []byte) {
	t.Helper()
	head := []byte{0x82, 126, 0, 0}
	binary.BigEndian.PutUint16(head[2:], uint16(len(payload))) //nolint:gosec // test payloads are small
	if _, err := c.Write(append(head, payload...)); err != nil {
		t.Fatal(err)
	}
}

// readFrame reads one server frame's payload.
func readFrame(br *bufio.Reader) ([]byte, error) {
	var h [2]byte
	if _, err := io.ReadFull(br, h[:]); err != nil {
		return nil, err
	}
	n := int(h[1] & 127)
	switch n {
	case 126:
		var b [2]byte
		if _, err := io.ReadFull(br, b[:]); err != nil {
			return nil, err
		}
		n = int(binary.BigEndian.Uint16(b[:]))
	case 127:
		var b [8]byte
		if _, err := io.ReadFull(br, b[:]); err != nil {
			return nil, err
		}
		n = int(binary.BigEndian.Uint64(b[:])) //nolint:gosec // test frames are small
	}
	buf := make([]byte, n)
	_, err := io.ReadFull(br, buf)
	return buf, err
}

// A guest that stops reading must not hold up the host's frames to the
// others: the relay's fan-out never waits on one peer.
func TestStalledGuestDoesNotBlockOthers(t *testing.T) {
	s := quiet()
	host, hbr, ok, hdone := handshake(t, s, "@host", s.HostPW)
	if !ok {
		t.Fatal("host refused")
	}
	stalled, _, ok, sdone := handshake(t, s, "stalled", s.SlavePW) // never read again
	if !ok {
		t.Fatal("guest refused")
	}
	live, lbr, ok, ldone := handshake(t, s, "live", s.SlavePW)
	if !ok {
		t.Fatal("guest refused")
	}
	cids := map[string]uint16{}
	for len(cids) < 2 {
		f, err := readFrame(hbr)
		if err != nil {
			t.Fatal(err)
		}
		typ, cid, payload, err := parseToHost(f)
		if err != nil || typ != TypeConnect {
			t.Fatalf("frame %d, %v; want a connect", typ, err)
		}
		ident, err := parseConnect(payload)
		if err != nil {
			t.Fatal(err)
		}
		cids[ident] = cid
	}
	go func() { _, _ = io.Copy(io.Discard, hbr) }()

	for range 4 { // net.Pipe has no buffer: the first one alone would block
		writeFrame(t, host, packFromHost(cids["stalled"], bytes.Repeat([]byte{1}, 1000)))
	}
	writeFrame(t, host, packFromHost(cids["live"], []byte("hello")))
	got := make(chan []byte, 1)
	go func() {
		f, err := readFrame(lbr)
		if err == nil {
			got <- f
		}
	}()
	select {
	case f := <-got:
		if string(f) != "hello" {
			t.Fatalf("live guest got %q", f)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the live guest's frame is stuck behind the stalled guest")
	}
	for _, c := range []net.Conn{host, stalled, live} {
		_ = c.Close()
	}
	<-hdone
	<-sdone
	<-ldone
}
