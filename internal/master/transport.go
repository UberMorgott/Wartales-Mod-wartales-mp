package master

import (
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/UberMorgott/wartales-mp/internal/nat"
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
// %LOCALAPPDATA%\wartales-mp\sdr.status: "ok", "pending <why>" while the Steam
// API is still coming up, "unavailable <why>" when it can never work in this
// process. Known is false when the file is missing (the shim has not got that
// far yet, or is not running at all).
type SDRStatus struct {
	Known  bool
	OK     bool
	Reason string
}

// ReadSDRStatus parses the status file the shim writes.
func ReadSDRStatus(path string) SDRStatus {
	b, err := os.ReadFile(path) //nolint:gosec // the path is ours: %LOCALAPPDATA%\wartales-mp\sdr.status, written by the shim
	if err != nil {
		return SDRStatus{}
	}
	return ParseSDRStatus(string(b))
}

// ParseSDRStatus parses the file's one line.
func ParseSDRStatus(text string) SDRStatus {
	text = strings.TrimSpace(text)
	if text == "" {
		return SDRStatus{}
	}
	verdict, reason, _ := strings.Cut(text, " ")
	switch verdict {
	case "ok":
		return SDRStatus{Known: true, OK: true}
	case "unavailable":
		return SDRStatus{Known: true, OK: false, Reason: reason}
	case "pending":
		// Not a verdict yet: the Steam API is still initialising.
		return SDRStatus{Known: false, Reason: reason}
	}
	return SDRStatus{Known: true, OK: false, Reason: "unrecognised status " + strings.ToValidUTF8(text, "?")}
}

// errNoTransport is the honest failure: nothing we offer can carry this lobby.
var errNoTransport = errors.New("no usable transport")

// chooseTransport decides a lobby's transport at creation:
//
//   - a verified public endpoint (nat.Endpoint.Reachable) keeps the direct
//     relay, which needs no third party at all;
//   - otherwise SDR, unless the shim has already reported that SDR cannot
//     work in this game process, in which case the lobby is refused with the
//     reasons rather than quietly created on a path that cannot connect.
//
// mode forces either transport; forcing SDR against a known-bad SDR is still
// refused. The returned string is the reason, for the log.
func chooseTransport(mode string, ep nat.Endpoint, epErr error, sdr SDRStatus) (Transport, string, error) {
	endpoint := func() string {
		if epErr != nil {
			return "public endpoint unknown (" + epErr.Error() + ")"
		}
		if ep.Reachable {
			return fmt.Sprintf("public endpoint %s reachable (via %s)", ep.Addr, ep.Source)
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

	if epErr == nil && ep.Reachable {
		return TransportDirect, endpoint(), nil
	}
	if sdr.Known && !sdr.OK {
		return TransportDirect, "", fmt.Errorf("%w: %s; %s", errNoTransport, endpoint(), sdrState())
	}
	return TransportSDR, endpoint() + "; " + sdrState(), nil
}
