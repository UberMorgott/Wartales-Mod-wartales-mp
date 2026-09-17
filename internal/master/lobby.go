package master

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"maps"
	"net"
	"strconv"
	"sync"

	"github.com/UberMorgott/wartales-mp/internal/applog"
	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/link"
)

// member is one player inside a lobby. Data is a JSON string, as the client
// sends it (MPLobby encodes user data with haxe.Json.print).
type member struct {
	ID   string
	Name string
	Data json.RawMessage
	peer Peer
}

type lobby struct {
	id         string
	owner      string
	data       map[string]json.RawMessage // values are JSON strings (encodeData)
	isPrivate  *bool
	maxPlayers *int
	users      []*member
}

type store struct {
	srv *Server

	mu      sync.Mutex
	lobbies map[string]*lobby
	codes   map[string]string // short code -> lobby id

	packets     int64 // lobby transport packets seen (lobby/chat fan-out)
	packetBytes int64
}

func newStore(srv *Server) *store {
	return &store{srv: srv, lobbies: map[string]*lobby{}, codes: map[string]string{}}
}

// newLobbyID returns an id whose first character is a valid UserID platform
// char ('L' = Lobby); the client rejects anything else.
func newLobbyID() string {
	var b [8]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	return "L" + hex.EncodeToString(b[:])
}

func (l *lobby) info() map[string]any {
	users := make([]map[string]any, 0, len(l.users))
	for _, u := range l.users {
		data := u.Data
		if len(data) == 0 {
			data = json.RawMessage(`""`)
		}
		users = append(users, map[string]any{"id": u.ID, "name": u.Name, "data": data})
	}
	data := l.data
	if data == nil {
		data = map[string]json.RawMessage{}
	}
	return map[string]any{
		"id":    l.id,
		"owner": l.owner,
		"props": map[string]any{
			"data":       data,
			"isPrivate":  l.isPrivate,
			"maxPlayers": l.maxPlayers,
		},
		"users": users,
	}
}

// broadcast sends a lobby push to every member but the actor. The recipient
// list is snapshotted under the store lock: peerGone rewrites l.users from
// another goroutine.
func (s *store) broadcast(l *lobby, except Peer, cmd string, args map[string]any) int {
	s.mu.Lock()
	to := make([]Peer, 0, len(l.users))
	for _, u := range l.users {
		if u.peer == nil || (except != nil && u.ID == except.UserID()) {
			continue
		}
		to = append(to, u.peer)
	}
	s.mu.Unlock()
	for _, p := range to {
		p.Push(cmd, args)
	}
	return len(to)
}

// transportLogEvery is how many lobby transport packets pass between two
// summaries; the payloads themselves are never logged, only counted.
const transportLogEvery = 64

// transport counts one lobby transport packet and summarises it periodically.
func (s *store) transport(logger *log.Logger, id, from string, size, to int) {
	s.mu.Lock()
	s.packets++
	s.packetBytes += int64(size)
	n, total := s.packets, s.packetBytes
	s.mu.Unlock()
	if n == 1 || n%transportLogEvery == 0 {
		logger.Printf("master: lobby transport %s: packet #%d from %s, %d B to %d peer(s), %d B total",
			id, n, from, size, to, total)
	}
}

func (s *store) get(id string) *lobby {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.lobbies[id]
}

// peerGone drops a disconnected player from every lobby it was in.
func (s *store) peerGone(p Peer) {
	if p == nil || p.UserID() == "" {
		return
	}
	s.mu.Lock()
	var affected []*lobby
	for _, l := range s.lobbies {
		for i, u := range l.users {
			if u.ID == p.UserID() {
				l.users = append(l.users[:i], l.users[i+1:]...)
				affected = append(affected, l)
				break
			}
		}
	}
	for _, l := range affected {
		if len(l.users) == 0 {
			delete(s.lobbies, l.id)
			continue
		}
		if l.owner == p.UserID() {
			l.owner = l.users[0].ID
		}
	}
	s.mu.Unlock()

	for _, l := range affected {
		if s.srv != nil {
			s.srv.opt.Log.Printf("master: %s left lobby %s, %d member(s) remain, owner %s",
				p.UserID(), l.id, len(l.users), l.owner)
		}
		s.broadcast(l, p, "lobby/leave", map[string]any{"id": l.id, "uid": p.UserID()})
		if l.owner != p.UserID() && len(l.users) > 0 {
			s.broadcast(l, p, "lobby/transfer", map[string]any{"id": l.id, "uid": l.owner})
		}
	}
}

