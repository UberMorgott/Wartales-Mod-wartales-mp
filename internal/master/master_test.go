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
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/install"
	"github.com/UberMorgott/wartales-mp/internal/nat"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge"
	"github.com/UberMorgott/wartales-mp/internal/sdrbridge/sdrbridgetest"
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
var publicEndpoint = nat.Endpoint{Addr: "203.0.113.7:14250", IP: net.IPv4(203, 0, 113, 7), Source: "UPnP", Reachable: true, Verified: true}

// lanEndpoint is a host nobody on the internet can reach: SDR is chosen.
var lanEndpoint = nat.Endpoint{Addr: "192.168.1.5:14250", IP: net.IPv4(192, 168, 1, 5), Source: "LAN", Reachable: false,
	Warning: "no public address found"}

func startMaster(t *testing.T) string {
	t.Helper()
	return startMasterWith(t, publicEndpoint, SDRStatus{Known: true, OK: true})
}

func startMasterWith(t *testing.T, ep nat.Endpoint, sdr SDRStatus) string {
	t.Helper()
	_, addr := startMasterOpts(t, func(o *Options) {
		o.PublicAddr = func() (string, error) { return ep.Addr, nil }
		o.Endpoint = func() (nat.Endpoint, error) { return ep, nil }
		o.SDRStatus = func() SDRStatus { return sdr }
	})
	return addr
}

// startMasterOpts starts a master on a free port with the public endpoint
// verified reachable and SDR ready; mod adjusts the options.
func startMasterOpts(t *testing.T, mod func(*Options)) (*Server, string) {
	t.Helper()
	ep, sdr := publicEndpoint, SDRStatus{Known: true, OK: true}
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

	opt := Options{
		Addr: addr, TLS: cfg, RelayPort: 14250,
		HostPW: "hpw", SlavePW: "spw",
		Log:        log.New(io.Discard, "", 0),
		PublicAddr: func() (string, error) { return ep.Addr, nil },
		Endpoint:   func() (nat.Endpoint, error) { return ep, nil },
		SDRStatus:  func() SDRStatus { return sdr },
	}
	if mod != nil {
		mod(&opt)
	}
	// PublicAddr follows Endpoint unless a test set it explicitly.
	if !modifiedPublicAddr(opt) {
		opt.PublicAddr = func() (string, error) {
			e, err := opt.Endpoint()
			return e.Addr, err
		}
	}
	s := New(opt)
	go func() { _ = s.ListenAndServe() }() // returns when t.Cleanup closes the server
	t.Cleanup(s.Close)
	return s, addr
}

// modifiedPublicAddr reports whether PublicAddr disagrees with the default
// endpoint, i.e. a test set it on purpose.
func modifiedPublicAddr(opt Options) bool {
	addr, err := opt.PublicAddr()
	return err != nil || addr != publicEndpoint.Addr
}

