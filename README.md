# wartales-mp

Direct co-op for Wartales. One file, dropped into the game folder.

Wartales arranges co-op through Shiro's master server (`master.shirogames.com`)
and then carries the session over a Steam relay. Both hops are the part that
fails for players in Russia: the rendezvous never completes, or the relay does,
and the session is unusable. The game itself is fine — the meeting place is not.

wartales-mp replaces the rendezvous with a local one. The game talks to a master
server running on `127.0.0.1` instead of Shiro's, and that master tells the game
how to connect. There are exactly two ways, tried in this order:

1. **Direct** — straight to the host's machine, when the host's public port
   has actually been reached from the internet. Neither Shiro's infrastructure
   nor any relay is in the path, and it is the faster of the two.
2. **SDR** — Valve's modern relay network (Steam Datagram Relay) through
   `ISteamNetworkingMessages`, for everything: the lobby and the game. The host
   needs no open port and no public address at all — behind any number of
   NATs, the join code carries the host's Steam id as well as its address.

One join code carries both routes. The guest tries the direct one first (a
few seconds at most) and falls through to SDR on its own; you only notice a
short pause.

The game's own legacy Steam P2P path (`ISteamNetworking`, the old relay that
is the one that fails) has been removed: the mod diverts those calls for good
and never lets them carry traffic, not even as a fallback. If SDR is not
available either, hosting fails with the reasons instead of quietly running on
the old relay.

## Status

This is an early release. Read the verification section below before you rely on
it: the single-machine path is verified in detail, an actual two-player session
is not.

## Install

1. Copy `winmm.dll` into the Wartales game folder — the one containing
   `Wartales.exe` (normally
   `...\Steam\steamapps\common\Wartales`).
2. Launch the game the way you always do (Steam, "Play").

That is the whole installation. No launch options, no `hosts` file, no
certificate installed into Windows, no registry change, no administrator
rights, and nothing injected from outside.

