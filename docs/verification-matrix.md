# Verification matrix (2026-10-09, branch fix/panels-verify)

Status per patcher pass (`patcher/src/lib.rs`, 71 pass statements + the test-only `harness`) and per shim / helper feature.

- **PASS**: exercised in the co-op harness (two instances, `docs/coop-harness-plan.md`), run folder cited.
- **unit**: only unit tests (`cargo test --release`: 186 passed, every pass module has a `patches_installed_game` test against the installed `hlboot.dat`, several have testsim behaviour tests; `go test ./...` ok).
- **UNTESTED**: neither.

Applied: the test build's `shim.log` (run `20261009-015931`) has 110 `tips: patched ...` lines, no `skipped`; `patch_image` refuses the whole image when any pass skips, so every listed pass is applied whenever the game starts patched. Release build + `shim\check.ps1`: `OK: 0 failure(s)`; seamcheck: release dll clean, test dll has the seam. Real game folder unchanged (file list/size/mtime diff 0, `winmm.dll` SHA256 `FB5157EB…4AC9`).

Runs (`D:\WartalesTest\runs\…`): R1 `20261008-224953` (S1–S3, S6), R2 `20261008-233800` + `20261008-235752` (S4–S7, fixture save006), R3 `20261009-010932` (S8 own inventory), R4 `20261009-012459` (chest 71x57 before the fix), R5 `20261009-013534` (chest / other player's panel scroll, camp, confession repro), R6 `20261009-015931` (panel fixes, S3, loot, S2 reload burst, rejoin burst), R7 `20261009-022255` (patch-progress window).

## Patcher passes

| Pass | Status | Evidence |
| --- | --- | --- |
| start-choice item tips | unit | |
| start-choice unit tips | unit | |
| friendly_fire | unit | |
| career_plan | unit | |
| customize_slots | unit | |
| coop_gates | unit | |
| force_leave | unit | |
| npc_talk | unit | |
| dialog_recruit | unit | |
| dialog_inventory (new, f9047d8) | PASS | R6: guest inventory + chest hidden during the host's confession (`B-020943-b-conf1.png`), back after (`B-021000-b-conf2.png`); R5 repro before (`B-014837-conf-b.png`) |
| camp_talk | unit | gate refusal not exercised |
| camp_any_unit | unit | |
| camp_choice | PASS | R2 S7: aptitude 1 -> 2 logged on host, both dumps |
| hold_speed | unit | |
| loot_level (higher-level drops) | unit | |
| armor_any | unit | |
| champion_gear | unit | |
| loot_order | PASS | one kill drops worn gear, equal on both: R1 BowCommonOutlaws1, R6 LightArmorCommonGeneric |
| loot_pity | PASS | same runs as loot_order |
| job_xp | unit | |
| job_confirm | unit | |
| barrier | PASS | R1 S2/S3; R6 S3 (backup 0, both in battle in 10 s), S2 (in-game reload, both in game in 64 s) |
| ready_start | PASS | R1/R6 S2: host's "Waiting for players" then start |
| battle_rejoin | PASS | R2 S4 + 71c58e7 freeze fix |
| battle_takeover | PASS | R2 S4: host drives the guest's units after WM_CLOSE |
| drop_in | PASS | R2 S4; R6: guest WM_CLOSE, relaunch, `join FW00009QNCC` into the running game |
| returning_units | PASS | R2 S4: guest owns its units again after rejoin |
| diag | PASS | R6: `mp: error stack` lines in shim.log |
| activity_diag | unit | |
| follow | unit | |
| marker_names | PASS | R5/R6 shots: player labels on the world map |
| skill_cost | unit | |
| skill_sync | PASS | R2 S5 |
| skill_vars | PASS | R2 S5 |
| chest_buttons | unit | buttons drawn (R5), not clicked |
| camp_chest (+ 5f4dfd2) | PASS | R3 world-map chest; R4 first dump chest 71x57, R6 first dump 331x281 (A and B) |
| party_inventory / counts / lists / recipes / activities (5) | unit | |
| loot_all | unit | Take all not clicked |
| debrief_cure | unit | |
| debrief_diag | unit | |
| debrief_enable | unit | |
| window_close | unit | |
| stale_lobby | unit | needs a lobby left mid-connect |
| log_error_cast | PASS | R1 S1 (relay connect failure path) |
| direct_lobby | PASS | R1 S1, R6 join by code |
| window_drag (+ 6f1219b) | PASS | R3 own inventory; R5 chest min/mid/full (dump `chestc` + shots), other player's panel min/mid/full + header move; R6 chest above inventory on world map and in camp (`B-020850-b-overlap.png`, `B-020908-b-camp.png`) |
| take_all | unit | |
| tavern_resume | unit | |
| mod_version | unit | |
| net_guard | unit | |
| tip_overflow | unit | |
| tooltip_input | unit | |
| censer_tip | unit | |
| ping_cell | unit | |
| forge_mirror | unit | |
| work_mirror | unit | |
| coop_spectate | unit | |
| timeline_hud | unit | |
| activity_injury | unit | |
| style_guard | unit | |
| all_inv | PASS | R5: open, resize min/mid/full, wheel + scrollbar drag, header move; give/take not exercised |
| alt_world | unit | |
| battle_camera | unit | no camera field in the dump; testsim `follow_speed_run` |
| title_version | PASS | R6 `B-*-jflk1.png`: "Co-op Fix v0.2.8" on the title screen |
| loading_draw (#3) | PASS | R6 bursts at ~0.4 s: S2 reload (54 frames) and rejoin (65 frames), mean brightness monotonic world -> loading -> game, no flash back |
| scroll_hit | unit | |
| jit_names | PASS | every run: the patched image starts (the JIT crash it prevents is at startup) |
| harness (test build only) | PASS | all runs |

## Shim (`winmm.dll`) and helper (`wartales-mp.exe`)

| Feature | Status | Evidence |
| --- | --- | --- |
| winmm proxy, in-memory hlboot patch, cached image | PASS | every run; `check.ps1` |
| patch-progress window (bb555bf) | PASS | R7 `A-022259-splash0.png`: "Applying co-op patches: 29 of 72... 4 s" |
| game print hook -> shim.log | PASS | every run (cap: 20000 lines, see findings) |
| helper extraction + launch, single-instance mutex | PASS | every run (test seam: per-instance mutex) |
| master emulation: lobby, join code, member ids | PASS | R1 S1, R6 rejoin |
| relay (`wsx`, sha1 X-Pass), `instance/get` answer | PASS | R1 S1 |
| direct transport | PASS | all runs (loopback) |
| `uid`, `code`, `modver`, `sendq`, `link`, `hlpatch` | unit | go test ok |
| NAT (UPnP / STUN) | unit | disabled by the harness seam |
| SDR bridge / Steam invite join | unit | sdrbridge tests; untestable on one PC (same Steam account) |
| firewall rule | UNTESTED | no tests, not needed on loopback |
| install | UNTESTED | no tests |
| applog | PASS | logs in every run |

## Counts

Patcher passes (71; party_inventory counted as 5, harness not counted): PASS 22, unit 49, UNTESTED 0. Shim / helper rows: PASS 8, unit 3 (the six-package row, NAT, SDR), UNTESTED 2 (firewall, install).

## Findings (2026-10-09)

- Fixed: 5f4dfd2 (chest 71x57 until toggled twice), 6f1219b (chest behind a resized inventory), f9047d8 (inventory over a shared dialog on the other player). Codex review: all OK.
- Open: a double-push reset of a resized chest puts its header above the screen (dump `chest` y -359 h 599, saved in `udat`): the chest is bottom-anchored (`offset-y: -410`), the row resize keeps the top by growing offsetY, the reset restores the CSS offset without that growth, and `mpDragClamp` leaves a panel at its styled spot alone (issue #4 guard). The harness copy A keeps this state (`D:\WartalesTest\A\save\udat_X19f792402.sav`).
- Open: the host's shim.log reaches its 20000-line cap in ~15 min of play (`[S] SYNC > ent.Roaming… aggroTime` spam), so later host log evidence is lost.
- Open (vanilla-like): the chest panel at its styled spot grows down behind the HUD bottom bar when resized tall (world map, R5 `A-014426-cfull-a.png`).
- Note: every InventoryContent has ~42 px of content beyond whole rows (content 584 vs viewport 542), so its scrollbar always shows; vanilla has the same extra (6 rows: 372 vs 330).