// startBridge runs a bridge against a status file until the test ends and
// waits for it to connect (or not, when the status says SDR is unusable).
func startBridge(t *testing.T, statusPath string, wantReady bool) *sdrbridge.Bridge {
	t.Helper()
	b := sdrbridge.New(statusPath, log.New(io.Discard, "", 0))
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	go b.Run(ctx)
	if !wantReady {
		return b
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if ok, _ := b.Ready(); ok {
			return b
		}
		if time.Now().After(deadline) {
			_, why := b.Ready()
			t.Fatalf("bridge never became ready: %s", why)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// TestJoinOverSDR is the whole point of the SDR bridge: a host with NO public
// endpoint issues a join code that carries its SteamID, and a guest's master
// reaches the host's master through the (fake) SDR bridge, with no address
// anywhere. Both helpers sit behind their own fake shim identity on one
// switch.
func TestJoinOverSDR(t *testing.T) {
	sw := sdrbridgetest.New(t)
	const hostID64, guestID64 = uint64(0x0110000100BC614E), uint64(0x0110000100000007)
	hostSteam, guestSteam := uid.FromSteamID64(hostID64), uid.FromSteamID64(guestID64)

	hostBridge := startBridge(t, sw.Add(t, hostID64), true)
	hostSrv, hostAddr := startMasterOpts(t, func(o *Options) {
		o.Endpoint = func() (nat.Endpoint, error) { return lanEndpoint, nil }
		o.SDRStatus = hostBridge.Status
		o.Bridge = hostBridge
		o.LinkKey = 0xdeadbeef
	})
	hostBridge.OnPeer = hostSrv.ServeSDRLink

	guestBridge := startBridge(t, sw.Add(t, guestID64), true)
	_, guestAddr := startMasterOpts(t, func(o *Options) {
		o.Endpoint = func() (nat.Endpoint, error) { return lanEndpoint, nil }
		o.SDRStatus = guestBridge.Status
		o.Bridge = guestBridge
		o.LinkKey = 0x01020304 // its own key; irrelevant for joining
	})

	host := dialMaster(t, hostAddr)
	defer func() { _ = host.c.Close() }()
	host.call(t, "user/login", map[string]any{"name": "Host", "uid": hostSteam, "version": 2})
	var id string
	raw := host.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}
	var short struct {
		ShortCode string `json:"shortCode"`
	}
	raw = host.call(t, "lobby/makeShortCode", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &short); err != nil {
		t.Fatal(err)
	}
	// The host's endpoint is LAN-only, unverified: the code still offers it
	// first (a LAN guest would take it) and the SDR route after.
	sc, err := code.DecodeAny(short.ShortCode)
	if err != nil || sc.Steam == nil || sc.Steam.SteamID64() != hostID64 || sc.Steam.Key != 0xdeadbeef ||
		sc.Endpoint == nil || sc.Endpoint.Addr() != lanEndpoint.Addr {
		t.Fatalf("join code %q = %+v, %v; want SteamID %d key deadbeef plus %s", short.ShortCode, sc, err, hostID64, lanEndpoint.Addr)
	}

	guest := dialMaster(t, guestAddr)
	defer func() { _ = guest.c.Close() }()
	guest.call(t, "user/login", map[string]any{"name": "Guest", "uid": guestSteam, "version": 2})
	var info struct {
		ID    string `json:"id"`
		Owner string `json:"owner"`
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw = guest.call(t, "lobby/resolveShortCode", map[string]any{"shortCode": short.ShortCode, "filters": map[string]any{}})
	if err := json.Unmarshal(raw, &info); err != nil || info.ID != id || info.Owner != hostSteam {
		t.Fatalf("resolveShortCode over SDR = %s, %v", raw, err)
	}
	raw = guest.call(t, "lobby/join", map[string]any{"id": id, "data": `{"ready":false}`})
	if err := json.Unmarshal(raw, &info); err != nil || len(info.Users) != 2 ||
		info.Users[0].ID != hostSteam || info.Users[1].ID != guestSteam {
		t.Fatalf("lobby/join over SDR = %s, %v", raw, err)
	}
	// The push reached the host through the bridge as well.
	join := host.readPush(t, "lobby/join")
	if !strings.Contains(string(join), guestSteam) {
		t.Fatalf("lobby/join push = %s", join)
	}
	// And the lobby transport (lobby/chat) flows both ways over it.
	guest.call(t, "lobby/chat", map[string]any{"id": id, "msg": "y1:x"})
	if chat := host.readPush(t, "lobby/chat"); !strings.Contains(string(chat), guestSteam) {
		t.Fatalf("lobby/chat push = %s", chat)
	}

	// A code with the wrong key is refused by the host's master, not served.
	_, intruderAddr := startMasterOpts(t, func(o *Options) {
		b := startBridge(t, sw.Add(t, 0x0110000100000099), true)
		o.SDRStatus, o.Bridge = b.Status, b
	})
	intruder := dialMaster(t, intruderAddr)
	defer func() { _ = intruder.c.Close() }()
	intruder.call(t, "user/login", map[string]any{"name": "Intruder", "uid": uid.FromSteamID64(0x0110000100000099), "version": 2})
	tampered := code.EncodeSteam(code.Steam{AccountID: 0x00BC614E, Key: 0xdeadbeef ^ 1})
	msg := intruder.callErr(t, "lobby/resolveShortCode", map[string]any{"shortCode": tampered, "filters": map[string]any{}})
	if !strings.Contains(msg, "Invalid join code") {
		t.Fatalf("tampered code: %q, want the host's refusal", msg)
	}
}

// cascadeHost is a host master with both routes on offer: a real TCP
// proxy-link listener (what the relay's dispatch hands ServeLink) and an SDR
// identity on the switch. It returns the master, its TCP link address and
// the host's Steam id.
func cascadeHost(t *testing.T, sw *sdrbridgetest.Switch, hostID64 uint64, listen bool) (*Server, string) {
	t.Helper()
	hostBridge := startBridge(t, sw.Add(t, hostID64), true)
	hostSrv, _ := startMasterOpts(t, func(o *Options) {
		o.SDRStatus, o.Bridge, o.LinkKey = hostBridge.Status, hostBridge, 0xdeadbeef
	})
	hostBridge.OnPeer = hostSrv.ServeSDRLink
	if !listen {
		return hostSrv, ""
	}
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = ln.Close() })
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go hostSrv.ServeLink(c)
		}
	}()
	return hostSrv, ln.Addr().String()
}

