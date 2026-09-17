package master

import (
	"bufio"
	"crypto/rand"
	"crypto/tls"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"io"
	"log"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/install"
)

// wsClient is the minimum of a masked RFC6455 client needed to drive the
// master the way the game does.
type wsClient struct {
	c   net.Conn
	br  *bufio.Reader
	uid int
}

func dialMaster(t *testing.T, addr string) *wsClient {
	t.Helper()
	var c net.Conn
	var err error
	for i := 0; i < 100; i++ { // the listener starts in a goroutine
		c, err = tls.Dial("tcp", addr, &tls.Config{InsecureSkipVerify: true})
		if err == nil {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if err != nil {
		t.Fatal(err)
	}
	var key [16]byte
	if _, err := rand.Read(key[:]); err != nil {
		t.Fatal(err)
	}
	req := "GET / HTTP/1.1\r\nHost: master.shirogames.com\r\nUpgrade: websocket\r\n" +
		"Connection: Upgrade\r\nSec-WebSocket-Key: " + base64.StdEncoding.EncodeToString(key[:]) +
		"\r\nSec-WebSocket-Version: 13\r\nX-Ident: wartales\r\nX-Pass: whatever\r\n\r\n"
	if _, err := c.Write([]byte(req)); err != nil {
		t.Fatal(err)
	}
	br := bufio.NewReader(c)
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			t.Fatal(err)
		}
		if strings.TrimSpace(line) == "" {
			break
		}
		if strings.HasPrefix(line, "HTTP/") && !strings.Contains(line, "101") {
			t.Fatalf("handshake refused: %s", line)
		}
	}
	return &wsClient{c: c, br: br}
}

// call sends a command and returns the reply's args.
func (w *wsClient) call(t *testing.T, cmd string, args any) json.RawMessage {
	t.Helper()
	w.uid++
	raw, err := json.Marshal(args)
	if err != nil {
		t.Fatal(err)
	}
	frame, err := json.Marshal(map[string]any{"uid": w.uid, "cmd": cmd, "args": json.RawMessage(raw)})
	if err != nil {
		t.Fatal(err)
	}
	w.writeMasked(t, frame)

	for {
		payload := w.readFrame(t)
		var e struct {
			UID  int             `json:"uid"`
			Cmd  string          `json:"cmd"`
			Args json.RawMessage `json:"args"`
		}
		if err := json.Unmarshal(payload, &e); err != nil {
			t.Fatalf("bad reply %q: %v", payload, err)
		}
		if e.UID != -w.uid {
			continue // a push; not what this call waits for
		}
		if e.Cmd == "err" {
			t.Fatalf("%s failed: %s", cmd, e.Args)
		}
		return e.Args
	}
}

func (w *wsClient) writeMasked(t *testing.T, payload []byte) {
	t.Helper()
	var head []byte
	if len(payload) < 126 {
		head = []byte{0x81, byte(0x80 | len(payload))}
	} else {
		head = []byte{0x81, 0x80 | 126, 0, 0}
		binary.BigEndian.PutUint16(head[2:], uint16(len(payload)))
	}
	var mask [4]byte
	if _, err := rand.Read(mask[:]); err != nil {
		t.Fatal(err)
	}
	body := append([]byte{}, payload...)
	for i := range body {
		body[i] ^= mask[i&3]
	}
	if _, err := w.c.Write(append(append(head, mask[:]...), body...)); err != nil {
		t.Fatal(err)
	}
}

func (w *wsClient) readFrame(t *testing.T) []byte {
	t.Helper()
	var h [2]byte
	if _, err := io.ReadFull(w.br, h[:]); err != nil {
		t.Fatal(err)
	}
	n := uint64(h[1] & 127)
	switch n {
	case 126:
		var b [2]byte
		if _, err := io.ReadFull(w.br, b[:]); err != nil {
			t.Fatal(err)
		}
		n = uint64(binary.BigEndian.Uint16(b[:]))
	case 127:
		var b [8]byte
		if _, err := io.ReadFull(w.br, b[:]); err != nil {
			t.Fatal(err)
		}
		n = binary.BigEndian.Uint64(b[:])
	}
	buf := make([]byte, n)
	if _, err := io.ReadFull(w.br, buf); err != nil {
		t.Fatal(err)
	}
	return buf
}

