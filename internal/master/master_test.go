package master

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/tls"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"io"
	"log"
	"math"
	"net"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/install"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/uid"
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
	//nolint:gosec // the test dials the self-signed cert install.EnsureCerts just generated
	d := tls.Dialer{Config: &tls.Config{InsecureSkipVerify: true}}
	for range 100 { // the listener starts in a goroutine
		c, err = d.DialContext(context.Background(), "tcp", addr)
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
	if len(payload) > math.MaxUint16 {
		t.Fatalf("payload of %d bytes needs a 64 bit length", len(payload))
	}
	n := uint16(len(payload)) //nolint:gosec // the length is bounded by the check above
	var head []byte
	if n < 126 {
		head = []byte{0x81, 0x80 | byte(n)}
	} else {
		head = []byte{0x81, 0x80 | 126, 0, 0}
		binary.BigEndian.PutUint16(head[2:], n)
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

// readPush waits for a server initiated command and answers it the way the
// game does ({"uid": -n, "args": true}).
func (w *wsClient) readPush(t *testing.T, cmd string) json.RawMessage {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		_ = w.c.SetReadDeadline(deadline)
		payload := w.readFrame(t)
		var e struct {
			UID  int             `json:"uid"`
			Cmd  string          `json:"cmd"`
			Args json.RawMessage `json:"args"`
		}
		if err := json.Unmarshal(payload, &e); err != nil {
			t.Fatalf("bad frame %q: %v", payload, err)
		}
		if e.UID <= 0 || e.Cmd == "" {
			continue // a reply to one of our own calls
		}
		reply, err := json.Marshal(map[string]any{"uid": -e.UID, "args": true})
		if err != nil {
			t.Fatal(err)
		}
		w.writeMasked(t, reply)
		if e.Cmd == cmd {
			_ = w.c.SetReadDeadline(time.Time{})
			return e.Args
		}
	}
	t.Fatalf("no %s push arrived", cmd)
	return nil
}

// expectNoPush fails if anything at all is pushed within d.
func (w *wsClient) expectNoPush(t *testing.T, d time.Duration) {
	t.Helper()
	_ = w.c.SetReadDeadline(time.Now().Add(d))
	defer func() { _ = w.c.SetReadDeadline(time.Time{}) }()
	var h [2]byte
	if _, err := io.ReadFull(w.br, h[:]); err != nil {
		return // timed out: nothing was pushed, which is what we want
	}
	t.Fatal("a frame arrived, but the sender must not see its own lobby/chat")
}

// publicEndpoint is the verified-reachable host: the direct relay is chosen.
var publicEndpoint = nat.Endpoint{Addr: "203.0.113.7:14250", IP: net.IPv4(203, 0, 113, 7), Source: "UPnP", Reachable: true}

// lanEndpoint is a host nobody on the internet can reach: SDR is chosen.
var lanEndpoint = nat.Endpoint{Addr: "192.168.1.5:14250", IP: net.IPv4(192, 168, 1, 5), Source: "LAN", Reachable: false,
	Warning: "no public address found"}

func startMaster(t *testing.T) string {
	t.Helper()
	return startMasterWith(t, publicEndpoint, SDRStatus{Known: true, OK: true})
}

func startMasterWith(t *testing.T, ep nat.Endpoint, sdr SDRStatus) string {
	t.Helper()
	dir := t.TempDir()
	if err := install.EnsureCerts(dir); err != nil {
		t.Fatal(err)
	}
	cfg, err := install.TLSConfig(dir)
	if err != nil {
		t.Fatal(err)
	}
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close() // only used to reserve a free port

	s := New(Options{
		Addr: addr, TLS: cfg, RelayPort: 14250,
		HostPW: "hpw", SlavePW: "spw",
		Log:        log.New(io.Discard, "", 0),
		PublicAddr: func() (string, error) { return ep.Addr, nil },
		Endpoint:   func() (nat.Endpoint, error) { return ep, nil },
		SDRStatus:  func() SDRStatus { return sdr },
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

// callErr sends a command and returns the "err" text, failing the test when
// the command succeeds.
func (w *wsClient) callErr(t *testing.T, cmd string, args any) string {
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
			continue
		}
		if e.Cmd != "err" {
			t.Fatalf("%s succeeded (%s), want an error", cmd, e.Args)
		}
		var msg string
		_ = json.Unmarshal(e.Args, &msg)
		return msg
	}
}

// TestEmittedUIDsAreNotSteamShaped guards the trap behind
// Lobby.isSteamOnly@24596 for a lobby on the DIRECT relay: it returns true
// when EVERY member id of a lobby starts with 'S', and
// Lobby.setupPlatform@24597 then never sends instance/get, so the game takes
// its Steam path and our relay is bypassed. Nothing the master emits for such
// a lobby may therefore carry a Steam shaped id, whatever the clients report
// about themselves.
func TestEmittedUIDsAreNotSteamShaped(t *testing.T) {
	addr := startMaster(t)

	// every id the server put on the wire during this test
	var emitted []string
	check := func(what, id string) {
		t.Helper()
		if id == "" {
			t.Fatalf("%s: empty id", what)
		}
		if id[0] != 'X' {
			t.Fatalf("%s = %q, want a Session ('X') id", what, id)
		}
		emitted = append(emitted, id)
	}

	host := dialMaster(t, addr)
	defer func() { _ = host.c.Close() }()
	var login struct {
		SID string `json:"sid"`
	}
	raw := host.call(t, "user/login", map[string]any{
		"name": "Host", "uid": "S0011223344556677", "version": 2,
	})
	if err := json.Unmarshal(raw, &login); err != nil {
		t.Fatal(err)
	}
	check("user/login sid", login.SID)

	var id string
	raw = host.call(t, "lobby/create", map[string]any{
		"props": map[string]any{"data": map[string]any{}, "isPrivate": false, "maxPlayers": 4},
	})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}

	guest := dialMaster(t, addr)
	defer func() { _ = guest.c.Close() }()
	guest.call(t, "user/session", map[string]any{
		"name": "Guest", "uid": "S7766554433221100", "sid": login.SID, "version": 2,
	})

	var info struct {
		Owner string `json:"owner"`
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw = guest.call(t, "lobby/join", map[string]any{"id": id, "data": `{"ready":false}`})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if len(info.Users) != 2 {
		t.Fatalf("lobby/join returned %d users, want 2: %s", len(info.Users), raw)
	}
	check("lobby/join owner", info.Owner)
	for i, u := range info.Users {
		check("lobby/join users["+strconv.Itoa(i)+"].id", u.ID)
	}

	raw = host.call(t, "lobby/info", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if len(info.Users) != 2 {
		t.Fatalf("lobby/info returned %d users, want 2: %s", len(info.Users), raw)
	}
	check("lobby/info owner", info.Owner)
	for i, u := range info.Users {
		check("lobby/info users["+strconv.Itoa(i)+"].id", u.ID)
	}
	if info.Users[0].ID == info.Users[1].ID {
		t.Fatalf("both members share the id %q", info.Users[0].ID)
	}

	// what the client would compute: isSteamOnly is "every member is 'S'".
	steamOnly := true
	for _, u := range info.Users {
		if !strings.HasPrefix(u.ID, "S") {
			steamOnly = false
		}
	}
	if steamOnly {
		t.Fatal("isSteamOnly would be true: the game would bypass the relay")
	}
	for _, id := range emitted {
		if strings.HasPrefix(id, "S") || !uid.IsSession(id) {
			t.Fatalf("emitted id %q is not a Session id", id)
		}
	}
}

// TestSDRLobbyEmitsRealSteamIDs is the other side of the switch: when the
// host has no verified public endpoint the lobby runs over SDR, which means
// every id the master emits for it is the player's REAL Steam id (the game
// derives the peer's SteamID64 from it, UserID.hx:113), so that
// Lobby.isSteamOnly is true on every game and no instance/get is ever asked.
func TestSDRLobbyEmitsRealSteamIDs(t *testing.T) {
	addr := startMasterWith(t, lanEndpoint, SDRStatus{Known: true, OK: true})
	const hostSteam = "S0011223344556677"
	const guestSteam = "S7766554433221100"

	host := dialMaster(t, addr)
	defer func() { _ = host.c.Close() }()
	var login struct {
		SID string `json:"sid"`
	}
	raw := host.call(t, "user/login", map[string]any{"name": "Host", "uid": hostSteam, "version": 2})
	if err := json.Unmarshal(raw, &login); err != nil {
		t.Fatal(err)
	}
	if !uid.IsSession(login.SID) {
		t.Fatalf("sid = %q: the session id is never Steam shaped, whatever the lobby transport", login.SID)
	}

	var id string
	raw = host.call(t, "lobby/create", map[string]any{
		"props": map[string]any{"data": map[string]any{}, "isPrivate": false, "maxPlayers": 4},
	})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}

	guest := dialMaster(t, addr)
	defer func() { _ = guest.c.Close() }()
	guest.call(t, "user/login", map[string]any{"name": "Guest", "uid": guestSteam, "version": 2})

	var info struct {
		Owner string `json:"owner"`
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw = guest.call(t, "lobby/join", map[string]any{"id": id, "data": `{"ready":false}`})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if info.Owner != hostSteam || len(info.Users) != 2 || info.Users[0].ID != hostSteam || info.Users[1].ID != guestSteam {
		t.Fatalf("SDR lobby/join = %s, want owner %s and members %s, %s", raw, hostSteam, hostSteam, guestSteam)
	}
	for _, u := range info.Users {
		if !uid.IsSteam(u.ID) {
			t.Fatalf("member id %q is not a well-formed Steam id: isSteamOnly would be false", u.ID)
		}
	}

	// The pushes carry the same ids: the host's game keys its LobbyState by them.
	join := host.readPush(t, "lobby/join")
	var joinArgs struct {
		UID string `json:"uid"`
	}
	if err := json.Unmarshal(join, &joinArgs); err != nil {
		t.Fatal(err)
	}
	if joinArgs.UID != guestSteam {
		t.Fatalf("lobby/join push uid = %q, want %q", joinArgs.UID, guestSteam)
	}
	guest.call(t, "lobby/chat", map[string]any{"id": id, "msg": "y1:x"})
	chat := host.readPush(t, "lobby/chat")
	var chatArgs struct {
		UID string `json:"uid"`
	}
	if err := json.Unmarshal(chat, &chatArgs); err != nil {
		t.Fatal(err)
	}
	if chatArgs.UID != guestSteam {
		t.Fatalf("lobby/chat push uid = %q, want %q", chatArgs.UID, guestSteam)
	}

	// Ownership checks use the rendered id too.
	host.call(t, "lobby/setData", map[string]any{"id": id, "data": map[string]any{"k": `"v"`}})
	host.call(t, "lobby/transfer", map[string]any{"id": id, "uid": guestSteam})
	raw = host.call(t, "lobby/info", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if info.Owner != guestSteam {
		t.Fatalf("after transfer owner = %q, want %q", info.Owner, guestSteam)
	}
}

// TestSDRLobbyWithoutSteamIDFallsBackToSession: a member that reported no
// (well-formed) Steam id cannot be named on the Steam path, so it keeps its
// Session id, which honestly flips the lobby back to instance/get.
func TestSDRLobbyWithoutSteamIDFallsBackToSession(t *testing.T) {
	addr := startMasterWith(t, lanEndpoint, SDRStatus{})

	host := dialMaster(t, addr)
	defer func() { _ = host.c.Close() }()
	host.call(t, "user/login", map[string]any{"name": "Host", "uid": "S0011223344556677", "version": 2})
	var id string
	raw := host.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}

	guest := dialMaster(t, addr)
	defer func() { _ = guest.c.Close() }()
	guest.call(t, "user/login", map[string]any{"name": "Guest", "uid": "Snot-a-steam-id", "version": 2})
	var info struct {
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw = guest.call(t, "lobby/join", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &info); err != nil {
		t.Fatal(err)
	}
	if len(info.Users) != 2 || !uid.IsSteam(info.Users[0].ID) || !uid.IsSession(info.Users[1].ID) {
		t.Fatalf("lobby/join = %s, want the host's Steam id and a Session id for the guest", raw)
	}
}

// TestNoTransportIsRefused: no verified endpoint and the shim says SDR cannot
// work. The lobby is refused with the reasons instead of being created on a
// path that cannot connect; nothing ever routes to the legacy Steam relay.
func TestNoTransportIsRefused(t *testing.T) {
	addr := startMasterWith(t, lanEndpoint, SDRStatus{Known: true, OK: false, Reason: "steam_api64.dll is not loaded in this process"})
	host := dialMaster(t, addr)
	defer func() { _ = host.c.Close() }()
	host.call(t, "user/login", map[string]any{"name": "Host", "uid": "S0011223344556677", "version": 2})
	msg := host.callErr(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if !strings.Contains(msg, "no usable transport") || !strings.Contains(msg, "steam_api64.dll is not loaded") ||
		!strings.Contains(msg, "not internet-reachable") {
		t.Fatalf("refusal = %q, want the endpoint and the SDR reason", msg)
	}
}

// TestLobbyTransport drives the lobby transport exactly the way mpman does.
//
// mpman.net.LobbyService (mpman/net/LobbyService.hx, connectTo@54992) opens no
// socket of its own: HostWT.connect@12155 case 7 hands it the MPLobby that
// lobby/create returned, and every hxbit packet travels as a lobby/chat
// message whose "msg" is haxe.Serializer.run(LobbyMessageData.Packet(bytes,
// targetUid)) - see broadcastMessage@54947 and MPLobby.onCommand@54985. The
// server's whole job is the fan-out: deliver every lobby/chat to the other
// members verbatim, with the sender's uid attached, and never back to the
// sender. The receiver filters on the target uid inside the payload
// (onMessage@54929), so the server must not look into "msg" at all.
func TestLobbyTransport(t *testing.T) {
	addr := startMaster(t)

	host := dialMaster(t, addr)
	defer func() { _ = host.c.Close() }()
	host.call(t, "user/login", map[string]any{"name": "Host", "uid": "S00", "version": 2})

	var id string
	raw := host.call(t, "lobby/create", map[string]any{
		"props": map[string]any{"data": map[string]any{}, "isPrivate": false, "maxPlayers": 4},
	})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}
	hostUID := uid.Mint("S00")
	guestUID := uid.Mint("S01")

	guest := dialMaster(t, addr)
	defer func() { _ = guest.c.Close() }()
	guest.call(t, "user/login", map[string]any{"name": "Guest", "uid": "S01", "version": 2})
	guest.call(t, "lobby/join", map[string]any{"id": id, "data": `{"ready":false}`})

	// The host must learn about the joiner: LobbyService.onUserJoined@54927 is
	// what creates the per-guest LobbyUserService the packets are routed to.
	join := host.readPush(t, "lobby/join")
	var joinArgs struct {
		ID   string `json:"id"`
		UID  string `json:"uid"`
		Name string `json:"name"`
	}
	if err := json.Unmarshal(join, &joinArgs); err != nil {
		t.Fatal(err)
	}
	if joinArgs.ID != id || joinArgs.UID != guestUID || joinArgs.Name != "Guest" {
		t.Fatalf("lobby/join push = %s", join)
	}

	// A guest packet addressed to the lobby owner. The exact bytes are a
	// haxe.Serializer enum value; the server treats them as opaque text.
	up := `wy20:mpman.net.LobbyMessageDatay6:Packet:2s12:` +
		`/v8AAQIDBAUGBw==y` + strconv.Itoa(len(hostUID)) + `:` + hostUID
	guest.call(t, "lobby/chat", map[string]any{"id": id, "msg": up})

	got := host.readPush(t, "lobby/chat")
	var chat struct {
		ID  string `json:"id"`
		UID string `json:"uid"`
		Msg string `json:"msg"`
	}
	if err := json.Unmarshal(got, &chat); err != nil {
		t.Fatal(err)
	}
	if chat.ID != id {
		t.Fatalf("lobby/chat push id = %q, want %q", chat.ID, id)
	}
	if chat.UID != guestUID {
		t.Fatalf("lobby/chat push uid = %q, want the sender %q", chat.UID, guestUID)
	}
	if chat.Msg != up {
		t.Fatalf("lobby/chat push msg = %q, want it forwarded verbatim", chat.Msg)
	}

	// ... and the answer the host sends back through its LobbyUserService.
	down := `wy20:mpman.net.LobbyMessageDatay6:Packet:2s8:AAECAwQFBgc=y` +
		strconv.Itoa(len(guestUID)) + `:` + guestUID
	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": down})

	got = guest.readPush(t, "lobby/chat")
	if err := json.Unmarshal(got, &chat); err != nil {
		t.Fatal(err)
	}
	if chat.UID != hostUID || chat.Msg != down {
		t.Fatalf("lobby/chat push back to the guest = %s", got)
	}

	// The sender never sees its own packet: onMessage@54929 would hand it to
	// the wrong LobbyUserService.
	host.expectNoPush(t, 250*time.Millisecond)
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
	// The owner is the id we minted for "S00", never the Steam id itself.
	if info.ID != id || info.Owner != uid.Mint("S00") || len(info.Users) != 1 || info.Users[0].Name != "Host" {
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
