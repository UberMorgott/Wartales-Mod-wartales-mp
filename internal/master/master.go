// Package master is the local stand-in for master.shirogames.com.
//
// It speaks the JSON envelope of mpman/net/WSConnection over a TLS websocket
// on 127.0.0.1:60442, and answers the commands listed in
// decomp/SERVER-CONTRACT.md.
package master

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/applog"
	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
	"github.com/UberMorgott/wartales-mp/internal/wsx"
)

// Peer is whoever issued a command: the local game, or a guest behind a
// proxy-link.
type Peer interface {
	// UserID is the minted Session ('X') id: what a direct-relay lobby emits.
	UserID() string
	// SteamID is the real Steam id the player's game reported, or "" when it
	// reported none (or one that is not well-formed). An SDR lobby emits it.
	SteamID() string
	Name() string
	Remote() bool
	Push(cmd string, args any)
}

// Options configures the master.
type Options struct {
	Addr      string      // listen address, normally 127.0.0.1:60442
	TLS       *tls.Config // certificate for master.shirogames.com
	RelayPort int         // local relay port, used for the host's serverID
	HostPW    string      // relay host password
	SlavePW   string      // relay guest password
	Log       *log.Logger

	// PublicAddr returns the "ip:port" other players must reach us on. It is
	// used for guests' serverID and for short codes.
	PublicAddr func() (string, error)

	// Transport is ModeAuto, ModeDirect or ModeSDR ("" = auto).
	Transport string
	// Endpoint returns the full public endpoint verdict, for the transport
	// choice. nil means "unknown".
	Endpoint func() (nat.Endpoint, error)
	// SDRStatus returns the shim's verdict on the SDR transport. nil means
	// "unknown".
	SDRStatus func() SDRStatus
	// Bridge carries the proxy-link over SDR; nil means the lobby phase can
	// only travel over TCP.
	Bridge *sdrbridge.Bridge
	// LinkKey is embedded in join codes that carry an SDR route; a proxy-link
	// over SDR must present it, and one over TCP must when it presents any.
	LinkKey uint32

	// DirectTimeout bounds a guest's connect to the host's endpoint before
	// the cascade moves on to SDR (0 = DefaultDirectTimeout).
	DirectTimeout time.Duration
	// DialDirect replaces the TCP dialer of the direct route (tests).
	DialDirect func(ctx context.Context, addr string) (net.Conn, error)
}

// Server is the master.
type Server struct {
	opt     Options
	lobbies *store

	mu     sync.Mutex
	local  *session     // the local game's connection
	client *link.Client // set once we joined a remote lobby as a guest
	ln     net.Listener // the master listener, nil until ListenAndServe runs
	conns  map[net.Conn]struct{}
	closed bool
}

// wireErr carries protocol text the game client sees verbatim: Handle's error
// is marshalled straight into the "err" reply (see answer), so the message is
// payload, not a Go diagnostic. Its capitalization is part of the wire format,
// which is why these are built here instead of with fmt.Errorf - ST1005 would
// otherwise push us to lowercase text the client already expects.
type wireErr struct{ msg string }

func (e wireErr) Error() string { return e.msg }

// wireErrf formats protocol text for the client.
func wireErrf(format string, a ...any) error {
	return wireErr{msg: fmt.Sprintf(format, a...)}
}

// New builds a master server.
func New(opt Options) *Server {
	s := &Server{opt: opt}
	s.lobbies = newStore(s)
	return s
}

// ListenAndServe serves the master until the listener fails or Close is
// called; a Close returns nil, because a deliberate shutdown is not an error.
func (s *Server) ListenAndServe() error {
	ln, err := tls.Listen("tcp", s.opt.Addr, s.opt.TLS)
	if err != nil {
		return err
	}
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		_ = ln.Close() // Close raced us; nothing must be left listening
		return nil
	}
	s.ln = ln
	s.mu.Unlock()
	s.opt.Log.Printf("master listening on %s", s.opt.Addr)
	for {
		c, err := ln.Accept()
		if err != nil {
			if s.isClosed() {
				return nil
			}
			return err
		}
		s.mu.Lock()
		if s.closed {
			s.mu.Unlock()
			_ = c.Close()
			return nil
		}
		if s.conns == nil {
			s.conns = map[net.Conn]struct{}{}
		}
		s.conns[c] = struct{}{}
		s.mu.Unlock()
		go s.serve(c)
	}
}

