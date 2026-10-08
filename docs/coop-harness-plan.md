# Local co-op test harness — feasibility, design, estimate

Status: plan only (2026-10-08). Nothing implemented. Codex reviewed (agrees, with changes folded in below).

## 1. Feasibility verdicts (with evidence)

| Question | Verdict | Evidence |
| --- | --- | --- |
| Two `Wartales.exe` at once | **Yes**, with env `SteamAppId=1527950` (+`SteamGameId`) | Experiment 2026-10-08: pids 39640 + 5340 both alive after 40 s, ~1.3 GB each, both logged in to the master. No single-instance mutex in the game. |
| Launch without the env | No: exit code 1 | `shim.log`: `game: Steam failed to activate` (no `steam_appid.txt`; Steam passes the id only when it launches the game). |
| Second instance without Steam | **Not possible** | Game exits when SteamAPI init fails (above). Both instances therefore run on the same Steam account. SDR to oneself is impossible, so co-op on one PC must use the **direct** transport over loopback. Steam-invite/SDR path cannot be tested on one PC (needs a 2nd account/PC). |
| Mod as shipped, 2 instances | **Collides** | `proxy.c:1092` mutex `Local\wartales-mp-running` → 2nd shim logs `helper: already running in this session, not started`; both games resolve `master*.shirogames.com` to fixed `127.0.0.1` (`proxy.c:214-232`) → same helper master `:60442`; both log in as the **same** uid `X4ebd8fb527f99050e707` (`uid.Mint` of the same Steam id, `internal/master/user.go` `adopt`) → `lobbyJoin` would treat the guest as the host slot (`lobby.go:465-476`). Master keeps one-local-game state (`inviteLobby`, `gameTransport`). |
| Game folder dll | untouched | `winmm.dll` SHA256 `FB5157EB…4AC9` before and after. |

## 2. Per-instance separation