// ---------------------------------------------------------------- commands

type propsArgs struct {
	Data       map[string]json.RawMessage `json:"data"`
	IsPrivate  *bool                      `json:"isPrivate"`
	MaxPlayers *int                       `json:"maxPlayers"`
	MyData     json.RawMessage            `json:"myData"`
}

type lobbyArgs struct {
	ID        string                     `json:"id"`
	UID       string                     `json:"uid"`
	User      string                     `json:"user"`
	Invite    string                     `json:"invite"`
	ShortCode string                     `json:"shortCode"`
	Msg       json.RawMessage            `json:"msg"`
	Data      json.RawMessage            `json:"data"`
	Props     *propsArgs                 `json:"props"`
	Filters   map[string]json.RawMessage `json:"filters"`
}

// lobbyCommand implements SERVER-CONTRACT §3.8. A guest forwards everything to
// the host's master, which holds the authoritative state.
func (s *Server) lobbyCommand(cmd string, args json.RawMessage, p Peer) (any, error) {
	if !p.Remote() && cmd != "lobby/resolveShortCode" {
		if raw, ok, err := s.forward(cmd, args); ok {
			return raw, err
		}
	}
	var a lobbyArgs
	_ = json.Unmarshal(args, &a)

	switch cmd {
	case "lobby/create":
		return s.lobbyCreate(a, p)
	case "lobby/join":
		return s.lobbyJoin(a, p)
	case "lobby/leave":
		s.lobbies.peerGone(p)
		return nil, nil
	case "lobby/list":
		return s.lobbyList(), nil
	case "lobby/info":
		return s.lobbyInfo(a.ID)
	case "lobby/infoInvite":
		return s.lobbyInfo(a.Invite)
	case "lobby/setData":
		return nil, s.lobbySetData(a, p)
	case "lobby/setUserData":
		return nil, s.lobbySetUserData(a, p)
	case "lobby/chat":
		return nil, s.lobbyChat(a, p)
	case "lobby/transfer":
		return nil, s.lobbyTransfer(a, p)
	case "lobby/report":
		return nil, nil
	case "lobby/makeShortCode":
		return s.lobbyMakeShortCode(a)
	case "lobby/resolveShortCode":
		return s.lobbyResolveShortCode(a, args, p)
	}
	return nil, wireErrf("Unknown command %s", cmd)
}

// lobbyCreate replies with the new lobby id only; the client builds the
// LobbyInfo itself.
func (s *Server) lobbyCreate(a lobbyArgs, p Peer) (any, error) {
	l := &lobby{id: newLobbyID(), owner: p.UserID(), data: map[string]json.RawMessage{}}
	if a.Props != nil {
		if a.Props.Data != nil {
			l.data = a.Props.Data
		}
		l.isPrivate = a.Props.IsPrivate
		l.maxPlayers = a.Props.MaxPlayers
		l.users = append(l.users, &member{ID: p.UserID(), Name: p.Name(), Data: a.Props.MyData, peer: p})
	} else {
		l.users = append(l.users, &member{ID: p.UserID(), Name: p.Name(), peer: p})
	}
	s.lobbies.mu.Lock()
	s.lobbies.lobbies[l.id] = l
	s.lobbies.mu.Unlock()
	s.opt.Log.Printf("master: lobby %s created by %s (%s), owner %s, props %s",
		l.id, p.Name(), p.UserID(), l.owner, applog.Trunc(a.Props))
	s.opt.Log.Printf("master: lobby %s transport is lobby/chat on this connection "+
		"(mpman LobbyService, no second socket)", l.id)
	return l.id, nil
}