// combinedCode builds the code a host at addr (or none) with the given key
// would issue.
func combinedCode(t *testing.T, addr string, hostID64 uint64, key uint32) string {
	t.Helper()
	account, _ := code.AccountID(hostID64)
	st := code.Steam{AccountID: account, Key: key}
	if addr == "" {
		return code.EncodeSteam(st)
	}
	tcp, err := net.ResolveTCPAddr("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	s, err := code.EncodeCombined(code.Endpoint{IP: tcp.IP, Port: uint16(tcp.Port)}, st) //nolint:gosec // a listener port
	if err != nil {
		t.Fatal(err)
	}
	return s
}

// guestFor starts a guest master on the switch with its own bridge (or an
// unusable one) and returns its game-side client, logged in.
func guestFor(t *testing.T, sw *sdrbridgetest.Switch, guestID64 uint64, sdrUsable bool, mod func(*Options)) *wsClient {
	t.Helper()
	var statusPath string
	if sdrUsable {
		statusPath = sw.Add(t, guestID64)
	} else {
		statusPath = sw.Unavailable(t, "steam_api64.dll is not loaded in this process")
	}
	guestBridge := startBridge(t, statusPath, sdrUsable)
	_, guestAddr := startMasterOpts(t, func(o *Options) {
		o.Endpoint = func() (nat.Endpoint, error) { return lanEndpoint, nil }
		o.SDRStatus, o.Bridge = guestBridge.Status, guestBridge
		if mod != nil {
			mod(o)
		}
	})
	guest := dialMaster(t, guestAddr)
	t.Cleanup(func() { _ = guest.c.Close() })
	guest.call(t, "user/login", map[string]any{"name": "Guest", "uid": uid.FromSteamID64(guestID64), "version": 2})
	return guest
}

// createLobby logs the host's game in and creates a lobby, returning its id.
func createLobby(t *testing.T, hostSrv *Server, hostID64 uint64) (*wsClient, string) {
	t.Helper()
	host := dialMaster(t, hostSrv.opt.Addr)
	t.Cleanup(func() { _ = host.c.Close() })
	host.call(t, "user/login", map[string]any{"name": "Host", "uid": uid.FromSteamID64(hostID64), "version": 2})
	var id string
	raw := host.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}
	return host, id
}

func resolveOK(t *testing.T, guest *wsClient, short, wantID string) {
	t.Helper()
	var info struct {
		ID string `json:"id"`
	}
	raw := guest.call(t, "lobby/resolveShortCode", map[string]any{"shortCode": short, "filters": map[string]any{}})
	if err := json.Unmarshal(raw, &info); err != nil || info.ID != wantID {
		t.Fatalf("resolveShortCode = %s, %v; want lobby %s", raw, err, wantID)
	}
}

const cascadeHostID = uint64(0x0110000100BC614E)

// TestCascadeDirectWins: the endpoint answers, so the guest joins over TCP
// even though its own SDR is unusable, and the host's code mapping is shared
// by both routes.
func TestCascadeDirectWins(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, linkAddr := cascadeHost(t, sw, cascadeHostID, true)
	_, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, linkAddr, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000007, false, nil)
	resolveOK(t, guest, short, id)
}

