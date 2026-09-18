package master

import (
	"errors"
	"fmt"

	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
)

// Transport is how a lobby's game traffic travels. There are exactly two:
// our direct relay on the host's public port, or Steam Datagram Relay through
// the shim's SDR transport. The game's legacy Steam P2P path is gone by
// construction (the shim diverts it for good), so nothing here can select it.
type Transport int

const (
	// TransportDirect mints Session ('X') ids: the game asks instance/get and
	// is pointed at our relay on the host's public endpoint.
	TransportDirect Transport = iota
	// TransportSDR hands out the players' real Steam ids: Lobby.isSteamOnly
	// is then true for every member, the game takes its Steam path, and the
	// shim carries it over ISteamNetworkingMessages (SDR).
	TransportSDR
)

func (t Transport) String() string {
	if t == TransportSDR {
		return "SDR"
	}
	return "direct"
}

// Modes accepted by Options.Transport.
const (
	ModeAuto   = "auto"
	ModeDirect = "direct"
	ModeSDR    = "sdr"
)

// SDRStatus is the shim's verdict on the SDR transport, read from
// %LOCALAPPDATA%\wartales-mp\sdr.status (see sdrbridge.ParseStatus).
type SDRStatus = sdrbridge.Status

// errNoTransport is the honest failure: nothing we offer can carry this lobby.
var errNoTransport = errors.New("no usable transport")

// chooseTransport decides a lobby's transport at creation:
//
//   - a VERIFIED public endpoint (nat.Endpoint.Verified: inbound from the
//     internet has actually arrived on the port this run) keeps the direct
//     relay, which needs no third party at all and stays the faster rung. A
//     UPnP mapping plus a STUN address is only a hint and never enough: it
//     was seen to be wrong on a real machine (mapping present, address
//     public, port refused from outside);
//   - otherwise SDR for everything, lobby phase included (the join code then
//     carries our SteamID and the proxy-link rides the SDR bridge). The join
//     code still offers the hinted endpoint as its first route, so guests
//     that can reach it do, and their arrival is what verifies it for the
//     next lobby;
//   - if the shim has reported that SDR cannot work in this game process, a
//     hinted endpoint is used unverified (and logged as such) because it is
//     the only route left; with no endpoint at all the lobby is refused with
//     the reasons rather than quietly created on a path that cannot connect.
//     "Not known yet" is not a refusal: the shim may still be waiting for the
//     Steam API, and the join code request is what fails, with a retry hint,
//     until it is ready.
//
// mode forces either transport; forcing SDR against a known-bad SDR is still
// refused. The returned string is the reason, for the log.
func chooseTransport(mode string, ep nat.Endpoint, epErr error, sdr SDRStatus) (Transport, string, error) {
	endpoint := func() string {
		if epErr != nil {
			return "public endpoint unknown (" + epErr.Error() + ")"
		}
		if ep.Verified {
			return fmt.Sprintf("public endpoint %s verified reachable (inbound seen; via %s)", ep.Addr, ep.Source)
		}
		if ep.Reachable {
			return fmt.Sprintf("public endpoint %s hinted by %s but UNVERIFIED (no inbound seen yet)", ep.Addr, ep.Source)
		}
		if ep.Warning != "" {
			return fmt.Sprintf("endpoint %s not internet-reachable: %s", ep.Addr, ep.Warning)
		}
		return fmt.Sprintf("endpoint %s not internet-reachable", ep.Addr)
	}
	sdrState := func() string {
		switch {
		case !sdr.Known && sdr.Reason != "":
			return "SDR pending (" + sdr.Reason + ")"
		case !sdr.Known:
			return "SDR status unknown (shim has not reported yet)"
		case sdr.OK:
			return "SDR ready"
		}
		return "SDR unavailable (" + sdr.Reason + ")"
	}

	switch mode {
	case ModeDirect:
		return TransportDirect, "forced by -transport direct; " + endpoint(), nil
	case ModeSDR:
		if sdr.Known && !sdr.OK {
			return TransportDirect, "", fmt.Errorf("%w: -transport sdr, but %s", errNoTransport, sdrState())
		}
		return TransportSDR, "forced by -transport sdr; " + sdrState(), nil
	case ModeAuto, "":
	default:
		return TransportDirect, "", fmt.Errorf("unknown transport mode %q", mode)
	}

	if epErr == nil && ep.Verified {
		return TransportDirect, endpoint(), nil
	}
	if sdr.Known && !sdr.OK {
		if epErr == nil && ep.Reachable {
			// The only route left is the unproven one: take it, and say so.
			return TransportDirect, "no SDR, falling back to the unverified endpoint: " + endpoint() + "; " + sdrState(), nil
		}
		return TransportDirect, "", fmt.Errorf("%w: %s; %s", errNoTransport, endpoint(), sdrState())
	}
	return TransportSDR, endpoint() + "; " + sdrState(), nil
}
