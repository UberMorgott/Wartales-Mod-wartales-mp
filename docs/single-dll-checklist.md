# single-dll checklist

Design: [design-single-dll.md](design-single-dll.md). Branch `single-dll`, worktree
`E:\DEV\Wartales\mp-sd`. Tick as done; a continuation resumes from the first open box.

## Done on main before the branch

- [x] helper crate (Rust) linked as the one staticlib; patcher C ABI inside (7896543)
- [x] Rust ports on main: applog conn log code uid modver nat sendq wsx relay sdrbridge install hlpatch gendef hlpatchgen (5ff866e..1fc10e8)
- [x] link port: branch worktree-agent-a89d44eb2e00a3506 commit 2170700 (not on main)
- [x] punch port (kcp, conv per stream): branch worktree-agent-ac61f52afbc3c1215 commit 08ada98 (not on main)

## Steps

- [x] 0. worktree + branch `single-dll`; merge link + punch ports (e767692)
- [x] 1. lint gate `tools/lint.ps1` (fmt, clippy pedantic -D warnings, cargo-deny, undocumented_unsafe_blocks); cdylib `winmm` skeleton: build.rs thunks + .def, DllMain, log; export table == System32 (180 named + NONAME @2) (c24c560..3968125)
- [x] 2. bytecode path: CreateFileW hook, needles, passes, splash, hl_sys_print game log; master_config pass (sd-bytecode, merged af2ff59); hooks via minhook crate (thread freeze)
- [x] 3. Steam/SDR natives + lobby advert; shimcheck scenarios as Rust tests vs fake flat API (sd-steam, merged af2ff59)
- [x] 4. master/relay/link/punch/nat in-process; install/TLS/ring dropped, CNG crypto (sd-net, merged 520a647); tools/build.ps1 (dist 5,308,416 B release)
- [x] 4b. harness smoke: game starts with dist-test dll, shim.log hooks + patched lines, master login (run `20261103-015616`, dll 1189aab: `forwards: 181 exports bound, 0 missing`, CreateFileW/A + hl_sys_print + 6 steam natives + RunCallbacks hooks installed, `patched master config fn@14965`, 100 distinct `tips: patched` (old main run `20261102-165715`: 97; +master config, reconnect, save list), no `skipped`; wartales-mp.log `master listening on 127.0.0.1:60442`, `login Morgott`, menu reached, clean WM_CLOSE, no Application Error event)
- [x] 5. delete Go, C shim, MinHook, embed/UPX; docs (README, DESIGN, TECHNICAL) (031d303 deletion, 2ee655c old-install cleanup, 8683a24 docs; dist 5,314,048 B release, 5,843,968 B -Test)
- [x] 6. harness: code join, host save load + rejoin, basic play, punch route; no exe / no wartales-mp.exe process / no %ProgramData% certs (d04f672 adds `regress.ps1 -Case reload|reconnect`). Direct, runs `20261103-015724` + `-021510`: up.ps1 PASS (`link: guest "Morgott" has the same mod files`, `joined lobby ... (direct)`), chest PASS (guest `invOp op=5`, host `invNetAdd count=3` + `invOp op=5`), reload PASS (guest `mp: reload: connecting to the server id this game joined by`, host 2nd `> Join`), reconnect PASS (freeze B 135 s: host `relay: client cid 21530 gone`, B `mp: reconnect: connection to the host lost, rejoining`, host `client connected (cid 23842)` 17 s after thaw). Punch (`WARTALES_MP_TEST_PUNCH=1`), run `20261103-020421`: B `route direct ... failed: harness: direct route disabled`, `route punch 127.0.0.1:14251 WORKS`, game over `127.0.0.1:65214`; chest, reload, rats battle (3v8 -> 3v21, `console win`, debrief loot equal, 11 stacks) PASS, reconnect PASS but ~62 s after thaw (see Found). No `wartales-mp` process during/after any run, no *.exe under runs `20261103-*`, `C:\ProgramData\wartales-mp` untouched (4 old cert files, all 18.09.2026 0:22:12, dir mtime same). No panicked/FATAL/harness error/Uncaught in any log
- [x] 7. every patcher pass -> evidence (table below): 86 pass calls, PASS 40, NOT-OBSERVABLE 46 (UI input / new-game / race or error guards / no trigger), FAIL 1 fixed (ready_start, 96efa41); harness fixes 8de499d, ee6bc53; puzzle rejoin FAIL fixed (faf2201, new pass activity_join); verbs learn/equip + shield dump (9e135ad); now 87 pass calls, PASS 46, NOT-OBSERVABLE 41. Open: UI-only passes need a foreground pass by hand; rejoined guest's confession dialog text (Found row)
- [x] 8. merged to main; dist\winmm.dll 5,325,824 B, SHA256 CD53E83C0D07D15C9505DE48CFA2BEDE2F16F9E734429E2F58B814EF965FC4AA (release, stripped, no UPX)

## Found in old code

