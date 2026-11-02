# Single-DLL runtime: everything in one Rust `winmm.dll`

Status: approved 2026-11-03, decided with Codex (two review rounds). Work branch `single-dll`;
progress and findings: [single-dll-checklist.md](single-dll-checklist.md).

## Why

Today `winmm.dll` = C shim (proxy/sdr/bridge/lobby/splash, ~3.6k lines, MinHook, gcc) + Rust
staticlib (patcher) + an embedded Go helper exe (~7.2k lines) that the DLL writes to
`%LOCALAPPDATA%` and starts with `CreateProcessW`. Release builds are UPX-packed. Defender
flags the pattern (drop exe + launch + packer). The helper also installs a CA and a TLS
leaf cert in `%ProgramData%` so the game's strict TLS accepts our fake master.

## Target

One Rust crate, `cdylib` named `winmm.dll`, target `x86_64-pc-windows-msvc` (VS 2022 Build
Tools present). Cargo does the only link; no gcc, objcopy, MinHook, C headers or UPX.
Release = stripped, `opt-level=3`, `panic=unwind` (every thread entry catches and logs).

| Part | How |
| --- | --- |
| winmm forwarding | `build.rs` reads System32 `winmm.dll` exports (the Rust gendef logic) and emits one naked tail-jump thunk per export plus a `.def` for names/ordinals. Pointers bound from the absolute System32 path. |
| DllMain | Minimal: open log, bind forwards, install the `hlboot.dat` open hook (must exist before the game's `main`), start one worker thread. Nothing else under the loader lock. |
| Inline hooks | `ilhook` (pure Rust, iced-x86), no own detour code. |
| Bytecode | `CreateFileW` hook hands out `%LOCALAPPDATA%\wartales-mp\hlboot.dat` = original + needle patches (old `hlpatch.h`) + patcher passes, called as a Rust API (no C ABI). |
| Game log | `hl_sys_print` hook -> `shim.log` (buffered writer thread), as today. |
| Steam/SDR | The six legacy P2P natives of `steam.hdll` diverted to `ISteamNetworkingMessages` (port of `sdr.c`), friends-list lobby advert (`lobby.c`). The master-link over SDR becomes direct in-process calls on channel 100: no loopback bridge, no `sdr.status`, no token. |
| Splash | Patch-progress window (`splash.c`) via `windows-sys`. |
| Network | master, relay, link, punch, nat, code, uid, modver in the same crate, on std threads. |

## Master: plain WebSocket on loopback (option A)

The game's master config (`Main.initMpman`/`getMPManConfig`: `master.shirogames.com`,
`master2…`, port 60442, `ssl:true`) is patched in the bytecode: both hosts -> `127.0.0.1`,
`ssl` -> `false`. The same JSON/WS protocol is served in plain text on `127.0.0.1:60442`.
The patch is all-or-nothing: a mismatch leaves the game unpatched and talking to Shiro over
TLS, never plaintext to a public host.

Deletes: CA/leaf generation, `%ProgramData%\wartales-mp` cert files, the TLS listener,
`ssl_conf_set_ca` hook and its `ssl.hdll` poll, the `hl_host_resolve` hook, `rustls`,
`rcgen`, `ring` (random and SHA-1/SHA-256 come from Windows CNG via `windows-sys`).

Rejected: B (bytecode calls our natives directly, no socket): HashLink resolves natives by
library name (`<lib>.hdll` + `hlp_<name>`), so it needs a loader hook, new native ABI
entries and HL-thread/GC-safe event delivery: ~1-3k extra lines on a fragile ABI to remove
one loopback socket that already works and never prompts the firewall. C (keep TLS): all of
the above stays for no gain on a loopback rendezvous.

## Game traffic

