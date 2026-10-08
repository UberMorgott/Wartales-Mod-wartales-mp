//go:build harness

package main

import (
	"log"
	"net"
	"os"
	"strconv"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/nat"
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
