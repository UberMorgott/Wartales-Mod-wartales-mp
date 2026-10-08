# Code join via UDP hole punching (plan, not implemented)

Status: future work. Plan only, written 2026-10-08. Nothing here is built.

## Goal

Join by code for NON-Steam players when the host has no open port (no UPnP, no
forward). Constraints: no relay server of our own, no third-party relay of game
traffic. Allowed: free public STUN (`stun.l.google.com:19302`) and the join code.

## Why the current path fails

`lobbyMakeShortCode` (internal/master/lobby.go:700) encodes the host's TCP
endpoint (UPnP, then STUN) with `code.EncodeDirect`. The guest's `cascade`
(lobby.go:942) dials TCP `ip:14250`. Without a forward the dial times out.
DESIGN.md:716 says hole punching does not apply because game traffic is TCP;
the fix is to carry that TCP over a UDP tunnel.

## Design

1. **One UDP socket per host session.** Wrap it in `quic.Transport`
   (quic-go v0.63.0, API checked with `go doc`): `Transport.WriteTo` and
   `ReadNonQUICPacket` send and receive the STUN binding on the same socket
   that later carries QUIC. The STUN-reported `ip:port` is then the real
   mapping. `Config.KeepAlivePeriod` (about 10 s) keeps the NAT mapping
   alive; some routers drop UDP mappings after 30 s idle.
2. **Code.** The code holds the host's TCP endpoint (as today), the UDP
   endpoint, and a 16-byte random secret, so it gets longer.
3. **Signaling.** A host behind a port-restricted NAT (the common home case)
   must send to the guest first, so the host needs the guest's endpoint.
   - Recommended: **ntfy.sh** HTTP pub/sub, using only the standard library.
     - Topic: HMAC(secret).
     - Payload: AES-GCM(secret) of {guest UDP endpoint, nonce}.
     - The host subscribes while the lobby is open.
     - Only endpoint messages travel through it, never game traffic.
   - Optional second channel: a public MQTT broker (`broker.emqx.io`,
     `test.mosquitto.org`), with a minimal hand-rolled MQTT 3.1.1 QoS0
     client.
   - Rejected for now:
     - Nostr: needs a schnorr/secp256k1 dependency.
     - BitTorrent DHT BEP44: slow and heavy.
   - Fallback with no third party: a manual reply code. The guest sends a
     reply code to the host out of band, and the host pastes it into the
     game's own join-code box. The master recognizes it as a punch reply. The
     user experience is awkward.
4. **Punch.** Both sides send small punch packets to each other's endpoint for
   about 10 s. Then the guest calls `Transport.Dial` and the host calls
   `Transport.Listen`. TLS uses a self-signed certificate, and a check derived
   from the secret authenticates the peer.
5. **Guest wiring.**
   - Add a route `punch` to `cascade` after `direct`, using the same
     `try(what, dial func() (net.Conn, error), ...)` shape as SDR. A QUIC
     stream wrapped as `net.Conn` carries the link.
   - The guest master opens `127.0.0.1:0`. Each accepted TCP connection gets
     a new QUIC stream to the host relay `127.0.0.1:14250` (`relay.dispatch`,
     internal/relay/relay.go:111, splits game websocket from proxy-link as
     today).
   - The guest rewrites the host's `instance/get` answer (internal/master/
     user.go:108) to `R127.0.0.1:<port>`.
6. **Order.** Direct TCP first (open port or UPnP, about 3 s), then punch.

## Expected success rate (estimate, not measured)

- Works when at least one side has endpoint-independent mapping (full,
  restricted or port-restricted cone), which covers most home routers.
  Rough estimate: 70-85% of pairs.
- Fails when:
  - one side has a symmetric NAT and the other side is port-restricted, or
  - both sides are on CGNAT or symmetric NAT (common on mobile and some
    Russian ISPs).
- Port prediction is not planned: the gain is small and it adds complexity.
- Error message on failure: "Cannot connect: both networks block direct
  connections (NAT). The host must forward UDP/TCP port N, or both players
  use Steam."

## Estimate

Go only, no patcher or bytecode changes.

- New package `internal/punch`, about 350-450 lines:
  - STUN on the shared socket
  - ntfy signaling
  - punch loop
  - QUIC listen/dial
  - stream-to-`net.Conn` and TCP forwarder
- Edits, about 80-120 lines:
  - `internal/code`: new fields
  - `lobby.go`: issue the code and add the cascade route
  - `user.go`: `instance/get` rewrite
  - `nat`: reuse the STUN code
