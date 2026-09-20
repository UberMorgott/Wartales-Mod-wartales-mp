package sdrbridge

import (
	"io"
	"log"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

type resetWire struct {
	net.Conn
	fin        chan struct{}
	release    chan struct{}
	mu         sync.Mutex
	dataFrames int
}

func (c *resetWire) Write(p []byte) (int, error) {
	if p[frameHead] == opFin {
		close(c.fin)
		<-c.release
	} else {
		c.mu.Lock()
		c.dataFrames++
		c.mu.Unlock()
	}
	return len(p), nil
}

func TestDialRetiresOldStreamBeforeReset(t *testing.T) {
	wire := &resetWire{fin: make(chan struct{}), release: make(chan struct{})}
	b := New("", log.New(io.Discard, "", 0))
	b.conn = wire
	old := b.newPeer(123)
	dialed := make(chan error, 1)
	go func() { _, err := b.Dial(123); dialed <- err }()
	<-wire.fin
	select {
	case <-old.done:
	default:
		close(wire.release)
		<-dialed
		t.Fatal("old stream remains writable while reset is on the wire")
	}
	written := make(chan error, 1)
	closed := make(chan error, 1)
	go func() { _, err := old.Write([]byte("stale")); written <- err }()
	go func() { closed <- old.Close() }()
	close(wire.release)
	if err := <-dialed; err != nil {
		t.Fatal(err)
	}
	if err := <-written; err == nil {
		t.Fatal("old write succeeded after reset")
	}
	if err := <-closed; err != nil {
		t.Fatal(err)
	}
	wire.mu.Lock()
	defer wire.mu.Unlock()
	if wire.dataFrames != 0 {
		t.Fatal("stale data followed reset")
	}
}

func TestSetLobbyInviteDoesNotWaitForWire(t *testing.T) {
	b := New("", log.New(io.Discard, "", 0))
	b.wmu.Lock()
	defer b.wmu.Unlock()
	done := make(chan struct{})
	go func() { b.SetLobbyInvite("FIRST"); b.SetLobbyInvite("LATEST"); close(done) }()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("setter blocked on the wire")
	}
	b.SetLobbyInvite(strings.Repeat("x", 33))
	b.SetLobbyInvite("bad\nframe")
	b.SetLobbyInvite("bad frame")
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.lobbyInvite != "LATEST" {
		t.Fatal("invalid payload changed desired invite")
	}
}
