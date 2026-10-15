//go:build harness

package main

import (
	"context"
	"errors"
	"log"
	"net"
	"os"
	"strconv"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/master"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/punch"
)

// testEndpoint is the local co-op harness seam (test build only,
// WMP_TEST_SEAM). With WARTALES_MP_TEST_INSTANCE=N inherited from the game,
// join codes advertise 127.0.0.1:<port> (the shim passes -port 1425N) and no
// UPnP/STUN lookup runs: two games on one PC talk over loopback.
func testEndpoint(port int, logger *log.Logger) (func() (nat.Endpoint, error), bool) {
	n, err := strconv.Atoi(os.Getenv("WARTALES_MP_TEST_INSTANCE"))
	if err != nil || n < 1 || n > 9 {
		return nil, false
	}
	ep := nat.Endpoint{
		Addr:      net.JoinHostPort("127.0.0.1", strconv.Itoa(port)),
		IP:        net.IPv4(127, 0, 0, 1),
		Source:    "harness",
		Reachable: true,
		Verified:  true,
		At:        time.Now(),
	}
	logger.Printf("WMP_TEST_SEAM: harness instance %d, endpoint %s (no UPnP/STUN)", n, ep.Addr)
	return func() (nat.Endpoint, error) { return ep, nil }, true
}

// testPunch: in a harness instance the punch socket's address is its own
// loopback port (no STUN), so join codes carry it. With
// WARTALES_MP_TEST_PUNCH=1 the guest's direct TCP route is refused, so a join
// goes through the UDP punch route and the KCP tunnel end to end.
func testPunch(ps *punch.Socket, o *master.Options, logger *log.Logger) {
	if _, err := strconv.Atoi(os.Getenv("WARTALES_MP_TEST_INSTANCE")); err != nil || ps == nil {
		return
	}
	ps.Fixed = &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: ps.LocalAddr().Port}
	logger.Printf("WMP_TEST_SEAM: punch address %s (no STUN)", ps.Fixed)
	if os.Getenv("WARTALES_MP_TEST_PUNCH") == "1" {
		logger.Printf("WMP_TEST_SEAM: direct TCP route disabled, joins go through the punch route")
		o.DialDirect = func(context.Context, string) (net.Conn, error) {
			return nil, errors.New("harness: direct route disabled (WARTALES_MP_TEST_PUNCH)")
		}
	}
}