| What | Impact | Fixed how |
| --- | --- | --- |
| `TcpStream` shutdown does not wake a blocked recv on Windows (Rust port; Go used closesocket) | close() could hang a reader | `conn::close_tcp` = shutdown + CancelIoEx (1fc10e8) |
| nat: `Verified` kept after the WAN address changed (Go comment says it must reset) | stale "verified" endpoint picks direct for a new address | reset on change (Rust nat port) |
| nat: STUN attribute without padding panicked (Go) | helper crash on a malformed STUN reply | bounds-checked (Rust nat port) |
| C shim never exported System32 winmm's NONAME ordinal 2 | an importer by ordinal fails to load (low) | exported @2 NONAME (1026f74) |
| patcher #[no_mangle] C ABI were winmm.dll's only exports in a Rust cdylib | wrong export table | C ABI removed, Rust API (c24c560) |
| splash.c: failed-patch message box lost if the splash window failed | player not told the co-op fixes are off | box shown anyway (sd-bytecode) |
| proxy.c: tips refusal reason cut at 256 bytes | truncated diagnostics | logged in full (sd-bytecode) |
| sdr.c: sdr_accepted incremented outside the lock | data race on a counter | under the lock (sd-steam) |
| sdr.c: RunCallbacks hooked while the game runs it; safe only thanks to MinHook's thread freeze | crash risk with a non-freezing hooker | kept MinHook semantics (minhook crate) |
| sdr.c fail(): retry delay disabled when GetTickCount()+2000 wraps to 0 | rare missed retry | Option instead of 0 sentinel (sd-steam) |
| Go lobby handlers: lookup then relock; a lobby emptied in between accepted a join | join into a deleted lobby | single lock, "Unknown lobby" (sd-net) |
| Go peerGone sends a no-op lobby/transfer push when any non-owner leaves | needless traffic | kept for parity (noted) |
| Rust std leaves Nagle on (Go disables it): relay/punch ports lacked TCP_NODELAY | latency on small game frames | set_nodelay (3c43ad5) |
| Go assumed a refused direct route fails at once; Windows needs ~2 s on loopback | cascade timing | test bound to the 3 s direct timeout (sd-net) |
| Go helper's master spoke TLS with a self-made CA in %ProgramData% | cert install, CA hook, Defender-relevant | plain WS on loopback via master_config pass |
| ready_start absent player (7ccd79f): readyTick times WAIT_ABSENT only while a counted client exists (`JNull sur -> unsince`), so a 2-player save whose only guest is gone never starts (run `20261103-032417`: guest killed, host in-game reload, `ready=1`, loading 240 s, no `never connected` line); same pass code on main | host of a 2-player co-op stuck on the loading screen after a reload / battle restart with the guest crashed | 96efa41: with no client the absent player is counted directly, then gameplayStart; test `only_guest_who_never_connects`; R5/R6 15 s then start (Codex agreed) |
| guest full sync (reconnect / drop-in) while a CylinderPuzzle / NinePuzzle Activity is replicated: Game.start runs Activity.initData before the guest's place builds Element 3D objects; its camera offset (Activity.hx:177 `target.obj.getRotationQuat()`, only for those two ids) threw `Null access .getRotationQuat`. Not the song: R2's culprit was the ruins NinePuzzle the harness left open (`console quit` from E1Ruins keeps the Activity replicated; harness-only, normal UI cannot leave mid-puzzle). Repro R8 `20261103-041811` (left-open puzzle, rejoin in T2Tavern) and R9 `20261103-042659` (rejoin inside the host's open NinePuzzle, real case). Vanilla has no mid-game join; main has the same passes (no pass touches Activity.initData) + reconnect/drop_in, so main is affected (judged from bytecode, main dll not run) | guest who reconnects / drops in during a ruins or cylinder puzzle hangs on loading | faf2201 new pass activity_join: initData's two NullChecks (target, target.obj) -> JNull to needUnit (same op count), test `patches_installed_game`; R10 `20261103-044140`: guest frozen 135 s inside the host's NinePuzzle rejoins (0 getRotationQuat, B world.Ruins E1Ruins connected, not loading). Codex: RCA right; left: the rejoined guest's input is held by chooseUnit's full-screen wait until the host ends the puzzle (stale forever after a harness `console quit`); host-side leak not fixed (harness-only). Self-contained, cherry-pickable to main |
| rejoin during a confession (R12, `console confession ConfessionHate` + `talk`, guest frozen 135 s): guest loads into world.Confession but logs `Dialog not found #ConfessionHate.Start` (Element.getDialog from Dialog.showText); fake_npc's miss path not hit (id is an allNpcs key) | the exception (caught, timer) aborts the guest's Dialog.showText: rejoined guest's confession dialog has no text/choices; no softlock seen (mode world.Confession, connected) | FIXED 6a90a09 (mp-sd single-dll; main has the same fake_npc.rs): RCA (diag builds R13 `20261103-052024` / R14 `20261103-052645`): at the rejoin FakeNpc.initData runs with id `ConfessionHate`, which IS an allNpcs key, so the vanilla copy is that prefab's inf (no in-memory confession dialog, no `Start`); fake_npc rebuilt only on a miss. Not cheat-specific (real confessions use the same confID rename), vanilla has it too. Fix: rebuild from ConfessionSetup + lastConf whenever `lastConf.confID == id`. R15 `20261103-053857`: `mp: fakenpc: confession NPC rebuilt from confession data: ConfessionHate`, no `Dialog not found`. Left: the rejoined guest's camera/pose (confession__impl's focus + PoseSitFloor) are not replayed: scene framed wide (cosmetic, OPEN) |
| harness coop.ps1 `wait` threw on a dump `error` ack (half-built game during a reload: `Null access .players`) | scenario aborted mid-reconnect (run `20261103-030902`) | wait keeps polling on `'dump' failed: error` (sd-verify) |
| harness coop.ps1 `freeze` assumed the helper outlives a suspended game; `stop` killed helper exes | stale semantics on the single DLL | doc + stop fixed, reconnect case waits for the guest's log line (d04f672) |
| punch (Rust port): host drops KCP of a tombstoned (addr,conv) silently, pongs keep the peer alive, so a guest resumed after >60 s suspension notices its dead stream only via the 60 s stuck timeout (and never while it sends nothing); Go used smux keepalive instead (Codex) | rejoin ~60 s late after a guest hang/sleep; harness freeze over punch | OPEN: smallest fix = reset reply for a tombstoned conv, receiver ends the stream (punch track) |
| `C:\ProgramData\wartales-mp` cert files of the old helper stay on players' PCs | leftover CA key material after upgrade | deleted on every start with exe/.new/sdr.status, logged `old install: removed ...` (2ee655c) |
| setup.ps1 calls `shim\seamcheck.ps1` | harness setup breaks when step 5 deletes shim\ | inline WMP_TEST_SEAM check in setup.ps1 (031d303) |
| patcher dlc_untouched test parsed the generated `shim/proxy/hlpatch.h` | test breaks once shim\ goes; table duplicated in Go + C | test compiles `winmm/src/hlpatch.rs` via #[path] and runs its `apply` (031d303) |
| hlpatch rationale lived only in the Go package doc | lost with internal/ | moved into winmm/src/hlpatch.rs (031d303) |
| shimcheck.c check_sdr hooked a fake steam.hdll via real `hlp_` resolvers and asserted legacy calls == 0; Rust steam tests call the transport directly | no unit proof that the hooks land on the natives (only harness `hook ...: installed`) | OPEN: gap noted (Codex, step 5); README claim corrected (d8fb35c) |
| README/TECHNICAL said join by code still works when the patches are refused | false since master_config: refused image = Shiro master | docs corrected (d8fb35c) |

