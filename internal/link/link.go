// Package link implements the proxy-link: the guest's master forwards every
// lobby command to the host's master so that lobby state lives in one place.
//
// It is our own protocol, not the game's: newline delimited JSON using the
// same {uid, cmd, args} envelope as the master websocket, carried over the
// host's public TCP port (see relay.Server.dispatch for the multiplexing).
package link

import (
	"bufio"
	"context"
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
	ID   string `json:"uid"`
	Name string `json:"name"`
}

// Handler answers a command forwarded by a guest.
type Handler func(cmd string, args json.RawMessage, peer *Peer) (any, error)

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

// UserID returns the guest's platform id.
func (p *Peer) UserID() string { return p.user.ID }

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

// Serve runs one guest connection on the host side until it closes.
func Serve(c net.Conn, h Handler, onClose func(*Peer)) {
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
	// The guest announces itself, so its id is not trusted as given: a Steam
	// shaped id would travel into every LobbyInfo we serve and make the whole
	// lobby take the client's Steam only path (see internal/uid).
	u.ID = uid.Ensure(u.ID)
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

// Client is the guest's connection to the host's master.
type Client struct {
	out *conn

	mu      sync.Mutex
	uid     int
	pending map[int]chan Envelope
	closed  bool
}

// Dial connects to the host's public endpoint and announces the local user.
// Pushes coming from the host are handed to onPush.
func Dial(addr string, u User, onPush func(cmd string, args json.RawMessage), onClose func()) (*Client, error) {
	d := net.Dialer{Timeout: 10 * time.Second}
	c, err := d.DialContext(context.Background(), "tcp", addr)
	if err != nil {
		return nil, err
	}
	cl := &Client{out: &conn{c: c}, pending: map[int]chan Envelope{}}

	hello, _ := json.Marshal(u)
	if err := cl.out.send(Envelope{Cmd: "link/hello", Args: hello}); err != nil {
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
		c.mu.Unlock()
		return nil, errors.New("link: connection closed")
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
			return nil, errors.New("link: connection closed")
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
