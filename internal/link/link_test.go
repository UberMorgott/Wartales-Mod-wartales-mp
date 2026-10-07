package link

import (
	"bufio"
	"encoding/json"
	"io"
	"net"
	"strings"
	"testing"
	"time"
)

func noHandler(string, json.RawMessage, *Peer) (any, error) { return nil, nil }

// serve runs Serve against a peer that writes req and then stays silent,
// and returns how long Serve took to give up on it.
func serve(t *testing.T, req string) time.Duration {
	t.Helper()
	srv, cli := net.Pipe()
	defer func() { _ = cli.Close() }()
	go func() { _, _ = io.WriteString(cli, req) }()
	go func() { _, _ = io.Copy(io.Discard, cli) }()
	start := time.Now()
	done := make(chan struct{})
	go func() { defer close(done); Serve(srv, nil, noHandler, nil) }()
	select {
	case <-done:
	case <-time.After(HelloTimeout + 5*time.Second):
		t.Fatal("Serve still waits on a guest that never said hello")
	}
	return time.Since(start)
}

func TestServeRefusesAnOversizedHello(t *testing.T) {
	if took := serve(t, strings.Repeat("a", 2*maxHello)); took >= HelloTimeout {
		t.Fatalf("took %v: the oversized hello was not refused as it arrived", took)
	}
}

func TestServeDropsASilentGuest(t *testing.T) {
	t.Parallel()
	if took := serve(t, `{"uid":1,"cmd":"link/hel`); took < HelloTimeout {
		t.Fatalf("gave up after %v, before HelloTimeout", took)
	}
}

func TestReadEnvelopeLimit(t *testing.T) {
	srv, cli := net.Pipe()
	defer func() { _ = srv.Close(); _ = cli.Close() }()
	big := `{"uid":2,"args":"` + strings.Repeat("x", 10000) + `"}` + "\n"
	go func() { _, _ = io.WriteString(cli, big+big) }()
	br := bufio.NewReader(srv)
	if _, err := readEnvelope(br, len(big)); err != nil {
		t.Fatalf("a line at the limit: %v", err)
	}
	if _, err := readEnvelope(br, len(big)-1); err == nil {
		t.Fatal("a line over the limit was read")
	}
}
