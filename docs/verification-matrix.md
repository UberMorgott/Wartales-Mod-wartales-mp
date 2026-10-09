# Verification matrix (2026-10-09, branch fix/last-bugs)

Status per patcher pass (`patcher/src/lib.rs`, 71 pass statements + the test-only `harness`) and per shim / helper feature.

- **PASS**: exercised in the co-op harness (two instances, `docs/coop-harness-plan.md`), run folder cited.
- **unit**: only unit tests (`cargo test --release`: 186 passed, every pass module has a `patches_installed_game` test against the installed `hlboot.dat`, several have testsim behaviour tests; `go test ./...` ok).
- **UNTESTED**: neither.

Applied: the test build's `shim.log` (run `20261009-015931`) has 110 `tips: patched ...` lines, no `skipped`; `patch_image` refuses the whole image when any pass skips, so every listed pass is applied whenever the game starts patched. Release build + `shim\check.ps1`: `OK: 0 failure(s)`; seamcheck: release dll clean, test dll has the seam. Real game folder unchanged (file list/size/mtime diff 0, `winmm.dll` SHA256 `FB5157EB…4AC9`).

Runs (`D:\WartalesTest\runs\…`): R1 `20261008-224953` (S1–S3, S6), R2 `20261008-233800` + `20261008-235752` (S4–S7, fixture save006), R3 `20261009-010932` (S8 own inventory), R4 `20261009-012459` (chest 71x57 before the fix), R5 `20261009-013534` (chest / other player's panel scroll, camp, confession repro), R6 `20261009-015931` (panel fixes, S3, loot, S2 reload burst, rejoin burst), R7 `20261009-022255` (patch-progress window), R8 `20261009-024815` (chest reset after resize, resize above the HUD bar, battle camera trace, panel keys cleared), R9 `20261009-030222` (relaunch: vanilla panels from the cleaned user data).

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
| window_drag (+ 6f1219b, reset / HUD bar fixes) | PASS | R8: double click on the chest header after a resize -> vanilla spot and height (dump `chest` y 373 h 227, `chestc` h 170; was y -359 h 599), `A-025022-reset2.png`, `A-025049-reset3.png`; full-height resize stops above the HUD bottom bar (chest y 372 + h 599 = 971 < `hudbar` y 1014), `A-025037-cfull.png`; R9 relaunch vanilla `A-030538-relaunch.png`; R3 own inventory; R5 chest min/mid/full (dump `chestc` + shots), other player's panel min/mid/full + header move; R6 chest above inventory on world map and in camp (`B-020850-b-overlap.png`, `B-020908-b-camp.png`) |
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
| battle_camera | PASS | R8 `camtrace` (per-frame `battle.state.camera`): target speed per 100 ms since the pan key went down is the same with and without a right-button rotation (S only 8.9/5.8/8.8/10.4/11.3/11.7/12.2/13.2; S then RMB 8.5/5.8/8.4/10.5/11.0/12.1/12.0/13.1; RMB then W 8.6/6.2/8.6/10.0/11.0/12.1/12.3/12.6), `targetFollowSpeed` stays 20 (the pan's) during `DragRotateCamera`; ticks `camtest-ticks.json`, `A-025436-camtest.png` |
| title_version | PASS | R6 `B-*-jflk1.png`: "Co-op Fix v0.2.8" on the title screen |
| loading_draw (#3) | PASS | R6 bursts at ~0.4 s: S2 reload (54 frames) and rejoin (65 frames), mean brightness monotonic world -> loading -> game, no flash back |
| scroll_hit | unit | |
| save_kind | unit | in-game: load list shows "Name (Autosave/Quicksave/Manual)" |
| jit_names | PASS | every run: the patched image starts (the JIT crash it prevents is at startup) |
| harness (test build only) | PASS | all runs |

## Shim (`winmm.dll`) and helper (`wartales-mp.exe`)

| Feature | Status | Evidence |
| --- | --- | --- |
| winmm proxy, in-memory hlboot patch, cached image | PASS | every run; `check.ps1` |
| patch-progress window (bb555bf) | PASS | R7 `A-022259-splash0.png`: "Applying co-op patches: 29 of 72... 4 s" |
| game print hook -> shim.log | PASS | every run; R8: hxbit SYNC trace lines counted, not logged (`game: N network SYNC trace lines not logged` at exit), shimcheck covers it |
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

Patcher passes (71; party_inventory counted as 5, harness not counted): PASS 23, unit 48, UNTESTED 0. Shim / helper rows: PASS 8, unit 3 (the six-package row, NAT, SDR), UNTESTED 2 (firewall, install).

## Findings (2026-10-09)

- Fixed: 5f4dfd2 (chest 71x57 until toggled twice), 6f1219b (chest behind a resized inventory), f9047d8 (inventory over a shared dialog on the other player). Codex review: all OK.
- Fixed (fix/last-bugs): a double click on the header of a resized panel put the chest header above the screen (y -359, saved). Cause: the row resize keeps a bottom-anchored panel's top by raising offsetY; the reset restored the styled offsets but kept the grown height, and the issue #4 guard (a panel at its styled spot is left alone) kept it there. The reset now also restores the vanilla height (stored as `mpWinBaseH:<key>` on install) and drops `mpWinSize:<key>`; #4's guard and test unchanged, test `chest_reset_after_resize_is_vanilla`.
- Fixed: the row resize could grow a panel behind the HUD bottom bar; its max now stops at `Game.inst.ui.worldButtonsBar`'s top while shown (test `resize_stops_above_hud_bar`).
- Fixed: the host's shim.log hit its 20000-line cap in ~15 min. Cause: harness copy A's `prefs.sav` had `networkLog` on (an admin console `networkLog` toggled during a harness run; the harness `console` verb forces `PREFS.admin`, and a command that saves prefs then also persists `admin=true`), so hxbit printed a `SYNC >` line per replicated field change; the shim forwards every game print. The shim now counts the trace's SYNC lines instead of logging them (RPC and other lines kept, a note on the first, the count at exit). A's prefs restored from the real game's (`networkLog` off, `admin` off; old file `D:\WartalesTest\fixtures\A-prefs-networkLog-on.sav`).
- Codex review (fix/last-bugs): reset OK, HUD bar OK, shim filter first dropped a whole message starting with a SYNC line (fixed: per line, re-review OK), harness drag now always releases mouse and key.
- Harness copy A's user data reset: the R8 header reset fixed the chest, `ud <key>` dropped the remaining panel keys; clean copy `D:\WartalesTest\fixtures\A-udat_X19f792402-clean.sav` (stuck one: `A-udat_X19f792402-stuck.sav`); R9 relaunch shows vanilla panels.
- Note: every InventoryContent has ~42 px of content beyond whole rows (content 584 vs viewport 542), so its scrollbar always shows; vanilla has the same extra (6 rows: 372 vs 330).