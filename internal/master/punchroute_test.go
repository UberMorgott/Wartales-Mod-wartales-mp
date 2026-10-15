package master

import (
	"encoding/json"
	"errors"
	"io"
	"log"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/punch"
)

type fakePuncher struct {
	mapped  *net.UDPAddr
	dialErr error
	fwd     string
	dialed  []string
	toward  []string
}

func (f *fakePuncher) Ready() bool { return f.mapped != nil }
func (f *fakePuncher) Endpoint() (punch.Result, error) {
	if f.mapped == nil {
		return punch.Result{}, errors.New("no STUN server answered")
	}
	return punch.Result{Mapped: f.mapped, Mapping: nat.MappingStable}, nil
}
func (f *fakePuncher) Dial(addr string) (string, error) {
	f.dialed = append(f.dialed, addr)
	return f.fwd, f.dialErr
}
func (f *fakePuncher) Toward(addr string) error {
	f.toward = append(f.toward, addr)
	return nil
}

// punchPeer is the local game.
type punchPeer struct{}

func (punchPeer) UserID() string   { return "X1" }
func (punchPeer) SteamID() string  { return "" }
func (punchPeer) GameID() string   { return "S1" }
func (punchPeer) Name() string     { return "Host" }
func (punchPeer) Remote() bool     { return false }
func (punchPeer) Push(string, any) {}

func punchServer(p Puncher) *Server {
	return New(Options{Log: log.New(io.Discard, "", 0), Punch: p})
}

func TestPunchCodeCarriesUDPAddress(t *testing.T) {
	f := &fakePuncher{mapped: &net.UDPAddr{IP: net.IPv4(45, 154, 88, 66), Port: 8104}}
	s := punchServer(f)
	short, ok := s.punchCode(&code.Endpoint{IP: net.IPv4(45, 154, 88, 66), Port: 14251})
	if !ok {
		t.Fatal("no punch code")
	}
	c, err := code.DecodeAny(short)
	if err != nil || c.Punch == nil || c.Endpoint.Addr() != "45.154.88.66:14251" || c.Punch.Addr() != "45.154.88.66:8104" {
		t.Fatalf("code %q = %+v, %v", short, c, err)
	}
	// A TCP endpoint on another IP is kept: no punch code mixes the two.
	if _, ok := s.punchCode(&code.Endpoint{IP: net.IPv4(198, 51, 100, 2), Port: 14251}); ok {
		t.Fatal("punch code across two WAN addresses")
	}
	// No TCP endpoint at all: the default port on the UDP IP.
	short, _ = s.punchCode(nil)
	if c, _ := code.DecodeAny(short); c.Endpoint.Addr() != "45.154.88.66:14250" {
		t.Fatalf("no TCP endpoint: %s", c.Endpoint)
	}
	if _, ok := punchServer(&fakePuncher{}).punchCode(nil); ok {
		t.Fatal("a punch code without a STUN answer")
	}
}

func TestPunchReplyCodeOnHost(t *testing.T) {
	f := &fakePuncher{}
	s := punchServer(f)
	reply, _ := code.EncodeReply(code.Endpoint{IP: net.IPv4(100, 64, 7, 9), Port: 50123})
	args, _ := json.Marshal(map[string]any{"shortCode": reply})
	var a lobbyArgs
	_ = json.Unmarshal(args, &a)
	_, err := s.lobbyResolveShortCode(a, args, punchPeer{})
	var we wireErr
	if !errors.As(err, &we) || !strings.Contains(we.msg, "100.64.7.9:50123") {
		t.Fatalf("reply code answered %v", err)
	}
	if len(f.toward) != 1 || f.toward[0] != "100.64.7.9:50123" {
		t.Fatalf("Toward calls %v", f.toward)
	}
}

func TestPunchRouteFailureIsTheReplyText(t *testing.T) {
	f := &fakePuncher{dialErr: &punch.NoAnswerError{Host: "45.154.88.66:8104", Reply: "81ZG0001TMVGJ"}}
	s := punchServer(f)
	c := code.Code{Endpoint: &code.Endpoint{IP: net.IPv4(45, 154, 88, 66), Port: 14250}, Punch: &code.Endpoint{IP: net.IPv4(45, 154, 88, 66), Port: 8104}}
	try := func(string, string, func() (net.Conn, error), uint32, time.Duration) (json.RawMessage, bool) {
		t.Fatal("no link may be tried without an answer")
		return nil, false
	}
	_, err := s.punchRoute(c, try)
	if err == nil || !strings.Contains(err.Error(), "81ZG0001TMVGJ") {
		t.Fatalf("punchRoute error %v", err)
	}

	// An answer: the link is tried over the forwarder, which is also the
	// relay address the game is pointed at.
	f.dialErr, f.fwd = nil, "127.0.0.1:50000"
	var relay string
	try = func(_, r string, _ func() (net.Conn, error), _ uint32, _ time.Duration) (json.RawMessage, bool) {
		relay = r
		return json.RawMessage(`{"id":"L1D"}`), true
	}
	if raw, err := s.punchRoute(c, try); err != nil || raw == nil || relay != f.fwd {
		t.Fatalf("punchRoute = %s, %v; relay %q", raw, err, relay)
	}
}

// No SDR and no reachable TCP endpoint: a punch address still makes the
// lobby direct instead of refusing it.
func TestChooseTransportPunch(t *testing.T) {
	lan := nat.Endpoint{Addr: "192.168.1.5:14250", IP: net.IPv4(192, 168, 1, 5), Source: "LAN"}
	bad := SDRStatus{Known: true, Reason: "no Steam"}
	if _, _, err := chooseTransport(ModeAuto, lan, nil, bad, false); err == nil {
		t.Fatal("without punch the lobby must be refused")
	}
	got, reason, err := chooseTransport(ModeAuto, lan, nil, bad, true)
	if err != nil || got != TransportDirect || !strings.Contains(reason, "punch") {
		t.Fatalf("with punch: %v, %q, %v", got, reason, err)
	}
}