func (s *Server) isClosed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closed
}

// ServeLink handles one guest's proxy-link connection over TCP (host role,
// direct transport). A guest holding a code with our key presents it and it
// must match: a stale address may now be somebody else's host. A guest with
// an endpoint-only code (no key) is still served, as it always was.
func (s *Server) ServeLink(c net.Conn) {
	s.serveLink(c, "proxy-link", func(u link.User) error {
		if u.Key != 0 && u.Key != s.opt.LinkKey {
			s.opt.Log.Printf("proxy-link: connection from %s refused: wrong join code key", c.RemoteAddr())
			return wireErrf("Invalid join code")
		}
		return nil
	})
}

// ServeSDRLink handles one guest's proxy-link stream over the SDR bridge
// (host role, SDR transport). Anyone who knows our SteamID can open a stream,
// so the hello must carry the key from our join code.
func (s *Server) ServeSDRLink(c net.Conn, peer uint64) {
	s.serveLink(c, "sdr-link", func(u link.User) error {
		if u.Key != s.opt.LinkKey {
			s.opt.Log.Printf("sdr-link: stream from %d refused: wrong join code key", peer)
			return wireErrf("Invalid join code")
		}
		return nil
	})
}

func (s *Server) serveLink(c net.Conn, kind string, accept link.Accept) {
	addr := c.RemoteAddr().String()
	s.opt.Log.Printf("link: accepted %s from %s", kind, addr)
	link.Serve(c, accept, func(cmd string, args json.RawMessage, peer *link.Peer) (any, error) {
		s.opt.Log.Printf("link: <- %s from %s (%s) %s", cmd, peer.Name(), peer.UserID(), applog.Trunc(args))
		result, err := s.Handle(cmd, args, peer)
		if err != nil {
			s.opt.Log.Printf("link: -> %s err %s", cmd, applog.Trunc(err.Error()))
		} else {
			s.opt.Log.Printf("link: -> %s ok %s", cmd, applog.Trunc(result))
		}
		return result, err
	}, func(peer *link.Peer) {
		s.opt.Log.Printf("link: %s from %s (%s) closed", kind, addr, peer.UserID())
		s.lobbies.peerGone(peer)
	})
}

// session is the local game's master connection.
type session struct {
	srv   *Server
	ws    *wsx.Conn
	mu    sync.Mutex
	uid   string
	steam string
	name  string
	push  int
}

func (p *session) UserID() string  { return p.uid }
func (p *session) SteamID() string { return p.steam }
func (p *session) Name() string    { return p.name }
func (p *session) Remote() bool    { return false }

// Push sends a server initiated command, per SERVER-CONTRACT §3.9.
func (p *session) Push(cmd string, args any) {
	raw, err := json.Marshal(args)
	if err != nil {
		return
	}
	p.mu.Lock()
	p.push++
	uid := p.push
	p.mu.Unlock()
	b, err := json.Marshal(link.Envelope{UID: uid, Cmd: cmd, Args: raw})
	if err != nil {
		p.srv.opt.Log.Printf("master: cannot marshal the push %s: %v", cmd, err)
		return
	}
	p.srv.opt.Log.Printf("master: -> push #%d %s %s", uid, cmd, applog.Trunc(raw))
	if err := p.ws.WriteText(string(b)); err != nil {
		p.srv.opt.Log.Printf("master: push %s failed: %v", cmd, err)
	}
}

