package wsx

import (
	"io"
	"net"
	"strings"
	"testing"
	"time"
)

// accept runs Accept against a peer that writes req (and then stays open),
// returning Accept's error.
func accept(t *testing.T, req string) error {
	t.Helper()
	srv, cli := net.Pipe()
	defer func() { _ = srv.Close(); _ = cli.Close() }()
	go func() { _, _ = io.WriteString(cli, req) }()
	_, err := Accept(srv, nil, nil)
	return err
}

func TestAcceptBoundsTheRequest(t *testing.T) {
	t.Run("longLine", func(t *testing.T) {
		err := accept(t, "GET / HTTP/1.1\r\nX-Junk: "+strings.Repeat("a", maxRequest)+"\r\n\r\n")
		if err == nil || !strings.Contains(err.Error(), "too large") {
			t.Fatalf("got %v, want a refusal", err)
		}
	})
	t.Run("endlessLines", func(t *testing.T) {
		err := accept(t, "GET / HTTP/1.1\r\n"+strings.Repeat("junk\r\n", maxRequest))
		if err == nil || !strings.Contains(err.Error(), "too large") {
			t.Fatalf("got %v, want a refusal", err)
		}
	})
}

// A peer that never finishes its request is dropped after HandshakeTimeout.
func TestAcceptTimesOut(t *testing.T) {
	t.Parallel()
	start := time.Now()
	err := accept(t, "GET / HTTP/1.1\r\n")
	if err == nil {
		t.Fatal("an unfinished request was accepted")
	}
	if took := time.Since(start); took < HandshakeTimeout || took > HandshakeTimeout+5*time.Second {
		t.Fatalf("gave up after %v, want %v", took, HandshakeTimeout)
	}
}