// TestLobbyTransportOverLink is TestLobbyTransport with the guest behind a
// proxy-link (direct TCP, the route of the first two-machine session): the
// guest's Join packet must reach the host's game as a lobby/chat push, and
// the host's answer must come back through the link to the guest's game.
func TestLobbyTransportOverLink(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, linkAddr := cascadeHost(t, sw, cascadeHostID, true)
	host, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, linkAddr, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000007, false, nil)
	resolveOK(t, guest, short, id)
	var info struct {
		Owner string `json:"owner"`
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw := guest.call(t, "lobby/join", map[string]any{"id": id, "data": "og"})
	if err := json.Unmarshal(raw, &info); err != nil || len(info.Users) != 2 {
		t.Fatalf("lobby/join over the link = %s, %v", raw, err)
	}
	hostUID, guestUID := info.Owner, info.Users[1].ID
	if join := host.readPush(t, "lobby/join"); !strings.Contains(string(join), guestUID) {
		t.Fatalf("lobby/join push = %s", join)
	}

	// Each game addresses the packet to the member id it knows, and must get
	// it back addressed to the id it calls its own (LobbyService.hx:101).
	hostGame, guestGame := uid.FromSteamID64(cascadeHostID), uid.FromSteamID64(0x0110000100000007)
	const packet = `wy26:mpman.net.LobbyMessageDatay6:Packet:2s12:/v8AAQIDBAUGBw==`
	guest.call(t, "lobby/chat", map[string]any{"id": id, "msg": packet + haxeString(hostUID)})
	var chat struct {
		ID  string `json:"id"`
		UID string `json:"uid"`
		Msg string `json:"msg"`
	}
	if err := json.Unmarshal(host.readPush(t, "lobby/chat"), &chat); err != nil {
		t.Fatal(err)
	}
	if chat.ID != id || chat.UID != guestUID || chat.Msg != packet+haxeString(hostGame) {
		t.Fatalf("lobby/chat push on the host = %+v, want id %s uid %s target %s", chat, id, guestUID, hostGame)
	}

	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": packet + haxeString(guestUID)})
	if err := json.Unmarshal(guest.readPush(t, "lobby/chat"), &chat); err != nil {
		t.Fatal(err)
	}
	if chat.ID != id || chat.UID != hostUID || chat.Msg != packet+haxeString(guestGame) {
		t.Fatalf("lobby/chat push on the guest = %+v, want id %s uid %s target %s", chat, id, hostUID, guestGame)
	}
	host.expectNoPush(t, 250*time.Millisecond)
}

// TestGuestPushSurvivesSecondSocketAndRelogin replays the guest of the
// 2026-09-18 two-machine session: at startup the game opens a second master
// socket (master2.shirogames.com) and drops it at once, and it sends
// user/login again before every command on the socket it keeps. The host's
// lobby/chat replies over the link must still reach that socket; they were
// all dropped with "no game is connected".
func TestGuestPushSurvivesSecondSocketAndRelogin(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, linkAddr := cascadeHost(t, sw, cascadeHostID, true)
	host, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, linkAddr, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000007, false, nil)
	second := dialMaster(t, guest.c.RemoteAddr().String())
	_ = second.c.Close() // master2: connected, never logs in, EOF at once
	time.Sleep(100 * time.Millisecond)

	login := map[string]any{"name": "Guest", "uid": uid.FromSteamID64(0x0110000100000007), "version": 2}
	guest.call(t, "user/login", login)
	resolveOK(t, guest, short, id)
	guest.call(t, "user/login", login)
	var info struct {
		Owner string `json:"owner"`
	}
	if err := json.Unmarshal(guest.call(t, "lobby/join", map[string]any{"id": id, "data": "og"}), &info); err != nil {
		t.Fatal(err)
	}
	host.readPush(t, "lobby/join")

	const packet = `wy26:mpman.net.LobbyMessageDatay6:Packet:2s3::P8`
	guest.call(t, "user/login", login)
	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": packet + haxeString(uid.FromSteamID64(0x0110000100000007))})
	var chat struct {
		ID  string `json:"id"`
		UID string `json:"uid"`
	}
	if err := json.Unmarshal(guest.readPush(t, "lobby/chat"), &chat); err != nil {
		t.Fatal(err)
	}
	if chat.ID != id || chat.UID != info.Owner {
		t.Fatalf("lobby/chat push on the guest = %+v, want id %s from %s", chat, id, info.Owner)
	}
}

// send fires a command without waiting for its reply and returns its uid.
func (w *wsClient) send(t *testing.T, cmd string, args any) int {
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
	return w.uid
}

// await reads frames until the reply to uid arrives, answering pushes on
// the way, and returns it as (args, error text).
func (w *wsClient) await(t *testing.T, uid int) (json.RawMessage, string) {
	t.Helper()
	for {
		payload := w.readFrame(t)
		var e struct {
			UID  int             `json:"uid"`
			Cmd  string          `json:"cmd"`
			Args json.RawMessage `json:"args"`
		}
		if err := json.Unmarshal(payload, &e); err != nil {
			t.Fatalf("bad frame %q: %v", payload, err)
		}
		if e.UID > 0 {
			reply, _ := json.Marshal(map[string]any{"uid": -e.UID, "args": true})
			w.writeMasked(t, reply)
			continue
		}
		if e.UID != -uid {
			continue
		}
		if e.Cmd == "err" {
			return nil, string(e.Args)
		}
		return e.Args, ""
	}
}