func (s *Server) serve(c net.Conn) {
	defer func() {
		_ = c.Close() // the game's connection is finished either way
		s.mu.Lock()
		delete(s.conns, c)
		s.mu.Unlock()
	}()
	peer := c.RemoteAddr().String()
	s.opt.Log.Printf("master: accepted TLS connection from %s", peer)
	ws, err := wsx.Accept(c, nil, func(string) (string, bool) {
		// The game authenticates with a password obfuscated inside the
		// bytecode; we cannot check it, and we do not need to.
		return "", true
	})
	if err != nil {
		s.opt.Log.Printf("master: handshake from %s failed: %v", peer, err)
		return
	}
	sess := &session{srv: s, ws: ws}
	s.mu.Lock()
	s.local = sess
	s.mu.Unlock()
	s.opt.Log.Printf("master: game connected from %s (ident %q, headers %s)",
		peer, ws.Ident, applog.Trunc(headerLine(ws.Headers)))

	defer func() {
		s.opt.Log.Printf("master: game session from %s ended (uid %q)", peer, sess.UserID())
		s.lobbies.peerGone(sess)
		s.mu.Lock()
		if s.local == sess {
			s.local = nil
		}
		s.mu.Unlock()
	}()

	for {
		op, payload, err := ws.Read()
		if err != nil {
			s.opt.Log.Printf("master: game disconnected: %v", err)
			return
		}
		if op != wsx.OpText {
			continue
		}
		var e link.Envelope
		if err := json.Unmarshal(payload, &e); err != nil {
			s.opt.Log.Printf("master: bad frame from %s: %v (%s)", peer, err, applog.Trunc(payload))
			continue
		}
		if e.UID < 0 {
			s.opt.Log.Printf("master: <- push reply #%d %s", -e.UID, applog.Trunc(e.Args))
			continue // the game answering one of our pushes
		}
		go s.answer(sess, e)
	}
}

func (s *Server) answer(sess *session, e link.Envelope) {
	s.opt.Log.Printf("master: <- #%d %s %s", e.UID, e.Cmd, applog.Trunc(e.Args))
	result, err := s.Handle(e.Cmd, e.Args, sess)
	var reply link.Envelope
	if err != nil {
		s.opt.Log.Printf("master: -> #%d %s err %s", e.UID, e.Cmd, applog.Trunc(err.Error()))
		raw, _ := json.Marshal(err.Error())
		reply = link.Envelope{UID: -e.UID, Cmd: "err", Args: raw}
	} else {
		raw, mErr := json.Marshal(result)
		if mErr != nil || result == nil {
			raw = nil
		}
		s.opt.Log.Printf("master: -> #%d %s ok %s", e.UID, e.Cmd, applog.Trunc(raw))
		reply = link.Envelope{UID: -e.UID, Args: raw}
	}
	b, err := json.Marshal(reply)
	if err != nil {
		s.opt.Log.Printf("master: cannot marshal the reply to %s: %v", e.Cmd, err)
		return
	}
	if err := sess.ws.WriteText(string(b)); err != nil {
		s.opt.Log.Printf("master: reply to %s failed: %v", e.Cmd, err)
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

// Handle runs one command. Local lobby commands are forwarded to the host's
// master when we are a guest; everything the host answers is served from the
// authoritative local state.
func (s *Server) Handle(cmd string, args json.RawMessage, p Peer) (any, error) {
	switch {
	case cmd == "user/login":
		return s.userLogin(args, p)
	case cmd == "user/session":
		return s.userSession(args, p)
	case cmd == "user/time":
		return nowSeconds(), nil
	case cmd == "insight/event":
		return nil, nil
	case cmd == "code/redeem":
		return map[string]any{"code": 0}, nil
	case cmd == "instance/get":
		return s.instanceGet(args, p)
	case cmd == "instance/ingame":
		return map[string]any{"inGame": false}, nil
	case strings.HasPrefix(cmd, "lobby/"):
		return s.lobbyCommand(cmd, args, p)
	}
	return nil, wireErrf("Unknown command %s", cmd)
}

// forward proxies a command to the host's master. It returns ok=false when we
// are not linked to a host.
func (s *Server) forward(cmd string, args json.RawMessage) (json.RawMessage, bool, error) {
	s.mu.Lock()
	cl := s.client
	s.mu.Unlock()
	if cl == nil {
		return nil, false, nil
	}
	raw, err := cl.Call(cmd, args)
	return raw, true, err
}

func (s *Server) linked() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.client != nil
}

// localSession returns the game's connection, if any.
func (s *Server) localSession() *session {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.local
}

// Close stops the listener, drops every game connection and releases the link
// to the host, if we hold one. It is safe to call more than once.
func (s *Server) Close() {
	s.mu.Lock()
	s.closed = true
	cl := s.client
	s.client = nil
	ln := s.ln
	s.ln = nil
	conns := make([]net.Conn, 0, len(s.conns))
	for c := range s.conns {
		conns = append(conns, c)
	}
	s.conns = nil
	s.mu.Unlock()
	// shutting down; a failed close needs no recovery
	if ln != nil {
		_ = ln.Close()
	}
	for _, c := range conns {
		_ = c.Close() // unblocks the serve goroutine's ws.Read
	}
	if cl != nil {
		_ = cl.Close()
	}
}
