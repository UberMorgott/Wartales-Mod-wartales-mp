package relay

import (
	"bytes"
	"testing"
)

func TestPackToHostRoundTrip(t *testing.T) {
	payload := []byte{0xDE, 0xAD, 0xBE, 0xEF}
	frame := packToHost(TypeData, 0x1234, payload)
	if len(frame) != HSize+len(payload) {
		t.Fatalf("frame length %d", len(frame))
	}
	// cid must be little endian on the wire
	if frame[0] != TypeData || frame[1] != 0x34 || frame[2] != 0x12 {
		t.Fatalf("header = % x", frame[:HSize])
	}
	typ, cid, got, err := parseToHost(frame)
	if err != nil {
		t.Fatal(err)
	}
	if typ != TypeData || cid != 0x1234 || !bytes.Equal(got, payload) {
		t.Fatalf("got typ=%d cid=%d payload=% x", typ, cid, got)
	}
}

func TestPackConnectRoundTrip(t *testing.T) {
	const ident = "S0011223344556677:Player One"
	frame := packConnect(7, ident)
	typ, cid, payload, err := parseToHost(frame)
	if err != nil {
		t.Fatal(err)
	}
	if typ != TypeConnect || cid != 7 {
		t.Fatalf("typ=%d cid=%d", typ, cid)
	}
	got, err := parseConnect(payload)
	if err != nil {
		t.Fatal(err)
	}
	if got != ident {
		t.Fatalf("ident = %q", got)
	}
}

func TestDisconnectFrame(t *testing.T) {
	typ, cid, payload, err := parseToHost(packToHost(TypeDisconnect, 40000, nil))
	if err != nil {
		t.Fatal(err)
	}
	if typ != TypeDisconnect || cid != 40000 || len(payload) != 0 {
		t.Fatalf("typ=%d cid=%d payload=% x", typ, cid, payload)
	}
}

func TestFromHostRoundTrip(t *testing.T) {
	payload := []byte("hello")
	cid, got, err := parseFromHost(packFromHost(513, payload))
	if err != nil {
		t.Fatal(err)
	}
	if cid != 513 || !bytes.Equal(got, payload) {
		t.Fatalf("cid=%d payload=% x", cid, got)
	}
}

func TestShortFramesAreRejected(t *testing.T) {
	if _, _, _, err := parseToHost([]byte{1, 2}); err == nil {
		t.Fatal("parseToHost accepted a 2 byte frame")
	}
	if _, _, err := parseFromHost([]byte{1}); err == nil {
		t.Fatal("parseFromHost accepted a 1 byte frame")
	}
	if _, err := parseConnect([]byte{5, 0, 'a'}); err == nil {
		t.Fatal("parseConnect accepted a truncated identity")
	}
}
