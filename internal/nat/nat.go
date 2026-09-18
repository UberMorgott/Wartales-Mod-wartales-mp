// Package nat discovers the public endpoint other players must connect to:
// UPnP first (it can also open the port), STUN as a fallback.
//
// A router that is itself behind carrier NAT happily answers
// GetExternalIPAddress with a private address; such an answer is rejected here
// and STUN is asked instead, while the UPnP port mapping is kept, since the
// router still needs it to forward traffic inwards.
package nat

import (
	"context"
	"fmt"
	"log"
	"net"
	"strconv"
	"sync"
	"time"
)

// MappingLease is how long the UPnP port mapping is requested for; it is
// refreshed while we run.
const MappingLease = 2 * time.Hour

// Endpoint is the address we hand out, plus how honest we can be about it.
//
// Reachable is only a HINT: a router that answered UPnP and a STUN server
// that saw a public address prove nothing about inbound traffic (a firewall
// rule missing on this machine, a mapping the router silently dropped, a
// second NAT). Verified is the proof: a connection from a public address has
// actually arrived on the port during this run.
type Endpoint struct {
	Addr      string // "ip:port"
	IP        net.IP
	Source    string // "UPnP", "STUN", "UPnP+STUN" (mapped port + STUN address), "LAN"
	Reachable bool   // IP is a public IPv4 (a hint, see above)
	Verified  bool   // inbound from the internet has been seen on the port this run
	Mapped    bool   // a UPnP port mapping is in place
	Warning   string // why the endpoint is not internet-reachable
	At        time.Time
}

// DefaultMaxAge is how old a resolved endpoint may be before it is resolved
// again: the WAN address can change between lobbies (observed alternating on
// a carrier that hands out two), and the UPnP gateway may answer one time and
// not the next.
const DefaultMaxAge = 90 * time.Second

// Mapper resolves and caches the public "ip:port" for our relay port.
type Mapper struct {
	Port   int
	Log    *log.Logger
	MaxAge time.Duration // 0 = DefaultMaxAge; negative = never re-resolve

	mu           sync.Mutex
	igd          *IGD
	ep           Endpoint
	done         bool
	mapped       bool
	verifiedFrom net.IP
}

// NewMapper creates a mapper for the given public TCP port.
func NewMapper(port int, logger *log.Logger) *Mapper {
	return &Mapper{Port: port, Log: logger}
}

