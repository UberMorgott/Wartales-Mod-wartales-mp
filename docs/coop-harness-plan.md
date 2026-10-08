# Local co-op test harness — feasibility, design, estimate

Status: slices 1 and 2 implemented (2026-10-08, see section 8). Plan Codex reviewed (agrees, with changes folded in below).

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

## 8. Slice 1 (implemented)

Test seam, compiled only by `shim\build.ps1 -Test` (output `dist-test\`, `dist\` stays shippable); all of it carries `WMP_TEST_SEAM` and/or reads `WARTALES_MP_TEST_INSTANCE`:

- shim `proxy.c` (`#ifdef WMP_TEST`): mutex `Local\wartales-mp-running-N`, master hosts resolve to `127.0.0.N`, helper started as `run -master 127.0.0.N:60442 -port 1425N -transport direct`.
- helper (Go tag `harness`): `cmd/wartales-mp/testseam_harness.go` (join codes advertise `127.0.0.1:1425N`, verified, no UPnP/STUN; relay listens on loopback only). Lobby member ids need no seam: every lobby names members by their games' own ids (finding 1, fixed).
- patcher (cargo feature `harness`): `patcher/src/harness.rs`, see below.
- `shim\seamcheck.ps1` (run by `build.ps1` and `check.ps1`): markers absent from the release `winmm.dll`, from the helper embedded in it (cut out, UPX-unpacked) and from `wartales-mp.exe`; `-Test` requires them.

Verified: saves and `prefs.sav` are game-folder relative (each copy writes its own `save\udat_*.sav` / `prefs.sav`, the real folder's timestamps stay put). Separate uids, masters and relay ports per instance in the logs.

### Command channel (`harness.rs`)

`harnessTick()` at op 0 of `hxd.App.mainLoop`; off unless `WARTALES_MP_TEST_INSTANCE` is set. Every 6th frame it reads `%LOCALAPPDATA%\wartales-mp\harness\cmd.txt` (`<seq> <verb> [arg]`, written as `cmd.tmp` + rename), deletes it, answers `ack.txt` = `<seq> <result>` and a `harness: ack` line in `shim.log`.

- `console <line>`: `Game.inst.console` (else `TitleScreen.inst.console`, both `AppBase.console`) with `PREFS.admin` forced true around `resetCommands` + `runCommand` (Battle/Cheats modes need `get_isAdmin() && canCheat()`), restored and the command set rebuilt after, also when the command throws. Result `dispatched` (the console shows errors on its own screen), `nogame` when neither exists.
- `dump`: `state.json`: lobby (join `code`, `players`), game set, mode (`Std.string`), isAuth / isMulti / isCoopGame / connectedToHost / isLoading / hasGameplayStarted / fading, playersReady, host, me, battle (isPlayerTurn; current unit name, owner, connected, apSkillPlayed, skills `{id, ok = canUseSkill}`), army (name, owner, connected, aptitudePoints), chest panel (`GameInventory.chestInventory`: visible, absX/Y, calculatedWidth/Height), loot (open `ui.win.Debrief`: `Std.string(debrief.loot.getAllItems())`). Strings go through the game's own `haxe.format.JsonPrinter.print`.

Debug console commands (from the mode constructors; `cargo test --release --features harness --target-dir target-test list_console_commands -- --ignored --nocapture`):

- Public: `admin`. Place: `quit`. Game: `speed show-tuto dump2d dumpscene`.
- Admin: `logdb autoplay networkLog clearTutos dumpmem live checkLiveObjects memprof gc prof pr sprof debug tonemapping luminance colorBlind trackGpuAlloc gpudump blur`.
- Battle: `drawGrid nav gen gen-test removeFog win run winr lose ap killFoes searchIssue heal hit moral kill capture madness remove ai aiReasoning objective renfort renfort-anim skill spawnAllSkills spawnAllTraps trap nextRound play-anims captainDuel drawTriggers drawCollider`.
- Cheats: `scale stats reset item items power setpower wealth gold influence brigands dispose sss sao quadtree terrainTiles lighting envToColor hideOptimized hideUnits genname confession reloadgroupleader reloadallgroupleader interact control collide gennavmesh wireframe lang armySimulator setTavernLevel slReput omniscience skills-master pathlvl rank song counter counters bonus title grim class trait jobs teleport recruit escape prison plague werewolf werewolfCure tavernSpe jobXp traitXp xp respec relation goal goals clearGoals resetrelation status addskill greenscreen seed meteo difficulty logfight sfx playEvent listener trailer-dialog load save tavEvent tavAllEvents tavEventCond tavStopEvent tavEventLog tavSilver tavBurn tavBonus tavLevel tavUnlock tavFactionCount beastHint beastLog beastInfo beastGoto debugGhostPacks notify fiefNeeds fiefAudience fiefEvent fiefMandate fiefPopDisplay fiefPop fiefRelation fiefRegime fiefGoalComplete fiefDebug fiefExportStart fiefExportStop wlab setSanity setFuel export`.
- World: `noise wanted heretic happiness ap xp tire genteam unit zoo loadteam unlockPits fow fowregion fowOffset bridge notifyMap water campmap time camera discoverRegion travel buildComptoir etp eye battle rouste beast battlemap checkGenBattle aggro brigands-aggro systemic weeklybounty horde debugDir killNearest tp goto enter setting script activity debugactivity battletest battletrap battletrigger autobattle mapbuild nav generateRegionsShapes lightShrink visit-places visit-camp visit-taverns visit-tavern checkBattleTextures visit-battle visit-world tp-places terrainBakeAlbedo checkHunts visit-next-hunt huntBonus hunt log-battle-gen perfBenchmark slAggro slStopAggro e2free e2placeOccupied e2placeDestroyed e2skills sanity sanityData circleHallucination chaos addChaos propaganda burn burnAll extinguish endCrisis lockChaos`.

### Running it

```powershell
.\shim\build.ps1 -Release -Test; .\shim\check.ps1 -Test          # dist-test\winmm.dll
.\tools\harness\setup.ps1 -Saves     # D:\WartalesTest\{A,B}: hardlinks + own save\, prefs, test dll (re-run: refreshes the dll)
.\tools\harness\coop.ps1 start       # run folder D:\WartalesTest\runs\<stamp>\{A,B} = per-instance LOCALAPPDATA
.\tools\harness\coop.ps1 cmd -Inst A -Line 'dump'      # or 'console <command>'
.\tools\harness\coop.ps1 shot -Tag x; .\tools\harness\coop.ps1 place
.\tools\harness\coop.ps1 close -Inst B   # WM_CLOSE;  kill -Inst B = TerminateProcess
.\tools\harness\coop.ps1 stop; .\tools\harness\coop.ps1 logs -Inst A,B
```

### Slice 2

Verbs (all `harness.rs`, ack `ok` / `nosave` / `nowindow` / `unknown`):

- `host`: newest save via `LoadGame.loadGames` in Pause.doLoad's solo mode (title) or co-op mode (in game), a real `LoadGame` window, save `playerId` stamped with the local player id, then `convertGame()` (solo save) or `loadGame()`: co-op lobby + `LoadMultiGame` on the title screen, the vanilla in-game reload in game.
- `slot`: `LobbyState.addSlot()` (the lobby's "+"; a loaded save admits a new player only into an opened slot).
- `join <code>` (driver: `join auto` = A's `lobby.code`): the title screen's join-by-code submit closure (only caller of `Lobby.joinCode`).
- `start`: `LoadMultiGame.startGame()`; `backup <n>`: `onClick` of the open pause menu's backup button n.
- Windows are found by runtime type in `TitleScreen.inst.ui`, `Game.globalUI`, `Game.mode` (`harnessFind`).
- First frame: `Game.PREFS.displayMode = 0` + `GraphicsControl.applyDisplayMode()` (windowed, in memory; no game command-line flag exists, `getSysArg` has no caller).
- Player ids: both copies share one Steam account, so `harnessUid` (after `makeSteamUser` in `mpman.Api.getUser`) makes instance N play as `User.make("X" + N + id.substr(1))`; the helper renders it as the member id, as for every player.

Driver (`coop.ps1`): `start -Keep` (relaunch into the current run), game stdout/stderr to `<run>\<i>\console.log` (stays empty: the game's prints go through the shim's `hl_sys_print` hook into `shim.log`), `wait -Until '<expr on $s>'` (dump poll, timeout), `key -Key Escape|Enter|I` (WM_KEYDOWN/UP), `place -W -H` (default 852x480). Every wait has a timeout.

Findings (2026-10-08), fixed on branch `fix/direct-join`:

1. Direct lobby + loaded co-op save: members were minted Session ids (`X…`) while each game knows itself and its saves by its own id (`S…`): the host denied `Join` (`userCanJoin`), the guest's `LobbyState.update` threw `Null access .dlcs` and `LoadMultiGame.isHostIn` was false. Root cause: player identity depended on the transport (the `X` ids only existed to make `Lobby.isSteamOnly` false). Fix: `internal/master` `idOf` names every member by its game id on both transports (SDR: the authenticated Steam id), a peer without a game id cannot create/join; the transport is carried by the lobby id (`DirectLobbyMark` "D"), read by `patcher/src/direct_lobby.rs` (`isSteamOnly` false for such a lobby). The `lobbyChat` retarget band-aid and the harness member-id seam are gone.
2. Host start over the direct relay hung on the loading screen. Three causes on one path: (a) `instance/get` answered `R<host>:<port>`; the client's RelayP2P parser always pops a password field, so it parsed host null -> `SysError(Unresolved host null)`; now `R<host>:<port>:<hostpw>` (`user.go` `instanceAnswer`). (b) That SysError reached `Api.logError`, whose wrapper cast it to String and threw, so the failure path never ran; `patcher/src/log_error_cast.rs` stringifies it. (c) The relay checked X-Pass as md5; the game sends `Sha1.encode` (encode@18770 is haxe.crypto.Sha1) -> every host login was "bad password"; `internal/wsx` `checkPass` is sha1 now.

Driver notes: `coop.ps1` is per-monitor DPI aware (at 150 % shots were cropped and clicks missed), `shot` is a screen capture of the foregrounded window (PrintWindow is black on the world map), `click -X -Y` (pixels of a shot) is a real cursor click, `wait` keeps polling while a loading game does not ack. A loaded co-op game waits on the host's "Waiting for players" window until its Start button is clicked (vanilla). The join code is stable per host endpoint (`FW00009QNCC` = 127.0.0.1:14251), so a relaunched guest rejoins with it.

Scenario run 2026-10-08 (run `D:\WartalesTest\runs\20261008-224953`): S1 host+join PASS, S2 in-game reload PASS, S3 battle restart (pause backup 0) PASS, S4 client WM_CLOSE + relaunch + rejoin mid-battle PASS (`battle rejoin: the running battle was replayed to a late joiner`), takeover not exercised, S5 skills equal on both (host-owned unit only), S6 debrief loot equal on both PASS, S7/S8 blocked. Fixture limit: the harness save is a converted solo save, so the guest owns no units (takeover, client skills, camp, confession need assigned companions). Open: after the S4 rejoin, battle end left the guest silent at the mode switch (`doLeaveMode lockAlives=true`) until the barrier's 30 s rejoin; camp refused with "companions of player Morgott are too far" (the unit-less guest).