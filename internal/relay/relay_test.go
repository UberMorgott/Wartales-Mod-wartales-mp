package relay

import (
	"bufio"
	"crypto/md5" //nolint:gosec // the game's X-Pass is an md5
	"encoding/base64"
	"encoding/hex"
	"io"
	"log"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

// handshake opens a relay connection over a pipe the way the game does and
// reports whether the relay switched protocols. The relay side runs in its own
// goroutine; done is closed when it has returned.
func handshake(t *testing.T, s *Server, ident, pass string) (c net.Conn, br *bufio.Reader, ok bool, done chan struct{}) {
	t.Helper()
	srv, cli := net.Pipe()
	done = make(chan struct{})
	go func() { defer close(done); s.Serve(srv, nil) }()
	key := []byte("0123456789abcdef")
	hash := base64.StdEncoding.EncodeToString(key)
	sum := md5.Sum([]byte(hex.EncodeToString(key) + pass)) //nolint:gosec // wire protocol
	req := "GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: " + hash +
		"\r\nX-Ident: " + ident + "\r\nX-Pass: " + hex.EncodeToString(sum[:]) + "\r\n\r\n"
	if _, err := io.WriteString(cli, req); err != nil {
		return cli, nil, false, done
	}
	br = bufio.NewReader(cli)
	line, err := br.ReadString('\n')
	if err != nil || !strings.Contains(line, " 101 ") {
		return cli, br, false, done
	}
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			return cli, br, false, done
		}
		if strings.TrimSpace(line) == "" {
			return cli, br, true, done
		}
	}
}

func quiet() *Server { return New(log.New(io.Discard, "", 0)) }

// waitHost waits until the relay has published its host: the handshake's 101
// reaches the peer before Serve registers the connection.
func waitHost(t *testing.T, s *Server) {
	t.Helper()
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); time.Sleep(time.Millisecond) {
		s.mu.Lock()
		up := s.hostCid != 0
		s.mu.Unlock()
		if up {
			return
		}
	}
	t.Fatal("the host was never published")
}

// Two hosts racing through the handshake: one of them is the host, the other
// is dropped, and nothing is published before it is authenticated.
func TestOneHostOutOfConcurrentHandshakes(t *testing.T) {
	s := quiet()
	const n = 8
	var wg sync.WaitGroup
	conns := make([]net.Conn, n)
	dones := make([]chan struct{}, n)
	for i := range n {
		wg.Go(func() {
			c, br, ok, done := handshake(t, s, "@host", s.HostPW)
			conns[i], dones[i] = c, done
			if ok {
				go func() { _, _ = io.Copy(io.Discard, br) }() // keep the pipe drained
			}
		})
	}
	wg.Wait()
	waitHost(t, s)
	s.mu.Lock()
	hosts := 0
	for _, c := range s.clients {
		if c.isHost {
			hosts++
		}
		if c.ws == nil {
			t.Error("a client was published before its handshake completed")
		}
	}
	s.mu.Unlock()
	if hosts != 1 {
		t.Fatalf("%d hosts published, want 1", hosts)
	}
	for i := range n {
		_ = conns[i].Close()
		<-dones[i]
	}
}

// A guest with a wrong password is never published, and a guest needs a host.
func TestUnauthenticatedGuestIsNeverPublished(t *testing.T) {
	s := quiet()
	c, _, ok, done := handshake(t, s, "guest", s.SlavePW)
	if ok {
		t.Fatal("a guest was accepted without a host")
	}
	_ = c.Close()
	<-done
	h, hbr, ok, hdone := handshake(t, s, "@host", s.HostPW)
	if !ok {
		t.Fatal("host refused")
	}
	go func() { _, _ = io.Copy(io.Discard, hbr) }()
	waitHost(t, s)
	c, _, ok, done = handshake(t, s, "guest", "wrong")
	if ok {
		t.Fatal("a guest with a wrong password was accepted")
	}
	_ = c.Close()
	<-done
	s.mu.Lock()
	n := len(s.clients)
	s.mu.Unlock()
	if n != 1 {
		t.Fatalf("%d clients published, want only the host", n)
	}
	_ = h.Close()
	<-hdone
}
