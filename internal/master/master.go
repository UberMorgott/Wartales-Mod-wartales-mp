// Package master is the local stand-in for master.shirogames.com.
//
// It speaks the JSON envelope of mpman/net/WSConnection over a TLS websocket
// on 127.0.0.1:60442, and answers the commands listed in
// decomp/SERVER-CONTRACT.md.
package master

import (
	"crypto/tls"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"strings"
	"sync"

	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/wsx"
)

// Peer is whoever issued a command: the local game, or a guest behind a
// proxy-link.
type Peer interface {
	UserID() string
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
}

// Server is the master.
type Server struct {
	opt     Options
	lobbies *store

	mu     sync.Mutex
	local  *session     // the local game's connection
	client *link.Client // set once we joined a remote lobby as a guest
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

// ListenAndServe serves the master until the listener fails.
func (s *Server) ListenAndServe() error {
	ln, err := tls.Listen("tcp", s.opt.Addr, s.opt.TLS)
	if err != nil {
		return err
	}
	s.opt.Log.Printf("master listening on %s", s.opt.Addr)
	for {
		c, err := ln.Accept()
		if err != nil {
			return err
		}
		go s.serve(c)
	}
}

// ServeLink handles one guest's proxy-link connection (host role).
func (s *Server) ServeLink(c net.Conn) {
	link.Serve(c, func(cmd string, args json.RawMessage, peer *link.Peer) (any, error) {
		return s.Handle(cmd, args, peer)
	}, func(peer *link.Peer) {
		s.lobbies.peerGone(peer)
	})
}

// session is the local game's master connection.
type session struct {
	srv  *Server
	ws   *wsx.Conn
	mu   sync.Mutex
	uid  string
	name string
	push int
}

func (p *session) UserID() string { return p.uid }
func (p *session) Name() string   { return p.name }
func (p *session) Remote() bool   { return false }

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
		return
	}
	if err := p.ws.WriteText(string(b)); err != nil {
		p.srv.opt.Log.Printf("master: push %s failed: %v", cmd, err)
	}
}

func (s *Server) serve(c net.Conn) {
	defer func() { _ = c.Close() }() // the game's connection is finished either way
	ws, err := wsx.Accept(c, nil, func(string) (string, bool) {
		// The game authenticates with a password obfuscated inside the
		// bytecode; we cannot check it, and we do not need to.
		return "", true
	})
	if err != nil {
		s.opt.Log.Printf("master: handshake failed: %v", err)
		return
	}
	sess := &session{srv: s, ws: ws}
	s.mu.Lock()
	s.local = sess
	s.mu.Unlock()
	s.opt.Log.Printf("master: game connected (ident %q)", ws.Ident)

	defer func() {
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
			s.opt.Log.Printf("master: bad frame: %v", err)
			continue
		}
		if e.UID < 0 {
			continue // the game answering one of our pushes
		}
		go s.answer(sess, e)
	}
}

func (s *Server) answer(sess *session, e link.Envelope) {
	result, err := s.Handle(e.Cmd, e.Args, sess)
	var reply link.Envelope
	if err != nil {
		s.opt.Log.Printf("master: %s failed: %v", e.Cmd, err)
		raw, _ := json.Marshal(err.Error())
		reply = link.Envelope{UID: -e.UID, Cmd: "err", Args: raw}
	} else {
		raw, mErr := json.Marshal(result)
		if mErr != nil || result == nil {
			raw = nil
		}
		reply = link.Envelope{UID: -e.UID, Args: raw}
	}
	b, err := json.Marshal(reply)
	if err != nil {
		return
	}
	if err := sess.ws.WriteText(string(b)); err != nil {
		s.opt.Log.Printf("master: reply failed: %v", err)
	}
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

// Close releases the link to the host, if we hold one.
func (s *Server) Close() {
	s.mu.Lock()
	cl := s.client
	s.client = nil
	s.mu.Unlock()
	if cl != nil {
		_ = cl.Close() // shutting down; a failed close needs no recovery
	}
}
