package master

import (
	"bufio"
	"net"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/link"
)

// silentHost is a link to a host that reads everything and answers nothing,
// so a forwarded command blocks for the link's whole call timeout.
func silentHost(t *testing.T, timeout time.Duration) *link.Client {
	t.Helper()
	hostEnd, guestEnd := net.Pipe()
	go func() {
		br := bufio.NewReader(hostEnd)
		for {
			if _, err := br.ReadBytes('\n'); err != nil {
				return
			}
		}
	}()
	cl, err := link.DialConn(guestEnd, link.User{ID: "Xguest", Name: "Guest"}, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	cl.CallTimeout = timeout
	t.Cleanup(func() { _ = cl.Close(); _ = hostEnd.Close() })
	return cl
}

// Commands the game queued behind a slow one must not run after its socket
// is gone: a late user/login would make the dead session the game's
// connection again (and a late lobby/create or join would put it in a lobby).
func TestNoCommandRunsAfterTheGameDisconnects(t *testing.T) {
	const block = 300 * time.Millisecond
	s, addr := startMasterOpts(t, nil)
	s.mu.Lock()
	s.client = silentHost(t, block)
	s.mu.Unlock()

	game := dialMaster(t, addr)
	game.send(t, "lobby/list", map[string]any{}) // forwarded to the silent host: blocks
	game.send(t, "user/login", map[string]any{"uid": "S1234", "name": "P"})
	time.Sleep(50 * time.Millisecond) // both are read before the socket ends
	_ = game.c.Close()

	deadline := time.Now().Add(block + time.Second)
	for {
		s.mu.Lock()
		served := len(s.conns)
		s.mu.Unlock()
		if served == 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the game's session never ended")
		}
		time.Sleep(10 * time.Millisecond)
	}
	time.Sleep(block) // anything still running has had its chance by now
	if sess := s.localSession(); sess != nil {
		t.Fatal("a command queued before the disconnect re-adopted the dead session")
	}
}
