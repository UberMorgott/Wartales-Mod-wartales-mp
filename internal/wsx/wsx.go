// Package wsx is a minimal RFC6455 server, matching mpman's WSConnection
// (see decomp/mpman/WSConnection.hx.txt).
//
// Deliberately hand rolled: the game's handshake is not quite standard. It
// authenticates with the extra headers X-Ident / X-Pass (and accepts X-Hash in
// place of Sec-WebSocket-Key), never negotiates a subprotocol and never sends
// fragmented frames.
package wsx

import (
	"bufio"
	"crypto/sha1" //nolint:gosec // RFC6455 Sec-WebSocket-Accept and the game's X-Pass are sha1
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/sendq"
)

// Opcodes we care about.
const (
	OpText   = 1
	OpBinary = 2
	OpClose  = 8
	OpPing   = 9
	OpPong   = 10
)

// MaxFrame mirrors WSConnection.MAX_BUFFER_SIZE order of magnitude; it only
// guards us against a malicious peer.
const MaxFrame = 64 << 20

const wsGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

// HandshakeTimeout bounds the upgrade request of a peer that has not
// authenticated yet; the game sends its request at once.
const HandshakeTimeout = 10 * time.Second

// maxRequest bounds the upgrade request; the game's is a few hundred bytes.
const maxRequest = 16 << 10

// AuthFunc is the server side of WSConnection.onAuth: given the X-Ident value
// it returns the password the client must have used, or ok=false to refuse.
// Returning an empty password with ok=true skips the X-Pass check.
type AuthFunc func(ident string) (pass string, ok bool)

// Conn is an accepted websocket connection.
type Conn struct {
	net.Conn
	Ident   string
	Headers map[string]string

	br *bufio.Reader
	q  *sendq.Queue // every write goes through it, so none waits on the peer

	idle time.Duration // see KeepAlive; 0 = Read waits forever
}

// sendBudget is how far a peer may fall behind before it is dropped: room for
// a frame of the largest size queued behind another.
const sendBudget = 2 * (MaxFrame + 10)

// Accept performs the server side handshake on an already accepted TCP (or TLS)
// connection. br must be the reader the caller has been peeking with, or nil.
//
// The peer is not authenticated yet, so the request must arrive whole within
// HandshakeTimeout and maxRequest bytes.
func Accept(c net.Conn, br *bufio.Reader, auth AuthFunc) (*Conn, error) {
	if br == nil {
		br = bufio.NewReader(c)
	}
	if err := c.SetDeadline(time.Now().Add(HandshakeTimeout)); err != nil {
		return nil, err
	}
	headers, err := readRequest(br)
	if err != nil {
		return nil, err
	}

	hash := headers["x-hash"]
	if hash == "" {
		hash = headers["sec-websocket-key"]
	}
	if hash == "" {
		return nil, errors.New("websocket: no Sec-WebSocket-Key")
	}
	ident := headers["x-ident"]

	if auth != nil {
		pass, ok := auth(ident)
		if !ok {
			return nil, fmt.Errorf("websocket: auth refused for ident %q", ident)
		}
		if pass != "" && !checkPass(hash, pass, headers["x-pass"]) {
			return nil, fmt.Errorf("websocket: bad password for ident %q", ident)
		}
	}

	//nolint:gosec // sha1 is mandated by RFC6455 for Sec-WebSocket-Accept
	sum := sha1.Sum([]byte(hash + wsGUID))
	resp := "HTTP/1.1 101 Switching Protocols\r\n" +
		"Upgrade: websocket\r\n" +
		"Connection: Upgrade\r\n" +
		"Sec-WebSocket-Accept: " + base64.StdEncoding.EncodeToString(sum[:]) + "\r\n\r\n"
	if _, err := c.Write([]byte(resp)); err != nil {
		return nil, err
	}
	if err := c.SetDeadline(time.Time{}); err != nil {
		return nil, err
	}
	return &Conn{Conn: c, Ident: ident, Headers: headers, br: br, q: sendq.New(c, sendBudget)}, nil
}

// checkPass reproduces WSConnection.handleRequest: the client sends
// X-Pass = Sha1.encode(hex(base64decode(hash)) + password), lowercase hex.
// encode@18770 is haxe.crypto.Sha1.encode (Sha1.hx:27-29: digest + toHex);
// an md5 here refused every host login with "bad password".
func checkPass(hash, pass, got string) bool {
	if got == "" {
		return false
	}
	key, err := base64.StdEncoding.DecodeString(hash)
	if err != nil {
		return false
	}
	//nolint:gosec // sha1 is what the game's WSConnection computes for X-Pass
	sum := sha1.Sum([]byte(hex.EncodeToString(key) + pass))
	return got == hex.EncodeToString(sum[:])
}

