package sendq

import (
	"errors"
	"io"
	"net"
	"testing"
	"time"

	"go.uber.org/goleak"
)

func TestMain(m *testing.M) { goleak.VerifyTestMain(m) }

// net.Pipe has no buffer at all, so its peer not reading is a stalled peer.

func TestSendNeverWaitsOnThePeer(t *testing.T) {
	a, b := net.Pipe()
	defer func() { _ = b.Close() }()
	q := New(a, 1<<20)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range 100 {
			if err := q.Send([]byte("frame")); err != nil {
				t.Error(err)
				return
			}
		}
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("Send waited on a peer that does not read")
	}
	_ = q.Close()
}

func TestOrderIsKept(t *testing.T) {
	a, b := net.Pipe()
	q := New(a, 1<<20)
	want := ""
	for i := range 50 {
		s := string(rune('a'+i%26)) + "|"
		want += s
		if err := q.Send([]byte(s[:1]), []byte(s[1:])); err != nil {
			t.Fatal(err)
		}
	}
	q.Drain()
	got, err := io.ReadAll(b)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != want {
		t.Fatalf("got %q, want %q", got, want)
	}
}

func TestOverflowDropsThePeer(t *testing.T) {
	a, b := net.Pipe()
	defer func() { _ = b.Close() }()
	q := New(a, 10)
	if err := q.Send(make([]byte, 6)); err != nil {
		t.Fatal(err)
	}
	if err := q.Send(make([]byte, 6)); !errors.Is(err, ErrOverflow) {
		t.Fatalf("got %v, want ErrOverflow", err)
	}
	if err := q.Send([]byte{1}); !errors.Is(err, ErrOverflow) {
		t.Fatalf("a send after the overflow: got %v", err)
	}
	if _, err := b.Read(make([]byte, 1)); err == nil {
		// the writer may have got a byte out before the close; the next read ends
		if _, err := io.ReadAll(b); err != nil {
			t.Fatal(err)
		}
	}
}

func TestDrainOfAnIdleQueueCloses(t *testing.T) {
	a, b := net.Pipe()
	q := New(a, 10)
	q.Drain()
	if _, err := b.Read(make([]byte, 1)); !errors.Is(err, io.EOF) {
		t.Fatalf("got %v, want EOF", err)
	}
	if err := q.Send([]byte{1}); err == nil {
		t.Fatal("a drained queue took a send")
	}
}