> **Antivirus note.** The released `winmm.dll` is UPX-packed, and packed DLLs
> that load into a game process draw antivirus false positives more often than
> plain binaries. If yours flags it, you can build an unpacked, byte-for-byte
> equivalent DLL yourself with `.\shim\build.ps1` (without `-Release`) — see
> [Building](#building).

## Uninstall

Delete `winmm.dll` from the game folder. The game goes back to vanilla
networking on the next launch.

## No game file is modified

Nothing in the game folder is renamed, replaced or edited. `winmm.dll` is a new
file that Windows loads because it searches the application directory before
`System32` for this name.

The game's bytecode (`hlboot.dat`) is not modified either. Six bytes in it have
to change (see "How it works"), so the mod keeps a patched **copy** under
`%LOCALAPPDATA%\wartales-mp\hlboot.dat` and hands the game that copy when it
opens the file. The original on disk is only ever read. If a game update moves
the code the patch targets, the mod detects it and gives the game its own
unmodified file instead.

A Steam update will not remove the mod (it is a new file, not a replacement of
one of theirs), and it will not break the game if it does not fit any more.

## Hosting

- **Host:** nothing is required. Only Steam has to be running: the session
  runs over SDR behind CGNAT, a locked-down router, "ten routers deep". If
  the host's public port (`14250` by default) is reachable — the mod tries
  UPnP and learns the external address via STUN, `stun.l.google.com:19302`,
  and Windows Firewall lets the helper in — guests connect direct instead,
  which is faster.
- **Guest:** needs nothing either. Enter the join code the host gives you, or
  use "Join Game" on the host in your Steam friends list.

**Steam invites.** The game's own "invite friends" button and the friends-list
"Join Game" work with the mod: the host's game creates a friends-only Steam
lobby and stores the join code in it, the guest's game reads that code back and
hands it to its helper, which resolves it exactly like a typed one. Both
players must run the game through Steam. This path is implemented and covered
by tests against a fake Steam relay, but has not yet been tried end to end on
two machines.

The lobby's own transport is chosen when it is created and logged with the
reason. Direct is chosen only once the port has been **verified**: a
connection from the internet actually arrived on it during this run. A UPnP
mapping plus a public address from STUN is treated as a hint, not proof — on a
real machine it claimed reachability while the port was refused from outside.
Until verified, the lobby runs over SDR, but the join code still offers the
hinted address as its first route; the first guest who gets through over TCP
verifies it, and the next lobby is direct. `wartales-mp run -transport
direct|sdr` forces one.

The join code tells the layouts apart by length: 13 symbols carry `ip:port`
only (direct), 16 symbols carry the host's Steam account and a per-run key
(SDR), 25 symbols carry both, for example `R0PSMP226YN01F319VFAVFQFF` =
`45.154.88.66:14250` then Steam account `12345678`. Old codes keep working.

**Windows Firewall.** The direct route also needs an inbound rule for the
helper, and the helper never asks for elevation: it runs hidden, started from
inside the game, and a UAC prompt behind a full-screen game would be dismissed
and fail silently. So the mod only checks and reports (`WARNING: firewall: no
inbound rule "wartales-mp"` in `wartales-mp.log`), and the direct route simply
stays unverified until you add it — once, from an administrator prompt:

```powershell
wartales-mp firewall          # adds the inbound rule for the helper, TCP 14250
wartales-mp firewall check    # reports whether it exists
```

SDR needs no rule; without one, sessions still work, over Valve's relay.

Timing: the helper starts before the game's Steam client is ready. Until the
mod reports SDR up, creating a lobby works, but asking for the join code (or
entering one) answers "Steam relay not ready yet … ask again in a few seconds"
— the game shows it, you retry, and `wartales-mp.log` records it. If SDR can
never work in this game process, hosting without a public address is refused
with both reasons instead.

What SDR still needs: both players' Steam clients must reach Valve's relay
network. A network that blocks that has no rung left; the mod says so rather
than falling back to the old relay.

## Logs

Two files, both under `%LOCALAPPDATA%\wartales-mp\`:

| File | What is in it |
| --- | --- |
| `wartales-mp.log` | the helper: connections, every master command with its arguments and reply, relay counters, NAT discovery, errors |
| `shim.log` | the in-game part: DLL attach, each hook (address or why it failed), each bytecode open with its size and hash, helper startup, the SDR transport (ready, or exactly why not; first packet, sessions, counters) |

A third, one-line file `sdr.status` (`ok bridge=127.0.0.1:<port> token=…`,
`pending <why>` or `unavailable <why>`) is the shim's verdict on SDR and the
loopback port of its bridge; the helper follows it for as long as it runs.

The previous run of `wartales-mp.log` is kept as `wartales-mp.log.1`. Nothing is
sampled and nothing leaves the machine — this is a debugging aid, not telemetry.
Attach both files to any bug report.

Certificates (a private CA and one leaf, generated on first run) live under
`%ProgramData%\wartales-mp\`. They are used only by the local master; the CA is
never added to the Windows trust store.

## What is verified, and what is not

Verified — a single-machine run:

- the shim loads,
- both hooks install,
- the bytecode patch applies,
- the game talks to the local master over TLS,
- a lobby is created,
- a join code is issued and decodes to the host's public endpoint,
- the stock 60 s lobby crash no longer fires.

Verified without Steam — `shim\check.ps1` loads the DLL into a test process
with stand-in `steam.hdll` / `steam_api64.dll` libraries:

- all six legacy P2P natives are diverted and their original bodies are never
  called,
- packet semantics over the loopback `ISteamNetworkingMessages`: reliability
  flags per send type, packet boundaries, truncation, per-channel queues, the
  sender's Steam id, automatic session acceptance, close dropping a peer's
  queue,
- the SDR bridge: the helper's loopback connection into the game process is
  token-protected, relays on its own channel (100), and never touches the
  game's channels,
- with no `steam_api64.dll` at all the shim fails closed and says why.

Verified in Go tests: two helpers behind a fake SDR switch, a host with no
address issuing a Steam join code, the guest resolving it, joining and chatting
through the bridge, a tampered key refused, a join attempted before SDR is up
answered with the retry error; and the cascade — direct wins when the port
answers, a refused port falls through to SDR at once, a silent port falls
through after the bounded wait, a stranger behind a reused address is refused
by key and SDR reaches the real host, and a combined code round-trips and
rejects every corrupted symbol.

Not yet verified:

- **an actual second player joining by code.** No end-to-end session has ever
  been run, on either transport. The guest path — resolving a code, the
  proxy-link to the host's master, the guest's game connecting to the host's
  relay — is implemented and unit-tested, but it has never carried a real
  second player.
- **SDR against Valve's real relay.** The shim's transport and bridge are
  proven against loopback stand-ins, not against a running Steam client;
  whether the real `SteamNetworkingMessages002` accepts sessions and delivers
  between two accounts, on channel 0 and on channel 100, has not been
  observed.

Treat the two-player path as untested. Reports with both log files are useful.

## How it works

The game does not choose its own transport — the master server does. When a
lobby starts, the game asks the master for an instance and the master answers
with a `serverID`; the first character of that string selects the transport.
Whoever controls the master therefore controls how players connect, and the game
needs no patch for it.

`winmm.dll` is loaded by the game at startup and does five things:

1. **Forwards every call through.** Every export is forwarded to the real
   `System32\winmm.dll`, so the game's audio behaves exactly as before.
2. **Redirects the master.** It hooks the game's name resolution so
   `master*.shirogames.com` resolves to `127.0.0.1`. Everything else resolves
   normally.
3. **Makes TLS work.** The game verifies its master's certificate strictly, and
   that is left alone. Instead, the mod's own CA is appended to the chain the
   game configures, so the local master is trusted. Certificate verification is
   never disabled.
4. **Patches six bytes of the bytecode, in a copy.** In this build, the client
   timeout path is fatal for every role: about a minute of sitting in a lobby
   ends in a `Null access` crash. Two bytes turn the timeout comparison into one
   that is never true, so the crash cannot fire. The other four relax the
   title screen's "join by code" field, which is built around 5-symbol codes
   (input cap, truncation, validation, submit check), to accept at least 5
   and up to 32 symbols, so the mod's 13/16/25-symbol codes are accepted and
   vanilla codes still work. The patches are applied to a copy under
   `%LOCALAPPDATA%`, never to the file in the game folder.
5. **Moves the game's Steam transport onto SDR.** The game's Steam path is
   built on the deprecated `ISteamNetworking` P2P calls in `steam.hdll`. The
   mod hooks those six calls and re-implements them on
   `ISteamNetworkingMessages` through `steam_api64.dll`, keeping the exact
   semantics the game relies on (per-channel queues, reliability per send type,
   packet boundaries and truncation, the sender's Steam id). The old
   implementations are never called. If SDR cannot be set up, the calls fail
   visibly and `shim.log` says why. It also opens a token-protected loopback
   port for the helper, so the helper's master can talk to the other player's
   master over SDR too (channel 100, apart from the game's own channels).

It then starts the helper, `wartales-mp.exe` (embedded in the DLL, extracted to
`%LOCALAPPDATA%\wartales-mp\`), hidden. The helper watches the game's process
and exits with it. It runs three things:

| Part | Listens on | Purpose |
| --- | --- | --- |
| master | `127.0.0.1:60442`, TLS | stands in for Shiro's master |
| relay | public TCP port (default `14250`) | carries game traffic between host and guests |
| proxy-link | the same public port | forwards a guest's master commands to the host's master |

The host's lobby state is authoritative. When the host asks for a join code, the
helper encodes its endpoint (`ip:port`, re-discovered via UPnP then STUN for
each code, since the address can change) and, when the SDR bridge is up, its
Steam account plus a key, into one Crockford-base32 code. A guest entering
that code tries the routes in order: a proxy-link over TCP to the endpoint
(3 s to connect, 5 s for the first answer), then a stream over the SDR bridge
to that Steam id (8 s per command) — well inside the game's own 20 s — and
from then on both games see one lobby. The guest's game then
connects to the host's relay directly (direct transport) or, when the host's
master decided on SDR, both games take their Steam path and the shim carries
it over Valve's relay network. The decision travels inside the code and the
player ids the host's master renders, so the guest's helper cannot disagree.

On the direct transport, game traffic goes guest to host machine, and nowhere
else.

The full design is in [DESIGN.md](DESIGN.md) (Russian).

## Building

Windows, with `go`, `gcc` and `objcopy` on `PATH`:

```powershell
.\shim\build.ps1            # unstripped, unpacked -- for development/debugging
.\shim\build.ps1 -Release   # the GitHub release asset: stripped + UPX-packed
.\shim\check.ps1            # verifies the DLL (whatever is in dist\) without the game
```

### Release build (the uploaded asset)

`-Release` produces the `winmm.dll` published on GitHub. It strips symbols
(`-ldflags "-s -w"` for the embedded exe, `-s -Wl,--strip-all` for the DLL) and
UPX-packs both stages: the embedded `wartales-mp.exe` (~8.0 MB → ~2.5 MB) and
the final DLL. Because the embedded exe is already compressed, the DLL barely
shrinks at that last step, but the whole artifact drops from ~11.8 MB (unpacked)
to ~2.6 MB.

**Antivirus trade-off (read this).** UPX-packed binaries raise antivirus
false positives noticeably, and this DLL is loaded into a running game process,
which makes heuristic engines more suspicious still. The release asset is
packed to keep the download small; if your AV flags it, or you simply prefer an
unpacked file, build your own with plain `.\shim\build.ps1` (no `-Release`) —
byte-identical behaviour, no packing, no stripping. Packing was verified not to
break anything (see below); it is a size/AV trade-off, not a correctness one.

The packed DLL is verified after the build, not assumed: its export table still
lists all 180 names at their original ordinals (byte-for-byte identical to the
unpacked build, `go run ./tools/gendef -in dist\winmm.dll -list`), `shim\check.ps1`
passes in both modes against the packed DLL (forwarding thunks, the `CreateFileW`
hook and MinHook, the SDR transport and bridge all work), and the packed
embedded exe still runs (`wartales-mp code decode …`).

`check.ps1` loads the built DLL into a test process, opens the real
`hlboot.dat` through it and compares the result against the expected patched
image, so the bytecode path can be checked without the game running. It then
exercises the SDR transport and the bridge twice: with a loopback stand-in
`steam_api64.dll` (packet semantics and the bridge frames end to end) and
without one (fail closed, no bridge advertised). The game folder is only ever
read.

## Licence

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 UberMorgott.

MinHook, vendored under `shim/minhook`, is BSD-2-Clause and carries its own
copyright; see the headers in that directory.

Wartales is a trademark of Shiro Games. This project is unaffiliated with and
unendorsed by Shiro Games or Valve.
