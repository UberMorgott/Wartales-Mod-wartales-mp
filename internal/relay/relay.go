// Package relay implements the server half of mpman's RelayP2P transport.
//
// The game's host connects here with an identity starting with "@:" and the
// host password; every other connection is a guest and uses the slave
// password. The relay owns the public TCP port and multiplexes it: a
// connection whose first bytes are "GET " is a websocket (relay traffic),
// anything else is handed to the proxy-link handler.
package relay

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/binary"
	"log"
	"net"
	"sort"
	"strings"
	"sync"

	"github.com/UberMorgott/wartales-mp/internal/applog"
	"github.com/UberMorgott/wartales-mp/internal/wsx"
)

type client struct {
	cid    uint16
	ident  string
	isHost bool
	ws     *wsx.Conn

	// Frame counters, summarised in the log instead of dumping payloads.
	framesIn  int64
	bytesIn   int64
	framesOut int64
	bytesOut  int64
}

// frameLogEvery is how many frames pass between two relay traffic summaries.
const frameLogEvery = 512

// Server is the relay. It serves exactly one host at a time, which is all the
// game ever needs.
type Server struct {
	HostPW  string
	SlavePW string
	Log     *log.Logger

	mu      sync.Mutex
	clients map[uint16]*client
	hostCid uint16
}

// New creates a relay with freshly generated passwords.
func New(logger *log.Logger) *Server {
	return &Server{
		HostPW:  randomPassword(),
		SlavePW: randomPassword(),
		Log:     logger,
		clients: map[uint16]*client{},
	}
}

func randomPassword() string {
	const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	b := make([]byte, 24)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	for i, v := range b {
		b[i] = alphabet[int(v)%len(alphabet)]
	}
	return string(b)
}

// ListenAndServe owns the public port. Connections that do not look like an
// HTTP upgrade are passed to onLink (the proxy-link protocol).
func (s *Server) ListenAndServe(addr string, onLink func(net.Conn)) error {
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", addr)
	if err != nil {
		return err
	}
	s.Log.Printf("relay listening on %s", addr)
	for {
		c, err := ln.Accept()
		if err != nil {
			return err
		}
		go s.dispatch(c, onLink)
	}
}

// dispatch peeks at the first bytes to tell a websocket from a proxy-link.
func (s *Server) dispatch(c net.Conn, onLink func(net.Conn)) {
	br := bufio.NewReader(c)
	head, err := br.Peek(4)
	if err != nil {
		_ = c.Close() // the peer never sent anything; nothing to report to
		return
	}
	if string(head) == "GET " {
		s.Log.Printf("relay: accepted websocket from %s", c.RemoteAddr())
		s.Serve(c, br)
		return
	}
	if onLink == nil {
		s.Log.Printf("relay: no proxy-link handler, dropping %s", c.RemoteAddr())
		_ = c.Close() // no proxy-link handler configured; drop the connection
		return
	}
	onLink(&bufferedConn{Conn: c, r: br})
}

// bufferedConn hands the already buffered bytes back to the next reader.
type bufferedConn struct {
	net.Conn
	r *bufio.Reader
}

func (b *bufferedConn) Read(p []byte) (int, error) { return b.r.Read(p) }

