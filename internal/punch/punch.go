// Package punch carries join-by-code traffic through NAT without Steam: one
// UDP socket per helper does STUN (to learn the address peers must aim at),
// UDP hole punching, and KCP sessions with smux streams on top. Each stream
// is one TCP connection of the existing protocols (proxy-link, the game's
// websocket): the host splices it into its relay port on loopback, the guest
// takes it from a loopback TCP forwarder whose address is the route the guest
// joined over. No encryption and no FEC: the link is as open as the TCP port
// it stands in for.
//
// Who sends first: the guest punches toward the host's code address and the
// host answers every punch, so a host whose NAT lets that through connects
// at once. Otherwise the guest gets a reply code; the host enters it and
// punches back, which opens its NAT for the guest's next try. Punching toward
// one address lasts Budget, re-armed by every attempt and every reply code.
package punch

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"sync"
	"time"

	"github.com/xtaci/kcp-go/v5"
	"github.com/xtaci/smux"

	"github.com/UberMorgott/wartales-mp/internal/code"
	"github.com/UberMorgott/wartales-mp/internal/nat"
)

const (
	// Budget is how long one side punches toward one address.
	Budget = 2 * time.Minute
	// AnswerWait is how long a guest waits for the host's first answer.
	AnswerWait = 5 * time.Second
	// punchEvery spaces the punch packets.
	punchEvery = 500 * time.Millisecond
	// keepaliveTicks: one STUN binding every 20 s keeps our own mapping
	// (the address in the codes we hand out) from expiring while idle.
	keepaliveTicks = 40
	// heardFresh: an answer this recent means the path is open.
	heardFresh = 30 * time.Second
	// stunWait bounds one STUN round over all servers.
	stunWait = 3500 * time.Millisecond
	// maxGuests caps the tunnels a host serves at once.
	maxGuests = 8
)

// Punch packets are 8 bytes: never STUN (20+ bytes) nor KCP (24+ bytes).
var (
	punchMsg = []byte("WMPPUNCH")
	pongMsg  = []byte("WMPPONG!")
)

// Result is what STUN said about our socket.
type Result struct {
	Mapped  *net.UDPAddr // our address as the internet sees it
	Mapping nat.Mapping
}

// Socket is the helper's UDP socket.
type Socket struct {
	// Fixed, when set before use, is returned by Endpoint instead of asking
	// STUN (the loopback harness and tests).
	Fixed *net.UDPAddr
	// Wait overrides AnswerWait (tests).
	Wait time.Duration

	conn    net.PacketConn
	relay   string // host role: the relay port on loopback
	servers []string
	log     *log.Logger
	done    chan struct{}

	mu      sync.Mutex
	stun    map[string]stunWaiter // pending binding requests by transaction id
	targets map[string]target     // addresses being punched
	heard   map[string]time.Time  // last punch or pong from an address
	news    chan struct{}         // closed and replaced on every hear
	peers   map[string]*peer      // tunnels by peer address, both roles
	keep    *net.UDPAddr          // STUN server for the keepalive
	keepTx  string                // its pending transaction
	mapped  *net.UDPAddr          // last address STUN saw
	closed  bool
}

type stunWaiter struct {
	server *net.UDPAddr
	got    func(*net.UDPAddr)
}

type target struct {
	addr  *net.UDPAddr
	until time.Time
}

// peer is one tunnel: a guest's to a host (fwd set), or a host's from a
// guest (fwd nil).
type peer struct {
	view *view
	mux  *smux.Session
	fwd  net.Listener
}

// Open binds UDP port (any free port when it is taken) and starts serving:
// KCP sessions from guests are spliced into relay ("127.0.0.1:<relay port>").
func Open(port int, relay string, servers []string, logger *log.Logger) (*Socket, error) {
	c, err := net.ListenUDP("udp4", &net.UDPAddr{Port: port})
	if err != nil {
		logger.Printf("punch: UDP %d unavailable (%v); using any free port", port, err)
		if c, err = net.ListenUDP("udp4", &net.UDPAddr{}); err != nil {
			return nil, err
		}
	}
	s := &Socket{
		conn:    c,
		relay:   relay,
		servers: servers,
		log:     logger,
		done:    make(chan struct{}),
		stun:    map[string]stunWaiter{},
		targets: map[string]target{},
		heard:   map[string]time.Time{},
		news:    make(chan struct{}),
		peers:   map[string]*peer{},
	}
	go s.read()
	go s.tick()
	logger.Printf("punch: UDP socket %s", c.LocalAddr())
	return s, nil
}

