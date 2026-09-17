package master

import (
	"errors"
	"net"
	"strings"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/nat"
)

func TestParseSDRStatus(t *testing.T) {
	cases := []struct {
		in   string
		want SDRStatus
	}{
		{"", SDRStatus{}},
		{"ok\n", SDRStatus{Known: true, OK: true}},
		{"ok relay=100", SDRStatus{Known: true, OK: true}},
		{"unavailable steam_api64.dll is not loaded in this process\n",
			SDRStatus{Known: true, OK: false, Reason: "steam_api64.dll is not loaded in this process"}},
		{"pending Steam API not initialised yet (SteamAPI_GetHSteamUser() == 0)",
			SDRStatus{Known: false, Reason: "Steam API not initialised yet (SteamAPI_GetHSteamUser() == 0)"}},
	}
	for _, c := range cases {
		if got := ParseSDRStatus(c.in); got != c.want {
			t.Errorf("ParseSDRStatus(%q) = %+v, want %+v", c.in, got, c.want)
		}
	}
	if got := ParseSDRStatus("garbage here"); !got.Known || got.OK {
		t.Errorf("unrecognised status must be a known failure, got %+v", got)
	}
}

func TestChooseTransport(t *testing.T) {
	public := nat.Endpoint{Addr: "198.51.100.7:14250", IP: net.IPv4(198, 51, 100, 7), Source: "UPnP", Reachable: true}
	lan := nat.Endpoint{Addr: "192.168.1.5:14250", IP: net.IPv4(192, 168, 1, 5), Source: "LAN", Reachable: false,
		Warning: "no public address found: UPnP=192.168.0.1 (private (RFC1918)), STUN=none; the join code works on the LAN only"}
	sdrOK := SDRStatus{Known: true, OK: true}
	sdrBad := SDRStatus{Known: true, OK: false, Reason: "SteamNetworkingMessages002 unavailable"}
	sdrUnknown := SDRStatus{}

	cases := []struct {
		name    string
		mode    string
		ep      nat.Endpoint
		epErr   error
		sdr     SDRStatus
		want    Transport
		wantErr bool
		reason  string // substring the log line must carry
	}{
		{"auto, reachable endpoint -> direct", ModeAuto, public, nil, sdrOK, TransportDirect, false, "reachable (via UPnP)"},
		{"auto, reachable endpoint, SDR unknown -> direct", "", public, nil, sdrUnknown, TransportDirect, false, "reachable"},
		{"auto, reachable endpoint, SDR bad -> direct", ModeAuto, public, nil, sdrBad, TransportDirect, false, "reachable"},
		{"auto, LAN only, SDR ready -> SDR", ModeAuto, lan, nil, sdrOK, TransportSDR, false, "SDR ready"},
		{"auto, LAN only, SDR unknown -> SDR (optimistic)", ModeAuto, lan, nil, sdrUnknown, TransportSDR, false, "SDR status unknown"},
		{"auto, endpoint error, SDR ready -> SDR", ModeAuto, nat.Endpoint{}, errors.New("could not determine any address"), sdrOK, TransportSDR, false, "public endpoint unknown"},
		{"auto, LAN only, SDR bad -> refused", ModeAuto, lan, nil, sdrBad, TransportDirect, true, ""},
		{"auto, endpoint error, SDR bad -> refused", ModeAuto, nat.Endpoint{}, errors.New("nope"), sdrBad, TransportDirect, true, ""},
		{"direct forced, LAN only -> direct", ModeDirect, lan, nil, sdrOK, TransportDirect, false, "forced"},
		{"sdr forced, reachable -> SDR", ModeSDR, public, nil, sdrOK, TransportSDR, false, "forced"},
		{"sdr forced, SDR unknown -> SDR", ModeSDR, public, nil, sdrUnknown, TransportSDR, false, "forced"},
		{"sdr forced, SDR bad -> refused", ModeSDR, public, nil, sdrBad, TransportDirect, true, ""},
		{"unknown mode -> error", "legacy", public, nil, sdrOK, TransportDirect, true, ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, reason, err := chooseTransport(c.mode, c.ep, c.epErr, c.sdr)
			if (err != nil) != c.wantErr {
				t.Fatalf("err = %v, wantErr %v", err, c.wantErr)
			}
			if err != nil {
				if c.mode != "legacy" && !errors.Is(err, errNoTransport) {
					t.Fatalf("err = %v, want errNoTransport", err)
				}
				if c.mode != "legacy" && !strings.Contains(err.Error(), "SDR unavailable") {
					t.Fatalf("a refusal must name the SDR reason: %v", err)
				}
				return
			}
			if got != c.want {
				t.Fatalf("transport = %s, want %s (%s)", got, c.want, reason)
			}
			if !strings.Contains(reason, c.reason) {
				t.Fatalf("reason %q does not mention %q", reason, c.reason)
			}
		})
	}
}