func (s *Server) lobbyJoin(a lobbyArgs, p Peer) (any, error) {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return nil, wireErrf("Unknown lobby %s", a.ID)
	}
	s.lobbies.mu.Lock()
	if l.maxPlayers != nil && len(l.users) >= *l.maxPlayers {
		s.lobbies.mu.Unlock()
		return nil, wireErrf("Lobby is full")
	}
	found := false
	for _, u := range l.users {
		if u.ID == p.UserID() {
			u.Data, u.peer, u.Name, found = a.Data, p, p.Name(), true
			break
		}
	}
	if !found {
		l.users = append(l.users, &member{ID: p.UserID(), Name: p.Name(), Data: a.Data, peer: p})
	}
	info := l.info()
	s.lobbies.mu.Unlock()

	if !found {
		// MPLobby.onCommand@54985 runs haxe.Unserializer over args.data and
		// answers false - dropping the joiner - when it yields null, so the
		// push always carries something that unserializes: "z" is the haxe
		// encoding of Int 0.
		data := a.Data
		if len(data) == 0 {
			data = json.RawMessage(`"z"`)
		}
		n := s.lobbies.broadcast(l, p, "lobby/join", map[string]any{
			"id": l.id, "uid": p.UserID(), "name": p.Name(), "data": data,
		})
		s.opt.Log.Printf("master: %s (%s) joined lobby %s, told %d peer(s)",
			p.Name(), p.UserID(), l.id, n)
	} else {
		s.opt.Log.Printf("master: %s (%s) rejoined lobby %s", p.Name(), p.UserID(), l.id)
	}
	return info, nil
}

func (s *Server) lobbyList() []map[string]any {
	s.lobbies.mu.Lock()
	defer s.lobbies.mu.Unlock()
	out := []map[string]any{}
	for _, l := range s.lobbies.lobbies {
		if l.isPrivate != nil && *l.isPrivate {
			continue
		}
		out = append(out, l.info())
	}
	return out
}

func (s *Server) lobbyInfo(id string) (any, error) {
	l := s.lobbies.get(id)
	if l == nil {
		return nil, nil // the client treats a null answer as "not found"
	}
	s.lobbies.mu.Lock()
	defer s.lobbies.mu.Unlock()
	return l.info(), nil
}

func (s *Server) lobbySetData(a lobbyArgs, p Peer) error {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return wireErrf("Unknown lobby %s", a.ID)
	}
	if l.owner != p.UserID() {
		return wireErrf("Cannot set data if not owner")
	}
	var data map[string]json.RawMessage
	if err := json.Unmarshal(a.Data, &data); err != nil {
		return wireErrf("Invalid data")
	}
	s.lobbies.mu.Lock()
	if l.data == nil {
		l.data = map[string]json.RawMessage{}
	}
	maps.Copy(l.data, data)
	s.lobbies.mu.Unlock()
	s.lobbies.broadcast(l, p, "lobby/setData", map[string]any{"id": l.id, "data": data})
	return nil
}

func (s *Server) lobbySetUserData(a lobbyArgs, p Peer) error {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return wireErrf("Unknown lobby %s", a.ID)
	}
	s.lobbies.mu.Lock()
	for _, u := range l.users {
		if u.ID == p.UserID() {
			u.Data = a.Data
		}
	}
	s.lobbies.mu.Unlock()
	s.lobbies.broadcast(l, p, "lobby/setUserData", map[string]any{"id": l.id, "uid": p.UserID(), "data": a.Data})
	return nil
}

// lobbyChat is also the lobby transport: mpman's LobbyService has no socket of
// its own and tunnels every hxbit packet through lobby/chat as a
// haxe.Serializer string of LobbyMessageData.Packet(Bytes, targetUid). See
// decomp/SERVER-CONTRACT.md §7. The fan-out must reach every other member,
// which then filters on the target uid embedded in the payload.
func (s *Server) lobbyChat(a lobbyArgs, p Peer) error {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return wireErrf("Unknown lobby %s", a.ID)
	}
	n := s.lobbies.broadcast(l, p, "lobby/chat",
		map[string]any{"id": l.id, "uid": p.UserID(), "msg": a.Msg})
	s.lobbies.transport(s.opt.Log, l.id, p.UserID(), len(a.Msg), n)
	return nil
}