- Tests, about 200 lines, including a loopback punch test.
- New dependency: quic-go v0.63.0, which pulls in `golang.org/x/crypto`,
  `golang.org/x/net` and `golang.org/x/sys`.

## Risks

- Binary size: quic-go adds an estimated 3-4 MB to the packed `winmm.dll`
  (see DESIGN.md about release size). The alternative, kcp-go v5.6.72 with
  smux, is smaller but needs its own STUN demux and encryption.
- Availability of ntfy.sh and the MQTT brokers from Russia is unverified.
  Needs a live test from the user's network.
- Windows Firewall prompt for inbound UDP. The install already handles
  firewall rules (`internal/firewall`); extend it to UDP.
- Several guests: one listener and one QUIC connection per guest; the host
  matches them by the nonce in the signaling message.
- Lifetime of the host's NAT mapping between issuing the code and the guest
  joining. Keep a STUN keepalive every 15-20 s while the lobby is open.

## Open questions for the user

1. Is ntfy.sh, a third-party service carrying only encrypted endpoint
   messages, acceptable? Or should we use only the manual reply code?
2. Is a 3-4 MB larger DLL acceptable for quic-go? Or should we use
   kcp-go+smux?
3. Is a longer join code acceptable (secret plus UDP endpoint)?

## Codex review

Verdict: feasible as best effort. Codex recommends QUIC with ntfy, and keeping
the manual reply code as a fallback. Direct connection without a relay cannot
be guaranteed.

### Required changes (MUST)

- **Integration.**
  - `chooseTransport` (internal/master/transport.go:71) must allow punch
    lobbies.
  - The TCP endpoint becomes optional, both when issuing the code and in
    `instanceGet` (user.go:108).
  - Extend `code.DecodeAny` (internal/code/code.go:247).
- **Startup.**
  - The host starts `Listen` and a single `ReadNonQUICPacket` reader up front.
  - The guest runs `Dial` at the same time as punching, without waiting
    10 s.
  - Punch packets start with the bits `00`.
  - The current STUN parser returns only the IP. It must also decode the XOR
    port.
- **Security.**
  - Mutual proof of the secret before any forwarding.
  - Separate keys for the topic, AEAD and authentication.
  - Unique GCM nonces and replay protection.
  - Limits on punch packets and streams.
  - Never log codes that contain the secret.
  - One transport and listener, one QUIC connection per guest. Bind the
    request nonce to the authenticated connection.

### Simplifications

- Route every QUIC stream into the existing host relay.
- Also connect the link through the guest's localhost listener. This removes
  the need for a QUIC-to-`net.Conn` adapter. WebSocket goes through as raw
  bytes, including the HTTP Upgrade.

### Signaling

- ntfy over HTTPS port 443 is the simplest automatic option. It needs:
  - confirmation that the subscription is ready
  - bounded retries, a TTL and deduplication
- `Cache: no` prevents storage, but messages are lost when the subscription
  drops.
- The free limit is 250 messages per day.
- Drop MQTT for now: QoS0 needs its own retries and reconnects, and
  test.mosquitto.org warns that it is unstable.
- Nostr and BEP44 add complexity. Port prediction is not justified.

### Russia

- Reachability of ntfy, EMQX, Mosquitto and Nostr from Russian networks is
  unverified.
- Test QUIC separately: TSPU filtering of QUIC is documented (FOCI 2026). A
  working HTTPS signaling channel does not prove that game UDP works.

### Success rate

- Drop the 70-85% figure until it is measured in the field.
- Two CGNATs can work if both use endpoint-independent mapping.
- A timeout does not prove a NAT type. The error message must say "direct
  connection not established" and name the stage that failed: signaling,
  STUN or QUIC.

### NAT mapping

- QUIC keepalive only starts after the connection is up. Before that, refresh
  the STUN mapping every 15-20 s.
- If the endpoint changes, reissue the code.
- Add a LAN candidate for networks without hairpinning.

### Lifecycle and Windows

- The two-way copy must handle FIN, cancellation and reconnects. QUIC `Close`
  only closes the send side.
- The firewall rule (internal/firewall/firewall.go:73) currently allows only
  TCP. UDP needs either the actual port or an application rule, and a prompt
  is not guaranteed.

Full output (session scratchpad):
`E:\Temp\claude\E--DEV-Wartales\6ae33f76-4383-45dc-a4fa-ba67f9255ef4\scratchpad\codex-holepunch.txt`.
