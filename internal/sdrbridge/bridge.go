package sdrbridge

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"sync"
	"time"
)

const (
	frAuth = 0
	frSend = 1
	frRecv = 2
	frErr  = 3

	opData = 1
	opFin  = 2

	// maxChunk keeps one SDR message well under Steam's 512 KiB limit.
	maxChunk  = 64 * 1024
	frameHead = 13
	maxFrame  = 512 * 1024
)

// ErrNotReady is returned while the shim's bridge is not connected.
var ErrNotReady = errors.New("SDR bridge not connected")

// Bridge is the helper's connection to the shim's SDR bridge.
type Bridge struct {
	statusPath string
	log        *log.Logger

	// OnPeer receives a stream opened by a remote peer (host side). nil drops
	// such streams.
	OnPeer func(c net.Conn, peer uint64)

	mu     sync.Mutex
	conn   net.Conn // to the shim, nil while disconnected
	wmu    sync.Mutex
	status Status
	peers  map[uint64]*peerConn
}

// New prepares a bridge that will follow statusPath.
func New(statusPath string, logger *log.Logger) *Bridge {
	return &Bridge{statusPath: statusPath, log: logger, peers: map[uint64]*peerConn{}}
}

// Status is the shim's latest verdict, re-read from disk while disconnected.
func (b *Bridge) Status() Status {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.conn == nil {
		b.status = ReadStatus(b.statusPath)
	}
	return b.status
}

// Ready reports whether streams can be opened right now, and if not, why.
func (b *Bridge) Ready() (bool, string) {
	st := b.Status()
	b.mu.Lock()
	connected := b.conn != nil
	b.mu.Unlock()
	switch {
	case connected:
		return true, ""
	case st.Known && !st.OK:
		return false, "SDR unavailable: " + st.Reason
	case st.OK:
		return false, "SDR bridge at " + st.Bridge + " not connected yet"
	default:
		return false, "SDR still initialising: " + st.Reason
	}
}

// Run follows the status file, keeps a connection to the shim while it
// advertises one, and reconnects after a drop. Returns when ctx ends.
func (b *Bridge) Run(ctx context.Context) {
	logged := ""
	for ctx.Err() == nil {
		st := b.Status()
		if !st.OK || st.Bridge == "" || st.Token == "" {
			line := "waiting: " + st.Reason
			if st.Known && !st.OK {
				line = "SDR UNAVAILABLE in the game process: " + st.Reason
			}
			if line != logged {
				b.log.Printf("sdr-bridge: %s", line)
				logged = line
			}
			sleep(ctx, 500*time.Millisecond)
			continue
		}
		if err := b.session(ctx, st); err != nil {
			b.log.Printf("sdr-bridge: %v", err)
		}
		logged = ""
		sleep(ctx, time.Second)
	}
}

func sleep(ctx context.Context, d time.Duration) {
	select {
	case <-ctx.Done():
	case <-time.After(d):
	}
}

// session runs one connection to the shim.
func (b *Bridge) session(ctx context.Context, st Status) error {
	d := net.Dialer{Timeout: 3 * time.Second}
	c, err := d.DialContext(ctx, "tcp", st.Bridge)
	if err != nil {
		return fmt.Errorf("cannot reach the shim's bridge at %s: %w", st.Bridge, err)
	}
	if err := writeFrame(c, frAuth, 0, []byte(st.Token)); err != nil {
		_ = c.Close()
		return err
	}
	b.mu.Lock()
	b.conn = c
	b.status = st
	b.mu.Unlock()
	b.log.Printf("sdr-bridge: connected to the shim at %s, relaying on SDR channel %d", st.Bridge, Channel)

	stop := context.AfterFunc(ctx, func() { _ = c.Close() })
	defer stop()
	err = b.readLoop(c)

	b.mu.Lock()
	b.conn = nil
	peers := b.peers
	b.peers = map[uint64]*peerConn{}
	b.mu.Unlock()
	_ = c.Close()
	for _, p := range peers {
		p.remoteClosed(ErrNotReady)
	}
	if ctx.Err() != nil {
		return nil
	}
	return fmt.Errorf("connection to the shim ended: %w (%d stream(s) dropped)", err, len(peers))
}

func (b *Bridge) readLoop(c net.Conn) error {
	head := make([]byte, frameHead)
	for {
		if _, err := io.ReadFull(c, head); err != nil {
			return err
		}
		typ := head[0]
		peer := binary.LittleEndian.Uint64(head[1:9])
		n := binary.LittleEndian.Uint32(head[9:13])
		if n > maxFrame {
			return fmt.Errorf("frame of %d bytes from the shim", n)
		}
		payload := make([]byte, n)
		if _, err := io.ReadFull(c, payload); err != nil {
			return err
		}
		switch typ {
		case frRecv:
			b.deliver(peer, payload)
		case frErr:
			b.log.Printf("sdr-bridge: send to %d refused by Steam: %s", peer, payload)
			b.mu.Lock()
			p := b.peers[peer]
			b.mu.Unlock()
			if p != nil {
				p.remoteClosed(fmt.Errorf("SDR: %s", payload))
			}
		default:
			b.log.Printf("sdr-bridge: unexpected frame type %d from the shim, ignored", typ)
		}
	}
}

// deliver routes one SDR message to its stream, opening one for a new peer.
func (b *Bridge) deliver(peer uint64, payload []byte) {
	if len(payload) == 0 {
		return
	}
	op, data := payload[0], payload[1:]
	b.mu.Lock()
	p := b.peers[peer]
	if p == nil {
		if op != opData || b.OnPeer == nil {
			b.mu.Unlock()
			b.log.Printf("sdr-bridge: %d bytes from unknown peer %d dropped", len(data), peer)
			return
		}
		p = b.newPeer(peer)
		b.mu.Unlock()
		b.log.Printf("sdr-bridge: incoming stream from %d", peer)
		go b.OnPeer(p, peer)
	} else {
		b.mu.Unlock()
	}
	if op == opFin {
		b.forget(peer, p)
		p.remoteClosed(io.EOF)
		return
	}
	p.push(data)
}