func (s *Server) lobbyTransfer(a lobbyArgs, p Peer) error {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return wireErrf("Unknown lobby %s", a.ID)
	}
	if l.owner != p.UserID() {
		return wireErrf("Cannot transfer if not owner")
	}
	// The new owner comes from the client, so it is only ever accepted when it
	// names a member of this lobby: member ids are minted by us (internal/uid)
	// and echoing back an arbitrary, possibly Steam shaped id would put one
	// into every LobbyInfo we serve.
	s.lobbies.mu.Lock()
	known := false
	for _, u := range l.users {
		if u.ID == a.UID {
			known = true
			break
		}
	}
	if known {
		l.owner = a.UID
	}
	s.lobbies.mu.Unlock()
	if !known {
		return wireErrf("Unknown user %s", a.UID)
	}
	s.lobbies.broadcast(l, p, "lobby/transfer", map[string]any{"id": l.id, "uid": a.UID})
	return nil
}

// lobbyMakeShortCode encodes our public endpoint into the join code. Guests
// only need the endpoint: the host's master resolves the code to the lobby.
func (s *Server) lobbyMakeShortCode(a lobbyArgs) (any, error) {
	if s.lobbies.get(a.ID) == nil {
		return nil, wireErrf("Unknown lobby %s", a.ID)
	}
	addr, err := s.publicAddr()
	if err != nil {
		return nil, fmt.Errorf("no public address: %w", err)
	}
	host, portStr, err := net.SplitHostPort(addr)
	if err != nil {
		return nil, err
	}
	port, err := strconv.ParseUint(portStr, 10, 16)
	if err != nil {
		return nil, err
	}
	ip := net.ParseIP(host)
	if ip == nil || ip.To4() == nil {
		return nil, fmt.Errorf("public address %q is not IPv4", addr)
	}
	short, err := code.Encode(code.Endpoint{IP: ip, Port: uint16(port)})
	if err != nil {
		return nil, err
	}
	s.lobbies.mu.Lock()
	s.lobbies.codes[short] = a.ID
	s.lobbies.mu.Unlock()
	s.opt.Log.Printf("master: join code for %s is %s (%s)", a.ID, short, addr)
	return map[string]any{"shortCode": short}, nil
}

// lobbyResolveShortCode is where a guest becomes a guest: it decodes the code,
// opens the proxy-link to that host and asks the host's master for the lobby.
func (s *Server) lobbyResolveShortCode(a lobbyArgs, args json.RawMessage, p Peer) (any, error) {
	if p.Remote() {
		s.lobbies.mu.Lock()
		id := s.lobbies.codes[a.ShortCode]
		s.lobbies.mu.Unlock()
		if id == "" {
			return nil, nil
		}
		return s.lobbyInfo(id)
	}

	// Our own code (host pasting its own code) resolves locally.
	s.lobbies.mu.Lock()
	id := s.lobbies.codes[a.ShortCode]
	s.lobbies.mu.Unlock()
	if id != "" {
		return s.lobbyInfo(id)
	}

	if !s.linked() {
		ep, err := code.Decode(a.ShortCode)
		if err != nil {
			return nil, wireErrf("Invalid join code")
		}
		if err := s.dialHost(ep.Addr(), p); err != nil {
			return nil, wireErrf("Cannot reach host %s: %v", ep.Addr(), err)
		}
	}
	raw, ok, err := s.forward("lobby/resolveShortCode", args)
	if !ok {
		return nil, wireErrf("Not connected to a host")
	}
	return raw, err
}

// dialHost opens the proxy-link and starts relaying the host's pushes to the
// local game.
func (s *Server) dialHost(addr string, p Peer) error {
	cl, err := link.Dial(addr, link.User{ID: p.UserID(), Name: p.Name()},
		func(cmd string, args json.RawMessage) {
			if sess := s.localSession(); sess != nil {
				sess.Push(cmd, args)
			}
		},
		func() {
			s.mu.Lock()
			s.client = nil
			s.mu.Unlock()
			s.opt.Log.Printf("master: proxy-link to the host closed")
		})
	if err != nil {
		return err
	}
	s.mu.Lock()
	s.client = cl
	s.mu.Unlock()
	s.opt.Log.Printf("master: proxy-link to host %s established", addr)
	return nil
}