## Pass -> evidence

Runs (dll dist-test from f63ab57 unless noted; `D:\WartalesTest\runs\…`): R1 `20261103-030902` direct (up, chest, inv/sheet, rats 2 rounds + win, in-game reload, reconnect + host frozen 45 s); R2 `20261103-033110` punch (up, spectate lockpick/woodcut/ruins/forge/sing, reconnect); R3 `20261103-032417` (guest killed, host reload); R4 `20261103-034526` with 96efa41 (battle 8 turns + snaps, win, camp, confession talk, rest); R5 `20261103-035412` (rejoin in T2Tavern + host frozen 45 s, guest killed + host reload); R6 `20261103-040154` (R5 reload part again); R7 `20261103-040800` (reload without guest, late join, battle rejoin); R8 `20261103-041811` (ruins puzzle left open, rejoin in T2Tavern: repro); R9 `20261103-042659` (rejoin inside the open NinePuzzle: repro); with faf2201 + 9e135ad: R10 `20261103-044140` (same rejoin: fixed), R11 `20261103-045021` (learn/equip, shield in battle), R12 `20261103-050445` (inspect, skill bar pages, rejoin during a confession). (a) for every pass: R1 and R2 A+B shim.log 132 `tips: patched` lines, 101 distinct names = every pass of `patch_image` + harness + image line, no `skipped`, no panicked/FATAL/Uncaught. Below: (b).

