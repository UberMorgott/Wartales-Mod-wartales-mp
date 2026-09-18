package nat

import (
	"io"
	"log"
	"net"
	"testing"
	"time"
)

// TestMarkInbound: only a public source verifies the endpoint, and a change
// of WAN address starts unverified again.
func TestMarkInbound(t *testing.T) {
	m := NewMapper(14250, log.New(io.Discard, "", 0))
	m.done = true
	m.ep = Endpoint{IP: net.IPv4(45, 154, 88, 66), Addr: "45.154.88.66:14250", Reachable: true, At: time.Now()}
	m.MaxAge = -1

	if m.MarkInbound(net.IPv4(192, 168, 1, 7)) {
		t.Fatal("a LAN guest must not verify internet reachability")
	}
	if ep, _ := m.Endpoint(); ep.Verified {
		t.Fatal("verified without any public inbound")
	}
	if !m.MarkInbound(net.IPv4(8, 8, 8, 8)) {
		t.Fatal("a public source verifies")
	}
	if ep, _ := m.Endpoint(); !ep.Verified {
		t.Fatal("not verified after a public inbound")
	}
}