// TestGuestRejoinBurstKeepsMembership: the player clicks Join again on a
// lobby the game is already in. The game then fires lobby/leave, user/login
// and lobby/join back to back (guest log of 2026-09-18, #9-#11) without
// waiting for replies; the host must end up with the guest as a member
// exactly once, which needs the commands forwarded in the order they came.
func TestGuestRejoinBurstKeepsMembership(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, linkAddr := cascadeHost(t, sw, cascadeHostID, true)
	host, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, linkAddr, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guestID64 := uint64(0x0110000100000007)
	guest := guestFor(t, sw, guestID64, false, nil)
	resolveOK(t, guest, short, id)
	guest.call(t, "lobby/join", map[string]any{"id": id, "data": "og"})
	host.readPush(t, "lobby/join")

	login := map[string]any{"name": "Guest", "uid": uid.FromSteamID64(guestID64), "version": 2}
	guestUID := "" // the member id the host renders for the guest (a Session id: direct link)
	for range 5 {
		leave := guest.send(t, "lobby/leave", map[string]any{"id": id})
		guest.send(t, "user/login", login)
		join := guest.send(t, "lobby/join", map[string]any{"id": id, "data": "og"})
		if _, msg := guest.await(t, leave); msg != "" {
			t.Fatalf("lobby/leave: %s", msg)
		}
		raw, msg := guest.await(t, join)
		if msg != "" {
			t.Fatalf("lobby/join: %s", msg)
		}
		var info struct {
			Users []struct {
				ID string `json:"id"`
			} `json:"users"`
		}
		if err := json.Unmarshal(raw, &info); err != nil || len(info.Users) != 2 {
			t.Fatalf("lobby/join after leave = %s, %v; want host and guest", raw, err)
		}
		guestUID = info.Users[1].ID
	}
	l := hostSrv.lobbies.get(id)
	if l == nil {
		t.Fatal("the lobby vanished on the host")
	}
	hostSrv.lobbies.mu.Lock()
	var members []string
	for _, u := range l.users {
		members = append(members, u.ID)
	}
	hostSrv.lobbies.mu.Unlock()
	if len(members) != 2 || members[1] != guestUID {
		t.Fatalf("host lobby members = %v, want the host and the guest once", members)
	}
}

// haxeString is haxe.Serializer's encoding of a plain string.
func haxeString(s string) string { return "y" + strconv.Itoa(len(s)) + ":" + s }

// TestCascadeDirectRefusedFallsBackToSDR: the endpoint in the code refuses
// the connection (nothing listens there), so the guest falls through to SDR
// and still joins.
func TestCascadeDirectRefusedFallsBackToSDR(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, _ := cascadeHost(t, sw, cascadeHostID, false)
	_, id := createLobby(t, hostSrv, cascadeHostID)
	// A port nobody listens on: reserve one and close it.
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	dead := ln.Addr().String()
	_ = ln.Close()
	short := combinedCode(t, dead, cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000008, true, nil)
	start := time.Now()
	resolveOK(t, guest, short, id)
	if d := time.Since(start); d > 2*time.Second {
		t.Fatalf("a refused port must fail fast, took %v", d)
	}
}

// TestCascadeDirectTimesOutFallsBackToSDR: the endpoint swallows the SYN (a
// firewall), the bounded attempt expires, and the guest joins over SDR.
func TestCascadeDirectTimesOutFallsBackToSDR(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, _ := cascadeHost(t, sw, cascadeHostID, false)
	_, id := createLobby(t, hostSrv, cascadeHostID)
	short := combinedCode(t, "203.0.113.9:14250", cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	const timeout = 300 * time.Millisecond
	guest := guestFor(t, sw, 0x0110000100000009, true, func(o *Options) {
		o.DirectTimeout = timeout
		o.DialDirect = func(ctx context.Context, addr string) (net.Conn, error) {
			<-ctx.Done() // a black hole: nothing ever answers
			return nil, ctx.Err()
		}
	})
	start := time.Now()
	resolveOK(t, guest, short, id)
	if d := time.Since(start); d < timeout || d > timeout+2*time.Second {
		t.Fatalf("expected the direct attempt to last about %v before SDR, took %v", timeout, d)
	}
}

// TestCascadeWrongHostBehindTheEndpoint: the address in the code now answers
// as somebody else's helper (a reused WAN address). Its master refuses the
// key, the cascade drops that link and reaches the real host over SDR.
func TestCascadeWrongHostBehindTheEndpoint(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hostSrv, _ := cascadeHost(t, sw, cascadeHostID, false)
	_, id := createLobby(t, hostSrv, cascadeHostID)
	// The stranger's helper has its own (different) key, so the probe is refused.
	stranger, _ := startMasterOpts(t, func(o *Options) { o.LinkKey = 0x01020304 })
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = ln.Close() })
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go stranger.ServeLink(c)
		}
	}()
	short := combinedCode(t, ln.Addr().String(), cascadeHostID, 0xdeadbeef)
	hostSrv.lobbies.mu.Lock()
	hostSrv.lobbies.codes[short] = id
	hostSrv.lobbies.mu.Unlock()

	guest := guestFor(t, sw, 0x0110000100000010, true, nil)
	resolveOK(t, guest, short, id)
}

