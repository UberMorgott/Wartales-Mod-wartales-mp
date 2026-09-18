// Package sdrbridge is the helper's end of the shim's SDR bridge: a loopback
// TCP connection into the game process through which bytes are sent to, and
// received from, other players over ISteamNetworkingMessages (Steam Datagram
// Relay) on the bridge's own channel. On top of it, per-peer byte streams
// (net.Conn) carry the proxy-link, so the master-to-master lobby traffic
// needs no public endpoint at all.
//
// The shim advertises the bridge in %LOCALAPPDATA%\wartales-mp\sdr.status:
//
//	ok bridge=127.0.0.1:<port> token=<32 hex>
//	pending <why>          the Steam API is still coming up
//	unavailable <why>      SDR cannot work in this game process
//
// Wire format on the socket, both directions (bridge.c):
//
//	[type:u8][peer SteamID64:u64 LE][len:u32 LE][payload]
//	0 AUTH  helper -> shim  the token; must be the first frame
//	1 SEND  helper -> shim  payload to peer, reliable, on the bridge channel
//	2 RECV  shim -> helper  payload from peer
//	3 ERR   shim -> helper  text; a SEND to peer that Steam refused
//
// Inside each SDR message the stream layer adds one byte: 1 = data, 2 = end
// of stream (the peer closed its side).
package sdrbridge

import (
	"os"
	"strings"
)

// Channel is the SDR channel the bridge uses. The game's SteamService only
// uses channel 0 and the shim serves it channels 0..7, so nothing the game
// does can reach this one.
const Channel = 100

// Status is the shim's verdict on the SDR transport.
type Status struct {
	Known  bool   // a verdict exists (ok or unavailable); false while pending or absent
	OK     bool   // SDR works in the game process
	Reason string // why not, or why still pending
	Bridge string // "127.0.0.1:port" when the shim listens for us
	Token  string // what the shim expects first
}

// ReadStatus parses the status file the shim writes; absent = unknown.
func ReadStatus(path string) Status {
	b, err := os.ReadFile(path) //nolint:gosec // the path is ours: %LOCALAPPDATA%\wartales-mp\sdr.status
	if err != nil {
		return Status{Reason: "shim has not reported yet (" + err.Error() + ")"}
	}
	return ParseStatus(string(b))
}

// ParseStatus parses the file's one line.
func ParseStatus(text string) Status {
	text = strings.TrimSpace(text)
	if text == "" {
		return Status{Reason: "empty status"}
	}
	verdict, rest, _ := strings.Cut(text, " ")
	switch verdict {
	case "ok":
		st := Status{Known: true, OK: true}
		for f := range strings.FieldsSeq(rest) {
			k, v, _ := strings.Cut(f, "=")
			switch k {
			case "bridge":
				if v != "none" {
					st.Bridge = v
				}
			case "token":
				st.Token = v
			}
		}
		if st.Bridge == "" {
			st.Reason = "the shim could not open its bridge listener"
		}
		return st
	case "unavailable":
		return Status{Known: true, Reason: rest}
	case "pending":
		return Status{Reason: rest}
	}
	return Status{Known: true, Reason: "unrecognised status " + strings.ToValidUTF8(text, "?")}
}
