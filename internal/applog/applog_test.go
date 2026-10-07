package applog

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

// TestTruncRedactsEverySecretKey: each secret key keeps its name and the
// value's length, never the value, in every payload shape Trunc is given.
func TestTruncRedactsEverySecretKey(t *testing.T) {
	for _, key := range secretKeys {
		const secret = "S3cr3tValue!"
		raw := `{"name":"Host","` + key + `":"` + secret + `","n":1}`
		for what, v := range map[string]any{
			"string":     raw,
			"bytes":      []byte(raw),
			"RawMessage": json.RawMessage(raw),
			"map":        map[string]any{key: secret, "name": "Host"},
			"error":      errors.New("bad frame " + raw),
		} {
			got := Trunc(v)
			if strings.Contains(got, secret) {
				t.Errorf("%s as %s: %s keeps the secret", key, what, got)
			}
			if !strings.Contains(got, `"`+key+`":"<redacted:12>"`) {
				t.Errorf("%s as %s: %s lacks the key with its length", key, what, got)
			}
			if !strings.Contains(got, `"name":"Host"`) {
				t.Errorf("%s as %s: %s lost a plain field", key, what, got)
			}
		}
	}
}

func TestRedactShapes(t *testing.T) {
	for _, c := range []struct{ in, want string }{
		// The user/login the game sends, and its reply.
		{`{"name":"Morgott","token":"1527950@x","uid":"S9f792402","version":2}`,
			`{"name":"Morgott","token":"<redacted:9>","uid":"S9f792402","version":2}`},
		{`{"perm":"","sid":"X797cEw566W91O4CZTsSjS0fyNA","time":1.5}`,
			`{"perm":"","sid":"<redacted:27>","time":1.5}`},
		// instance/get: nested relay passwords.
		{`{"serverID":"R1.2.3.4:5","serverStartAnswer":{"hostpw":"abcd","slavepw":"efghij"}}`,
			`{"serverID":"R1.2.3.4:5","serverStartAnswer":{"hostpw":"<redacted:4>","slavepw":"<redacted:6>"}}`},
		// Case, spacing and escapes inside the value.
		{`{"Token" : "a\"b\\c"}`, `{"Token" : "<redacted:7>"}`},
		{`{"HostPW":""}`, `{"HostPW":"<redacted:0>"}`},
		// A JSON document embedded as a string.
		{`{"data":"{\"ticket\":\"xyz\",\"a\":1}"}`, `{"data":"{\"ticket\":\"<redacted:3>\",\"a\":1}"}`},
		// Numbers and plain fields stay.
		{`{"code":0,"id":"L1"}`, `{"code":0,"id":"L1"}`},
		// Similar but different key names stay.
		{`{"tokens":"x","mysid":"y"}`, `{"tokens":"x","mysid":"y"}`},
	} {
		if got := Redact(c.in); got != c.want {
			t.Errorf("Redact(%s)\n got %s\nwant %s", c.in, got, c.want)
		}
	}
}

// TestTruncRedactsBeforeCutting: a secret straddling MaxValue is not half kept.
func TestTruncRedactsBeforeCutting(t *testing.T) {
	secret := strings.Repeat("Z", 100)
	raw := `{"pad":"` + strings.Repeat("a", MaxValue-20) + `","token":"` + secret + `"}`
	got := Trunc(raw)
	if strings.Contains(got, "ZZZ") {
		t.Fatalf("cut secret kept: %s", got)
	}
}

func TestPayloadRedactsBareInvite(t *testing.T) {
	if got := Payload("lobby/initInvite", json.RawMessage(`"ABCD-EFGH-IJKL"`)); got != `"<redacted:14>"` {
		t.Fatalf("initInvite reply = %s", got)
	}
	if got := Payload("lobby/list", json.RawMessage(`"plain"`)); got != `"plain"` {
		t.Fatalf("other bare string = %s", got)
	}
	if got := Payload("user/login", map[string]any{"token": "abc"}); got != `{"token":"<redacted:3>"}` {
		t.Fatalf("keyed = %s", got)
	}
}

func TestHeaderLineRedactsCredentials(t *testing.T) {
	got := HeaderLine(map[string]string{
		"host": "master.shirogames.com", "x-pass": "0123456789abcdef",
		"Authorization": "Bearer xyz", "cookie": "a=b", "x-ident": "wartales",
	})
	want := "Authorization=<redacted:10> cookie=<redacted:3> host=master.shirogames.com x-ident=wartales x-pass=<redacted:16>"
	if got != want {
		t.Fatalf("HeaderLine\n got %s\nwant %s", got, want)
	}
}

func TestSecret(t *testing.T) {
	if got := Secret("ABCDEFGH"); got != "<redacted:8>" {
		t.Fatalf("Secret = %s", got)
	}
}