// LocalAddr is the socket's local address.
func (s *Socket) LocalAddr() *net.UDPAddr {
	a, _ := s.conn.LocalAddr().(*net.UDPAddr)
	return a
}

// Close stops everything; tunnels close with it.
func (s *Socket) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	close(s.done)
	peers := s.peers
	s.peers = map[string]*peer{}
	s.mu.Unlock()
	for _, p := range peers {
		p.close()
	}
	return s.conn.Close()
}

// Ready reports whether a STUN server has answered: codes can carry our UDP
// address.
func (s *Socket) Ready() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.mapped != nil || s.Fixed != nil
}

// read is the only reader of the socket: STUN answers and punch packets are
// handled here, everything else is KCP for the tunnel of its sender.
func (s *Socket) read() {
	buf := make([]byte, 2048)
	for {
		n, from, err := s.conn.ReadFrom(buf)
		if err != nil {
			if s.isClosed() || errors.Is(err, net.ErrClosed) {
				return
			}
			continue // e.g. an ICMP unreachable reported on a later read
		}
		udp, ok := from.(*net.UDPAddr)
		if !ok {
			continue
		}
		pkt := buf[:n]
		switch {
		case nat.IsSTUN(pkt):
			s.onSTUN(pkt, udp)
		case string(pkt) == string(punchMsg):
			s.hear(udp)
			s.send(pongMsg, udp)
		case string(pkt) == string(pongMsg):
			s.hear(udp)
		default:
			s.mu.Lock()
			pr := s.peers[udp.String()]
			if pr == nil {
				pr = s.newGuestLocked(udp, pkt)
			}
			s.mu.Unlock()
			if pr != nil {
				pr.view.push(packet{b: append([]byte(nil), pkt...), from: udp})
			}
		}
	}
}

func (s *Socket) isClosed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closed
}

func (s *Socket) send(b []byte, to *net.UDPAddr) {
	_, _ = s.conn.WriteTo(b, to) // UDP: a lost packet is the next tick's job
}

func (s *Socket) hear(from *net.UDPAddr) {
	s.mu.Lock()
	defer s.mu.Unlock()
	key := from.String()
	if _, seen := s.heard[key]; !seen {
		s.log.Printf("punch: %s answers", key)
	}
	s.heard[key] = time.Now()
	close(s.news)
	s.news = make(chan struct{})
}

// tick sends the punch packets and the STUN keepalive.
func (s *Socket) tick() {
	t := time.NewTicker(punchEvery)
	defer t.Stop()
	for n := 1; ; n++ {
		select {
		case <-s.done:
			return
		case now := <-t.C:
			s.mu.Lock()
			var to []*net.UDPAddr
			for k, tg := range s.targets {
				if now.After(tg.until) {
					delete(s.targets, k)
					s.log.Printf("punch: stopped punching %s (%v spent)", k, Budget)
					continue
				}
				to = append(to, tg.addr)
			}
			keep := s.keep
			s.mu.Unlock()
			for _, a := range to {
				s.send(punchMsg, a)
			}
			if keep != nil && n%keepaliveTicks == 0 {
				s.keepalive(keep)
			}
		}
	}
}

// keepalive refreshes our mapping and notices when it moves.
func (s *Socket) keepalive(server *net.UDPAddr) {
	req := nat.BindingRequest()
	tx := string(req[8:20])
	s.mu.Lock()
	delete(s.stun, s.keepTx)
	s.keepTx = tx
	s.stun[tx] = stunWaiter{server: server, got: func(m *net.UDPAddr) {
		s.mu.Lock()
		defer s.mu.Unlock()
		if s.mapped != nil && s.mapped.String() != m.String() {
			s.log.Printf("punch: our address moved %s -> %s; codes handed out before carry the old one", s.mapped, m)
		}
		s.mapped = m
		delete(s.stun, tx)
	}}
	s.mu.Unlock()
	s.send(req, server)
}

