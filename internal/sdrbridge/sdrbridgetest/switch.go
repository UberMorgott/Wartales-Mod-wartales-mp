// Package sdrbridgetest is a stand-in for the shim's bridge, for tests: one
// loopback listener speaking bridge.c's frame protocol, where each connected
// helper is a Steam identity and SEND frames are switched to the helper that
// owns the target SteamID64, arriving there as RECV frames from the sender.
// A SEND to nobody is answered with an ERR frame, like a refused send.
package sdrbridgetest

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

const frameHead = 13

// Switch is the fake shim.
type Switch struct {
	LobbyInvites chan LobbyInvite
	ln           net.Listener
	dir          string

	mu      sync.Mutex
	tokens  map[string]uint64   // token -> identity
	clients map[uint64]net.Conn // identity -> its helper
}

// LobbyInvite records a helper's desired native Steam lobby state.
type LobbyInvite struct {
	Peer   uint64
	Invite string
}

// Disconnect simulates the shim dropping its authenticated helper connection.
func (s *Switch) Disconnect(peer uint64) {
	s.mu.Lock()
	c := s.clients[peer]
	s.mu.Unlock()
	if c != nil {
		_ = c.Close()
	}
}

// New starts a switch; it stops when the test ends.
func New(t *testing.T) *Switch {
	t.Helper()
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	s := &Switch{ln: ln, dir: t.TempDir(), tokens: map[string]uint64{}, clients: map[uint64]net.Conn{}, LobbyInvites: make(chan LobbyInvite, 128)}
	go s.accept()
	t.Cleanup(func() { _ = ln.Close() })
	return s
}

// Add registers a Steam identity and returns the path of a status file that
// advertises this switch to it, exactly as the shim would.
func (s *Switch) Add(t *testing.T, steamID uint64) string {
	t.Helper()
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		t.Fatal(err)
	}
	token := hex.EncodeToString(b[:])
	s.mu.Lock()
	s.tokens[token] = steamID
	s.mu.Unlock()
	path := filepath.Join(s.dir, fmt.Sprintf("sdr-%d.status", steamID))
	line := fmt.Sprintf("ok bridge=%s token=%s\n", s.ln.Addr(), token)
	if err := os.WriteFile(path, []byte(line), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// Unavailable returns a status file saying SDR cannot work.
func (s *Switch) Unavailable(t *testing.T, why string) string {
	t.Helper()
	path := filepath.Join(s.dir, "sdr-unavailable.status")
	if err := os.WriteFile(path, []byte("unavailable "+why+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func (s *Switch) accept() {
	for {
		c, err := s.ln.Accept()
		if err != nil {
			return
		}
		go s.serve(c)
	}
}

func readFrame(c net.Conn) (byte, uint64, []byte, error) {
	head := make([]byte, frameHead)
	if _, err := io.ReadFull(c, head); err != nil {
		return 0, 0, nil, err
	}
	n := binary.LittleEndian.Uint32(head[9:13])
	payload := make([]byte, n)
	_, err := io.ReadFull(c, payload)
	return head[0], binary.LittleEndian.Uint64(head[1:9]), payload, err
}

func writeFrame(c net.Conn, typ byte, peer uint64, payload []byte) error {
	buf := make([]byte, frameHead+len(payload))
	buf[0] = typ
	binary.LittleEndian.PutUint64(buf[1:9], peer)
	binary.LittleEndian.PutUint32(buf[9:13], uint32(len(payload))) //nolint:gosec // test payloads are small
	copy(buf[frameHead:], payload)
	_, err := c.Write(buf)
	return err
}

func (s *Switch) serve(c net.Conn) {
	defer func() { _ = c.Close() }()
	typ, _, tok, err := readFrame(c)
	if err != nil || typ != 0 {
		return
	}
	s.mu.Lock()
	me, ok := s.tokens[string(tok)]
	if ok {
		s.clients[me] = c
	}
	s.mu.Unlock()
	if !ok {
		return // a wrong token: closed without a word, like the shim
	}
	defer func() {
		s.mu.Lock()
		if s.clients[me] == c {
			delete(s.clients, me)
		}
		s.mu.Unlock()
	}()
	for {
		typ, peer, payload, err := readFrame(c)
		if err != nil {
			return
		}
		if typ != 1 {
			if typ == 4 && peer == 0 {
				s.LobbyInvites <- LobbyInvite{Peer: me, Invite: string(payload)}
			}
			continue
		}
		s.mu.Lock()
		dst := s.clients[peer]
		if dst == nil {
			_ = writeFrame(c, 3, peer, []byte("SendMessageToUser failed: EResult 3"))
		} else {
			_ = writeFrame(dst, 2, me, payload)
		}
		s.mu.Unlock() // also serialises writes to every client
	}
}
