package nat

import (
	"context"
	"encoding/xml"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// IGD is a discovered UPnP Internet Gateway Device.
type IGD struct {
	controlURL  string
	serviceType string
	localIP     net.IP
}

var igdServices = []string{
	"urn:schemas-upnp-org:service:WANIPConnection:1",
	"urn:schemas-upnp-org:service:WANIPConnection:2",
	"urn:schemas-upnp-org:service:WANPPPConnection:1",
}

// DiscoverIGD looks for an internet gateway with SSDP (M-SEARCH to the
// multicast address 239.255.255.250:1900).
func DiscoverIGD(timeout time.Duration) (*IGD, error) {
	var lc net.ListenConfig
	c, err := lc.ListenPacket(context.Background(), "udp4", ":0")
	if err != nil {
		return nil, err
	}
	defer func() { _ = c.Close() }() // UDP socket, nothing to flush on close

	dst := &net.UDPAddr{IP: net.IPv4(239, 255, 255, 250), Port: 1900}
	msg := "M-SEARCH * HTTP/1.1\r\n" +
		"HOST: 239.255.255.250:1900\r\n" +
		"MAN: \"ssdp:discover\"\r\n" +
		"MX: 2\r\n" +
		"ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n"
	if _, err := c.WriteTo([]byte(msg), dst); err != nil {
		return nil, err
	}
	if err := c.SetReadDeadline(time.Now().Add(timeout)); err != nil {
		return nil, err
	}

	buf := make([]byte, 2048)
	for {
		n, from, err := c.ReadFrom(buf)
		if err != nil {
			return nil, fmt.Errorf("upnp: no gateway found: %w", err)
		}
		location := headerValue(string(buf[:n]), "location")
		if location == "" {
			continue
		}
		igd, err := describe(location)
		if err != nil {
			continue
		}
		if udp, ok := from.(*net.UDPAddr); ok {
			igd.localIP = localIPTowards(udp.IP)
		}
		return igd, nil
	}
}

func headerValue(resp, key string) string {
	for line := range strings.SplitSeq(resp, "\r\n") {
		if p := strings.Index(line, ":"); p > 0 && strings.EqualFold(strings.TrimSpace(line[:p]), key) {
			return strings.TrimSpace(line[p+1:])
		}
	}
	return ""
}

// describe fetches the device description and picks a WAN connection service.
func describe(location string) (*IGD, error) {
	req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, location, nil)
	if err != nil {
		return nil, err
	}
	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer func() { _ = resp.Body.Close() }() // read-only body, close error is moot
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, err
	}

	// The description nests services several levels deep, so walk the tree
	// instead of relying on a fixed path.
	type service struct {
		Type       string `xml:"serviceType"`
		ControlURL string `xml:"controlURL"`
	}
	var found []service
	dec := xml.NewDecoder(strings.NewReader(string(body)))
	for {
		tok, err := dec.Token()
		if err != nil {
			break
		}
		if se, ok := tok.(xml.StartElement); ok && se.Name.Local == "service" {
			var s service
			if dec.DecodeElement(&s, &se) == nil {
				found = append(found, s)
			}
		}
	}

	base, err := url.Parse(location)
	if err != nil {
		return nil, err
	}
	for _, want := range igdServices {
		for _, s := range found {
			if s.Type != want || s.ControlURL == "" {
				continue
			}
			ref, err := url.Parse(s.ControlURL)
			if err != nil {
				continue
			}
			return &IGD{controlURL: base.ResolveReference(ref).String(), serviceType: s.Type}, nil
		}
	}
	return nil, fmt.Errorf("upnp: %s exposes no WAN connection service", location)
}

// localIPTowards returns the local address used to reach the gateway.
func localIPTowards(gateway net.IP) net.IP {
	var d net.Dialer
	c, err := d.DialContext(context.Background(), "udp4", net.JoinHostPort(gateway.String(), "1900"))
	if err != nil {
		return nil
	}
	defer func() { _ = c.Close() }() // UDP socket opened only to learn the route
	if a, ok := c.LocalAddr().(*net.UDPAddr); ok {
		return a.IP
	}
	return nil
}

// soap issues one SOAP action and returns the response body.
func (g *IGD) soap(action, body string) (string, error) {
	env := `<?xml version="1.0"?>` +
		`<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" ` +
		`s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>` +
		fmt.Sprintf(`<u:%s xmlns:u="%s">%s</u:%s>`, action, g.serviceType, body, action) +
		`</s:Body></s:Envelope>`

	req, err := http.NewRequestWithContext(context.Background(), http.MethodPost, g.controlURL, strings.NewReader(env))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", `text/xml; charset="utf-8"`)
	req.Header.Set("SOAPAction", fmt.Sprintf(`"%s#%s"`, g.serviceType, action))

	client := &http.Client{Timeout: 8 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return "", err
	}
	defer func() { _ = resp.Body.Close() }() // read-only body, close error is moot
	out, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return "", err
	}
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("upnp: %s returned %s", action, resp.Status)
	}
	return string(out), nil
}

// ExternalIP runs GetExternalIPAddress.
func (g *IGD) ExternalIP() (net.IP, error) {
	body, err := g.soap("GetExternalIPAddress", "")
	if err != nil {
		return nil, err
	}
	raw := innerText(body, "NewExternalIPAddress")
	ip := net.ParseIP(strings.TrimSpace(raw))
	if ip == nil {
		return nil, fmt.Errorf("upnp: bad external ip %q", raw)
	}
	return ip, nil
}

// AddPortMapping forwards a public TCP port to us.
func (g *IGD) AddPortMapping(port int, description string, lease time.Duration) error {
	if g.localIP == nil {
		return fmt.Errorf("upnp: local address unknown")
	}
	body := fmt.Sprintf(
		"<NewRemoteHost></NewRemoteHost><NewExternalPort>%d</NewExternalPort>"+
			"<NewProtocol>TCP</NewProtocol><NewInternalPort>%d</NewInternalPort>"+
			"<NewInternalClient>%s</NewInternalClient><NewEnabled>1</NewEnabled>"+
			"<NewPortMappingDescription>%s</NewPortMappingDescription>"+
			"<NewLeaseDuration>%d</NewLeaseDuration>",
		port, port, g.localIP, description, int(lease.Seconds()))
	_, err := g.soap("AddPortMapping", body)
	return err
}

// DeletePortMapping removes a mapping added earlier.
func (g *IGD) DeletePortMapping(port int) error {
	body := fmt.Sprintf(
		"<NewRemoteHost></NewRemoteHost><NewExternalPort>%d</NewExternalPort><NewProtocol>TCP</NewProtocol>",
		port)
	_, err := g.soap("DeletePortMapping", body)
	return err
}

func innerText(doc, tag string) string {
	dec := xml.NewDecoder(strings.NewReader(doc))
	for {
		tok, err := dec.Token()
		if err != nil {
			return ""
		}
		if se, ok := tok.(xml.StartElement); ok && se.Name.Local == tag {
			var s string
			if dec.DecodeElement(&s, &se) == nil {
				return s
			}
			return ""
		}
	}
}
