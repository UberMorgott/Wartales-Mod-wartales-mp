// Package nat discovers the public endpoint other players must connect to:
// UPnP first (it can also open the port), STUN as a fallback.
package nat

import (
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

// Mapper resolves and caches the public "ip:port" for our relay port.
type Mapper struct {
	Port int
	Log  *log.Logger

	mu     sync.Mutex
	igd    *IGD
	addr   string
	mapped bool
}

// NewMapper creates a mapper for the given public TCP port.
func NewMapper(port int, logger *log.Logger) *Mapper {
	return &Mapper{Port: port, Log: logger}
}

// Addr returns the cached public endpoint, resolving it on first use.
func (m *Mapper) Addr() (string, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.addr != "" {
		return m.addr, nil
	}
	ip, err := m.resolve()
	if err != nil {
		return "", err
	}
	m.addr = net.JoinHostPort(ip.String(), strconv.Itoa(m.Port))
	return m.addr, nil
}

// resolve must be called with the lock held.
func (m *Mapper) resolve() (net.IP, error) {
	if igd, err := DiscoverIGD(3 * time.Second); err == nil {
		m.igd = igd
		if err := igd.AddPortMapping(m.Port, "wartales-mp", MappingLease); err != nil {
			m.Log.Printf("nat: AddPortMapping failed: %v", err)
		} else {
			m.mapped = true
			m.Log.Printf("nat: UPnP mapped TCP %d", m.Port)
		}
		if ip, err := igd.ExternalIP(); err == nil {
			m.Log.Printf("nat: external ip %s (UPnP)", ip)
			return ip, nil
		} else {
			m.Log.Printf("nat: GetExternalIPAddress failed: %v", err)
		}
	} else {
		m.Log.Printf("nat: no UPnP gateway: %v", err)
	}

	if ip, err := STUNExternalIP(DefaultSTUN); err == nil {
		m.Log.Printf("nat: external ip %s (STUN)", ip)
		return ip, nil
	} else {
		m.Log.Printf("nat: STUN failed: %v", err)
	}

	if ip := LocalIP(); ip != nil {
		m.Log.Printf("nat: falling back to the LAN address %s", ip)
		return ip, nil
	}
	return nil, fmt.Errorf("could not determine a public address")
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
	c, err := net.Dial("udp4", "8.8.8.8:53")
	if err != nil {
		return nil
	}
	defer func() { _ = c.Close() }() // unconnected UDP socket, nothing to flush
	if a, ok := c.LocalAddr().(*net.UDPAddr); ok {
		return a.IP.To4()
	}
	return nil
}