- Steam lobbies: the game's own Steam P2P path, diverted onto SDR in-process (unchanged).
- Non-Steam lobbies: the game's RelayP2P/WServer WebSocket to our relay. The relay stays
  (auth, cid, connect/disconnect framing are the game's protocol); it runs in-process. Host
  game -> loopback relay; guests -> host's public TCP port, or a loopback forwarder -> UDP
  punch tunnel -> host relay.
- Mixed SDR/direct lobbies stay refused (as today).

## Peer transport

| Route | Keep | Why |
| --- | --- | --- |
| SDR (Steam relay) | yes | Steam invite, works behind any NAT; master-link on channel 100 direct in-process. |
| Direct TCP (+ optional UPnP) | yes | Non-Steam, no UDP dependency; UPnP only a background hint, inbound traffic is the proof. |
| UDP punch + KCP | yes | Non-Steam behind NAT. kcp 0.6, one conversation per TCP stream (no smux), session nonce, FIN, idle timeout (already ported). |

Unstable networks (RU TSPU/DPI): established links are kept and redialled behind the same
loopback port; discovery keeps one attempt alive across the game's 20 s command timeout
and answers "retry" instead of racing short timeouts. Without Steam, behind a hard NAT with
UDP blocked, no route exists without a third-party relay (out of scope, said in README).

## Lifetime and safety

- One runtime per process: the worker starts once (atomic guard) and keeps the existing
  session mutex so a second game on the same profile does not fight over the ports.
- No PID watch: the helper dies with the game. UPnP mapping removal happens on a managed
  stop only; otherwise the 2 h lease expires.
- Every thread body runs under `catch_unwind`; a panic is logged to `wartales-mp.log` and
  ends only that service. OOM/UB are not catchable: bounded queues everywhere.
- Harness seam (`WARTALES_MP_TEST_INSTANCE`, `WARTALES_MP_TEST_PUNCH`) only under the
  `harness` feature; release seamcheck stays.
- Harness `freeze` (NtSuspendProcess) now also freezes the network side of that instance:
  scenarios relying on "game frozen, helper alive" must be revisited.

## Patcher passes

None of the network passes become obsolete: `start_transport`, `reload_route`,
`reconnect`, `net_guard`, `alive_bind`, `stale_lobby`, `mod_version` act on the game's
lobby/hxbit logic, not on how the master is reached. New pass: `master_config` (loopback +
`ssl:false`). The C copy of the needle patches (`hlpatch.h`) and the C ABI wrappers go.

## Migration (branch `single-dll`, merged when the harness co-op run passes)

1. Lint gate (`tools/lint.ps1`: fmt, clippy pedantic `-D warnings`, cargo-deny, unsafe
   docs) and the crate turned into the `winmm` cdylib skeleton: forwarding thunks,
   DllMain, log. Gate: export table equals the C build's, game starts unpatched.
2. Bytecode path in Rust (hook, needles, passes, splash, game log). Gate: `shim.log`
   shows the same "patched" lines as the C build; patched image byte-identical.
3. Steam/SDR natives + lobby advert in Rust. Gate: shimcheck scenarios (sdr/nosdr/partial)
   rewritten as Rust integration tests against fake modules.
4. `master_config` pass + Rust master/relay/link/punch/nat in-process (plain WS). Gate:
   harness co-op: code join, host save load + rejoin, basic play, punch route.
5. Delete Go (`cmd/`, `internal/`, `go.mod`, tools), C (`shim/proxy`, MinHook, check),
   embed/UPX steps; docs. Gate: no exe in `%LOCALAPPDATA%\wartales-mp`, no
   `wartales-mp.exe` process, no files in `%ProgramData%\wartales-mp`, all passes verified
   in the harness (pass -> evidence table in the checklist).

## Size estimate

| | Before | After (est.) |
| --- | --- | --- |
| C shim | 3.6k | 0 |
| Go helper (non-test) | 7.2k | 0 |
| Rust runtime (non-patcher) | 6.2k (partial port) | ~7-8k incl. tests |
| patcher | 84.5k | 84.5k + 1 pass |
| `winmm.dll` | 21.1 MiB debug / UPX release | 4-8 MiB stripped, no UPX |

Defender-relevant: no exe drop, no `CreateProcessW`, no packer, no cert install. Proxy
loading and executable-memory hooks remain (inherent to the mod); no guarantee of zero
detections.

## Risks

- Loader-lock: forwarders may be called by `libhl.dll` during process init; binding must
  happen in DllMain from the absolute System32 path (as the C shim does).
- `ilhook` installs hooks without suspending other threads: install before the game's
  threads touch the target (libhl at worker start, steam.hdll on load poll), as today.
- Loopback harness cannot prove WAN/NAT/TSPU or a real Steam invite; those need two
  machines and two Steam accounts.
