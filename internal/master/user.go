package master

import (
	"crypto/md5" //nolint:gosec // not a security hash: makeSID needs a stable digest of the id, see makeSID
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net"
	"strconv"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// nowSeconds is the epoch clock the client syncs against (user/time, and the
// "time" field of the login replies).
func nowSeconds() float64 { return float64(time.Now().UnixMilli()) / 1000 }

// currentToken mirrors mpman.Api.getCurrentToken: 16 hour buckets.
func currentToken() int { return int(time.Now().UnixMilli()/57600000) & 0xFFFF }

// makeSID returns a Session platform id, "X" + 4 hex digits + signature, as
// mpman.UserID parses it (getPlatform@25195 case 'X').
//
// The real signature is scrambleToken(token, sign) over a server secret we do
// not have; a mismatch only makes the client drop its cached $SID and log in
// again, which we accept.
func makeSID(userID, name string) string {
	//nolint:gosec // md5 is only a deterministic identifier mapping here, not a security primitive
	sum := md5.Sum([]byte("wartales-mp" + userID + strconv.Itoa(currentToken()) + name))
	sign := base64.RawStdEncoding.EncodeToString(sum[:])
	return fmt.Sprintf("X%04x%s", currentToken(), sign)
}

type loginArgs struct {
	Name  string `json:"name"`
	Token string `json:"token"`
	UID   string `json:"uid"`
	SID   string `json:"sid"`
}

// adopt records who the local game says it is; the lobby code needs it.
//
// The game reports its Steam id here ("S<steamid>"). A lobby on the direct
// relay never puts it back on the wire (see internal/uid); a lobby on SDR
// emits exactly it, because the game derives the peer's SteamID from it.
func adopt(p Peer, a loginArgs) {
	sess, ok := p.(*session)
	if !ok {
		return
	}
	sess.mu.Lock()
	defer sess.mu.Unlock()
	if a.UID != "" {
		sess.game = a.UID
		sess.uid = uid.Mint(a.UID)
		if uid.IsSteam(a.UID) {
			sess.steam = a.UID // only ever emitted for a lobby on SDR
		}
	}
	if sess.uid == "" {
		var b [8]byte
		_, _ = rand.Read(b[:]) // crypto/rand.Read never returns an error
		sess.uid = uid.Mint(fmt.Sprintf("anonymous-%x", b))
	}
	if a.Name != "" {
		sess.name = a.Name
	}
	if sess.name == "" {
		sess.name = "Player"
	}
}

// userLogin answers SERVER-CONTRACT §3.1. "sid" must be non-null or the client
// reports "Login Failure".
func (s *Server) userLogin(args json.RawMessage, p Peer) (any, error) {
	var a loginArgs
	_ = json.Unmarshal(args, &a)
	adopt(p, a)
	s.opt.Log.Printf("master: login %s (%s)", p.Name(), p.UserID())
	return map[string]any{
		"sid":     makeSID(p.UserID(), p.Name()),
		"perm":    "",
		"time":    nowSeconds(),
		"version": 2,
	}, nil
}

// userSession answers SERVER-CONTRACT §3.2.
func (s *Server) userSession(args json.RawMessage, p Peer) (any, error) {
	var a loginArgs
	_ = json.Unmarshal(args, &a)
	adopt(p, a)
	return map[string]any{
		"accept":  true,
		"version": 2,
		"time":    nowSeconds(),
		"perm":    "",
	}, nil
}

// instanceGet answers SERVER-CONTRACT §2: the serverID decides the transport.
// "R<host>:<port>" makes the game's host speak RelayP2P and its guests
// WServer, both against our relay.
func (s *Server) instanceGet(args json.RawMessage, p Peer) (any, error) {
	if p.Remote() {
		// A guest asking the host's master: hand out our public endpoint.
		addr, err := s.publicAddr()
		if err != nil {
			return nil, fmt.Errorf("no public address: %w", err)
		}
		return s.instanceAnswer(addr), nil
	}
	if raw, ok, err := s.forward("instance/get", args); ok {
		// We are a guest: the host's master decides.
		if err != nil {
			return nil, err
		}
		return raw, nil
	}
	return s.instanceAnswer(net.JoinHostPort("127.0.0.1", strconv.Itoa(s.opt.RelayPort))), nil
}

func (s *Server) instanceAnswer(addr string) map[string]any {
	return map[string]any{
		"serverID": "R" + addr,
		"serverStartAnswer": map[string]any{
			"hostpw":  s.opt.HostPW,
			"slavepw": s.opt.SlavePW,
		},
	}
}

func (s *Server) publicAddr() (string, error) {
	if s.opt.PublicAddr == nil {
		return "", fmt.Errorf("no public address resolver")
	}
	return s.opt.PublicAddr()
}
