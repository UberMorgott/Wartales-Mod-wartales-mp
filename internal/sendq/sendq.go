// Package sendq gives a connection an ordered send queue of bounded size,
// drained by a goroutine of its own, so a sender never waits on its peer: one
// player's stalled connection must not hold up the frames of everyone else
// (the relay's fan-out from the host, the master's lobby pushes). A peer that
// falls more than the budget behind is disconnected instead of being waited
// for.
package sendq

import (
	"errors"
	"net"
	"sync"
	"time"
)

// ErrOverflow is what a send gets once the peer fell too far behind.
var ErrOverflow = errors.New("sendq: the peer is not reading; send queue overflowed")

// flushTimeout bounds how long Drain keeps writing to a peer before it closes
// the connection anyway.
const flushTimeout = 5 * time.Second

// Queue sends to one connection. Its zero value is not usable; see New.
type Queue struct {
	c      net.Conn
	budget int

	mu      sync.Mutex
	bufs    [][]byte
	size    int   // bytes queued or being written
	err     error // once set, nothing more is accepted
	running bool  // a writer goroutine is draining bufs
	drain   bool  // close c once bufs are written
}

// New returns a queue that holds at most budget bytes for c.
func New(c net.Conn, budget int) *Queue {
	return &Queue{c: c, budget: budget}
}

// Send queues bufs to be written back to back, in order with every earlier
// send, and returns at once. The slices are kept, not copied. An error means
// nothing was queued: the connection already failed or was closed, or this
// send overflowed the budget, which closes it.
func (q *Queue) Send(bufs ...[]byte) error {
	n := 0
	for _, b := range bufs {
		n += len(b)
	}
	q.mu.Lock()
	if q.err != nil {
		err := q.err
		q.mu.Unlock()
		return err
	}
	if q.size+n > q.budget {
		q.err, q.bufs = ErrOverflow, nil
		q.mu.Unlock()
		_ = q.c.Close() // the peer is dropped; its reader sees the close
		return ErrOverflow
	}
	q.bufs = append(q.bufs, bufs...)
	q.size += n
	start := !q.running
	q.running = true
	q.mu.Unlock()
	if start {
		go q.run()
	}
	return nil
}

// run writes until the queue is empty, then exits: an idle connection holds
// no goroutine.
func (q *Queue) run() {
	for {
		q.mu.Lock()
		bufs := q.bufs
		q.bufs = nil
		if len(bufs) == 0 {
			q.running = false
			drain := q.drain
			q.mu.Unlock()
			if drain {
				_ = q.c.Close()
			}
			return
		}
		q.mu.Unlock()
		n := 0
		for _, b := range bufs {
			if _, err := q.c.Write(b); err != nil {
				q.fail(err)
				return
			}
			n += len(b)
		}
		q.mu.Lock()
		q.size -= n
		q.mu.Unlock()
	}
}

func (q *Queue) fail(err error) {
	q.mu.Lock()
	if q.err == nil {
		q.err = err
	}
	q.bufs, q.running = nil, false
	q.mu.Unlock()
	_ = q.c.Close()
}

// Close drops whatever is queued and closes the connection now.
func (q *Queue) Close() error {
	q.mu.Lock()
	if q.err == nil {
		q.err = net.ErrClosed
	}
	q.bufs = nil
	q.mu.Unlock()
	return q.c.Close()
}

// Drain accepts nothing more, writes what is queued (for at most
// flushTimeout) and then closes the connection. It does not wait.
func (q *Queue) Drain() {
	q.mu.Lock()
	if q.err == nil {
		q.err = net.ErrClosed
	}
	q.drain = true
	running := q.running
	q.mu.Unlock()
	if !running {
		_ = q.c.Close()
		return
	}
	_ = q.c.SetWriteDeadline(time.Now().Add(flushTimeout))
}