- **Game dir + saves:** saves (`save\*.dat`, `meta.sav`) and `prefs.sav` live in the game folder, shared by both instances. Fix: a harness copy `D:\WartalesTest\{A,B}` built from **hardlinks** to the paks/exe/dlls (same volume D:, zero extra GB) with its own `save\`, `prefs.sav` (windowed, small resolution) and the **test** `winmm.dll`. The real game folder and its dll are never touched. To verify first: save path is exe-dir/cwd relative (assumed from layout).
- **Mod data:** everything keys off `LOCALAPPDATA` (`proxy.c:96` `helper_path`, `applog.Dir`) → per-instance `LOCALAPPDATA=E:\…\harness\run-<id>\{A,B}\` gives separate `shim.log`, `wartales-mp.log`, `hlboot.dat` copy, `sdr.status`. Certs come from `ProgramData\wartales-mp` (shared, fine).
- **Helper:** needs a test seam (env `WARTALES_MP_TEST_INSTANCE=N`, N=1,2):
  - shim: mutex name `…-running-N`; `hl_host_resolve` returns `127.0.0.N`; helper started with `-master 127.0.0.N:60442 -port 1425N -transport direct`.
  - helper: salt the `uid.Mint` input in `adopt` with N (keep real Steam id stored); advertise endpoint `127.0.0.1:1425N` for join codes, no UPnP/STUN, no SDR publish.
  - helper already attaches to its **parent** game pid (`game_windows.go:25`), so two helpers watch the right games.
- **Firewall:** loopback only, no rule needed.

## 3. Driving — options and recommendation

- (a) computer-use only: slow (seconds/step), fragile (two windows, focus, 3D scene clicks), not repeatable. Use only where unavoidable.
- (b) mod-side control channel. Key find: the game ships a debug console — `debug.Console.runCommand(String)` fn@2893 (created in `Game.init` fn@3240 and `TitleScreen.init`), modes `debug.{Cheats,Battle,World,Place,Admin,Public}ConsoleMode`; Battle/Cheats `isEnable` require `get_isAdmin() && Game.canCheat()` (fn@32586). So most "do X" verbs already exist in the game.
- **Recommended (c) hybrid, simplest reliable:**
  1. **PowerShell driver** `tools/harness/coop.ps1`: build hardlink dirs, launch A (host) then B (client) with env, window placement, `PrintWindow` screenshots per HWND, kill (`TerminateProcess` = crash/Alt+F4-hard) or `WM_CLOSE` (Alt+F4-soft), collect logs, per-run folder, cleanup only harness-owned PIDs.
  2. **Command file channel** (bytecode, patcher module `harness.rs`, compiled only with `build.ps1 -Test` **and** active only with the env): per-frame (~100 ms) poll of `%LOCALAPPDATA%\wartales-mp\cmd\<seq>.cmd` (atomic rename by driver), run on the game thread, ACK `<seq>.ack` with result. Verbs: `console <line>` (→ `runCommand`, with isAdmin/canCheat forced true in test build), plus a handful of own verbs: `load <slot>`, `host`/`code` (create co-op lobby, print join code), `join <code>`, `dump` (state JSON line: mode, battle turn/units/owners/HP, party, chest contents, skill-bar/vars, camp state), `endturn`, `chest open <id>`.
  3. Computer-use only for UI-geometry checks a human cares about (chest panel offset/scroll/click, skill bar look, flicker) — screenshot capture is automated; clicking via driver `SendInput` at coordinates from `dump` where possible.
- Rule (Codex): cheats prepare the fixture only; the action under test goes through the normal UI/RPC path, else the harness hides the bug.

## 4. Verification coverage

| Feature (commit) | Logs | dump/assert | Screenshot | Human |
| --- | --- | --- | --- | --- |
| Host reload / battle restart no-hang (8ceadb0) | yes (barrier/ready-start lines) | client mode reaches battle/world within T | yes | no |
| Mid-battle rejoin incl. Alt+F4 (e6b9fe5) | yes (battleRejoin, returning units) | client back in battle, owns its units | yes | no |
| Host takeover of disconnected units (10af1f6) | yes | host can act on B's units after kill | yes | no |
| Client skill bar/vars #2 (19d5cae, 34d1197) | partial | skill vars equal on A and B | yes | look check |
| Loading flicker #3 (152acc0) | no | no | needs frame burst (10 fps capture) | **yes** |
| Chest panel offset/scroll/click (c010d28, fcccf61, 9bc1af9) | partial | item moved after click | yes | **yes** (layout) |
| Battle camera (020c3b4) | no | camera target in dump | yes | **yes** |
| Loot changes (loot_level.rs) | yes if logged | loot list after battle in dump | optional | no |
| Confession skill point (camp_choice.rs) | **yes** (diagnostics already log before/after) | points before/after | optional | no (UI click needed to choose) |
| Steam-invite / SDR join | — | — | — | **untestable on one PC** |

## 5. Estimate

| Component | Lang | Files | Lines |
| --- | --- | --- | --- |
| Shim test seam (mutex suffix, 127.0.0.N, helper args) behind `-Test` define | C | proxy.c, build.ps1 | ~60 |
| Helper seam (uid salt, advertise override, no NAT/SDR) | Go | user.go, run.go, nat | ~80 + tests ~60 |
| `harness.rs` bytecode: poll/ACK loop, console bridge, isAdmin/canCheat force, ~8 verbs, `dump` JSON | Rust (patcher) | 1–2 new + lib.rs | ~600–900 + testsim tests ~200 |
| Driver `coop.ps1` (hardlink dirs, launch, windows, PrintWindow, kill, logs) | PowerShell (+small C# P/Invoke) | 1–2 | ~350 |
| Scenarios (one per feature, ~9) + assert helpers | PowerShell/JSON | 9–10 | ~40 each ≈ 400 |
| Total | | ~15 | **~1.6–2.0k lines**; ~3–5 agent-days, `harness.rs` is the bulk and the risk |

Cheaper first slice (~0.5 k lines, 1 day): shim+helper seam + driver + `console`/`dump` verbs only; menu/join by computer-use. Proves the two-instance co-op loop before investing in verbs.

## 6. Risks

- Same Steam account on both: Steam cloud/achievements/rich presence races; game may still use the Steam id somewhere outside the master ids (direct lobbies render `X` ids, so probably fine — verify).
- Save path may not be exe-relative → then needs a save-dir hook (shim `CreateFileW` hook already exists).
- Console commands unknown in detail (names not enumerated yet); some may be dev-only stubs in release.
- Two 1.3 GB instances + GPU on one PC: fine for windowed low-res; fullscreen focus fights.
- Bytecode verbs break on game updates (same as all patcher modules; locate by name, skip with log line).
- Test seam must not ship active: compile-time `-Test` flag + env; release check in `shim/check.ps1`.
- Untested on one PC: SDR/Steam invite, real NAT/hole-punch, latency.

## 7. Decisions for the user

1. Approve hardlinked test game copies under `D:\WartalesTest\` (own saves/prefs/dll) vs. sharing the real folder.
2. Test seam only in a `-Test` build (recommended) vs. env-gated in the shipped dll.
3. First slice only, or full verb set.
4. Accept that Steam-invite/SDR and visual items (flicker, chest layout, camera) stay human-checked.
