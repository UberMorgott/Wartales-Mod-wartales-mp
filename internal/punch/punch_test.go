package punch

import (
	"encoding/binary"
	"errors"
	"io"
	"log"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/nat"
)

var loopback = net.IPv4(127, 0, 0, 1)

// fakeSTUN answers binding requests with the sender's address, its port moved
// by delta (a symmetric NAT shows each server another port).
func fakeSTUN(t *testing.T, delta int) string {
	t.Helper()
	c, err := net.ListenUDP("udp4", &net.UDPAddr{IP: loopback})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	go func() {
		buf := make([]byte, 1500)
		for {
			n, from, err := c.ReadFromUDP(buf)
			if err != nil {
				return
			}
			if !nat.IsSTUN(buf[:n]) {
				continue
			}
			resp := make([]byte, 32)
			binary.BigEndian.PutUint16(resp[0:], 0x0101)
			binary.BigEndian.PutUint16(resp[2:], 12)
			copy(resp[4:20], buf[4:20]) // cookie + transaction id
			binary.BigEndian.PutUint16(resp[20:], 0x0020)
			binary.BigEndian.PutUint16(resp[22:], 8)
			resp[25] = 0x01
			binary.BigEndian.PutUint16(resp[26:], uint16(from.Port+delta)^0x2112) //nolint:gosec // test port
			ip := from.IP.To4()
			for i, k := range []byte{0x21, 0x12, 0xa4, 0x42} {
				resp[28+i] = ip[i] ^ k
			}
			_, _ = c.WriteToUDP(resp, from)
		}
	}()
	return c.LocalAddr().String()
}

// silent is a UDP address that never answers.
func silent(t *testing.T) string {
	t.Helper()
	c, err := net.ListenUDP("udp4", &net.UDPAddr{IP: loopback})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	go func() {
		buf := make([]byte, 1500)
		for {
			if _, _, err := c.ReadFrom(buf); err != nil {
				return
			}
		}
	}()
	return c.LocalAddr().String()
}

// local is the socket's loopback address.
func local(s *Socket) string {
	return (&net.UDPAddr{IP: loopback, Port: s.LocalAddr().Port}).String()
}

func open(t *testing.T, relay string, servers ...string) *Socket {
	t.Helper()
	s, err := Open(0, relay, servers, log.New(io.Discard, "", 0))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

func TestEndpointFallsBackAcrossServers(t *testing.T) {
	// The first server never answers; the other two do, with the same address.
	s := open(t, "", silent(t), fakeSTUN(t, 0), fakeSTUN(t, 0))
	start := time.Now()
	r, err := s.Endpoint()
	if err != nil {
		t.Fatal(err)
	}
	if r.Mapped.Port != s.LocalAddr().Port || !r.Mapped.IP.Equal(loopback) {
		t.Fatalf("mapped %v, want 127.0.0.1:%d", r.Mapped, s.LocalAddr().Port)
	}
	if r.Mapping != nat.MappingStable {
		t.Fatalf("mapping %s, want stable", r.Mapping)
	}
	if d := time.Since(start); d > 2*time.Second {
		t.Fatalf("two answers took %v: the silent server held the round up", d)
	}
	if !s.Ready() {
		t.Fatal("Ready false after a STUN answer")
	}
}

func TestEndpointSymmetric(t *testing.T) {
	s := open(t, "", fakeSTUN(t, 0), fakeSTUN(t, 7))
	r, err := s.Endpoint()
	if err != nil {
		t.Fatal(err)
	}
	if r.Mapping != nat.MappingSymmetric {
		t.Fatalf("mapping %s, want symmetric", r.Mapping)
	}
}

func TestEndpointNoServer(t *testing.T) {
	s := open(t, "", silent(t))
	r, err := s.Endpoint()
	if err == nil || r.Mapping != nat.MappingNone || s.Ready() {
		t.Fatalf("Endpoint = %+v, %v; want an error and no mapping", r, err)
	}
}

// echo is a TCP server that echoes every connection: the host's relay port.
func echo(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = ln.Close() })
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer func() { _ = c.Close() }()
				_, _ = io.Copy(c, c)
			}()
		}
	}()
	return ln.Addr().String()
}

