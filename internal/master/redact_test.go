package master

import (
	"bytes"
	"encoding/json"
	"log"
	"strings"
	"sync"
	"testing"
	"time"
)

// syncBuffer is a log sink the master goroutines and the test share.
type syncBuffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (s *syncBuffer) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

func (s *syncBuffer) String() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.String()
}

// TestLogRedactsSecrets drives the commands that carry secrets through a real
// master and checks the log keeps the key names and lengths, never the values:
// the Steam session token, the session id, the relay passwords and the join
// codes (manual and Steam invite).
func TestLogRedactsSecrets(t *testing.T) {
	var buf syncBuffer
	_, addr := startMasterOpts(t, func(o *Options) {
		o.Log = log.New(&buf, "", 0)
		o.HostPW, o.SlavePW = "HOSTPWsecret01", "SLAVEPWsecret02"
	})
	w := dialMaster(t, addr)
	defer func() { _ = w.c.Close() }()

	const token = "140000001234abcdSTEAMTOKEN@x"
	raw := w.call(t, "user/login", map[string]any{"name": "Host", "token": token, "uid": "S4e61bc0000000000", "version": 2})
	var login struct {
		SID string `json:"sid"`
	}
	if err := json.Unmarshal(raw, &login); err != nil || login.SID == "" {
		t.Fatalf("login = %s, %v", raw, err)
	}
	w.call(t, "user/session", map[string]any{"sid": login.SID, "uid": "S4e61bc0000000000"})
	w.call(t, "instance/get", map[string]any{"game": "p2p", "version": "*"})
	var id string
	if err := json.Unmarshal(w.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}}), &id); err != nil {
		t.Fatal(err)
	}
	var manual struct {
		ShortCode string `json:"shortCode"`
	}
	if err := json.Unmarshal(w.call(t, "lobby/makeShortCode", map[string]any{"id": id}), &manual); err != nil || manual.ShortCode == "" {
		t.Fatalf("makeShortCode: %+v, %v", manual, err)
	}
	var invite string
	if err := json.Unmarshal(w.call(t, "lobby/initInvite", map[string]any{"id": id}), &invite); err != nil || invite == "" {
		t.Fatalf("initInvite: %q, %v", invite, err)
	}
	w.call(t, "lobby/infoInvite", map[string]any{"invite": invite})

	// The replies are written after the log line; give the last one a moment.
	deadline := time.Now().Add(2 * time.Second)
	for !strings.Contains(buf.String(), "lobby/infoInvite ok") && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	out := buf.String()
	for name, secret := range map[string]string{
		"token": token, "sid": login.SID, "hostpw": "HOSTPWsecret01", "slavepw": "SLAVEPWsecret02",
		"shortCode": manual.ShortCode, "invite": invite,
	} {
		if strings.Contains(out, secret) {
			t.Errorf("the log holds the %s value %q:\n%s", name, secret, out)
		}
	}
	for _, want := range []string{
		`"token":"<redacted:28>"`,
		`"sid":"<redacted:`,
		`"hostpw":"<redacted:14>"`,
		`"slavepw":"<redacted:15>"`,
		`"shortCode":"<redacted:`,
		`lobby/initInvite ok "<redacted:`,
		`"invite":"<redacted:`,
		`join code for ` + id + ` is <redacted:`,
	} {
		if !strings.Contains(out, want) {
			t.Errorf("the log misses %s:\n%s", want, out)
		}
	}
}