// Dial opens a stream to peer (guest side).
func (b *Bridge) Dial(peer uint64) (net.Conn, error) {
	if ok, why := b.Ready(); !ok {
		return nil, fmt.Errorf("%w: %s", ErrNotReady, why)
	}
	b.wmu.Lock()
	defer b.wmu.Unlock()
	// The remote helper may still hold our previous stream, even when this
	// helper has restarted and has no local record of it. End that stream
	// before sending the new hello; otherwise it becomes an ordinary command
	// on the old link and the host rejects it as "Unknown command link/hello".
	// FIN and the subsequent data use the same reliable, ordered channel.
	b.mu.Lock()
	if old := b.peers[peer]; old != nil {
		delete(b.peers, peer)
		b.mu.Unlock()
		old.remoteClosed(errors.New("replaced by a new stream"))
		b.mu.Lock()
	}
	b.mu.Unlock()
	if err := b.send(peer, []byte{opFin}); err != nil {
		return nil, err
	}
	b.mu.Lock()
	p := b.newPeer(peer)
	b.mu.Unlock()
	return p, nil
}

// newPeer registers a stream; lock held.
func (b *Bridge) newPeer(peer uint64) *peerConn {
	p := &peerConn{b: b, peer: peer, in: make(chan []byte, 256), done: make(chan struct{})}
	b.peers[peer] = p
	return p
}

func (b *Bridge) forget(peer uint64, p *peerConn) {
	b.mu.Lock()
	if b.peers[peer] == p {
		delete(b.peers, peer)
	}
	b.mu.Unlock()
}

// send writes one SEND frame to the shim. The caller holds wmu, which
// serializes stream replacement, writes and local closes in wire order.
func (b *Bridge) send(peer uint64, payload []byte) error {
	b.mu.Lock()
	c := b.conn
	b.mu.Unlock()
	if c == nil {
		return ErrNotReady
	}
	return writeFrame(c, frSend, peer, payload)
}

func writeFrame(w io.Writer, typ byte, peer uint64, payload []byte) error {
	buf := make([]byte, frameHead+len(payload))
	buf[0] = typ
	binary.LittleEndian.PutUint64(buf[1:9], peer)
	binary.LittleEndian.PutUint32(buf[9:13], uint32(len(payload))) //nolint:gosec // bounded by maxFrame at every call site
	copy(buf[frameHead:], payload)
	_, err := w.Write(buf)
	return err
}

// ---------------------------------------------------------------- streams

// peerConn is one byte stream to one peer over SDR: reliable and ordered per
// channel, so the payloads simply concatenate.
type peerConn struct {
	b    *Bridge
	peer uint64
	in   chan []byte
	rest []byte

	once sync.Once
	done chan struct{}
	err  error
}

type addr struct{ peer uint64 }

func (a addr) Network() string { return "sdr" }
func (a addr) String() string  { return fmt.Sprintf("steam:%d", a.peer) }

func (p *peerConn) push(data []byte) {
	select {
	case p.in <- data:
	case <-p.done:
	default:
		// The reader is not keeping up; a proxy-link never legitimately
		// queues 256 frames, so this is a runaway peer.
		p.b.log.Printf("sdr-bridge: stream from %d overflowed, closing it", p.peer)
		p.b.forget(p.peer, p)
		p.remoteClosed(errors.New("stream overflow"))
	}
}

// remoteClosed ends the stream from the far side: reads drain then fail.
func (p *peerConn) remoteClosed(err error) {
	p.once.Do(func() {
		p.err = err
		close(p.done)
	})
}

func (p *peerConn) Read(buf []byte) (int, error) {
	for len(p.rest) == 0 {
		select {
		case data := <-p.in:
			p.rest = data
		case <-p.done:
			// drain what arrived before the end
			select {
			case data := <-p.in:
				p.rest = data
				continue
			default:
			}
			if errors.Is(p.err, io.EOF) {
				return 0, io.EOF
			}
			return 0, p.err
		}
	}
	n := copy(buf, p.rest)
	p.rest = p.rest[n:]
	return n, nil
}

func (p *peerConn) Write(data []byte) (int, error) {
	p.b.wmu.Lock()
	defer p.b.wmu.Unlock()
	select {
	case <-p.done:
		return 0, fmt.Errorf("sdr stream to %d closed: %w", p.peer, p.err)
	default:
	}
	sent := 0
	for len(data) > 0 {
		n := min(len(data), maxChunk)
		if err := p.b.send(p.peer, append([]byte{opData}, data[:n]...)); err != nil {
			return sent, err
		}
		sent += n
		data = data[n:]
	}
	return sent, nil
}

// Close ends the stream from our side and tells the peer.
func (p *peerConn) Close() error {
	p.b.wmu.Lock()
	defer p.b.wmu.Unlock()
	p.b.forget(p.peer, p)
	var err error
	p.once.Do(func() {
		p.err = net.ErrClosed
		close(p.done)
		err = p.b.send(p.peer, []byte{opFin})
	})
	return err
}

func (p *peerConn) LocalAddr() net.Addr              { return addr{0} }
func (p *peerConn) RemoteAddr() net.Addr             { return addr{p.peer} }
func (p *peerConn) SetDeadline(time.Time) error      { return nil }
func (p *peerConn) SetReadDeadline(time.Time) error  { return nil }
func (p *peerConn) SetWriteDeadline(time.Time) error { return nil }
