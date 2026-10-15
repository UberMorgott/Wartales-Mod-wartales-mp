package master

import (
	"encoding/json"
	"io"
	"log"
	"testing"
)

// The StartClients message exactly as a host's game sent it (harness run
// 2026-10-15, password replaced).
const startClients = "wy12:LobbyMessagey12:StartClients:1y45:W127.0.0.1%3A14251%3ALWAmcF78Bvk7d8jnJmGg8vX2"

func TestRewriteWServer(t *testing.T) {
	cases := []struct {
		name, msg, want string
		n               int
	}{
		{"StartClients", startClients,
			"wy12:LobbyMessagey12:StartClients:1y50:W192.168.77.108%3A14250%3ALWAmcF78Bvk7d8jnJmGg8vX2", 1},
		{"two ids", "y20:W127.0.0.1%3A1%3Apwxy20:W127.0.0.1%3A2%3Apwx",
			"y29:W192.168.77.108%3A14250%3Apwxy29:W192.168.77.108%3A14250%3Apwx", 2},
		{"other host", "y21:W10.0.0.1%3A14250%3Apw", "y21:W10.0.0.1%3A14250%3Apw", 0},
		{"length past the end", "y99:W127.0.0.1%3A14251%3Apw", "y99:W127.0.0.1%3A14251%3Apw", 0},
		{"lobby packet", "wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::v8y10:X29f792402", "wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::v8y10:X29f792402", 0},
	}
	for _, c := range cases {
		got, n := rewriteWServer(c.msg, "192.168.77.108", "14250")
		if got != c.want || n != c.n {
			t.Errorf("%s: got %q (%d), want %q (%d)", c.name, got, n, c.want, c.n)
		}
	}
}

func TestGuestRelayAddrKeepsOtherFields(t *testing.T) {
	s := &Server{opt: Options{Log: log.New(io.Discard, "", 0)}}
	in, _ := json.Marshal(map[string]any{"id": "L1D", "uid": "S1", "msg": startClients})
	var got struct {
		ID  string `json:"id"`
		UID string `json:"uid"`
		Msg string `json:"msg"`
	}
	if err := json.Unmarshal(s.guestRelayAddr(in, "192.168.77.108:14250"), &got); err != nil {
		t.Fatal(err)
	}
	if got.ID != "L1D" || got.UID != "S1" || got.Msg != "wy12:LobbyMessagey12:StartClients:1y50:W192.168.77.108%3A14250%3ALWAmcF78Bvk7d8jnJmGg8vX2" {
		t.Fatalf("got %+v", got)
	}
	if out := s.guestRelayAddr(in, "not an address"); string(out) != string(in) {
		t.Fatal("a bad route must leave the message alone")
	}
}