// TestCascadeBothFail: nothing works and the error names both routes.
func TestCascadeBothFail(t *testing.T) {
	sw := sdrbridgetest.New(t)
	var lc net.ListenConfig
	ln, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	dead := ln.Addr().String()
	_ = ln.Close()
	short := combinedCode(t, dead, 0x0110000100000999, 0xdeadbeef) // nobody on the switch has that id
	guest := guestFor(t, sw, 0x0110000100000011, true, nil)
	msg := guest.callErr(t, "lobby/resolveShortCode", map[string]any{"shortCode": short, "filters": map[string]any{}})
	if !strings.Contains(msg, "direct "+dead) || !strings.Contains(msg, "SDR to SteamID") || !strings.Contains(msg, "EResult 3") {
		t.Fatalf("both-fail error = %q", msg)
	}
}

// TestIssuedCodeCarriesBothRoutes: a host with an unverified endpoint and a
// ready bridge issues a combined code; the endpoint is offered but the lobby
// itself is on SDR, and a verified endpoint makes the lobby direct.
func TestIssuedCodeCarriesBothRoutes(t *testing.T) {
	sw := sdrbridgetest.New(t)
	hinted := nat.Endpoint{Addr: "45.154.88.66:14250", IP: net.IPv4(45, 154, 88, 66), Source: "UPnP+STUN", Reachable: true}
	bridge := startBridge(t, sw.Add(t, cascadeHostID), true)
	srv, _ := startMasterOpts(t, func(o *Options) {
		o.PublicAddr = func() (string, error) { return hinted.Addr, nil }
		o.Endpoint = func() (nat.Endpoint, error) { return hinted, nil }
		o.SDRStatus, o.Bridge, o.LinkKey = bridge.Status, bridge, 0xdeadbeef
	})
	host, id := createLobby(t, srv, cascadeHostID)
	if srv.lobbies.get(id).transport != TransportSDR {
		t.Fatal("an unverified endpoint must not put the lobby on the direct relay")
	}
	var short struct {
		ShortCode string `json:"shortCode"`
	}
	raw := host.call(t, "lobby/makeShortCode", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &short); err != nil {
		t.Fatal(err)
	}
	c, err := code.DecodeAny(short.ShortCode)
	if err != nil || c.Endpoint == nil || c.Steam == nil {
		t.Fatalf("code %q = %+v, %v; want both routes", short.ShortCode, c, err)
	}
	if c.Endpoint.Addr() != hinted.Addr || c.Steam.SteamID64() != cascadeHostID || c.Steam.Key != 0xdeadbeef {
		t.Fatalf("routes = %s / %d key %x", c.Endpoint.Addr(), c.Steam.SteamID64(), c.Steam.Key)
	}
}