// MarkInbound records that a connection arrived on the port from ip. Only a
// public source proves internet reachability; a LAN guest proves nothing
// about the router. Reports whether the endpoint is now verified.
func (m *Mapper) MarkInbound(ip net.IP) bool {
	if !IsPublicIPv4(ip) {
		return false
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.verifiedFrom == nil {
		m.verifiedFrom = ip
		m.ep.Verified = true
		m.Log.Printf("nat: inbound connection from %s: the public endpoint is now VERIFIED reachable", ip)
	}
	return true
}

// Addr returns the cached public endpoint, resolving it on first use.
func (m *Mapper) Addr() (string, error) {
	ep, err := m.Endpoint()
	if err != nil {
		return "", err
	}
	return ep.Addr, nil
}

// Endpoint resolves (once) and returns the full result, including whether the
// address is actually reachable from the internet.
func (m *Mapper) Endpoint() (Endpoint, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	maxAge := m.MaxAge
	if maxAge == 0 {
		maxAge = DefaultMaxAge
	}
	if m.done && (maxAge < 0 || time.Since(m.ep.At) < maxAge) {
		return m.ep, nil
	}
	ep, err := m.resolve()
	if err != nil {
		if m.done {
			m.Log.Printf("nat: re-resolving the public endpoint failed (%v), keeping %s", err, m.ep.Addr)
			return m.ep, nil
		}
		return Endpoint{}, err
	}
	ep.Addr = net.JoinHostPort(ep.IP.String(), strconv.Itoa(m.Port))
	ep.Mapped = m.mapped
	ep.At = time.Now()
	// Verification is per address: a new WAN address starts unproven.
	ep.Verified = m.verifiedFrom != nil && m.done && m.ep.IP.Equal(ep.IP)
	if m.done && !m.ep.IP.Equal(ep.IP) {
		m.Log.Printf("nat: public address changed %s -> %s", m.ep.IP, ep.IP)
	}
	m.ep, m.done = ep, true
	note := ", unverified"
	switch {
	case ep.Verified:
		note = ", verified by inbound traffic"
	case !ep.Reachable:
		note = ", NOT internet-reachable"
	}
	m.Log.Printf("nat: public endpoint %s (source %s%s)", ep.Addr, ep.Source, note)
	return m.ep, nil
}

// resolve must be called with the lock held.
func (m *Mapper) resolve() (Endpoint, error) {
	var upnpIP net.IP

	if igd, err := DiscoverIGD(3 * time.Second); err == nil {
		m.igd = igd
		if err := igd.AddPortMapping(m.Port, "wartales-mp", MappingLease); err != nil {
			m.Log.Printf("nat: AddPortMapping failed: %v", err)
		} else {
			m.mapped = true
			m.Log.Printf("nat: UPnP mapped TCP %d", m.Port)
		}
		if ip, err := igd.ExternalIP(); err == nil {
			upnpIP = ip
			if IsPublicIPv4(ip) {
				m.Log.Printf("nat: external ip %s (UPnP)", ip)
				return Endpoint{IP: ip, Source: "UPnP", Reachable: true}, nil
			}
			// Double NAT: keep the mapping, distrust the address.
			m.Log.Printf("nat: UPnP reported external ip %s, not usable; keeping the port mapping and asking STUN",
				describeIP(ip))
		} else {
			m.Log.Printf("nat: GetExternalIPAddress failed: %v", err)
		}
	} else {
		m.Log.Printf("nat: no UPnP gateway: %v", err)
	}

	var stunIP net.IP
	if ip, err := STUNExternalIP(DefaultSTUN); err == nil {
		stunIP = ip
		if IsPublicIPv4(ip) {
			source := "STUN"
			if m.mapped {
				// Port from the UPnP mapping, address from STUN.
				source = "UPnP+STUN"
			}
			m.Log.Printf("nat: external ip %s (STUN); UPnP said %s", ip, describeIP(upnpIP))
			return Endpoint{IP: ip, Source: source, Reachable: true}, nil
		}
		m.Log.Printf("nat: STUN reported external ip %s, not usable", describeIP(ip))
	} else {
		m.Log.Printf("nat: STUN failed: %v", err)
	}

	warning := fmt.Sprintf("no public address found: UPnP=%s, STUN=%s; the join code works on the LAN only",
		describeIP(upnpIP), describeIP(stunIP))
	m.Log.Printf("nat: %s", warning)

	ip := LocalIP()
	if ip == nil {
		ip = firstNonNil(upnpIP, stunIP)
	}
	if ip == nil {
		return Endpoint{}, fmt.Errorf("could not determine any address: %s", warning)
	}
	m.Log.Printf("nat: falling back to the LAN address %s", ip)
	return Endpoint{IP: ip, Source: "LAN", Reachable: false, Warning: warning}, nil
}

func firstNonNil(ips ...net.IP) net.IP {
	for _, ip := range ips {
		if ip != nil {
			return ip
		}
	}
	return nil
}

// Close removes the UPnP mapping we added, if any.
func (m *Mapper) Close() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.igd != nil && m.mapped {
		if err := m.igd.DeletePortMapping(m.Port); err != nil {
			m.Log.Printf("nat: DeletePortMapping failed: %v", err)
		}
		m.mapped = false
	}
}

// LocalIP returns this machine's primary IPv4 address.
func LocalIP() net.IP {
	var d net.Dialer
	c, err := d.DialContext(context.Background(), "udp4", "8.8.8.8:53")
	if err != nil {
		return nil
	}
	defer func() { _ = c.Close() }() // unconnected UDP socket, nothing to flush
	if a, ok := c.LocalAddr().(*net.UDPAddr); ok {
		return a.IP.To4()
	}
	return nil
}