func (s *Socket) onSTUN(pkt []byte, from *net.UDPAddr) {
	s.mu.Lock()
	w, ok := s.stun[string(pkt[8:20])]
	s.mu.Unlock()
	if !ok || !w.server.IP.Equal(from.IP) || w.server.Port != from.Port {
		return
	}
	m, err := nat.ParseBinding(pkt, pkt[8:20])
	if err != nil {
		return
	}
	w.got(m)
}

// Endpoint asks every STUN server for our address over this socket, with
// retries, and stops at two answers (enough to compare) or after stunWait.
func (s *Socket) Endpoint() (Result, error) {
	if s.Fixed != nil {
		return Result{Mapped: s.Fixed, Mapping: nat.MappingStable}, nil
	}
	type answer struct{ server, mapped *net.UDPAddr }
	got := make(chan answer, len(s.servers))
	ctx, cancel := context.WithTimeout(context.Background(), stunWait)
	defer cancel()
	var wg sync.WaitGroup
	for _, srv := range s.servers {
		wg.Go(func() {
			if server, m := s.ask(ctx, srv); m != nil {
				got <- answer{server, m}
			}
		})
	}
	go func() { wg.Wait(); close(got) }()
	mapped := map[string]*net.UDPAddr{}
	var first answer
	for a := range got {
		if len(mapped) == 0 {
			first = a
		}
		mapped[a.server.String()] = a.mapped
		if len(mapped) == 2 {
			cancel()
		}
	}
	res := Result{Mapping: nat.Classify(mapped)}
	if first.mapped == nil {
		return res, errors.New("no STUN server answered")
	}
	res.Mapped = first.mapped
	s.mu.Lock()
	s.mapped, s.keep = first.mapped, first.server
	s.mu.Unlock()
	return res, nil
}

// ask sends one binding request to server, retransmitting it (same
// transaction) after 0.5 s and 1 s more: lossy networks drop single tries.
func (s *Socket) ask(ctx context.Context, server string) (*net.UDPAddr, *net.UDPAddr) {
	host, port, err := net.SplitHostPort(server)
	if err != nil {
		return nil, nil
	}
	ips, err := net.DefaultResolver.LookupIP(ctx, "ip4", host)
	if err != nil || len(ips) == 0 {
		return nil, nil
	}
	ra, err := net.ResolveUDPAddr("udp4", net.JoinHostPort(ips[0].String(), port))
	if err != nil {
		return nil, nil
	}
	req := nat.BindingRequest()
	tx := string(req[8:20])
	ch := make(chan *net.UDPAddr, 1)
	s.mu.Lock()
	s.stun[tx] = stunWaiter{server: ra, got: func(m *net.UDPAddr) {
		select {
		case ch <- m:
		default:
		}
	}}
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		delete(s.stun, tx)
		s.mu.Unlock()
	}()
	for _, wait := range []time.Duration{500 * time.Millisecond, time.Second, 2 * time.Second} {
		s.send(req, ra)
		select {
		case m := <-ch:
			return ra, m
		case <-time.After(wait):
		case <-ctx.Done():
			return nil, nil
		}
	}
	return nil, nil
}

// Toward punches toward a guest's address for Budget (re-armed if running):
// the host side of a reply code.
func (s *Socket) Toward(addr string) error {
	a, err := net.ResolveUDPAddr("udp4", addr)
	if err != nil {
		return err
	}
	s.arm(a)
	s.log.Printf("punch: punching toward %s for %v (reply code)", a, Budget)
	return nil
}

func (s *Socket) arm(a *net.UDPAddr) {
	s.mu.Lock()
	s.targets[a.String()] = target{addr: a, until: time.Now().Add(Budget)}
	s.mu.Unlock()
	s.send(punchMsg, a)
}

// NoAnswerError is a guest's failed punch. Its text is for the player.
type NoAnswerError struct {
	Host  string
	Self  Result // our own STUN result; Self.Mapped nil if STUN failed
	Reply string // the reply code for the host, "" without a mapped address
}