func roundTrip(t *testing.T, addr, msg string) {
	t.Helper()
	c, err := net.DialTimeout("tcp", addr, 3*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = c.Close() }()
	_ = c.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := c.Write([]byte(msg)); err != nil {
		t.Fatal(err)
	}
	got := make([]byte, len(msg))
	if _, err := io.ReadFull(c, got); err != nil {
		t.Fatal(err)
	}
	if string(got) != msg {
		t.Fatalf("echo %q, want %q", got, msg)
	}
}

// Both sides punch at once (the reply-code case); the guest's TCP bytes go
// through KCP to the host's relay and back, on two streams of one tunnel.
func TestPunchTunnelEndToEnd(t *testing.T) {
	host := open(t, echo(t))
	guest := open(t, "", fakeSTUN(t, 0))
	hostAddr := local(host)
	guestAddr := local(guest)

	if err := host.Toward(guestAddr); err != nil {
		t.Fatal(err)
	}
	fwd, err := guest.Dial(hostAddr)
	if err != nil {
		t.Fatal(err)
	}
	if h, _, _ := net.SplitHostPort(fwd); h != "127.0.0.1" {
		t.Fatalf("forwarder %s is not on loopback", fwd)
	}
	roundTrip(t, fwd, "hello over kcp")
	big := strings.Repeat("0123456789abcdef", 64*1024) // 1 MiB: windows and frames
	done := make(chan struct{})
	go func() { defer close(done); roundTrip(t, fwd, big) }()
	roundTrip(t, fwd, "second stream")
	<-done

	// A second join to the same host reuses the live tunnel.
	again, err := guest.Dial(hostAddr)
	if err != nil || again != fwd {
		t.Fatalf("Dial again = %q, %v; want the live tunnel %q", again, err, fwd)
	}
}

// Guest-first: a host that lets the punch through answers it, no reply code.
func TestPunchGuestFirst(t *testing.T) {
	host := open(t, echo(t))
	guest := open(t, "")
	fwd, err := guest.Dial(local(host))
	if err != nil {
		t.Fatal(err)
	}
	roundTrip(t, fwd, "guest first")
}

// No answer: the error carries a reply code for the guest's own address and,
// on a symmetric NAT, the Steam invitation hint.
func TestPunchNoAnswer(t *testing.T) {
	guest := open(t, "", fakeSTUN(t, 0), fakeSTUN(t, 3))
	guest.Wait = 300 * time.Millisecond
	_, err := guest.Dial(silent(t))
	var na *NoAnswerError
	if !errors.As(err, &na) {
		t.Fatalf("Dial error %v, want *NoAnswerError", err)
	}
	c, derr := code.DecodeAny(na.Reply)
	if derr != nil || c.Reply == nil {
		t.Fatalf("reply code %q: %+v, %v", na.Reply, c, derr)
	}
	if c.Reply.Addr() != na.Self.Mapped.String() {
		t.Fatalf("reply code says %s, STUN said %s", c.Reply.Addr(), na.Self.Mapped)
	}
	for _, want := range []string{na.Reply, "Join by code", "Steam invitation"} {
		if !strings.Contains(err.Error(), want) {
			t.Fatalf("error %q lacks %q", err, want)
		}
	}

	// Without STUN there is no reply code to offer.
	lone := open(t, "")
	lone.Wait = 200 * time.Millisecond
	_, err = lone.Dial(silent(t))
	if !errors.As(err, &na) || na.Reply != "" || !strings.Contains(err.Error(), "Steam invitation") {
		t.Fatalf("Dial without STUN = %v", err)
	}
}