| Pass | Evidence (b) | Status |
| --- | --- | --- |
| start_choice item/unit tips | new-game StartChoice screen: no harness path to a new game | NOT-OBSERVABLE (new-game UI) |
| friendly_fire | needs an enemy area attack that hits its own side; not forceable by console | NOT-OBSERVABLE (no trigger) |
| career_plan | needs the Career Plan perk + attribute apply in UnitInfo | NOT-OBSERVABLE (UI input) |
| customize_slots | new-game customize screen | NOT-OBSERVABLE (new-game UI) |
| coop_gates | R1/R2: no "waiting for the other players" after load (`welcome` only), host-only place leave/enter x5 in R2 followed by the guest (`enter client` x10 on B); camp/rest R4 | PASS |
| force_leave | R2: host `console quit` from 5 places with the guest idle, no `leave pending` hold; busy-guest timed release not driven | PASS (idle path) |
| npc_inspect | R12 `20261103-050445`: host `inspect T2Recruit1` in T2Tavern: A meLock `ent.p.Npc(T2Recruit1)`, UnitInfo open; B meLock null, `locked` [] (the lock stays on the inspecting machine) | PASS |
| dialog_recruit / camp_any_unit / hold_speed / skill_cost / censer_tip / tip_overflow / tooltip_input / scroll_hit / window_close / window_drag / alt_world / battle_camera / chest_buttons click / take_all / tavern_resume / job_confirm (JobSelector UI) | UI input (clicks, hover, drag, keys) or a tavern day report | NOT-OBSERVABLE (needs foreground) |
| dialog_inventory / camp_talk | R4: camp, `console confession ConfessionHate`, `talk`: A and B in world.Confession, `invVisible` true on both (talk not refused: nobody locked) | PASS |
| fake_npc | R12: camp, `console confession ConfessionHate`, `talk` (A and B world.Confession), B frozen 135 s, rejoins: B world.Confession, connected, not loading (no endless load). No `mp: fakenpc:` line (the cheat's NPC id is an allNpcs key: miss path not hit); B then logs `Dialog not found #ConfessionHate.Start` (Found row). R15 `20261103-053857` (6a90a09): B logs `mp: fakenpc: confession NPC rebuilt from confession data: ConfessionHate`, no `Dialog not found` | PASS (join loads; rebuild path hit) |
| loot_level / armor_any / champion_gear / loot_order / loot_pity | R1 rats `console win`: debrief loot equal on A and B (12 stacks incl. BowCommonOutlaws1 worn gear, ChampionStatuette); level/armor odds not asserted | PASS (drops equal; odds unit) |
| weapon_set | R11 `20261103-045021` (verbs 9e135ad): host `learn 4 WeaponSwapSword` (Анян, Sword2HStart): A and B slots gain SecondaryWeapon `allowed [Sword]`, `identicItemType false` with set 1 filled; guest `equip 4 Secondary SwordStart` ok: weapon Sword2HStart + weapon2 SwordStart (mixed 2H + 1H) on A and B; guest `equip 4 Shield Torch`: weapon cleared (Sword2HStart back in B's inventory), weapon2 SwordStart kept, left Torch, same on A | PASS |
| timeline_hud | R1: `timelineHud: why=2/4/5 ... shown=swords` A x72, B x70 | PASS (log) |
| skillbar_compact | R12 dump `battle.sbar` of the playing machine, 4 turns: one page each, skills packed from slot 0, null only as trailing padding (Юлик `[FirstAid,CrossbowReload,-x8]`, Одик `[Dash,-x9]`, Анян `[FirstAid,-x9]`); a saved layout with stale ids not forced | PASS (pages packed) |
| shield_total | R11: dump currentUnit `shield` = unitHud.healthBars.guard.value (the patched sync's value): Юлик guard 8 -> 8; after `console status Protection` (reduce 25) -> 31 = 100 - ceil(75*92/100); Волколак guard 0 reduce 10 -> 10 (vanilla 0, number shown); Одик 6/6, Анян 10/10 on A and on B (guest computes the same for the units it shows) | PASS |
| job_xp | R2 `job Thief/Woodcutter/Scholar/Blacksmith/Bard` (removeTrait/addTrait) ran without error; xp kept not read | PASS (runs; xp unit) |
| barrier | R1 battle enter/leave, reload: `doEnterMode`/`doLeaveMode`, `leave all clients ready`; R2 5 place switches | PASS |
| ready_start | R1 `mp: ready: count ready=2 of=2`; R3 FAIL (only guest gone, host reload: ready=1 forever); fixed 96efa41, R5 `20261103-035412`: guest killed, host reload: `host loaded ready=0` 04:00:12, `a player who never connected is no longer waited for: X29f792402` 04:00:27 (15 s), `gameplayStart ready=2`, host on WaitConnexionCoop | PASS (after 96efa41) |
| returning_units / drop_in late join | R7 `20261103-040800`: guest killed, host reload started without it (15 s), then B `join 01ZG00016YNKFARE`: host `> Join`, both dumps Одик/Анян owner X2 connected true | PASS |
| battle_rejoin | R7: `console battle`, B killed, relaunched, joined: host `mp: battle rejoin: the running battle was replayed to a late joiner`, B in battle.Battle connected | PASS |
| battle_takeover | R7: with B gone the host keeps B's units in the battle (`connected:false`), back to true after the rejoin; host driving them not asserted | PASS (state; play not driven) |
| drop_in | R1 reload + reconnect rejoins (`> Join` x2 on host, guest back) | PASS |
| fade_chain / diag | R1: `mp: fadeTo` x4 A+B, `mp: error stack:` lines (diag prints game errors) | PASS (log) |
| follow / marker_names | world map snaps R1 `world-A/B.png`; follow needs a move order | NOT-OBSERVABLE (UI input) |
| prepared_skill / rpc_cancel / skill_result | need a refused two-turn skill / cancelled RPC: race-only guards | NOT-OBSERVABLE (defensive) |
| skill_sync / skill_vars | R1 rats: guest turns played on B (`endturn` by owner, 3v8 -> 3v36), no error; bar redraw not read | PASS (no error; redraw visual) |
| camp_chest | R1 chest panel on the world map (dump `chest` visible 331x227, `world-A.png`) and `put`/`take` through it | PASS |
| party_inventory / counts / lists / recipes | costs from the party: not driven | NOT-OBSERVABLE (UI dialogs) |
| party_activities | R2 lockpick: host `play LockPick GearCrate` with picks (`spectate LockPick 0` on B) | PASS |
| loot_all / debrief | R1 `rats-debrief-guest.png`: guest debrief has "Take all" and repair 2/2, cure 1/1 enabled | PASS (shown; not clicked) |
| mode_windows | R4: B `sheet` before rest: B `gwins` keeps ui.win.UnitInfo through the confession, camp and RestReport | PASS |
| stale_lobby | needs a lobby left mid-connect | NOT-OBSERVABLE (race) |
| log_error_cast | no relay failure in these runs | NOT-OBSERVABLE (failure path) |
| master_config | run `20261103-015616`: `patched master config fn@14965`; MPMAN `Connecting to 127.0.0.1` x2, master `login Morgott`; B (instance 2) reached its own master 127.0.0.2:60442 (netstat) | PASS |
| reload_route | runs `-015724` (direct), `-020421` (punch: guest id `W127.0.0.1:65214`, differs from host's): guest `mp: reload: connecting to the server id this game joined by`, rejoined | PASS |
| reconnect | runs `-015724`, `-021510`, `-020421`: `mp: reconnect: connection to the host lost, rejoining`, guest back in world.World | PASS |
| drop-in (reconnect lobby) | run `-020421`: `lobby ... game transport: direct (reconnect lobby keeps the running game's transport)` after start | PASS |
| start_transport | R1 direct, R2 punch: game ran over the route it joined by (`host's relay address ... the game connects over the route it joined by`) | PASS |
| reconnect retry (hung connect) | R1 and R5: host frozen 45 s during B's rejoin: B `no answer to the game connect in 20 s, retrying` x2 (~21 s apart); R5: after the thaw B back in T2Tavern PlaceView, connected, not loading | PASS |
| reconnect mid-game rejoin | R1 direct and R2 punch: `connection to the host lost, rejoining` after B frozen 135 s, host `> Join` 2 s later; R2: B then stuck loading: Game.start threw `Null access .getRotationQuat` in `ui.win.Activity.initData` (Activity.hx:177) after `Removing Element A1BridgeGroup` (host in T2Tavern after sing) PASS on world map (R1) and in T2Tavern (R5); R2's hang = the open ruins NinePuzzle (Found row), fixed faf2201, R10 rejoin inside the host's NinePuzzle PASS | PASS (after faf2201) |
| punch redial | R2: stream reset by host on thaw (`punch: stream caef7d56 reset by 127.0.0.1:14251`), A `punch: 127.0.0.1:14252 answers`, `punch: tunnel from 127.0.0.1:14252`, relay client cid 3973 over it, Join + SyncDone | PASS |
| mod_version / alive_bind / net_guard / style_guard / patch_queue / jit_names | guards on error / mismatch paths: no `mp: net:` / `mp: style:` line in R1/R2 (nothing threw); jit_names: the patched image runs (JIT crash is at start) | PASS (jit_names), NOT-OBSERVABLE (others: fire only on a throw / mod mismatch) |
| lock_diag | R1: `mp: lock: end turn blocked, lockCounter=1` A x5, B x15 | PASS (log) |
| ping_cell | needs a ping (UI input) | NOT-OBSERVABLE (UI input) |
| forge_mirror | R2 forge: B `mp: forge recv` per hit (0 0 0 5 .. 2 2 0 4), A `forge send` | PASS |
| work_mirror / coop_spectate | R2: B `spectate LockPick 0`, `WoodCutting 1`, `NinePuzzle 0`, `Sing 1`, `work recv 0/1/2` for the song | PASS |
| activity_injury | needs a failed activity | NOT-OBSERVABLE (no trigger) |
| rest_report | R4 `rest` in camp: dump `gwins` has ui.win.RestReport on A and B | PASS |
| all_inv | world snaps show the bar button; give/take = UI | NOT-OBSERVABLE (UI input) |
| title_version | R1/R2: `mp: title version label: Co-op Fix v0.2.17<br/>v.1.0.48274` (title not snappable: `snap` needs Game) | PASS (log) |
| loading_draw / colorblind_load | visual loading frames / colour-blind option | NOT-OBSERVABLE (frame capture / option) |
| save_kind / save_order / save_list | R1/R2/R3 `host save006` picks the save from LoadGame.loadGames' list (headers via readSaveHeader), loads it (checkSave full read): `harness: host save save/save006.dat`; names / order not read | PASS (list + load) |

| activity_join (new, faf2201) | R10: guest rejoins inside the host's open NinePuzzle, loads (was R8/R9 `Null access .getRotationQuat`) | PASS |

Counts (per pass call of `patch_image`, 87 with activity_join): PASS 46, NOT-OBSERVABLE 41, FAIL 0 open (ready_start fixed 96efa41, puzzle rejoin fixed faf2201). Re-check of NOT-OBSERVABLE (R10-R12): weapon_set, shield_total, npc_inspect, fake_npc, skillbar_compact -> PASS; the rest keep their reason (new-game UI, mouse/keyboard input, race / failure-only guards, no forceable trigger).

## v0.3.0 harness re-check (2026-11-03, dist-test from bf2bf39 + harness-only changes)

Runs `D:\WartalesTest\runs\20261103-091434` (items 3, 4, 7, 8), `-100311` (save list without the save_list pass), `-100451` (save list, corrupt save), `-100839` (shield shown). Background only (dump, `snap`, PrintWindow flag 2 without raising).

| Item | Evidence | Status |
| --- | --- | --- |
| 3 host load from world | `regress.ps1 -Case reload`: B `mp: reload: connecting to the server id this game joined by`, host 2nd `> Join` (86655296) | PASS |
| 3 host load from a battle | host `host save006` in battle.Battle (RatInvasion): B reload line 86701593, host `> Join` 86713031, both world.World, B connected; again later from a stuck battle | PASS |
| 3 host load, guest dead | B killed, `host save006`: `host loaded ready=0` 86755437, `a player who never connected is no longer waited for: X29f792402` 86770437 (15.0 s), gameplayStart; B relaunched, `join 01ZG00016YNKFARE`: host `> Join` 86817968, Одик/Анян owner X2 connected | PASS |
| 4 guest network drop | new `coop.ps1 drop -Inst B -TimeoutSec 15` (SetTcpEntry DELETE_TCB on B's links to other processes, repeated 15 s; 5 resets): B `connection to the host lost, rejoining` x3 (86895296, +9.5 s, +18.4 s), host `> Join` 86915375, B connected 13 s after the drop ended, no input | PASS |
| 5 save list | 33 saves (save006 copies): `host zzznone` (LoadGame.loadGames, no load) minus a no-op verb, from ack ticks: without save_list (pass commented out, test build only) +528/+501/+490 ms; with it +33/+15 ms. `host save099` (64 bytes XOR-ed mid-file): listed, load refused, vanilla popup "Файл поврежден и не может быть прочитан. save/save099.dat" (PrintWindow); `host save020` opens the co-op lobby | PASS |
| 6 shield_total | Одик with Torch (no shield): currentUnit dump `shield=0 shieldShown=False guard=0 reduce=0`; host `console status Protection`: `shield=25 shieldShown=True reduce=25` on A and B (new dump field `shieldShown` = Counter.tf.visible) | PASS |
| 7 weapon_set | Анян + WeaponSwapSword, A and B dumps equal each step: 2H set1 + 1H set2 + Torch -> `w= w2=SwordStart l=Torch`; + Shield -> `w= w2=SwordStart l=ShieldStart`; 1H set1 + 2H set2 + Torch -> `w=SwordStart w2= l=Torch`; + Shield -> `w=SwordStart w2= l=ShieldStart`; 2H + 2H + Torch -> `w= w2= l=Torch`. Battle after 1H set2 + Torch: Анян skills `Move,SliceStart,FirstAid,WeaponSwapSword(off),TorchStrike,...` (2H before: `CleaveStart`), sbar `[FirstAid,TorchStrike]` | PASS |
| 8 remote popup over a minigame | guest `act TavernerT2` (click RPC) during the host's song opened the shared Dialog on both and ended the song (FAIL, run `20261103-122907`: GameMode.setWindow removes the current window). 45bd2c3 (replaces 2d3910b's click refusal): run `20261103-144641`: host sings + guest talks: guest Dialog at once, host `mp: window waits for the minigame`, song to its result, `window shown after the minigame`; guest sings + host talks: host Dialog at once, guest's held by Window.alive (D) until its song ends; guest forges (non-coop) + host talks (Npc.talk now gates on the clicker's own lock): host Dialog, guest keeps ForgeAction, Dialog later | PASS (after 45bd2c3) |
| 8 own sheet across modes | B `sheet` (UnitInfo): kept through place -> world -> place -> camp -> world; gone on battle entry | PASS |
| 8 non-party sheet | T2Tavern, B `inspect T2Recruit1` (UnitInfo, meLock Npc(T2Recruit1)); host `console quit`: B UnitInfo closed, meLock empty | PASS |

Found:
- harness `equip` verb calls Unit.equipSlot without taking the item out of the inventory: the item stays listed (count grows on every swap), even for a host item on the host; R11 shared it. Slot results above are still valid; inventory counts after `equip` are not. OPEN (verb should remove the stack like a drag's pick). FIXED 4ca81cc: pick into the hand + _drop + netEquipSlot (refused equip puts it back); guest Torch: inventory 5 -> 5 stacks, Torch gone, Sword2HStart back (run `20261103-105538`).
- two RatInvasion battles started after guest `equip` swaps (items both equipped and listed) stalled: once `Simultaneous attacks` + `assert: onEnd was not called on Bite` (wolf AI, lockCounter 2 forever, `console win` no effect), once no current unit on the host with no error; same battle with no swaps or with a host-equipped Torch ran normally. Not root-caused; suspected the duplicated item. OPEN. Re-check: 6 battles with real equip + 10 in-battle WeaponSwap (`skill`, CleaveStart<->SliceStart on B) + Torch, 0 stalls/errors (`-110944`, `-112839`); A/B with the old dupe equip (`-131851`, 3 battles, items equipped AND listed): no stall either. Cause not reproduced, so NOT attributed to the dupe (Codex: unproven); one ~100 s quiet round start at 3v51->1v51 rats in `-110944` cut by `console win` (slow AI or stall unresolved). OPEN (not reproducible).
- `coop.ps1 stop` returned while a game was still exiting (its last shim.log line came after the next run's start): the new A logged `net: already running in this session, not started`, no master, "Не удалось создать сеанс игры". Fixed: stop waits on the process objects (WaitForExit). Players: same if the game is relaunched while the old process still exits (Steam blocks that).
- a guest's Leave during the host's LockPick ends it at once (`leave pending: go`): LockPick sets no lockedWith (meLock empty), so force_leave has nothing to wait on. Vanilla gating is the same lock; noted, not changed. FIXED 2d3910b: a coop activity sets no lockedWith (startActivity__impl locks non-coop only); flActivity (ActivityWindow on the host) -> place/tavern/camp leave "player busy". Run `20261103-124143`: guest leave in LockPick -> `leave pending: player busy`, `Leave waits`, after `lpstop` `go`, both world; host song (`-121050`): waits, goes after the song result.

## v0.3.0 fix pass (2026-11-03 afternoon)

Update: 2d3910b's click refusal blocked players (owner rule: nobody is blocked by another's activity); replaced by 45bd2c3 (defer other players' windows on the minigame machine: setWindow / setFocus hooks, flMine ownership in flMinigame / flDefer, Npc.talk / Chest.action own-lock gate). Leave still waits for minigames (party move). Regression on 45bd2c3: run `20261103-145539` (+ `-150602` save list): items 3-8 PASS as in the table below (save099 shot skipped: window not on top; load refused, save020 lobby opens). 51-rat ~100 s round start: camera target moves every dump (enemy turns), normal per owner. dist\winmm.dll 5392384 B, SHA256 17D45288D7AC5CC7E2D957CCC14B8C8B03670A0F735D4787C4DCA334169C5D2D.

Commits 4ca81cc (harness: equip as a drag, `act` via Controller.playerTriggerAction, `skill <id>`), 2d3910b (force_leave: flActivity holds leaves; flClick refuses a player's element click on the host while a minigame window exists). Test dll from 2d3910b; regression run `20261103-134156` (+ `-135601` save list):

| Item | Evidence | Status |
| --- | --- | --- |
| 3 load world / battle / guest dead | B `reload: connecting to the server id this game joined by`, host 2nd `> Join`; from battle.Battle same; B killed: `host loaded ready=0` 102762718, `no longer waited for` +15.0 s, gameplayStart, B relaunched + join: Одик/Анян connected | PASS |
| 4 drop 15 s | 4 connections reset, B 3 rejoin lines, host `> Join`, B connected world.World | PASS |
| 5 save list | 34 saves; `host zzznone` ack within a no-op round trip; save099 (64 B XOR) listed, load refused, vanilla "Файл поврежден" popup; `host save020` opens the co-op lobby | PASS |
| 6 shield_total | Одик + Torch (equip verb): shield 0 shown False; +Protection: 25 shown True on A and B | PASS |
| 7 weapon_set | 2H+1H, +Torch `w= w2=SwordStart l=Torch`, +Shield `l=ShieldStart`, 2H set2 + Torch `w= w2= l=Torch`; A = B every step | PASS |
| 8 windows | own sheet kept place -> camp; inspect T2Recruit1 then host quit: UnitInfo closed, meLock empty | PASS |

Left: tutorial phase of a host's first-ever minigame (window not built yet) is not held by flActivity; a guest's non-coop minigame is invisible to the host's flActivity (vanilla lockedWith still gates talk / chest / leave). Harness `play` on a client asserts at the end (`doResult threw`, callback id), harness-only.

## Fix pass 3 (2026-11-03 evening): party switches, watchers, tutorial

Commits 2692354 + 5c2416e (force_leave M: syncEnterMode / syncLeaveMode's lockSyncMode read -> flSyncHeld; held while a minigame window exists or another connected player is locked (initiator excluded, replay passes once), started by flPump `SETTLE` 2 s after; window deferral hides only windowRoot), 68d9822 (coop_spectate: every watcher keeps input (no lockCamera / padCursor.locked), an idle watcher of a lockpick / ruins puzzle still sees it live, a busy watcher keeps its own camera, _cancel leaves a busy watcher's camera / locks; the host's minigame window is built before its first tutorial), a1ad895 (harness confirm answers CraftConfirm).

| Case | Evidence (runs `20261103-173326`, `-181847`) | Status |
| --- | --- | --- |
| party switch during host song | host `enter T2Forge`: `mode switch waits for a minigame`, song result, `goes` 5.5 s later, entered T2Forge, no error | PASS |
| party switch during guest forge (non-coop) | host `enter T2Tavern`: waits, `forge recv 2` (end), goes, entered; guest CraftConfirm after the switch: crafted ShieldCommonCraftT1 in inventory, `confirm` closes it | PASS |
| tutorial gap | host lockpick: LockPick window exists while its tutorial is open; guest Leave meanwhile: `leave pending: player busy` until `lpstop`, then go | PASS |
| watcher of host lockpick | guest sees the lock view (snap), opens its own sheet while watching | PASS |
| popups both directions, leave during lockpick / song | as fix pass 2 (deferral, wait), all again PASS | PASS |
| regression 3-8 | `-173326` / `-175458`: load world / battle / guest dead, drop, save list, weapon_set, windows PASS; shield dump fields empty / odd at some turns on this and on the fd21f96 build alike (harness dump timing, not the patch) | PASS (shield: harness flake) |

dist\winmm.dll 5447168 B, SHA256 D3B211C3F768B83D80B8958DEC073F7153F33527938A5278E9DCEBE19D87C9D5.

## Fix pass 4 (2026-11-03 late): watcher step-out, switch hold, shield dump

Commits 7a144e7 (harness: dump opens a closed hud and runs HealthBars.sync before reading the shield), 36c16cc (force_leave M: hold only for a recorded non-coop activity element (flActRec at Controller.startActivity__impl, dropped by flActEnd at _callbackResult__impl) or an ActivityGather lock, plus flActivity; window locks never hold), a76539e + 76e4504 (coop_spectate step-out: spectators lose the chooseUnit 10000x10000 Interactive, watched LockPick / NinePuzzle / CylinderPuzzle input catcher propagates, Escape (Game.update loop hook) and the Leave button step out locally, hidden spectator copies skipped by Escape, camera restored via _cancel__impl's block + lockCamera cleared; harness `esc` verb, dump `aw`). Tests 228/228 (harness) + 227/227, lint ok.

| Case | Evidence (run `20261103-190609`, test dll from the uncommitted tree = a76539e content; 76e4504 not run) | Status |
| --- | --- | --- |
| watcher lockpick, Escape | B `aw` watching: `vis true, iw 1280 ih 720, prop true, actInt false, camSave true`; `esc` -> ok, `mp: spectate LockPick out`, `vis false, camSave false`; snap w-after = place view; A (player) unchanged, A `lpstop` ends normally; 2nd `esc` -> hidden | PASS |
| watcher lockpick, Leave button | B `lpstop` (LockPick.stopActivity) -> `spectate LockPick out`, no `not allowed` RPC error, A keeps playing | PASS |
| watcher to the end, no step-out | snap w-noesc-end: place view back | PASS |
| watcher ruins NinePuzzle, Escape | B `esc` -> `spectate NinePuzzle out`, `vis false, camSave false` | PASS (camera snap not taken) |
| switch with guest NPC sheet + chest panel | T2Tavern, B `inspect T2Recruit1` (meLock Npc, UnitInfo, chest panel visible); A `enter T2Forge`: no `mode switch waits`, both T2Forge, B UnitInfo + chest panel still open | PASS (lock-type chest (CounterChest / camp chest) not reachable by harness) |
| shield dump | rats 10 turns: first-dump nulls 0 of 20 (was null on every new turn); values track guard/taken/reduce A = B; negatives (Одик taken 65 -> -65) are shield_total's design (`vulnerability keeps its sign` test), no flicker | PASS |
| host as watcher (guest plays coop lockpick) | no harness path (coop start uses player = host) | NOT-OBSERVABLE |

PENDING (harness stopped by owner request): full regression on the committed build 76e4504 (items 3-8, minigame popup both directions, switch hold during host song / guest forge, lockpick leave), and a re-run of the cases above on it. dist\winmm.dll 5508096 B, SHA256 F18BA43079C3865B7C21B740A6CBCA97F79904868CF194D76AFFAB6643CEA310; dist-test SHA256 6AD5B73E.... Done in fix pass 5 below.

## Fix pass 5 (2026-11-04): regression on bb080aa, guest UI, latch, drag, pump

Runs `D:\WartalesTest\runs\20261104-*`. Test dll = HEAD + harness probes (8bf8f7d: dump `mvis`/`gvis` on-screen per window, `np` tiles, `sdrag`/`drag`, `au`; verbs `tile`, `sdrag`), then + 674ad44 / 9cdf6ec (`-0214*` on). Scripts in the session scratchpad (reg/mini/latch/drag/pump/guestui .ps1); background only (cmd channel, `snap`, PostMessage for mouse / Escape in the drag case).

| Item | Evidence | Status |
| --- | --- | --- |
| 3 load world / battle / guest dead | `-001222`: B `reload: connecting to the server id`, host 2nd `> Join`; from battle.Battle same; B killed: `ready=0`, `no longer waited for` +15.0 s, gameplayStart, rejoin: Одик/Анян connected | PASS |
| 4 drop 15 s | 3 resets, 2 rejoin lines, `> Join`, B world.World connected | PASS |
| 5 save list | 34 saves, `host zzznone` 108-127 ms (= no-op round trip); save099 listed, no lobby; save020 lobby | PASS |
| 6 shield_total | Одик + Torch: 0 / shown False; +Protection 25 / True on A and B | PASS |
| 7 weapon_set | A = B every step (2H+1H, +Torch, +Shield, 1H+2H, +Torch) | PASS |
| 8 windows | `-002704`: own sheet place -> camp -> place; inspect T2Recruit1 + host quit: UnitInfo closed, meLock empty. (`-001222`: a debrief nobody closed blocks the harness `camp` toggle: vanilla modal, harness-only) | PASS |
| inventory re-show in battle (35ec1d5) | `-003242`: open inventory, battle, `sheet` (rebuildInventory): invc ch 372 / 3 children kept, also after the debrief | PASS |
| popup host sing / guest sing | host sings + guest talks: host `window waits` -> `shown after`; guest sings + host talks: host Dialog at once, guest's after the song | PASS |
| switch hold host song / guest song (5a) | `mode switch waits` -> `goes` after the song, host's SingAction copy gone, both T2Forge | PASS |
| switch hold guest forge | FAIL on HEAD (`-013916`, `-021402`: no hold, guest ForgeAction cut, craft lost): 36c16cc records only startActivity__impl starts, Tool.startCraft bypasses it; a craft locks its player with the st.item.Tool (dump: lock `Anvil:<item uid>`, activityUnits key `ent.p.Element(Anvil)`). Fixed 9cdf6ec (Tool lock holds); `-022353` / final: waits, `forge recv 2`, goes, CraftConfirm, item crafted. NPC sheet lock still does not hold (`-023409`) | PASS (after 9cdf6ec) |
| leave during guest forge / host lockpick | `Leave waits: player busy` until the end, then go | PASS |
| watcher step-out lockpick Esc / Leave / to the end, ruins Esc | `spectate LockPick out`, aw vis false camSave false; A unchanged; ruins `spectate NinePuzzle out` | PASS |
| 2 guest unit choice (act Anvil -> ChooseUnit) | ChooseUnit only in B's gwins | PASS |
| 2 guest NPC dialog / ruins barricade dialog | the Dialog is on screen on the host too (mvis true, host camera at the NPC): vanilla shared dialog (host builds it, Element.makeDialog + setFocus); not changed: owner decision | FAIL vs "guest only" (vanilla) |
| 2 theft dialog, leak to a 3rd client | no harness path (towns are CityWorld menus; 2 instances) | NOT TESTED |
| 3 remote-UI latch (forge, then NinePuzzle) | `-005427` HEAD and `-010300` pre-0f641b2 (cea3f6f): tiles move the same on A and B (`tile` x4), no Null access, A's dialog on B (mvis true). Live log (client, bf59f04 build): 40x `moveTileFromSlot threw: Null access` (NinePuzzle.hx:178, tiles null = init never ran): flDefer held the puzzle's alive under a false flMinigame (another player's started Craft / activity-less AnalyzeAction counted), fixed by 0f641b2 (Codex agrees on the mechanism; not reproduced here) | PASS (live cause by code, not reproduced) |
| 4 skill drag cancel (bb080aa) | `-012541`: B `sdrag` + host `console ap` (B's bar rebuilt): slot alloc false, pushed false, no drag after a 150 px move; drag started (dragElement skillSlots) + Escape: pushed false, no drag; + right click: same | PASS |
| 5a client minigame end | as switch hold guest song | PASS |
| 5b `pumping` after a Controller swap | static: only flPump's end cleared it; a load while held left it set, later held switches got no pump (Codex agrees). Fixed 674ad44 (global = owning Controller); `-023409`: held switch, host reload while held, then held again: waits -> goes | FIXED |
| 5c `go_once` | flPump ran processSyncMode with no game (Codex): syncEnterMode's NullCheck throws before flSyncHeld consumes go_once. 674ad44 drops a game-less pump | FIXED |

Tests: patcher 230/230, + harness 231/231 (1 ignored), winmm 163/163, lint ok. dist\winmm.dll 5530624 B, SHA256 A85B77313A4FFF9B9D9C724440BE44760917E8A071594A5D1CF23F30D2E3AF27.
