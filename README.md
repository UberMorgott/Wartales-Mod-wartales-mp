# wartales-mp

Direct co-op for Wartales. One file, dropped into the game folder.

Wartales arranges co-op through Shiro's master server (`master.shirogames.com`)
and then carries the session over a Steam relay. Both hops are the part that
fails for players in Russia: the rendezvous never completes, or the relay does,
and the session is unusable. The game itself is fine — the meeting place is not.

wartales-mp replaces the rendezvous with a local one. The game talks to a master
server running on `127.0.0.1` instead of Shiro's, and that master tells the game
to connect straight to the other player's machine. Neither Shiro's
infrastructure nor Valve's relays are in the path.

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

## Uninstall

Delete `winmm.dll` from the game folder. The game goes back to vanilla
networking on the next launch.

## No game file is modified

Nothing in the game folder is renamed, replaced or edited. `winmm.dll` is a new
file that Windows loads because it searches the application directory before
`System32` for this name.

The game's bytecode (`hlboot.dat`) is not modified either. Two bytes in it have
to change (see "How it works"), so the mod keeps a patched **copy** under
`%LOCALAPPDATA%\wartales-mp\hlboot.dat` and hands the game that copy when it
opens the file. The original on disk is only ever read. If a game update moves
the code the patch targets, the mod detects it and gives the game its own
unmodified file instead.

A Steam update will not remove the mod (it is a new file, not a replacement of
one of theirs), and it will not break the game if it does not fit any more.

## Hosting

- **Host:** needs a public address and one open inbound TCP port (`14250` by
  default). The mod tries UPnP automatically, and falls back to STUN
  (`stun.l.google.com:19302`) to learn the external address. If UPnP is off on
  the router the port has to be forwarded by hand. Behind CGNAT a direct
  connection cannot be built at all — that is a NAT limit, not something the mod
  can work around. The game's transport is TCP, so UDP hole punching does not
  apply.
- **Guest:** needs nothing. No port, no forwarding, no configuration. Enter the
  join code the host gives you.

If the mod cannot confirm that the address it found is reachable from the
internet, it says so in the log rather than handing out a code that quietly does
not work.

## Logs

Two files, both under `%LOCALAPPDATA%\wartales-mp\`:

| File | What is in it |
| --- | --- |
| `wartales-mp.log` | the helper: connections, every master command with its arguments and reply, relay counters, NAT discovery, errors |
| `shim.log` | the in-game part: DLL attach, each hook (address or why it failed), each bytecode open with its size and hash, helper startup |

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

Not yet verified:

- **an actual second player joining by code.** No end-to-end session has ever
  been run. The guest path — resolving a code, the proxy-link to the host's
  master, the guest's game connecting to the host's relay — is implemented and
  unit-tested, but it has never carried a real second player.

Treat the two-player path as untested. Reports with both log files are useful.

## How it works

The game does not choose its own transport — the master server does. When a
lobby starts, the game asks the master for an instance and the master answers
with a `serverID`; the first character of that string selects the transport.
Whoever controls the master therefore controls how players connect, and the game
needs no patch for it.

`winmm.dll` is loaded by the game at startup and does four things:

1. **Forwards every call through.** Every export is forwarded to the real
   `System32\winmm.dll`, so the game's audio behaves exactly as before.
2. **Redirects the master.** It hooks the game's name resolution so
   `master*.shirogames.com` resolves to `127.0.0.1`. Everything else resolves
   normally.
3. **Makes TLS work.** The game verifies its master's certificate strictly, and
   that is left alone. Instead, the mod's own CA is appended to the chain the
   game configures, so the local master is trusted. Certificate verification is
   never disabled.
4. **Patches two bytes of the bytecode, in a copy.** In this build, the client
   timeout path is fatal for every role: about a minute of sitting in a lobby
   ends in a `Null access` crash. Two bytes turn the timeout comparison into one
   that is never true, so the crash cannot fire. The patch is applied to a copy
   under `%LOCALAPPDATA%`, never to the file in the game folder.

It then starts the helper, `wartales-mp.exe` (embedded in the DLL, extracted to
`%LOCALAPPDATA%\wartales-mp\`), hidden. The helper watches the game's process
and exits with it. It runs three things:

| Part | Listens on | Purpose |
| --- | --- | --- |
| master | `127.0.0.1:60442`, TLS | stands in for Shiro's master |
| relay | public TCP port (default `14250`) | carries game traffic between host and guests |
| proxy-link | the same public port | forwards a guest's master commands to the host's master |

The host's lobby state is authoritative. When the host asks for a join code, the
helper discovers the public endpoint (UPnP, then STUN) and encodes `ip:port` into
a short Crockford-base32 code. A guest entering that code decodes it, opens a
proxy-link to the host's master, and from then on both games see one lobby. The
guest's game then connects to the host's relay directly.

Game traffic goes guest to host machine, and nowhere else.

The full design is in [DESIGN.md](DESIGN.md) (Russian).

## Building

Windows, with `go`, `gcc` and `objcopy` on `PATH`:

```powershell
.\shim\build.ps1     # produces dist\winmm.dll -- the only file players need
.\shim\check.ps1     # verifies the DLL without launching the game
```

`check.ps1` loads the built DLL into a test process, opens the real
`hlboot.dat` through it and compares the result against the expected patched
image, so the bytecode path can be checked without the game running.

## Licence

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 UberMorgott.

MinHook, vendored under `shim/minhook`, is BSD-2-Clause and carries its own
copyright; see the headers in that directory.

Wartales is a trademark of Shiro Games. This project is unaffiliated with and
unendorsed by Shiro Games or Valve.
