// Package link implements the proxy-link: the guest's master forwards every
// lobby command to the host's master so that lobby state lives in one place.
//
// It is our own protocol, not the game's: newline delimited JSON using the
// same {uid, cmd, args} envelope as the master websocket, carried over the
// host's public TCP port (see relay.Server.dispatch for the multiplexing).
package link

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"sync"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// CallTimeout is shorter than the game's own 20s command timeout so that a
// dead host surfaces as an error rather than as a client side timeout.
const CallTimeout = 15 * time.Second

// Envelope is the wire frame, identical in shape to the master's JSON frame.
type Envelope struct {
	UID  int             `json:"uid"`
	Cmd  string          `json:"cmd,omitempty"`
	Args json.RawMessage `json:"args,omitempty"`
}

// User identifies the player behind a link.
type User struct {
	ID    string `json:"uid"`
	Name  string `json:"name"`
	Steam string `json:"steam,omitempty"` // the real Steam id, for a lobby on SDR
	Key   uint32 `json:"key,omitempty"`   // from the join code; a link over SDR must present it
}

// Handler answers a command forwarded by a guest.
type Handler func(cmd string, args json.RawMessage, peer *Peer) (any, error)

// Accept vets a guest's hello before any command is served; a non-nil error
// closes the link with that text.
type Accept func(u User) error

type conn struct {
	c  net.Conn
	mu sync.Mutex
}

