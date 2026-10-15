package nat

import "net"

// STUNServers answer binding requests on UDP. They were each probed live on
// 2026-10-15; several are asked at once because any one of them may be
// unreachable from a given network (issue #7: Google's timed out on a Russian
// carrier). stun1.l.google.com is left out: it is the same IP as stun.l.
var STUNServers = []string{
	"stun.cloudflare.com:3478",
	"stun.l.google.com:19302",
	"global.stun.twilio.com:3478",
	"stun.nextcloud.com:3478",
	"stun.voip.blackberry.com:3478",
	"stun.sipnet.ru:3478",
	"stun.zadarma.com:3478",
}

// Mapping is what the STUN answers say about our NAT's port mapping. It is a
// diagnosis for the log and the error text, never a reason to refuse a join.
type Mapping int

const (
	// MappingNone: no STUN server answered (STUN or UDP blocked).
	MappingNone Mapping = iota
	// MappingUnknown: one server answered, nothing to compare with.
	MappingUnknown
	// MappingStable: two servers saw the same ip:port (endpoint-independent
	// mapping): the address in our codes is the one peers can punch.
	MappingStable
	// MappingSymmetric: two servers saw different ports: the NAT maps every
	// destination anew, so a peer punching the address STUN saw usually fails.
	MappingSymmetric
)

func (m Mapping) String() string {
	switch m {
	case MappingUnknown:
		return "unknown (one STUN answer)"
	case MappingStable:
		return "endpoint-independent"
	case MappingSymmetric:
		return "symmetric"
	case MappingNone:
	}
	return "no STUN answer"
}

// Classify compares the mapped addresses that different STUN servers (by
// server IP) reported for one socket.
func Classify(mapped map[string]*net.UDPAddr) Mapping {
	var first *net.UDPAddr
	for _, a := range mapped {
		if first == nil {
			first = a
			continue
		}
		if a.Port != first.Port || !a.IP.Equal(first.IP) {
			return MappingSymmetric
		}
	}
	switch len(mapped) {
	case 0:
		return MappingNone
	case 1:
		return MappingUnknown
	}
	return MappingStable
}
