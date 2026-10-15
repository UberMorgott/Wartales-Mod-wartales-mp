package master

import (
	"context"
	"encoding/json"
	"net"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/punch"
)

// Puncher is the helper's UDP socket for join codes behind NAT (package
// punch): the host's code carries its address, a guest that cannot reach the
// host's TCP port punches through it, and a host enters a guest's reply code
// to punch back.
type Puncher interface {
	// Ready reports whether our UDP address is known (a code can carry it).
	Ready() bool
	// Endpoint asks STUN for our UDP address now.
	Endpoint() (punch.Result, error)
	// Dial returns the loopback address that leads to the host at addr.
	Dial(addr string) (string, error)
	// Toward punches toward a guest's address (a reply code).
	Toward(addr string) error
}

// tryRoute is cascade's attempt at one route.
type tryRoute func(what, relay string, dial func() (net.Conn, error), key uint32, probe time.Duration) (json.RawMessage, bool)

// punchCode issues the host's code with its UDP address, when STUN knows it.
// The TCP route is the one the endpoint resolver found (the default port on
// the UDP IP when it found none); on another IP there is no punch code.
func (s *Server) punchCode(tcp *code.Endpoint) (string, bool) {
	if s.opt.Punch == nil {
		return "", false
	}
	r, err := s.opt.Punch.Endpoint()
	if err != nil {
		s.opt.Log.Printf("master: join code without a punch address: %v", err)
		return "", false
	}
	ep := code.Endpoint{IP: r.Mapped.IP, Port: code.DefaultPort}
	if tcp != nil {
		if !tcp.IP.Equal(r.Mapped.IP) {
			// Two WAN addresses: the code has room for one, keep the TCP one.
			s.opt.Log.Printf("master: join code without a punch address: UDP leaves by %s, TCP is reached at %s", r.Mapped.IP, tcp.IP)
			return "", false
		}
		ep.Port = tcp.Port
	}
	short, err := code.EncodePunch(ep, uint16(r.Mapped.Port)) //nolint:gosec // a port
	if err != nil {
		return "", false
	}
	note := ""
	if r.Mapping == nat.MappingSymmetric {
		note = "; guests will likely need a reply code or a Steam invitation"
	}
	s.opt.Log.Printf("master: punch address %s (NAT mapping %s%s)", r.Mapped, r.Mapping, note)
	return short, true
}

// punchRoute is cascade's route for a code with a punch address: the host
// either answers within punch.AnswerWait and the link goes through the
// tunnel, or the player gets the reply code to send. relay for the link is
// the tunnel's loopback address, so the game's relay connection takes the
// same way (guestRelayAddr).
func (s *Server) punchRoute(c code.Code, try tryRoute) (json.RawMessage, error) {
	what := "punch " + c.Punch.Addr()
	if s.opt.Punch == nil {
		return nil, nil
	}
	fwd, err := s.opt.Punch.Dial(c.Punch.Addr())
	if err != nil {
		s.opt.Log.Printf("master: route %s failed: %v", what, err)
		return nil, wireErrf("%s", err.Error())
	}
	dial := func() (net.Conn, error) {
		d := net.Dialer{Timeout: DefaultDirectTimeout}
		return d.DialContext(context.Background(), "tcp", fwd)
	}
	// directProbeTimeout, not sdrCallTimeout: TCP 3 s + its probe 5 s + the
	// answer wait 5 s + this 5 s stays inside the game's 20 s command timeout.
	raw, ok := try(what, fwd, dial, 0, directProbeTimeout)
	if !ok {
		return nil, nil
	}
	return raw, nil
}

// punchReply handles a reply code the host entered in its join box.
func (s *Server) punchReply(r code.Endpoint) error {
	if s.opt.Punch == nil {
		return wireErrf("This is a reply code, but this helper has no UDP socket for it")
	}
	if err := s.opt.Punch.Toward(r.Addr()); err != nil {
		return wireErrf("Reply code: %v", err)
	}
	return wireErrf("Reply code accepted: this PC now opens the way to the player at %s for 2 minutes. "+
		"Host the game and ask them to join again.", r.Addr())
}