func startMaster(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	if err := install.EnsureCerts(dir); err != nil {
		t.Fatal(err)
	}
	cfg, err := install.TLSConfig(dir)
	if err != nil {
		t.Fatal(err)
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close() // only used to reserve a free port

	s := New(Options{
		Addr: addr, TLS: cfg, RelayPort: 14250,
		HostPW: "hpw", SlavePW: "spw",
		Log:        log.New(io.Discard, "", 0),
		PublicAddr: func() (string, error) { return "203.0.113.7:14250", nil },
	})
	go func() { _ = s.ListenAndServe() }() // returns when t.Cleanup closes the server
	t.Cleanup(s.Close)
	return addr
}

func TestLoginAndInstanceGet(t *testing.T) {
	addr := startMaster(t)
	w := dialMaster(t, addr)
	defer func() { _ = w.c.Close() }()

	var login struct {
		SID     string  `json:"sid"`
		Version int     `json:"version"`
		Time    float64 `json:"time"`
	}
	raw := w.call(t, "user/login", map[string]any{
		"name": "Tester", "token": "1527950@x", "uid": "S0011223344556677", "version": 2,
	})
	if err := json.Unmarshal(raw, &login); err != nil {
		t.Fatal(err)
	}
	if login.SID == "" || login.SID[0] != 'X' {
		t.Fatalf("sid = %q, want a Session id", login.SID)
	}
	if login.Version != 2 || login.Time <= 0 {
		t.Fatalf("login reply = %s", raw)
	}

	// instance/get must route the host onto our local relay.
	var inst struct {
		ServerID string `json:"serverID"`
		Answer   struct {
			HostPW  string `json:"hostpw"`
			SlavePW string `json:"slavepw"`
		} `json:"serverStartAnswer"`
	}
	raw = w.call(t, "instance/get", map[string]any{
		"game": "p2p", "version": "*", "zone": "eu", "startParams": "{}", "clientVersion": 1,
	})
	if err := json.Unmarshal(raw, &inst); err != nil {
		t.Fatal(err)
	}
	if inst.ServerID != "R127.0.0.1:14250" {
		t.Fatalf("serverID = %q", inst.ServerID)
	}
	if inst.Answer.HostPW != "hpw" || inst.Answer.SlavePW != "spw" {
		t.Fatalf("serverStartAnswer = %s", raw)
	}
}

func TestLobbyLifecycle(t *testing.T) {
	addr := startMaster(t)
	w := dialMaster(t, addr)
	defer func() { _ = w.c.Close() }()

	w.call(t, "user/login", map[string]any{"name": "Host", "uid": "S00", "version": 2})

	var id string
	raw := w.call(t, "lobby/create", map[string]any{
		"props": map[string]any{
			"data":       map[string]any{"version": `"1.2.3"`},
			"isPrivate":  false,
			"maxPlayers": 4,
			"myData":     `{"ready":false}`,
		},
	})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatalf("lobby/create reply %s: %v", raw, err)
	}
	if id == "" || id[0] != 'L' {
		t.Fatalf("lobby id = %q, first char must be a platform char", id)
	}

	raw = w.call(t, "lobby/info", map[string]any{"id": id})
	var info struct {
		ID    string `json:"id"`
		Owner string `json:"owner"`
		Users []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		} `json:"users"`
	}
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if info.ID != id || info.Owner != "S00" || len(info.Users) != 1 || info.Users[0].Name != "Host" {
		t.Fatalf("lobby/info = %s", raw)
	}

	raw = w.call(t, "lobby/makeShortCode", map[string]any{"id": id})
	var short struct {
		ShortCode string `json:"shortCode"`
	}
	if err := json.Unmarshal(raw, &short); err != nil {
		t.Fatal(err)
	}
	if len(short.ShortCode) != 13 {
		t.Fatalf("shortCode = %q", short.ShortCode)
	}

	// our own code resolves locally, back to the same lobby
	raw = w.call(t, "lobby/resolveShortCode", map[string]any{"shortCode": short.ShortCode, "filters": map[string]any{}})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if info.ID != id {
		t.Fatalf("resolveShortCode = %s", raw)
	}
}
