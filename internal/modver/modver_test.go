package modver

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"
)

func TestCompare(t *testing.T) {
	a := &Info{Build: "1", DLL: "d1", Res1: "r1"}
	for _, tc := range []struct {
		name              string
		local, remote     *Info
		differ, unchecked []string
		mismatch          bool
	}{
		{"same", a, &Info{Build: "2", DLL: "d1", Res1: "r1"}, nil, nil, false},
		{"res1 differs", a, &Info{DLL: "d1", Res1: "r2"}, []string{FileRes1}, nil, true},
		{"both differ", a, &Info{DLL: "d2", Res1: None}, []string{FileDLL, FileRes1}, nil, true},
		{"older remote", a, nil, nil, []string{FileDLL, FileRes1}, false},
		{"local unknown", nil, a, nil, []string{FileDLL, FileRes1}, false},
		{"one unknown", a, &Info{DLL: "", Res1: "r2"}, []string{FileRes1}, []string{FileDLL}, true},
		{"all unknown", &Info{}, &Info{}, nil, []string{FileDLL, FileRes1}, false},
	} {
		r := Compare(tc.local, tc.remote)
		if !reflect.DeepEqual(r.Differ, tc.differ) || !reflect.DeepEqual(r.Unchecked, tc.unchecked) || r.Mismatch() != tc.mismatch {
			t.Errorf("%s: %+v", tc.name, r)
		}
	}
}

func TestMessage(t *testing.T) {
	m := Message([]string{FileRes1, FileDLL})
	for _, want := range []string{"Версия мода отличается от хоста: скачай архив заново", "res1.pak, winmm.dll", "download the archive again"} {
		if !strings.Contains(m, want) {
			t.Errorf("%q lacks %q", m, want)
		}
	}
	if strings.ContainsAny(m, "\n<>") {
		t.Errorf("%q is not one plain line", m)
	}
}

func TestCompute(t *testing.T) {
	dir := t.TempDir()
	body := []byte("winmm build")
	if err := os.WriteFile(filepath.Join(dir, FileDLL), body, 0o600); err != nil {
		t.Fatal(err)
	}
	info, err := Compute(dir)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(body)
	if info.DLL != hex.EncodeToString(sum[:]) || info.Res1 != None {
		t.Fatalf("Compute = %+v", info)
	}

	unknown, err := Compute("")
	if err == nil || unknown.DLL != "" || unknown.Res1 != "" {
		t.Fatalf("Compute(\"\") = %+v, %v", unknown, err)
	}
}

func TestWireShape(t *testing.T) {
	b, err := json.Marshal(&Info{Build: "abc", DLL: "d", Res1: None})
	if err != nil || string(b) != `{"build":"abc","dll":"d","res1":"none"}` {
		t.Fatalf("%s %v", b, err)
	}
	if s := (*Info)(nil).String(); !strings.Contains(s, "older") {
		t.Fatalf("nil String = %q", s)
	}
}

func TestSession(t *testing.T) {
	var none *Session
	if none.Get(time.Second) != nil {
		t.Fatal("nil session has a fingerprint")
	}
	want := &Info{DLL: "x"}
	if Fixed(want).Get(0) != want {
		t.Fatal("Fixed lost its fingerprint")
	}
	dir := t.TempDir()
	s := NewSession(dir, func(string, ...any) {})
	if got := s.Get(10 * time.Second); got == nil || got.DLL != None || got.Res1 != None {
		t.Fatalf("NewSession = %+v", got)
	}
	slow := &Session{done: make(chan struct{})}
	if slow.Get(10*time.Millisecond) != nil {
		t.Fatal("a pending fingerprint must read as unknown")
	}
}