func (w *conn) send(e Envelope) error {
	b, err := json.Marshal(e)
	if err != nil {
		return err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	_, err = w.c.Write(append(b, '\n'))
	return err
}

// ---------------------------------------------------------------- host side

// Peer is a guest as seen by the host's master.
type Peer struct {
	user User
	out  *conn

	mu      sync.Mutex
	pushUID int
}

// UserID returns the guest's minted Session id.
func (p *Peer) UserID() string { return p.user.ID }

// SteamID returns the guest's real Steam id, or "" when it reported none.
func (p *Peer) SteamID() string { return p.user.Steam }

// Name returns the guest's display name.
func (p *Peer) Name() string { return p.user.Name }

// Remote reports that this peer sits behind a proxy-link.
func (p *Peer) Remote() bool { return true }

// Push sends a server initiated command (a lobby event) to the guest, which
// relays it to its own game.
func (p *Peer) Push(cmd string, args any) {
	raw, err := json.Marshal(args)
	if err != nil {
		return
	}
	p.mu.Lock()
	p.pushUID++
	n := p.pushUID
	p.mu.Unlock()
	_ = p.out.send(Envelope{UID: n, Cmd: cmd, Args: raw})
}

// Serve runs one guest connection on the host side until it closes. accept
// may be nil.
func Serve(c net.Conn, accept Accept, h Handler, onClose func(*Peer)) {
	defer func() { _ = c.Close() }() // the guest is gone; a close error is moot
	br := bufio.NewReader(c)
	out := &conn{c: c}

	first, err := readEnvelope(br)
	if err != nil || first.Cmd != "link/hello" {
		return
	}
	var u User
	if err := json.Unmarshal(first.Args, &u); err != nil {
		return
	}
	if accept != nil {
		if err := accept(u); err != nil {
			raw, _ := json.Marshal(err.Error())
			_ = out.send(Envelope{UID: -first.UID, Cmd: "err", Args: raw})
			return
		}
	}
	// The guest announces itself, so its ids are not trusted as given: the
	// Session id must be one, or a Steam shaped id would travel into every
	// direct-relay LobbyInfo we serve and flip the lobby onto the Steam path;
	// and the Steam id must be well-formed, or an SDR lobby would hand the
	// host's game something it cannot turn into a SteamID (see internal/uid).
	u.ID = uid.Ensure(u.ID)
	if !uid.IsSteam(u.Steam) {
		u.Steam = ""
	}
	peer := &Peer{user: u, out: out}
	if onClose != nil {
		defer onClose(peer)
	}

	for {
		e, err := readEnvelope(br)
		if err != nil {
			return
		}
		if e.UID <= 0 {
			continue // a reply to one of our pushes; nothing to do
		}
		result, err := h(e.Cmd, e.Args, peer)
		if err != nil {
			raw, _ := json.Marshal(err.Error())
			_ = out.send(Envelope{UID: -e.UID, Cmd: "err", Args: raw})
			continue
		}
		raw, mErr := json.Marshal(result)
		if mErr != nil {
			raw = []byte("null")
		}
		_ = out.send(Envelope{UID: -e.UID, Args: raw})
	}
}

// --------------------------------------------------------------- guest side

// helloUID is the request id of link/hello, so a refusal comes back as its
// reply and never looks like a push.
const helloUID = 1

// Client is the guest's connection to the host's master.
type Client struct {
	out *conn

	mu      sync.Mutex
	uid     int
	pending map[int]chan Envelope
	closed  bool
	refused string // the host's answer to our hello, when it said no
}

// closedErr is the error a call gets on a closed link.
func (c *Client) closedErr() error {
	if c.refused != "" {
		return fmt.Errorf("link: host refused the connection: %s", c.refused)
	}
	return errors.New("link: connection closed")
}

// DialConn runs the guest side of a link over an already open connection
// (a TCP connection to the host's port, or a stream over the SDR bridge) and
// announces the local user. Pushes coming from the host are handed to onPush.
// The hello is sent with uid 1 so a refusal from the host comes back as a
// reply to it and reaches the caller instead of a later command.
func DialConn(c net.Conn, u User, onPush func(cmd string, args json.RawMessage), onClose func()) (*Client, error) {
	cl := &Client{out: &conn{c: c}, pending: map[int]chan Envelope{}, uid: helloUID}

	hello, _ := json.Marshal(u)
	if err := cl.out.send(Envelope{UID: helloUID, Cmd: "link/hello", Args: hello}); err != nil {
		_ = c.Close() // the hello already failed; report that error, not this one
		return nil, err
	}

	go func() {
		defer cl.shutdown()
		if onClose != nil {
			defer onClose()
		}
		br := bufio.NewReader(c)
		for {
			e, err := readEnvelope(br)
			if err != nil {
				return
			}
			if e.UID < 0 { // reply
				if -e.UID == helloUID && e.Cmd == "err" {
					// The host refused our hello: remember why, then end.
					var msg string
					if json.Unmarshal(e.Args, &msg) != nil {
						msg = string(e.Args)
					}
					cl.mu.Lock()
					cl.refused = msg
					cl.mu.Unlock()
					return
				}
				cl.mu.Lock()
				ch := cl.pending[-e.UID]
				delete(cl.pending, -e.UID)
				cl.mu.Unlock()
				if ch != nil {
					ch <- e
				}
				continue
			}
			if onPush != nil && e.Cmd != "" {
				onPush(e.Cmd, e.Args)
			}
			// answer the push so the host's side stays symmetric with the game
			_ = cl.out.send(Envelope{UID: -e.UID, Args: json.RawMessage("true")})
		}
	}()
	return cl, nil
}

// Call forwards a command to the host's master and waits for its reply.
func (c *Client) Call(cmd string, args json.RawMessage) (json.RawMessage, error) {
	ch := make(chan Envelope, 1)
	c.mu.Lock()
	if c.closed {
		err := c.closedErr()
		c.mu.Unlock()
		return nil, err
	}
	c.uid++
	n := c.uid
	c.pending[n] = ch
	c.mu.Unlock()

	if err := c.out.send(Envelope{UID: n, Cmd: cmd, Args: args}); err != nil {
		return nil, err
	}
	select {
	case e, ok := <-ch:
		if !ok {
			c.mu.Lock()
			err := c.closedErr()
			c.mu.Unlock()
			return nil, err
		}
		if e.Cmd == "err" {
			var msg string
			if json.Unmarshal(e.Args, &msg) != nil {
				msg = string(e.Args)
			}
			return nil, fmt.Errorf("%s", msg)
		}
		return e.Args, nil
	case <-time.After(CallTimeout):
		c.mu.Lock()
		delete(c.pending, n)
		c.mu.Unlock()
		return nil, errors.New("link: timeout")
	}
}

// Close tears the link down.
func (c *Client) Close() error {
	err := c.out.c.Close()
	c.shutdown()
	return err
}

func (c *Client) shutdown() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return
	}
	c.closed = true
	for n, ch := range c.pending {
		close(ch)
		delete(c.pending, n)
	}
}

func readEnvelope(br *bufio.Reader) (Envelope, error) {
	line, err := br.ReadBytes('\n')
	if err != nil {
		return Envelope{}, err
	}
	var e Envelope
	if err := json.Unmarshal(line, &e); err != nil {
		return Envelope{}, err
	}
	return e, nil
}
