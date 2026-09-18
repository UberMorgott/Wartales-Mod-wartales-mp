package sdrbridge_test

import (
	"bufio"
	"context"
	"errors"
	"io"
	"log"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge/sdrbridgetest"
)

func TestParseStatus(t *testing.T) {
	st := sdrbridge.ParseStatus("ok bridge=127.0.0.1:52669 token=50e80f126cce8178c774a2e18fbad573\n")
	if !st.Known || !st.OK || st.Bridge != "127.0.0.1:52669" || st.Token != "50e80f126cce8178c774a2e18fbad573" {
		t.Fatalf("ok status = %+v", st)
	}
	st = sdrbridge.ParseStatus("ok bridge=none")
	if !st.OK || st.Bridge != "" || st.Reason == "" {
		t.Fatalf("ok without bridge = %+v", st)
	}
	st = sdrbridge.ParseStatus("unavailable steam_api64.dll is not loaded in this process")
	if !st.Known || st.OK || st.Reason != "steam_api64.dll is not loaded in this process" {
		t.Fatalf("unavailable = %+v", st)
	}
	st = sdrbridge.ParseStatus("pending Steam API not initialised yet")
	if st.Known || st.OK || st.Reason != "Steam API not initialised yet" {
		t.Fatalf("pending = %+v", st)
	}
	if st := sdrbridge.ReadStatus(t.TempDir() + "/missing"); st.Known || st.OK {
		t.Fatalf("missing file = %+v", st)
	}
}

func start(t *testing.T, path string) (*sdrbridge.Bridge, context.CancelFunc) {
	t.Helper()
	b := sdrbridge.New(path, log.New(io.Discard, "", 0))
	ctx, cancel := context.WithCancel(context.Background())
	go b.Run(ctx)
	deadline := time.Now().Add(5 * time.Second)
	for {
		if ok, _ := b.Ready(); ok {
			return b, cancel
		}
		if time.Now().After(deadline) {
			_, why := b.Ready()
			t.Fatalf("bridge never became ready: %s", why)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// TestStreamsOverTheSwitch: two helpers, each behind its own fake shim
// identity; a stream dialed by one arrives at the other's OnPeer, bytes flow
// both ways in order, and closing one side ends the other's reads.
func TestStreamsOverTheSwitch(t *testing.T) {
	sw := sdrbridgetest.New(t)
	const host, guest = uint64(76561197960265728 + 111), uint64(76561197960265728 + 222)
	hb, cancelH := start(t, sw.Add(t, host))
	defer cancelH()
	incoming := make(chan net.Conn, 1)
	hb.OnPeer = func(c net.Conn, peer uint64) {
		if peer != guest {
			t.Errorf("OnPeer from %d, want %d", peer, guest)
		}
		incoming <- c
	}
	gb, cancelG := start(t, sw.Add(t, guest))
	defer cancelG()

	gc, err := gb.Dial(host)
	if err != nil {
		t.Fatal(err)
	}
	if gc.RemoteAddr().String() != "steam:76561197960265839" {
		t.Fatalf("RemoteAddr = %s", gc.RemoteAddr())
	}
	if _, err := gc.Write([]byte("hello host\n")); err != nil {
		t.Fatal(err)
	}
	var hc net.Conn
	select {
	case hc = <-incoming:
	case <-time.After(5 * time.Second):
		t.Fatal("the host never saw the stream")
	}
	hr := bufio.NewReader(hc)
	line, err := hr.ReadString('\n')
	if err != nil || line != "hello host\n" {
		t.Fatalf("host read %q, %v", line, err)
	}
	// A big write is chunked into several SDR messages and reassembled.
	big := strings.Repeat("x", 200*1024) + "\n"
	go func() { _, _ = hc.Write([]byte(big)) }()
	gr := bufio.NewReader(gc)
	got, err := gr.ReadString('\n')
	if err != nil || got != big {
		t.Fatalf("guest read %d bytes, %v", len(got), err)
	}
	// FIN from the guest ends the host's reads with EOF.
	_ = gc.Close()
	if _, err := hr.ReadString('\n'); !errors.Is(err, io.EOF) {
		t.Fatalf("after the guest closed, host read err = %v, want EOF", err)
	}
	// And the guest's own conn is closed for good.
	if _, err := gc.Write([]byte("x")); err == nil {
		t.Fatal("write on a closed stream succeeded")
	}
}

// TestDialUnknownPeerIsReported: a send to nobody comes back as ERR and ends
// the stream with the reason.
func TestDialUnknownPeerIsReported(t *testing.T) {
	sw := sdrbridgetest.New(t)
	b, cancel := start(t, sw.Add(t, 76561197960265728+1))
	defer cancel()
	c, err := b.Dial(76561197960265728 + 999)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.Write([]byte("anyone?")); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 8)
	_, err = c.Read(buf)
	if err == nil || !strings.Contains(err.Error(), "EResult 3") {
		t.Fatalf("read err = %v, want the ERR text", err)
	}
}

// TestNotReady: without a usable status nothing can be dialed, and the reason
// names the shim's verdict.
func TestNotReady(t *testing.T) {
	sw := sdrbridgetest.New(t)
	b := sdrbridge.New(sw.Unavailable(t, "steam_api64.dll is not loaded in this process"), log.New(io.Discard, "", 0))
	go b.Run(t.Context())
	_, err := b.Dial(1)
	if !errors.Is(err, sdrbridge.ErrNotReady) || !strings.Contains(err.Error(), "steam_api64.dll is not loaded") {
		t.Fatalf("Dial err = %v", err)
	}
	b2 := sdrbridge.New(t.TempDir()+"/absent", log.New(io.Discard, "", 0))
	if ok, why := b2.Ready(); ok || !strings.Contains(why, "initialising") {
		t.Fatalf("Ready = %v, %q", ok, why)
	}
}
