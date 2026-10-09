package wsx

import (
	"bufio"
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
	_, err := Accept(srv, nil, HandshakeTimeout, nil)
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

// upgrade runs a complete handshake over a pipe and returns both ends.
func upgrade(t *testing.T) (*Conn, net.Conn, *bufio.Reader) {
	t.Helper()
	srv, cli := net.Pipe()
	go func() {
		_, _ = io.WriteString(cli, "GET / HTTP/1.1\r\nSec-WebSocket-Key: a2V5\r\n\r\n")
	}()
	type result struct {
		c   *Conn
		err error
	}
	res := make(chan result, 1)
	go func() { c, err := Accept(srv, nil, HandshakeTimeout, nil); res <- result{c, err} }()
	br := bufio.NewReader(cli)
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			t.Fatal(err)
		}
		if line == "\r\n" {
			break
		}
	}
	r := <-res
	if r.err != nil {
		t.Fatal(r.err)
	}
	t.Cleanup(func() { _ = r.c.Close(); _ = cli.Close() })
	return r.c, cli, br
}

// The keepalive pings a live peer, which answering keeps alive, and gives up
// on one that stops answering.
func TestKeepAlive(t *testing.T) {
	c, cli, br := upgrade(t)
	stop := c.KeepAlive(20*time.Millisecond, 150*time.Millisecond)
	defer stop()
	answering := make(chan bool, 1)
	answering <- true
	go func() { // the game: answer every ping with a (masked) pong while told to
		for {
			var h [2]byte
			if _, err := io.ReadFull(br, h[:]); err != nil {
				return
			}
			if h[0]&15 != OpPing || h[1]&127 != 0 {
				return
			}
			on := <-answering
			answering <- on
			if on {
				if _, err := cli.Write([]byte{0x80 | OpPong, 0x80, 0, 0, 0, 0}); err != nil {
					return
				}
			}
		}
	}()
	readErr := make(chan error, 1)
	go func() { _, _, err := c.Read(); readErr <- err }()
	select {
	case err := <-readErr:
		t.Fatalf("a peer answering pings was dropped: %v", err)
	case <-time.After(400 * time.Millisecond):
	}
	<-answering
	answering <- false
	select {
	case err := <-readErr:
		if err == nil {
			t.Fatal("Read returned without an error")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("a peer that stopped answering was kept")
	}
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

// The deadline follows the timeout passed in: a request that takes 300 ms is
// dropped under a 100 ms timeout and accepted under a 5 s one (#5).
func TestAcceptUsesGivenTimeout(t *testing.T) {
	t.Parallel()
	slow := func(timeout time.Duration) error {
		srv, cli := net.Pipe()
		defer func() { _ = srv.Close(); _ = cli.Close() }()
		go func() {
			time.Sleep(300 * time.Millisecond)
			_, _ = io.WriteString(cli, "GET / HTTP/1.1\r\nSec-WebSocket-Key: a2V5\r\n\r\n")
			_, _ = io.Copy(io.Discard, cli) // take the 101 response
		}()
		_, err := Accept(srv, nil, timeout, nil)
		return err
	}
	if err := slow(100 * time.Millisecond); err == nil {
		t.Fatal("a slow request beat a 100 ms timeout")
	}
	if err := slow(5 * time.Second); err != nil {
		t.Fatalf("a slow request failed under a 5 s timeout: %v", err)
	}
}
