package master

import (
	"encoding/json"
	"sync"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/uid"
)

// TestReconnectLobbyKeepsGameTransport: the in-game reconnect lobby
// (isReconnect) reuses the transport of the host's game lobby even when the
// verdict for a fresh lobby has changed meanwhile, so its member ids match
// the ids the game's players were saved with. A later game lobby decides
// afresh. Covers SDR -> endpoint verified and direct -> endpoint lost.
func TestReconnectLobbyKeepsGameTransport(t *testing.T) {
	const hostSteam = "S0011223344556677"
	for _, tc := range []struct {
		name         string
		first, later nat.Endpoint
		wantSteam    bool // the game transport renders Steam ids (SDR)
	}{
		{"sdr game, endpoint verified later", lanEndpoint, publicEndpoint, true},
		{"direct game, endpoint lost later", publicEndpoint, lanEndpoint, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var mu sync.Mutex
			ep := tc.first
			_, addr := startMasterOpts(t, func(o *Options) {
				o.PublicAddr = func() (string, error) { return publicEndpoint.Addr, nil }
				o.Endpoint = func() (nat.Endpoint, error) {
					mu.Lock()
					defer mu.Unlock()
					return ep, nil
				}
				o.SDRStatus = func() SDRStatus { return SDRStatus{Known: true, OK: true} }
			})
			host := dialMaster(t, addr)
			defer func() { _ = host.c.Close() }()
			host.call(t, "user/login", map[string]any{"name": "Host", "uid": hostSteam, "version": 2})

			create := func(reconnect bool) string {
				t.Helper()
				data := map[string]any{}
				if reconnect {
					data["isReconnect"] = "t"
				}
				var id string
				raw := host.call(t, "lobby/create", map[string]any{
					"props": map[string]any{"data": data, "maxPlayers": 4},
				})
				if err := json.Unmarshal(raw, &id); err != nil {
					t.Fatal(err)
				}
				return id
			}
			owner := func(id string) string {
				t.Helper()
				var info struct {
					Owner string `json:"owner"`
				}
				if err := json.Unmarshal(host.call(t, "lobby/info", map[string]any{"id": id}), &info); err != nil {
					t.Fatal(err)
				}
				return info.Owner
			}
			check := func(what, id string, steam bool) {
				t.Helper()
				if got := uid.IsSteam(owner(id)); got != steam {
					t.Fatalf("%s owner %q: Steam-shaped = %v, want %v", what, owner(id), got, steam)
				}
			}

			game := create(false)
			check("game lobby", game, tc.wantSteam)
			host.call(t, "lobby/leave", map[string]any{"id": game})

			mu.Lock()
			ep = tc.later
			mu.Unlock()

			check("reconnect lobby", create(true), tc.wantSteam)
			check("next game lobby", create(false), !tc.wantSteam)
		})
	}
}

func TestHaxeTrue(t *testing.T) {
	for raw, want := range map[string]bool{`"t"`: true, `true`: true, `"f"`: false, `false`: false, ``: false, `"x"`: false} {
		if got := haxeTrue(json.RawMessage(raw)); got != want {
			t.Errorf("haxeTrue(%s) = %v, want %v", raw, got, want)
		}
	}
}