func (e *NoAnswerError) Error() string {
	if e.Reply == "" {
		return fmt.Sprintf("No answer from the host at %s, and no STUN server answered this PC (UDP blocked?). "+
			"Use a Steam invitation, or the host forwards port %d (TCP and UDP).", e.Host, code.DefaultPort)
	}
	msg := fmt.Sprintf("No answer from the host at %s yet. Send the host this reply code: %s. "+
		"The host enters it in Join by code, then join again within 2 minutes.", e.Host, e.Reply)
	if e.Self.Mapping == nat.MappingSymmetric {
		msg += " Your network changes its port for every destination (symmetric NAT), so this may still fail: " +
			"a Steam invitation works on any network."
	}
	return msg
}

// Dial reaches the host's punch address and returns a loopback TCP address
// that leads to the host's relay port: the route to join over. A live tunnel
// to the same host is reused. Without an answer within AnswerWait it returns
// a *NoAnswerError and keeps punching for Budget, so the host's reply-code
// punches find the way open.
func (s *Socket) Dial(host string) (string, error) {
	ha, err := net.ResolveUDPAddr("udp4", host)
	if err != nil {
		return "", err
	}
	key := ha.String()
	s.mu.Lock()
	if p := s.peers[key]; p != nil && !p.mux.IsClosed() {
		s.mu.Unlock()
		return p.fwd.Addr().String(), nil
	}
	s.mu.Unlock()
	s.arm(ha)

	self := make(chan Result, 1)
	go func() { r, _ := s.Endpoint(); self <- r }()
	wait := s.Wait
	if wait <= 0 {
		wait = AnswerWait
	}
	if !s.waitHeard(key, wait) {
		r := <-self
		e := &NoAnswerError{Host: host, Self: r}
		if r.Mapped != nil {
			e.Reply, _ = code.EncodeReply(code.Endpoint{IP: r.Mapped.IP, Port: uint16(r.Mapped.Port)}) //nolint:gosec // a port
		}
		s.log.Printf("punch: no answer from %s within %v; our mapping %v (%s)", host, wait, r.Mapped, r.Mapping)
		return "", e
	}
	return s.connect(ha)
}

// waitHeard waits until addr answered recently, or for d.
func (s *Socket) waitHeard(key string, d time.Duration) bool {
	timeout := time.After(d)
	for {
		s.mu.Lock()
		t, ok := s.heard[key]
		news := s.news
		s.mu.Unlock()
		if ok && time.Since(t) < heardFresh {
			return true
		}
		select {
		case <-news:
		case <-timeout:
			return false
		case <-s.done:
			return false
		}
	}
}

// connect opens the guest's tunnel to a host that answered.
func (s *Socket) connect(ha *net.UDPAddr) (string, error) {
	v := newView(s)
	sess, err := kcp.NewConn2(ha, nil, 0, 0, v)
	if err != nil {
		return "", err
	}
	tune(sess)
	mux, err := smux.Client(sess, muxConfig())
	if err != nil {
		_ = sess.Close()
		return "", err
	}
	var lc net.ListenConfig
	fwd, err := lc.Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		_ = mux.Close()
		return "", err
	}
	p := &peer{view: v, mux: mux, fwd: fwd}
	key := ha.String()
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		p.close()
		return "", net.ErrClosed
	}
	old := s.peers[key]
	s.peers[key] = p
	delete(s.targets, key) // the tunnel's own traffic keeps the way open now
	s.mu.Unlock()
	if old != nil {
		old.close()
	}
	go func() {
		for {
			c, err := fwd.Accept()
			if err != nil {
				return
			}
			go func() {
				st, err := mux.OpenStream()
				if err != nil {
					_ = c.Close()
					return
				}
				splice(c, st)
			}()
		}
	}()
	go s.watch(key, p, "tunnel to "+key)
	s.log.Printf("punch: tunnel to %s open; joining over %s", key, fwd.Addr())
	return fwd.Addr().String(), nil
}

// watch forgets a tunnel once its smux session ends (keepalive timeout,
// peer close, or ours).
func (s *Socket) watch(key string, p *peer, what string) {
	<-p.mux.CloseChan()
	s.mu.Lock()
	if s.peers[key] == p {
		delete(s.peers, key)
	}
	s.mu.Unlock()
	p.close()
	s.log.Printf("punch: %s closed", what)
}

func (p *peer) close() {
	if p.fwd != nil {
		_ = p.fwd.Close()
	}
	_ = p.mux.Close() // closes the KCP session
	_ = p.view.Close()
}

