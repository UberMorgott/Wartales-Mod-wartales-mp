package master

import (
	"encoding/json"
	"io"
	"log"
	"net/url"
	"strconv"
	"strings"
	"testing"

	"github.com/UberMorgott/wartales-mp/internal/modver"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge/sdrbridgetest"
)

// unhaxe is haxe.Unserializer for one "y<len>:<urlencoded>" string, decoding
// like HashLink's url_decode ('+' is a space, %XX are UTF-8 bytes).
func unhaxe(t *testing.T, s string) string {
	t.Helper()
	rest, ok := strings.CutPrefix(s, "y")
	n, body, ok2 := strings.Cut(rest, ":")
	l, err := strconv.Atoi(n)
	if !ok || !ok2 || err != nil || l != len(body) {
		t.Fatalf("not a haxe string: %q", s)
	}
	out, err := url.QueryUnescape(body)
	if err != nil {
		t.Fatalf("%q: %v", s, err)
	}
	return out
}

func TestHaxeSerializeRoundTrip(t *testing.T) {
	for _, s := range []string{"", "1.0.48274", "a+b %41 c", modver.Message([]string{"res1.pak", "winmm.dll"})} {
		enc := haxeSerialize(s)
		for _, c := range enc[strings.IndexByte(enc, ':')+1:] {
			if c > 127 || c == '+' {
				t.Fatalf("%q: unencoded %q", enc, c)
			}
		}
		if got := unhaxe(t, enc); got != s {
			t.Fatalf("round trip %q -> %q -> %q", s, enc, got)
		}
	}
	if got := haxeSerialize("ab1"); got != "y3:ab1" {
		t.Fatalf("haxeSerialize(ab1) = %q", got)
	}
}

func quietServer(mod *modver.Info) *Server {
	return New(Options{Log: log.New(io.Discard, "", 0), Mod: modver.Fixed(mod)})
}

var (
	modA = &modver.Info{Build: "aaaa", DLL: "dll-a", Res1: "res-a"}
	modB = &modver.Info{Build: "bbbb", DLL: "dll-a", Res1: "res-b"}
)

func lobbyRaw(t *testing.T, host *modver.Info, data map[string]string) json.RawMessage {
	t.Helper()
	info := map[string]any{"id": "L1", "owner": "X1", "props": map[string]any{"data": data, "maxPlayers": 4}}
	if host != nil {
		info[modField] = host
	}
	b, err := json.Marshal(info)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func decodeInfo(t *testing.T, raw json.RawMessage) (map[string]json.RawMessage, map[string]string) {
	t.Helper()
	var info map[string]json.RawMessage
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatalf("%s: %v", raw, err)
	}
	var props struct {
		Data       map[string]string `json:"data"`
		MaxPlayers int               `json:"maxPlayers"`
	}
	if err := json.Unmarshal(info["props"], &props); err != nil || props.Data == nil || props.MaxPlayers != 4 {
		t.Fatalf("props %s: %v", info["props"], err)
	}
	return info, props.Data
}

func TestCheckHostMod(t *testing.T) {
	data := map[string]string{"version": "y9:1.0.48274", "beta": "f"}

	t.Run("match strips the field", func(t *testing.T) {
		out, bad := quietServer(modA).checkHostMod(lobbyRaw(t, modA, data))
		info, d := decodeInfo(t, out)
		if bad || info[modField] != nil || d["version"] != "y9:1.0.48274" || d[modErrorField] != "" {
			t.Fatalf("match: bad=%v %s", bad, out)
		}
	})
	t.Run("mismatch blocks with the message", func(t *testing.T) {
		out, bad := quietServer(modA).checkHostMod(lobbyRaw(t, modB, data))
		info, d := decodeInfo(t, out)
		if !bad || info[modField] != nil {
			t.Fatalf("mismatch: bad=%v %s", bad, out)
		}
		if got := unhaxe(t, d["version"]); got != modMismatchVersion {
			t.Fatalf("version = %q", got)
		}
		want := modver.Message([]string{modver.FileRes1})
		if got := unhaxe(t, d[modErrorField]); got != want {
			t.Fatalf("error text = %q, want %q", got, want)
		}
		if d["beta"] != "f" || string(info["id"]) != `"L1"` {
			t.Fatalf("other fields changed: %s", out)
		}
	})
	t.Run("older host passes through", func(t *testing.T) {
		raw := lobbyRaw(t, nil, data)
		out, bad := quietServer(modA).checkHostMod(raw)
		if bad || string(out) != string(raw) {
			t.Fatalf("older host: bad=%v %s", bad, out)
		}
	})
	t.Run("own fingerprint unknown never blocks", func(t *testing.T) {
		out, bad := quietServer(nil).checkHostMod(lobbyRaw(t, modB, data))
		if info, _ := decodeInfo(t, out); bad || info[modField] != nil {
			t.Fatalf("unknown: bad=%v %s", bad, out)
		}
	})
	t.Run("a host cannot inject the error", func(t *testing.T) {
		evil := map[string]string{"version": "y9:1.0.48274", modErrorField: "y4:evil"}
		out, bad := quietServer(nil).checkHostMod(lobbyRaw(t, nil, evil))
		if _, d := decodeInfo(t, out); bad || d[modErrorField] != "" {
			t.Fatalf("foreign error kept: %s", out)
		}
	})
	t.Run("null answer", func(t *testing.T) {
		if out, bad := quietServer(modA).checkHostMod(json.RawMessage("null")); bad || string(out) != "null" {
			t.Fatalf("null: %s", out)
		}
	})
}

// joinWithMods resolves a code from a guest with guestMod to a host with
// hostMod over the real cascade and returns the LobbyInfo the game got and
// whether its lobby/join then reached the host's lobby.
func joinWithMods(t *testing.T, hostMod, guestMod *modver.Info) (json.RawMessage, bool) {
	t.Helper()
	sw := sdrbridgetest.New(t)
	hostSrv, linkAddr := cascadeHost(t, sw, cascadeHostID, true)
	hostSrv.opt.Mod = modver.Fixed(hostMod)
	_, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, linkAddr, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000007, false, func(o *Options) { o.Mod = modver.Fixed(guestMod) })
	raw := guest.call(t, "lobby/resolveShortCode", map[string]any{"shortCode": short, "filters": map[string]any{}})
	uid := guest.send(t, "lobby/join", map[string]any{"id": id})
	_, errText := guest.await(t, uid)
	return raw, errText == ""
}

func TestJoinWithSameModFiles(t *testing.T) {
	raw, joined := joinWithMods(t, modA, modA)
	info, _ := decodeInfo(t, raw)
	if info[modField] != nil || !joined {
		t.Fatalf("same files: joined=%v %s", joined, raw)
	}
}

func TestJoinWithDifferentModFilesIsRefused(t *testing.T) {
	raw, joined := joinWithMods(t, modA, modB)
	_, d := decodeInfo(t, raw)
	if got := unhaxe(t, d[modErrorField]); got != modver.Message([]string{modver.FileRes1}) {
		t.Fatalf("error text = %q (%s)", got, raw)
	}
	if joined {
		t.Fatal("the link was kept: lobby/join still reached the host")
	}
}

func TestJoinWithOlderHostIsNotBlocked(t *testing.T) {
	raw, joined := joinWithMods(t, nil, modB)
	info, d := decodeInfo(t, raw)
	if info[modField] != nil || d[modErrorField] != "" || !joined {
		t.Fatalf("older host: joined=%v %s", joined, raw)
	}
}