func readRequest(br *bufio.Reader) (map[string]string, error) {
	headers := map[string]string{}
	budget := maxRequest
	for i := 0; ; i++ {
		line, err := readLine(br, &budget)
		if err != nil {
			return nil, err
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			return headers, nil
		}
		if i == 0 { // request line
			continue
		}
		p := strings.Index(line, ":")
		if p < 0 {
			continue
		}
		key := strings.ToLower(strings.TrimSpace(line[:p]))
		headers[key] = strings.TrimSpace(line[p+1:])
		if len(headers) > 64 {
			return nil, errors.New("websocket: too many headers")
		}
	}
}

// readLine reads one request line, charging it to budget.
func readLine(br *bufio.Reader, budget *int) (string, error) {
	var line []byte
	for {
		frag, err := br.ReadSlice('\n')
		*budget -= len(frag)
		if *budget < 0 {
			return "", errors.New("websocket: request too large")
		}
		line = append(line, frag...)
		if err == nil {
			return string(line), nil
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			return "", err
		}
	}
}

// KeepAlive pings the peer every interval and makes Read fail once nothing at
// all (data, pong) has arrived for idle: an open socket alone does not prove
// the program behind it still runs. Call it before the first Read; stop ends
// the pings.
func (c *Conn) KeepAlive(interval, idle time.Duration) (stop func()) {
	c.idle = idle
	done := make(chan struct{})
	go func() {
		t := time.NewTicker(interval)
		defer t.Stop()
		for {
			select {
			case <-done:
				return
			case <-t.C:
				if c.Write(OpPing, nil) != nil {
					return
				}
			}
		}
	}()
	var once sync.Once
	return func() { once.Do(func() { close(done) }) }
}

// Read returns the next data frame. Control frames are handled internally.
func (c *Conn) Read() (opcode byte, payload []byte, err error) {
	for {
		if c.idle > 0 {
			if err := c.SetReadDeadline(time.Now().Add(c.idle)); err != nil {
				return 0, nil, err
			}
		}
		var h [2]byte
		if _, err := io.ReadFull(c.br, h[:]); err != nil {
			return 0, nil, err
		}
		fin := h[0]&0x80 != 0
		rsv := h[0] >> 4 & 7
		op := h[0] & 15
		masked := h[1]&0x80 != 0
		n := uint64(h[1] & 127)
		if rsv != 0 {
			return 0, nil, errors.New("websocket: RSV bits set")
		}
		if !fin {
			return 0, nil, errors.New("websocket: fragmented frames are not supported")
		}
		switch n {
		case 126:
			var b [2]byte
			if _, err := io.ReadFull(c.br, b[:]); err != nil {
				return 0, nil, err
			}
			n = uint64(binary.BigEndian.Uint16(b[:]))
		case 127:
			var b [8]byte
			if _, err := io.ReadFull(c.br, b[:]); err != nil {
				return 0, nil, err
			}
			n = binary.BigEndian.Uint64(b[:])
		}
		if n > MaxFrame {
			return 0, nil, errors.New("websocket: frame too large")
		}
		var mask [4]byte
		if masked {
			if _, err := io.ReadFull(c.br, mask[:]); err != nil {
				return 0, nil, err
			}
		}
		buf := make([]byte, n)
		if _, err := io.ReadFull(c.br, buf); err != nil {
			return 0, nil, err
		}
		if masked {
			for i := range buf {
				buf[i] ^= mask[i&3]
			}
		}

		switch op {
		case OpText, OpBinary:
			return op, buf, nil
		case OpPing:
			if err := c.Write(OpPong, buf); err != nil {
				return 0, nil, err
			}
		case OpPong:
			// ignored
		case OpClose:
			return 0, nil, io.EOF
		default:
			return 0, nil, fmt.Errorf("websocket: unsupported opcode %d", op)
		}
	}
}

// Write queues one unmasked frame (server frames are never masked) and
// returns without waiting for the peer; payload is kept, not copied. An error
// means the connection is gone, or was dropped because the peer stopped
// reading.
func (c *Conn) Write(opcode byte, payload []byte) error {
	var head [10]byte
	head[0] = 0x80 | opcode
	n := 2
	switch l := len(payload); {
	case l < 126:
		head[1] = byte(l)
	case l < 1<<16:
		head[1] = 126
		binary.BigEndian.PutUint16(head[2:], uint16(l))
		n = 4
	default:
		head[1] = 127
		binary.BigEndian.PutUint64(head[2:], uint64(l))
		n = 10
	}
	return c.q.Send(head[:n], payload)
}

// Close drops the frames not sent yet and closes the connection.
func (c *Conn) Close() error { return c.q.Close() }

// WriteText sends a text frame.
func (c *Conn) WriteText(s string) error { return c.Write(OpText, []byte(s)) }

// WriteBinary sends a binary frame.
func (c *Conn) WriteBinary(b []byte) error { return c.Write(OpBinary, b) }