// newGuestLocked opens the host side of a tunnel for a KCP packet from an
// unknown address (s.mu held); nil drops the packet. Only a KCP data or
// control segment opens one, and at most maxGuests run at once.
func (s *Socket) newGuestLocked(from *net.UDPAddr, pkt []byte) *peer {
	if s.closed || len(pkt) < kcp.IKCP_OVERHEAD || pkt[4] < 81 || pkt[4] > 84 {
		return nil
	}
	guests := 0
	for _, p := range s.peers {
		if p.fwd == nil {
			guests++
		}
	}
	if guests >= maxGuests {
		return nil
	}
	v := newView(s)
	sess, err := kcp.NewConn3(binary.LittleEndian.Uint32(pkt), from, nil, 0, 0, v)
	if err != nil {
		return nil
	}
	tune(sess)
	mux, err := smux.Server(sess, muxConfig())
	if err != nil {
		_ = sess.Close()
		return nil
	}
	p := &peer{view: v, mux: mux}
	key := from.String()
	s.peers[key] = p
	delete(s.targets, key)
	go s.watch(key, p, "tunnel from "+key)
	go s.serveGuest(p, key)
	s.log.Printf("punch: tunnel from %s", key)
	return p
}

// serveGuest splices every stream of one guest's tunnel into the relay port.
func (s *Socket) serveGuest(p *peer, from string) {
	for {
		st, err := p.mux.AcceptStream()
		if err != nil {
			return
		}
		go func() {
			d := net.Dialer{Timeout: 3 * time.Second}
			c, err := d.DialContext(context.Background(), "tcp", s.relay)
			if err != nil {
				s.log.Printf("punch: relay %s for %s: %v", s.relay, from, err)
				_ = st.Close()
				return
			}
			splice(c, st)
		}()
	}
}

// tune sets KCP for an interactive game link: fast retransmits, a window
// large enough for save transfers, and a datagram that fits any path.
func tune(sess *kcp.UDPSession) {
	sess.SetWriteDelay(false)
	sess.SetNoDelay(1, 20, 2, 1)
	sess.SetWindowSize(512, 512)
	sess.SetMtu(1200)
	sess.SetACKNoDelay(true)
}

// muxConfig: a long keepalive timeout, because a lossy line must stall
// before it is declared dead.
func muxConfig() *smux.Config {
	c := smux.DefaultConfig()
	c.KeepAliveTimeout = 60 * time.Second
	return c
}

// splice copies both ways and closes both ends when either side ends.
func splice(a, b io.ReadWriteCloser) {
	done := make(chan struct{}, 2)
	cp := func(dst io.Writer, src io.Reader) {
		_, _ = io.Copy(dst, src)
		done <- struct{}{}
	}
	go cp(a, b)
	go cp(b, a)
	<-done
	_ = a.Close()
	_ = b.Close()
	<-done
}

// packet is one KCP datagram handed to a view.
type packet struct {
	b    []byte
	from *net.UDPAddr
}

// view is the net.PacketConn a KCP session or the listener reads from: it
// gets only the datagrams read routes to it, and writes go to the socket.
type view struct {
	s    *Socket
	in   chan packet
	die  chan struct{}
	once sync.Once
}

func newView(s *Socket) *view {
	return &view{s: s, in: make(chan packet, 1024), die: make(chan struct{})}
}

func (v *view) push(p packet) {
	select {
	case v.in <- p:
	default: // full: dropped like any UDP datagram
	}
}

func (v *view) ReadFrom(b []byte) (int, net.Addr, error) {
	select {
	case p := <-v.in:
		return copy(b, p.b), p.from, nil
	case <-v.die:
		return 0, nil, net.ErrClosed
	}
}

func (v *view) WriteTo(b []byte, a net.Addr) (int, error) { return v.s.conn.WriteTo(b, a) }

func (v *view) Close() error {
	v.once.Do(func() { close(v.die) })
	return nil
}

func (v *view) LocalAddr() net.Addr              { return v.s.conn.LocalAddr() }
func (v *view) SetDeadline(time.Time) error      { return nil }
func (v *view) SetReadDeadline(time.Time) error  { return nil }
func (v *view) SetWriteDeadline(time.Time) error { return nil }
