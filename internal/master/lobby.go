package master

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"maps"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/applog"
	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/link"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/uid"
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
	transport  Transport // decided once, at creation; fixes how member ids are rendered
}

// idOf is the id a member is known by inside this lobby. A direct-relay lobby
// uses the minted Session id; an SDR lobby uses the player's real Steam id,
// which is what makes Lobby.isSteamOnly true on every game and what the game
// turns back into the peer's SteamID. A player without a usable Steam id
// keeps the Session id even in an SDR lobby: that flips the whole lobby back
// onto instance/get (our direct relay), which is logged at join time rather
// than hidden behind an id the game could not resolve.
func (l *lobby) idOf(p Peer) string {
	if l.transport == TransportSDR && p.SteamID() != "" {
		return p.SteamID()
	}
	return p.UserID()
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
	exceptID := ""
	if except != nil {
		exceptID = l.idOf(except)
	}
	for _, u := range l.users {
		if u.peer == nil || (except != nil && u.ID == exceptID) {
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
	// What is left of each lobby is snapshotted under the lock: another
	// member may be leaving at the same moment.
	type left struct {
		l      *lobby
		id     string // the leaver, as this lobby knew it
		owner  string
		remain int
	}
	s.mu.Lock()
	var affected []left
	for _, l := range s.lobbies {
		id := l.idOf(p)
		for i, u := range l.users {
			if u.ID == id {
				l.users = append(l.users[:i], l.users[i+1:]...)
				if len(l.users) == 0 {
					delete(s.lobbies, l.id)
					break
				}
				if l.owner == id {
					l.owner = l.users[0].ID
				}
				affected = append(affected, left{l: l, id: id, owner: l.owner, remain: len(l.users)})
				break
			}
		}
	}
	s.mu.Unlock()

	for _, a := range affected {
		if s.srv != nil {
			s.srv.opt.Log.Printf("master: %s left lobby %s, %d member(s) remain, owner %s",
				a.id, a.l.id, a.remain, a.owner)
		}
		s.broadcast(a.l, p, "lobby/leave", map[string]any{"id": a.l.id, "uid": a.id})
		if a.owner != a.id {
			s.broadcast(a.l, p, "lobby/transfer", map[string]any{"id": a.l.id, "uid": a.owner})
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
			if err == nil && cmd == "lobby/join" {
				s.logHostTransport(raw)
			}
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
		return s.lobbyInfoInvite(a, args, p)
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
		return s.lobbyMakeShortCode(a, p)
	case "lobby/initInvite":
		return s.lobbyInitInvite(a, p)
	case "lobby/resolveShortCode":
		return s.lobbyResolveShortCode(a, args, p)
	}
	return nil, wireErrf("Unknown command %s", cmd)
}

// logHostTransport is the guest's view of the host's verdict: a lobby whose
// owner id is Steam shaped is on SDR, anything else is on the host's direct
// relay. The verdict itself travels inside the ids the host's master renders,
// so both masters agree by construction; this only makes it visible.
func (s *Server) logHostTransport(raw json.RawMessage) {
	var info struct {
		ID    string `json:"id"`
		Owner string `json:"owner"`
	}
	if json.Unmarshal(raw, &info) != nil || info.ID == "" {
		return
	}
	t := TransportDirect
	if uid.IsSteam(info.Owner) {
		t = TransportSDR
	}
	s.opt.Log.Printf("master: joined lobby %s; the host's master chose %s (owner id %s)", info.ID, t, info.Owner)
}

// decideTransport runs the choice for a new lobby, with whatever the helper
// knows right now.
func (s *Server) decideTransport() (Transport, string, error) {
	var ep nat.Endpoint
	epErr := errors.New("no endpoint resolver")
	if s.opt.Endpoint != nil {
		ep, epErr = s.opt.Endpoint()
	}
	var sdr SDRStatus
	if s.opt.SDRStatus != nil {
		sdr = s.opt.SDRStatus()
	}
	return chooseTransport(s.opt.Transport, ep, epErr, sdr)
}

// lobbyCreate replies with the new lobby id only; the client builds the
// LobbyInfo itself. The transport is decided here, once per lobby, and fixes
// how every member id of this lobby is rendered from now on.
func (s *Server) lobbyCreate(a lobbyArgs, p Peer) (any, error) {
	transport, reason, err := s.decideTransport()
	if err != nil {
		s.opt.Log.Printf("master: lobby creation by %s (%s) REFUSED: %v", p.Name(), p.UserID(), err)
		return nil, wireErrf("Cannot host: %v", err)
	}
	l := &lobby{id: newLobbyID(), data: map[string]json.RawMessage{}, transport: transport}
	l.owner = l.idOf(p)
	if a.Props != nil {
		if a.Props.Data != nil {
			l.data = a.Props.Data
		}
		l.isPrivate = a.Props.IsPrivate
		l.maxPlayers = a.Props.MaxPlayers
		l.users = append(l.users, &member{ID: l.owner, Name: p.Name(), Data: a.Props.MyData, peer: p})
	} else {
		l.users = append(l.users, &member{ID: l.owner, Name: p.Name(), peer: p})
	}
	s.lobbies.mu.Lock()
	s.lobbies.lobbies[l.id] = l
	s.lobbies.mu.Unlock()
	s.opt.Log.Printf("master: lobby %s created by %s (%s), owner %s, props %s",
		l.id, p.Name(), l.owner, l.owner, applog.Trunc(a.Props))
	s.opt.Log.Printf("master: lobby %s game transport: %s (%s)", l.id, transport, reason)
	if transport == TransportSDR && p.SteamID() == "" {
		s.opt.Log.Printf("master: WARNING: lobby %s is on SDR but the host reported no Steam id; "+
			"the game will ask instance/get and use the direct relay instead", l.id)
	}
	s.opt.Log.Printf("master: lobby %s lobby-phase transport is lobby/chat on this connection "+
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
	id := l.idOf(p)
	found := false
	for _, u := range l.users {
		if u.ID == id {
			u.Data, u.peer, u.Name, found = a.Data, p, p.Name(), true
			break
		}
	}
	if !found {
		l.users = append(l.users, &member{ID: id, Name: p.Name(), Data: a.Data, peer: p})
	}
	info := l.info()
	transport := l.transport
	s.lobbies.mu.Unlock()

	if transport == TransportSDR && p.SteamID() == "" {
		s.opt.Log.Printf("master: WARNING: %s joined SDR lobby %s without a Steam id (as %s); "+
			"the lobby is no longer Steam-only and the game will fall back to instance/get (direct relay)",
			p.Name(), l.id, id)
	}
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
			"id": l.id, "uid": id, "name": p.Name(), "data": data,
		})
		s.opt.Log.Printf("master: %s (%s) joined lobby %s (%s), told %d peer(s)",
			p.Name(), id, l.id, transport, n)
	} else {
		s.opt.Log.Printf("master: %s (%s) rejoined lobby %s", p.Name(), id, l.id)
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
	if l.owner != l.idOf(p) {
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
	id := l.idOf(p)
	s.lobbies.mu.Lock()
	for _, u := range l.users {
		if u.ID == id {
			u.Data = a.Data
		}
	}
	s.lobbies.mu.Unlock()
	s.lobbies.broadcast(l, p, "lobby/setUserData", map[string]any{"id": l.id, "uid": id, "data": a.Data})
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
	id := l.idOf(p)
	n := s.lobbies.broadcast(l, p, "lobby/chat",
		map[string]any{"id": l.id, "uid": id, "msg": a.Msg})
	s.lobbies.transport(s.opt.Log, l.id, id, len(a.Msg), n)
	return nil
}

func (s *Server) lobbyTransfer(a lobbyArgs, p Peer) error {
	l := s.lobbies.get(a.ID)
	if l == nil {
		return wireErrf("Unknown lobby %s", a.ID)
	}
	if l.owner != l.idOf(p) {
		return wireErrf("Cannot transfer if not owner")
	}
	// The new owner comes from the client, so it is only ever accepted when it
	// names a member of this lobby: member ids are rendered by us (idOf) and
	// echoing back an arbitrary id would put one the lobby's transport does
	// not expect into every LobbyInfo we serve.
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

// lobbyMakeShortCode issues the join code with every route we can offer at
// this moment, so the guest can cascade: the endpoint (direct, tried first)
// whenever we have one, verified or not, and the SDR route (our Steam
// account + the link key) whenever the bridge is up. The endpoint is
// re-resolved for each code (nat.Mapper.MaxAge): the WAN address can change
// between lobbies. The host's master resolves the code to the lobby either
// way.
func (s *Server) lobbyMakeShortCode(a lobbyArgs, p Peer) (any, error) {
	short, err := s.issueCode(a.ID, p)
	if err != nil {
		return nil, err
	}
	return map[string]any{"shortCode": short}, nil
}

// lobbyInitInvite is the Steam "invite friends" path. The game asks for it
// once per lobby, before it creates a friends-only Steam lobby, and writes
// the string we answer into that lobby's "invite" data. A friend who clicks
// "Join Game" hands the same string to their own helper as lobby/infoInvite,
// so it must reach our lobby on its own: it is the join code.
func (s *Server) lobbyInitInvite(a lobbyArgs, p Peer) (any, error) {
	short, err := s.issueCode(a.ID, p)
	if err != nil {
		return nil, err
	}
	s.opt.Log.Printf("master: Steam invite for %s carries the join code %s", a.ID, short)
	return short, nil
}

// issueCode issues the join code for a lobby and remembers it. The SDR route
// is always the owner's: a guest asking for the code (inviting its own Steam
// friends) must still send them to the host.
func (s *Server) issueCode(id string, p Peer) (string, error) {
	l := s.lobbies.get(id)
	if l == nil {
		return "", wireErrf("Unknown lobby %s", id)
	}
	s.lobbies.mu.Lock()
	for _, u := range l.users {
		if u.ID == l.owner && u.peer != nil {
			p = u.peer
		}
	}
	s.lobbies.mu.Unlock()
	ep, epErr := s.endpointRoute()
	st, sdrWhy := s.steamRoute(p)

	var short string
	var err error
	switch {
	case ep != nil && st != nil:
		short, err = code.EncodeCombined(*ep, *st)
	case st != nil:
		short = code.EncodeSteam(*st)
	case ep != nil:
		// No SDR route. A pending bridge on a lobby that needs it is a
		// "not yet", not a code that could not work.
		if l.transport == TransportSDR && s.opt.Bridge != nil {
			if ready, why := s.opt.Bridge.Ready(); !ready && !s.opt.Bridge.Status().Known {
				s.opt.Log.Printf("master: join code for %s not issued yet: %s", l.id, why)
				return "", wireErrf("Steam relay not ready yet (%s); ask for the code again in a few seconds", why)
			}
		}
		short, err = code.Encode(*ep)
	default:
		s.opt.Log.Printf("master: join code for %s cannot be issued: endpoint: %v; SDR: %s", l.id, epErr, sdrWhy)
		if s.opt.Bridge != nil && !s.opt.Bridge.Status().Known {
			return "", wireErrf("Steam relay not ready yet (%s); ask for the code again in a few seconds", sdrWhy)
		}
		return "", wireErrf("No route to offer: %v; %s", epErr, sdrWhy)
	}
	if err != nil {
		return "", err
	}
	s.lobbies.mu.Lock()
	s.lobbies.codes[short] = l.id
	s.lobbies.mu.Unlock()
	routes := ""
	if ep != nil {
		routes += fmt.Sprintf("direct %s", ep.Addr())
		if v, _ := s.endpointVerified(); !v {
			routes += " (unverified)"
		}
	}
	if st != nil {
		if routes != "" {
			routes += ", then "
		}
		routes += fmt.Sprintf("SDR to SteamID %d", st.SteamID64())
	} else {
		routes += "; no SDR route: " + sdrWhy
	}
	s.opt.Log.Printf("master: join code for %s is %s (routes: %s)", l.id, short, routes)
	return short, nil
}

// lobbyInfoInvite serves a Steam "Join Game": the value is what
// lobby/initInvite answered on the host, so it resolves like a typed join
// code and the game's lobby/join that follows is forwarded to the host as
// usual. A bare lobby id, which is what the vanilla master used, still works
// locally.
func (s *Server) lobbyInfoInvite(a lobbyArgs, args json.RawMessage, p Peer) (any, error) {
	if s.lobbies.get(a.Invite) != nil {
		return s.lobbyInfo(a.Invite)
	}
	if p.Remote() {
		s.lobbies.mu.Lock()
		id := s.lobbies.codes[a.Invite]
		s.lobbies.mu.Unlock()
		if id == "" {
			return nil, nil
		}
		return s.lobbyInfo(id)
	}
	resolve, err := json.Marshal(map[string]any{"shortCode": a.Invite, "filters": map[string]any{}})
	if err != nil {
		return nil, err
	}
	var b lobbyArgs
	_ = json.Unmarshal(resolve, &b)
	return s.lobbyResolveShortCode(b, resolve, p)
}

// endpointRoute is our endpoint as a code route, if we have an IPv4 one.
func (s *Server) endpointRoute() (*code.Endpoint, error) {
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
	return &code.Endpoint{IP: ip, Port: uint16(port)}, nil
}

// endpointVerified reports whether inbound from the internet has been seen.
func (s *Server) endpointVerified() (bool, nat.Endpoint) {
	if s.opt.Endpoint == nil {
		return false, nat.Endpoint{}
	}
	ep, err := s.opt.Endpoint()
	return err == nil && ep.Verified, ep
}

// steamRoute is our SDR route, if the game gave us a Steam id and the bridge
// is up; otherwise the reason.
func (s *Server) steamRoute(p Peer) (*code.Steam, string) {
	id64, ok := uid.SteamID64(p.SteamID())
	if !ok {
		return nil, "the game reported no Steam id"
	}
	account, ok := code.AccountID(id64)
	if !ok {
		return nil, fmt.Sprintf("%d is not a player account", id64)
	}
	if s.opt.Bridge == nil {
		return nil, "this helper has no SDR bridge"
	}
	if ready, why := s.opt.Bridge.Ready(); !ready {
		return nil, why
	}
	return &code.Steam{AccountID: account, Key: s.opt.LinkKey}, ""
}

// DefaultDirectTimeout bounds the guest's attempt to connect to the host's
// endpoint before it falls back to SDR: a refused port fails at once, a
// silently dropped SYN (the common firewall case) must not keep a player
// waiting. 3 s is one retransmit past the first SYN on Windows, well inside
// the game's own 20 s command timeout with the SDR attempt still to come.
const DefaultDirectTimeout = 3 * time.Second

// directProbeTimeout bounds the first command over a freshly connected
// direct link: the host answered TCP, so anything slower than this is a
// wrong host or a dead one, and SDR is the better bet.
const directProbeTimeout = 5 * time.Second

// sdrCallTimeout bounds commands over the SDR link, so that a dead host
// surfaces before the game's own 20 s timeout even after a direct attempt.
const sdrCallTimeout = 8 * time.Second

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

	if s.linked() {
		raw, _, err := s.forward("lobby/resolveShortCode", args)
		return raw, err
	}
	c, err := code.DecodeAny(a.ShortCode)
	if err != nil {
		return nil, wireErrf("Invalid join code")
	}
	return s.cascade(c, args, p)
}

// cascade tries the code's routes in order: direct first (bounded by
// DefaultDirectTimeout, then one probe command), SDR second. A route counts
// only when the host's master actually answered lobby/resolveShortCode over
// it; a refused hello, a dead port or a timeout falls through to the next,
// and every outcome is logged with its reason. The guest sees nothing of it
// but a moment's delay.
func (s *Server) cascade(c code.Code, args json.RawMessage, p Peer) (any, error) {
	var failures []string
	try := func(what string, dial func() (net.Conn, error), key uint32, probe time.Duration) (json.RawMessage, bool) {
		conn, err := dial()
		if err != nil {
			s.opt.Log.Printf("master: route %s failed: %v", what, err)
			failures = append(failures, what+": "+err.Error())
			return nil, false
		}
		cl, err := s.attachLink(conn, link.User{ID: p.UserID(), Name: p.Name(), Steam: p.SteamID(), Key: key}, what, probe)
		if err != nil {
			s.opt.Log.Printf("master: route %s failed: %v", what, err)
			failures = append(failures, what+": "+err.Error())
			return nil, false
		}
		raw, err := cl.Call("lobby/resolveShortCode", args)
		if err != nil {
			// Connected, but not to a master that takes this code: a stale
			// address now owned by someone else, a refused key, or a dead host.
			s.opt.Log.Printf("master: route %s answered the probe with an error, dropping it: %v", what, err)
			failures = append(failures, what+": "+err.Error())
			s.dropLink(cl)
			return nil, false
		}
		s.opt.Log.Printf("master: route %s WORKS; joining over it", what)
		return raw, true
	}

	if c.Endpoint != nil {
		addr := c.Endpoint.Addr()
		var key uint32
		if c.Steam != nil {
			key = c.Steam.Key
		}
		if raw, ok := try("direct "+addr, func() (net.Conn, error) { return s.dialDirect(addr) }, key, directProbeTimeout); ok {
			return raw, nil
		}
		if c.Steam != nil {
			s.opt.Log.Printf("master: falling back to SDR after the direct route to %s failed", addr)
		}
	}
	if c.Steam != nil {
		target := c.Steam.SteamID64()
		what := fmt.Sprintf("SDR to SteamID %d", target)
		if s.opt.Bridge == nil {
			failures = append(failures, what+": this helper has no SDR bridge")
		} else if ready, why := s.opt.Bridge.Ready(); !ready {
			s.opt.Log.Printf("master: route %s deferred: %s", what, why)
			if len(failures) == 0 {
				return nil, wireErrf("Steam relay not ready yet (%s); try again in a few seconds", why)
			}
			failures = append(failures, what+": not ready ("+why+")")
		} else if raw, ok := try(what, func() (net.Conn, error) { return s.opt.Bridge.Dial(target) }, c.Steam.Key, sdrCallTimeout); ok {
			return raw, nil
		}
	}
	return nil, wireErrf("Cannot reach the host: %s", strings.Join(failures, "; "))
}

// dialDirect connects to the host's endpoint within DefaultDirectTimeout
// (Options.DirectTimeout), through Options.DialDirect when a test supplies one.
func (s *Server) dialDirect(addr string) (net.Conn, error) {
	timeout := s.opt.DirectTimeout
	if timeout <= 0 {
		timeout = DefaultDirectTimeout
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	if s.opt.DialDirect != nil {
		return s.opt.DialDirect(ctx, addr)
	}
	var d net.Dialer
	return d.DialContext(ctx, "tcp", addr)
}

// attachLink runs the guest side of a link over conn and makes it the
// master's uplink. probe bounds each command over it.
func (s *Server) attachLink(c net.Conn, u link.User, what string, probe time.Duration) (*link.Client, error) {
	cl, err := link.DialConn(c, u,
		func(cmd string, args json.RawMessage) {
			if sess := s.localSession(); sess != nil {
				sess.Push(cmd, args)
			}
		},
		func() {
			s.mu.Lock()
			s.client = nil
			s.mu.Unlock()
			s.opt.Log.Printf("master: %s closed", what)
		})
	if err != nil {
		return nil, err
	}
	cl.CallTimeout = probe
	s.mu.Lock()
	s.client = cl
	s.mu.Unlock()
	s.opt.Log.Printf("master: %s established", what)
	return cl, nil
}

// dropLink closes a link that turned out not to lead to the host.
func (s *Server) dropLink(cl *link.Client) {
	s.mu.Lock()
	if s.client == cl {
		s.client = nil
	}
	s.mu.Unlock()
	_ = cl.Close() // it is being discarded; its close error is moot
}