// Serve runs one relay websocket connection to completion.
func (s *Server) Serve(c net.Conn, br *bufio.Reader) {
	defer func() { _ = c.Close() }() // connection is finished either way

	var self *client
	ws, err := wsx.Accept(c, br, func(ident string) (string, bool) {
		s.mu.Lock()
		defer s.mu.Unlock()
		// RelayServer.onClient: the first '@' connection becomes the host.
		if strings.HasPrefix(ident, "@") {
			if s.hostCid != 0 {
				return "", false
			}
			self = &client{cid: s.genCid(), ident: ident, isHost: true}
			s.clients[self.cid] = self
			return s.HostPW, true
		}
		if s.hostCid == 0 {
			// "No host connected, slave connection refused"
			return "", false
		}
		self = &client{cid: s.genCid(), ident: ident}
		s.clients[self.cid] = self
		return s.SlavePW, true
	})
	if err != nil {
		s.Log.Printf("relay: handshake from %s refused: %v", c.RemoteAddr(), err)
		if self != nil {
			s.mu.Lock()
			delete(s.clients, self.cid)
			s.mu.Unlock()
		}
		return
	}
	self.ws = ws

	role := "client"
	if self.isHost {
		role = "host"
		s.mu.Lock()
		s.hostCid = self.cid
		s.mu.Unlock()
	}
	s.Log.Printf("relay: %s connected (cid %d, ident %q, peer %s, headers %s)",
		role, self.cid, self.ident, c.RemoteAddr(), applog.Trunc(headerLine(ws.Headers)))
	if !self.isHost {
		s.toHost(packConnect(self.cid, self.ident))
	}
	defer func() {
		s.Log.Printf("relay: %s cid %d gone after %d frames in (%d B), %d frames out (%d B)",
			role, self.cid, self.framesIn, self.bytesIn, self.framesOut, self.bytesOut)
		s.drop(self)
	}()

	for {
		op, payload, err := ws.Read()
		if err != nil {
			s.Log.Printf("relay: %s cid %d read ended: %v", role, self.cid, err)
			return
		}
		if op != wsx.OpBinary {
			s.Log.Printf("relay: %s cid %d sent a non-binary frame (opcode %d), ignored", role, self.cid, op)
			continue // the relay carries binary frames only
		}
		if len(payload) < HSize {
			s.Log.Printf("relay: %s cid %d sent a %d byte frame, shorter than the %d byte header",
				role, self.cid, len(payload), HSize)
			continue // RelayServer drops anything shorter than a header
		}
		self.framesIn++
		self.bytesIn += int64(len(payload))
		if self.framesIn%frameLogEvery == 0 {
			s.Log.Printf("relay: %s cid %d: %d frames in (%d B)", role, self.cid, self.framesIn, self.bytesIn)
		}
		if self.isHost {
			cid, body, err := parseFromHost(payload)
			if err != nil {
				s.Log.Printf("relay: bad host frame (%d B): %v", len(payload), err)
				continue
			}
			to := s.client(cid)
			if to == nil {
				s.Log.Printf("relay: host addressed unknown client %d, dropping %d B", cid, len(body))
				continue
			}
			if err := to.ws.WriteBinary(body); err != nil {
				s.Log.Printf("relay: error with client %d: %v", cid, err)
				s.closeClient(cid)
				continue
			}
			to.framesOut++
			to.bytesOut += int64(len(body))
		} else {
			s.toHost(packToHost(TypeData, self.cid, payload))
		}
	}
}

// headerLine renders the handshake headers in a stable order for the log.
func headerLine(h map[string]string) string {
	keys := make([]string, 0, len(h))
	for k := range h {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	for _, k := range keys {
		if b.Len() > 0 {
			b.WriteString(" ")
		}
		b.WriteString(k)
		b.WriteString("=")
		if k == "x-pass" {
			b.WriteString("<redacted>") // a password hash never belongs in a log
			continue
		}
		b.WriteString(h[k])
	}
	return b.String()
}

func (s *Server) genCid() uint16 {
	var b [2]byte
	for {
		if _, err := rand.Read(b[:]); err != nil {
			panic(err)
		}
		cid := binary.LittleEndian.Uint16(b[:])%32768 + 1
		if _, taken := s.clients[cid]; !taken {
			return cid
		}
	}
}

func (s *Server) client(cid uint16) *client {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.clients[cid]
}

// toHost forwards a framed message to the host, if one is connected.
func (s *Server) toHost(frame []byte) {
	s.mu.Lock()
	host := s.clients[s.hostCid]
	s.mu.Unlock()
	if host == nil || host.ws == nil {
		return
	}
	if err := host.ws.WriteBinary(frame); err != nil {
		s.Log.Printf("relay: can't send to host: %v", err)
	}
}

// closeClient drops one guest and tells the host about it.
func (s *Server) closeClient(cid uint16) {
	s.mu.Lock()
	c := s.clients[cid]
	delete(s.clients, cid)
	s.mu.Unlock()
	if c == nil || c.isHost {
		return
	}
	s.toHost(packToHost(TypeDisconnect, cid, nil))
	if c.ws != nil {
		_ = c.ws.Close() // the guest is being dropped; a close error changes nothing
	}
}

// drop cleans up when a connection ends. Losing the host closes everyone.
func (s *Server) drop(c *client) {
	if c == nil {
		return
	}
	if !c.isHost {
		s.closeClient(c.cid)
		return
	}
	s.Log.Printf("relay: host disconnected")
	s.mu.Lock()
	others := make([]*client, 0, len(s.clients))
	for cid, other := range s.clients {
		if cid != c.cid {
			others = append(others, other)
		}
	}
	s.clients = map[uint16]*client{}
	s.hostCid = 0
	s.mu.Unlock()
	for _, other := range others {
		if other.ws != nil {
			_ = other.ws.Close() // host is gone; every guest is dropped regardless
		}
	}
}
