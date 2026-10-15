package master

import (
	"encoding/json"
	"io"
	"log"
	"net"
	"strings"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/link"
)

// sdrAddrConn makes a pipe end look like an SDR bridge stream.
type sdrAddrConn struct{ net.Conn }

type sdrAddr struct{}

func (sdrAddr) Network() string          { return "sdr" }
func (sdrAddr) String() string           { return "steam:1" }
func (sdrAddrConn) RemoteAddr() net.Addr { return sdrAddr{} }

// linkPeer runs a real proxy-link over a pipe and returns the host's view of
// the guest.
func linkPeer(t *testing.T, name string, overSDR bool) *link.Peer {
	t.Helper()
	hostEnd, guestEnd := net.Pipe()
	var c net.Conn = hostEnd
	if overSDR {
		c = sdrAddrConn{hostEnd}
	}
	got := make(chan *link.Peer, 1)
	go link.Serve(c, nil, func(_ string, _ json.RawMessage, p *link.Peer) (any, error) {
		got <- p
		return nil, nil
	}, nil)
	cl, err := link.DialConn(guestEnd, link.User{ID: "X1", Name: name, Game: "S1"}, func(string, json.RawMessage) {}, func() {})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = cl.Close() })
	if _, err := cl.Call("ping", nil); err != nil {
		t.Fatal(err)
	}
	return <-got
}

// localPeer is the host's own game.
type localPeer struct{ game string }

func (p localPeer) UserID() string   { return "X0" }
func (p localPeer) SteamID() string  { return "" }
func (p localPeer) GameID() string   { return p.game }
func (p localPeer) Name() string     { return "host" }
func (p localPeer) Remote() bool     { return false }
func (p localPeer) Push(string, any) {}

func TestStartAnswerFollowsTheGuestsLinks(t *testing.T) {
	cases := []struct {
		name   string
		guests map[string]bool // name -> over SDR
		want   string          // serverID prefix
	}{
		{"alone", nil, "R127.0.0.1:14250:"},
		{"all direct (#7: LAN code join)", map[string]bool{"loftyrogue": false}, "R127.0.0.1:14250:"},
		{"all over SDR", map[string]bool{"a": true, "b": true}, "S5d6de506"},
		{"mixed", map[string]bool{"steamy": true, "coded": false}, "Игроки steamy вошли через Steam"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s := &Server{opt: Options{Log: log.New(io.Discard, "", 0), RelayPort: 14250, HostPW: "hpw", SlavePW: "spw", Transport: ModeAuto}}
			s.lobbies = newStore(s)
			host := localPeer{game: "S5d6de506"}
			l := &lobby{id: "L1D", owner: host.game, transport: TransportDirect}
			l.users = append(l.users, &member{ID: host.game, Name: "host", peer: host})
			for n, sdr := range c.guests {
				l.users = append(l.users, &member{ID: "S" + n, Name: n, peer: linkPeer(t, n, sdr)})
			}
			s.lobbies.lobbies[l.id], s.lobbies.inviteLobby = l, l.id
			got, _ := s.startAnswer()["serverID"].(string)
			if !strings.HasPrefix(got, c.want) {
				t.Fatalf("serverID = %q, want prefix %q", got, c.want)
			}
		})
	}
}
