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
type Endpoint struct {
	Addr      string // "ip:port"
	IP        net.IP
	Source    string // "UPnP", "STUN", "UPnP+STUN" (mapped port + STUN address), "LAN"
	Reachable bool   // true only when IP is a public IPv4
	Mapped    bool   // a UPnP port mapping is in place
	Warning   string // why the endpoint is not internet-reachable
}

// Mapper resolves and caches the public "ip:port" for our relay port.
type Mapper struct {
	Port int
	Log  *log.Logger

	mu     sync.Mutex
	igd    *IGD
	ep     Endpoint
	done   bool
	mapped bool
}

// NewMapper creates a mapper for the given public TCP port.
func NewMapper(port int, logger *log.Logger) *Mapper {
	return &Mapper{Port: port, Log: logger}
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
	if m.done {
		return m.ep, nil
	}
	ep, err := m.resolve()
	if err != nil {
		return Endpoint{}, err
	}
	ep.Addr = net.JoinHostPort(ep.IP.String(), strconv.Itoa(m.Port))
	ep.Mapped = m.mapped
	m.ep, m.done = ep, true
	note := ""
	if !ep.Reachable {
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