// TestSDRJoinBeforeTheBridgeIsReady: a join code request or a join attempt
// made while the shim is still bringing SDR up is answered with a clear,
// retryable error, never a code or a link that could not work.
func TestSDRJoinBeforeTheBridgeIsReady(t *testing.T) {
	sw := sdrbridgetest.New(t)
	pending := filepath.Join(t.TempDir(), "sdr.status")
	if err := os.WriteFile(pending, []byte("pending Steam API not initialised yet\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	bridge := startBridge(t, pending, false)
	_, addr := startMasterOpts(t, func(o *Options) {
		o.Endpoint = func() (nat.Endpoint, error) { return lanEndpoint, nil }
		o.SDRStatus, o.Bridge = bridge.Status, bridge
	})
	w := dialMaster(t, addr)
	defer func() { _ = w.c.Close() }()
	w.call(t, "user/login", map[string]any{"name": "Host", "uid": "S4e61bc0000000000", "version": 2})
	var id string
	raw := w.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatalf("lobby/create must succeed with SDR pending: %s, %v", raw, err)
	}
	msg := w.callErr(t, "lobby/makeShortCode", map[string]any{"id": id})
	if !strings.Contains(msg, "Steam relay not ready yet") || !strings.Contains(msg, "not initialised") {
		t.Fatalf("makeShortCode while pending = %q", msg)
	}
	steamCode := code.EncodeSteam(code.Steam{AccountID: 1, Key: 2})
	msg = w.callErr(t, "lobby/resolveShortCode", map[string]any{"shortCode": steamCode, "filters": map[string]any{}})
	if !strings.Contains(msg, "Steam relay not ready yet") {
		t.Fatalf("resolveShortCode while pending = %q", msg)
	}

	// Once the shim reports the bridge, the same lobby gets its code.
	ready := sw.Add(t, 0x0110000100BC614E)
	data, err := os.ReadFile(ready) //nolint:gosec // a test temp file
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(pending, data, 0o600); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if ok, _ := bridge.Ready(); ok {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("bridge did not pick the new status up")
		}
		time.Sleep(20 * time.Millisecond)
	}
	var short struct {
		ShortCode string `json:"shortCode"`
	}
	raw = w.call(t, "lobby/makeShortCode", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &short); err != nil {
		t.Fatal(err)
	}
	if c, err := code.DecodeAny(short.ShortCode); err != nil || c.Steam == nil {
		t.Fatalf("makeShortCode once ready = %s: %+v, %v; want an SDR route", raw, c, err)
	}
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

	// A guest packet addressed to the lobby owner by the member id the guest
	// knows. The bytes are a haxe.Serializer enum value the server leaves
	// alone; only the target is re-addressed to the id the host's game calls
	// its own ("S00", its user/login uid), or LobbyService.hx:101 drops it.
	const packet = `wy26:mpman.net.LobbyMessageDatay6:Packet:2s12:/v8AAQIDBAUGBw==`
	up := packet + haxeString(hostUID)
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
	if chat.Msg != packet+haxeString("S00") {
		t.Fatalf("lobby/chat push msg = %q, want the packet re-addressed to S00", chat.Msg)
	}

	// ... and the answer the host sends back through its LobbyUserService,
	// addressed to the guest's member id, arrives addressed to "S01".
	const answer = `wy26:mpman.net.LobbyMessageDatay6:Packet:2s8:AAECAwQFBgc=`
	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": answer + haxeString(guestUID)})

	got = guest.readPush(t, "lobby/chat")
	if err := json.Unmarshal(got, &chat); err != nil {
		t.Fatal(err)
	}
	if chat.UID != hostUID || chat.Msg != answer+haxeString("S01") {
		t.Fatalf("lobby/chat push back to the guest = %s", got)
	}

	// A packet for somebody else, and a plain LobbyMessage, travel verbatim.
	other := packet + haxeString("Xnobody")
	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": other})
	if err := json.Unmarshal(guest.readPush(t, "lobby/chat"), &chat); err != nil {
		t.Fatal(err)
	}
	if chat.Msg != other {
		t.Fatalf("lobby/chat push for another member = %q, want it verbatim", chat.Msg)
	}
	plain := `wy12:LobbyMessagey4:Join:1` + haxeString(guestUID)
	host.call(t, "lobby/chat", map[string]any{"id": id, "msg": plain})
	if err := json.Unmarshal(guest.readPush(t, "lobby/chat"), &chat); err != nil {
		t.Fatal(err)
	}
	if chat.Msg != plain {
		t.Fatalf("lobby/chat push of a plain LobbyMessage = %q, want it verbatim", chat.Msg)
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

// TestSteamInviteLocal: lobby/initInvite answers the join code itself (it is
// what the host's game writes into the Steam lobby's "invite" data), and
// lobby/infoInvite takes that code, a bare lobby id, or garbage.
func TestSteamInviteLocal(t *testing.T) {
	addr := startMaster(t)
	w := dialMaster(t, addr)
	defer func() { _ = w.c.Close() }()
	w.call(t, "user/login", map[string]any{"name": "Host", "uid": "S00", "version": 2})

	var id string
	raw := w.call(t, "lobby/create", map[string]any{"props": map[string]any{"maxPlayers": 4}})
	if err := json.Unmarshal(raw, &id); err != nil {
		t.Fatal(err)
	}

	var invite string
	raw = w.call(t, "lobby/initInvite", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &invite); err != nil {
		t.Fatalf("lobby/initInvite = %s: %v; want a JSON string", raw, err)
	}
	if c, err := code.DecodeAny(invite); err != nil || c.Endpoint == nil {
		t.Fatalf("invite %q = %+v, %v; want a join code with the direct route", invite, c, err)
	}

	var info struct {
		ID string `json:"id"`
	}
	for _, v := range []string{invite, id} {
		raw = w.call(t, "lobby/infoInvite", map[string]any{"invite": v})
		if err := json.Unmarshal(raw, &info); err != nil || info.ID != id {
			t.Fatalf("lobby/infoInvite %q = %s, %v; want lobby %s", v, raw, err, id)
		}
	}
	if msg := w.callErr(t, "lobby/infoInvite", map[string]any{"invite": "not-a-code"}); !strings.Contains(msg, "Invalid join code") {
		t.Fatalf("lobby/infoInvite garbage = %q", msg)
	}
	if msg := w.callErr(t, "lobby/initInvite", map[string]any{"id": "Lnope"}); !strings.Contains(msg, "Unknown lobby") {
		t.Fatalf("lobby/initInvite unknown = %q", msg)
	}
}

// TestSteamJoinGameOverSDR is the Steam friends-list "Join Game": the guest's
// game hands its helper the invite value read from the host's Steam lobby,
// which must resolve and join exactly like a typed code, here over SDR.
func TestSteamJoinGameOverSDR(t *testing.T) {
	sw := sdrbridgetest.New(t)
	const hostID64, guestID64 = uint64(0x0110000100BC614E), uint64(0x0110000100000007)
	hostSteam, guestSteam := uid.FromSteamID64(hostID64), uid.FromSteamID64(guestID64)

	hostBridge := startBridge(t, sw.Add(t, hostID64), true)
	hostSrv, _ := startMasterOpts(t, func(o *Options) {
		o.Endpoint = func() (nat.Endpoint, error) { return lanEndpoint, nil }
		o.SDRStatus, o.Bridge = hostBridge.Status, hostBridge
		o.LinkKey = 0xdeadbeef
	})
	hostBridge.OnPeer = hostSrv.ServeSDRLink
	host, id := createLobby(t, hostSrv, hostID64)

	var invite string
	raw := host.call(t, "lobby/initInvite", map[string]any{"id": id})
	if err := json.Unmarshal(raw, &invite); err != nil {
		t.Fatalf("lobby/initInvite = %s: %v", raw, err)
	}
	if c, err := code.DecodeAny(invite); err != nil || c.Steam == nil || c.Steam.SteamID64() != hostID64 {
		t.Fatalf("invite %q = %+v, %v; want the host's SDR route", invite, c, err)
	}

	guest := guestFor(t, sw, guestID64, true, nil)
	var info struct {
		ID    string `json:"id"`
		Owner string `json:"owner"`
		Users []struct {
			ID string `json:"id"`
		} `json:"users"`
	}
	raw = guest.call(t, "lobby/infoInvite", map[string]any{"invite": invite})
	if err := json.Unmarshal(raw, &info); err != nil || info.ID != id || info.Owner != hostSteam {
		t.Fatalf("lobby/infoInvite over SDR = %s, %v", raw, err)
	}
	raw = guest.call(t, "lobby/join", map[string]any{"id": id, "data": `{"ready":false}`})
	if err := json.Unmarshal(raw, &info); err != nil || len(info.Users) != 2 ||
		info.Users[0].ID != hostSteam || info.Users[1].ID != guestSteam {
		t.Fatalf("lobby/join after Join Game = %s, %v", raw, err)
	}
	if join := host.readPush(t, "lobby/join"); !strings.Contains(string(join), guestSteam) {
		t.Fatalf("lobby/join push = %s", join)
	}

	// A guest asking for the code (inviting its own friends) still hands out
	// the host's route, not its own.
	raw = guest.call(t, "lobby/initInvite", map[string]any{"id": id})
	var again string
	if err := json.Unmarshal(raw, &again); err != nil {
		t.Fatalf("guest lobby/initInvite = %s: %v", raw, err)
	}
	if c, err := code.DecodeAny(again); err != nil || c.Steam == nil || c.Steam.SteamID64() != hostID64 {
		t.Fatalf("guest's invite %q = %+v, %v; want the host's SDR route", again, c, err)
	}
}