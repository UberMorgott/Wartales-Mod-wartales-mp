# Patch audit (2026-11-02, main @ aea5192)

Read-only audit of the bytecode patch set (`patcher/src`, 80 passes in
`lib.rs::patch_image`) for counter-patches, redundant / duplicate passes,
re-entrancy and hot-path cost. Follows `docs/patch-layering-audit.md`
(2026-10-26): that audit's H1 (debrief) and the H3 split (alive_bind) are done
(ffaaa91); its other items are re-checked here. Nothing in the repo was changed.

## Status 2026-11-02

- F1 closed: no double hook runs (joinGate at op 0 + replaying flag; readyMark
  forced flag); merge saves stay under 100 lines.
- F2 done: npc_inspect root fix (the inspect lock stays on the reader's
  machine), npc_talk and dlgLockedWith removed. The timed leave-lock release in
  force_leave is kept on purpose: a lock held under a guest's personal modal
  must never block the host's leave.
- F3 kept: no re-arm event exists; the buttons are reused.
- F4, F6, diagnostic deletions (activity_diag, camp_choice, direct_lobby),
  lock_diag cap, battle_rejoin reset, bard workIdleOf leak: done.
- Open (low, F9): shared mpReport runtime, duplicate proto_slot / want_sig /
  finder helpers.

## Method

- **Mechanical map.** A scratch copy of the patcher (outside the repo) records,
  after every pass, a hash of every function's ops plus its call targets, and
  runs on the real `D:\Steam\steamapps\common\Wartales\hlboot.dat`
  (`wartales-tips patch`, shipped features, no `harness`). Output: which game
  functions each pass changed, which functions it appended, and which game
  functions the appended code calls. Run: exit 0, 32 s, no skip.
- **Totals:** 80 passes, 394 function edits on **360 distinct game functions**,
  **150 appended functions / 6949 ops**. Every pass changes at least one
  function (no pass matches nothing). Only **25 game functions are changed by
  more than one pass** (table below).
- Re-entrancy: appended function calls into a function some pass changed,
  plus new calls that a pass adds inside a changed game function.
- Module headers, `git log`, grep for duplicated helpers and timer constants.
- Second opinion: Codex (`codex exec --sandbox read-only`), last section.

## Functions changed by more than one pass

| Game fn (findex) | Passes | Note |
|---|---|---|
| Game.dispose (3253) | coop_gates, returning_units, window_drag, rpc_cancel, forge_mirror, work_mirror | shared `forget_on_game_dispose`; fine |
| Game.startServer onMessage closure, Game.hx:367 (24121) | barrier, ready_start, battle_rejoin, jit_names | F1 |
| Activity._cancel__impl (11941) | activity_diag, work_mirror, coop_spectate | F7 |
| Debrief.genLoot (11995) | champion_gear, loot_order, loot_pity | F6 |
| 9 candidate closures Debrief.hx:521 (28576-28584) | loot_level, armor_any | F6 |
| Controller.ping__impl (14613) | ping_cell, forge_mirror, work_mirror | by design (mirror.rs); F10 |
| h2d.Scene.handleEvent (1027) | tooltip_input, scroll_hit | prior L12, Codex disputed |
| Dialog.displayChoices (25281) | dialog_recruit, party_inventory | disjoint edits |
| FishingAction.startState (25426) | party_activities, activity_injury | disjoint edits |
| Activity.hx:408 closure (41995) | coop_spectate, jit_names | jit_names guard |
| Controller.waitForClients (14533) | barrier, diag | diag print |
| GameInventory constructor (13532) | chest_buttons, window_drag | disjoint (+ camp_chest in update) |
| World.update (12602) | follow, alt_world | disjoint, per frame, cheap early-outs |
| NetworkClient.processMessage (3975) | net_guard, rpc_cancel | |
| Game.start (3245) | net_guard, patch_queue | disjoint |
| Game.render (3231) | loading_draw, colorblind_load | F11 |
| StartChoice.hx:386 closure (44455) | item_tips, unit_tips | same module |

Appended code calling a hooked function: barrier fn@59928 and ready_start
fn@59934 call the onMessage closure (Join / ReadyToStart replay, F1); follow
fn@59947 calls Controller.playerGoto (own hook, `fIssuing` guard); force_leave
fn@59910/59911 call Tavern.askLeave__impl and fn@59915 Controller.toggleCamp__impl
(own hooks tlAsk / cpAsk re-check); all_inv fn@60037 calls ItemSlot.onRightClick
(own hook mpAllInvGive, F12); barrier fn@59924 / battle_rejoin fn@59938 call
NetworkHost.flush* (net_guard writer depth: intended); chest_buttons fn@59952
calls Inventory._networkOperation (diag trace only). net_guard adds a guest
`Game.initAlive()` call into all ~110 `networkRPC` dispatchers and both
processMessage (alive_bind patches initAlive): documented, a few length checks
when nothing is pending; not a hot-path problem.

## Findings (ranked by risk)

Est. diff = rough patcher-source change for the simplification.

### F1 (High) Host message handler owned by four passes, with undo + replay — prior M9, open
- Files: `barrier.rs:27-28,1339,1479` (joinGate parks Joins, tick replays them),
  `ready_start.rs:56-85` (readyHold: `playersReady--` undoes a counted
  ReadyToStart, readyTick replays it), `battle_rejoin.rs` (late-joiner branch),
  `jit_names.rs` (guard on the same closure), plus `drop_in.rs` (gameplayStart / getServerID).
- Game fn: onMessage closure Game.hx:367 (fn 24121).
- What: barrier parks, ready_start compensates the park (ready_start.rs:55-58:
  "parked by the barrier's joinGate ... never leaves the loading screen"), and
  both replay by calling the closure again (fn@59928, fn@59934), so a replay
  re-enters every other pass's hook in that closure; correctness depends on
  each pass's "a replay goes through" flag (barrier.rs:607, 1007).
- Proposal: one admission table (joined / synced / ready / parked) owned by one
  pass; barrier and ready start read it; one replay site. Delete readyHold's
  undo/replay and barrier's waitLocks cleanup of its own parked Joins.
- Est.: -800..-1200 lines across barrier.rs (2713) / ready_start.rs (2688); 3-4 days + harness.

### F2 (High) force_leave: timed lock release compensating the NPC-inspect lock — prior H4, open
- Files: `force_leave.rs:200-209` (WARN_AFTER 15 s, WARN_EVERY 30 s, PAUSED_EVERY 5 s,
  ASK_EVERY 1 s, RELEASE_AFTER 3 s), header F2 "flRelease ... cleared with
  `me.set_lockedWith(null)`", C cpAsks (closes camp tools every ASK_EVERY);
  `npc_talk.rs`, `dialog_recruit.rs` (edit 1), `camp_talk.rs`, force_leave guard 3.
- What: `Npc.inspect` takes a party-blocking `lockedWith`; four passes each
  re-derive "this lock is only an inspect", and force_leave finally clears any
  non-minigame lock after 3 s. A timer that clears game state is the safety-net
  pattern the owner rejected (memory keep-mod-code-simple 2026-10-08), and it is
  a counter to the lock vanilla and our passes keep.
- Proposal: fix at the source (multi: inspect does not take the party lock, or
  `getPlayerLocked` skips an inspect-only lock, keeping the same-NPC rule); then
  drop npc_talk's wrapper, dialog_recruit edit 1, guard 3 and the RELEASE_AFTER
  release. Keep the WARN notes only if still useful.
- Est.: -300..-500 lines; 1.5-2 days; largest pass (5810 lines, 21 appended fns / 1339 ops).

### F3 (Medium) coop_gates G1: forced first click + time-window repeat guard — prior M6, open
- File: `coop_gates.rs:120-123` (REPEAT_SAME_S 2 s, REPEAT_OTHER_S 5 s), 23 `repeat` refs.
- What: G1 forces the vote branch; the repeat guard (two ObjectMaps, Game.dispose
  cleanup) exists only because the forcing ran actions twice; force_leave relies on it.
- Proposal: run cb() once per armed ButtonData and clear on completion/re-arm
  (Codex 10-26: explicit completion/reset, not once-forever). Drop both maps and timers.
- Est.: -150..-250 lines; ~1 day.

### F4 (Medium) Steam path disabled, then re-added — prior H2, open
- Files: `direct_lobby.rs` (Lobby.isSteamOnly false for a 'D' id),
  `start_transport.rs` (instance/get callback gets the Steam case `p(serverID, null)`).
- Proposal: one edit in `Lobby.setupPlatform` taking the transport from the
  master's answer; delete direct_lobby (173 lines). Both join paths retest
  (Steam invite not harness-testable).
- Est.: -150 lines; ~1 day incl. master.

### F5 (Low) Sing on two paths — prior M8, open (M7 is done)
- M7 resolved: `coop_spectate.rs:1313-1323` now gates the vanilla camera / pad /
  lock writes with `JTrue` on a spectator; `spectPost` is gone (Codex check).
- Open: Sing is in the spectate CAMERA list (`coop_spectate.rs:89`) and also
  mirrored by `work_mirror.rs:782` (SingAction.startSong). Codex: the two do
  different jobs (camera vs animation), so not a counter-patch.
- Proposal: none unless Sing is touched again; then one owner decision.
- Est.: n/a.

### F6 (Medium-Low, NEW) loot_level: one edit made unreachable by a later pass of the same module
- File: `loot_level.rs:18-25` (patch_loot_level, edit 2: armor `canEquip` + JFalse ->
  `hasCantEquipReasons` + JTrue) vs `loot_level.rs:144-151` (patch_armor_any:
  the armor-branch `JTrue` -> Nop, armor pushed like weapons). Same 9 closures
  (fn 28576-28584), both 2026-10-08 (c7e095a, then 01e89ca).
- What: once armor_any skips the armor branch, the branch loot_level rewrote is
  never reached: a fix-on-fix whose first half is dead code that still has to
  match (a game update that changes it refuses the whole image).
- Proposal: delete edit 2 of patch_loot_level (and its test asserts). Fold
  patch_champion_gear / patch_loot_order / patch_loot_pity (all on genLoot) and
  the two closure passes into one `patch_loot` pass with one validation.
- Est.: -40..-80 lines; 1-2 h.

### F7 (Low, NEW) Three entry hooks on Activity._cancel__impl
- Files: `activity_diag.rs:148` (trace), `work_mirror.rs:142,829` (workEnd at op 0),
  `coop_spectate.rs:532,1558` (spectEnd at op 0, "the rest shifted by one").
- What: three inserts at op 0; the resulting order is pass order (spectEnd runs
  first because coop_spectate is applied later). Each re-validates the shape.
- Proposal: drop the diag (F8); one `mpActEnd(this)` calling spectEnd then
  workEnd, appended by one pass.
- Est.: -30..-60 lines; 1-2 h.

### F8 (Low) Shipped print-only diagnostics — prior L14, open
- `activity_diag.rs` (22 game fns; the freeze it traced was fixed by
  activity_injury / coop_spectate), `camp_choice.rs` (3 fns, "trace ... without
  changing gameplay"), also `lock_diag.rs` (4 fns + 1 appended) and `diag.rs` (17 fns).
- What: each costs a match on every start and a refusal risk on a game update;
  activity_diag and camp_choice serve closed investigations.
- Proposal: delete activity_diag and camp_choice; keep diag (and lock_diag while
  battle-lock logging is open), or put diagnostics behind the test build.
- Est.: -800 lines (574 + 492 incl. tests); 1 h.

### F9 (Low, NEW) Duplicated per-module trap + capped-log runtime; duplicated patcher helpers
- Runtime: ~20 modules wrap their appended code in their own `Trap` that
  swallows and logs, with separate caps: `all_inv.rs:109`, `window_drag.rs:109`,
  `style_guard.rs:55`, `net_guard.rs:76` (LOG_CAP), `lock_diag.rs:31` (REPORT_CAP),
  `timeline_hud.rs:769` (add_report). Same pattern as net_guard's swallow
  (prior H3): an exception in our code becomes a log line and half-done state.
- Patcher-side copies: `proto_slot` (alt_world.rs:92, barrier.rs:193,
  friendly_fire.rs:28), `want_sig` (harness.rs:444, returning_units.rs:105,
  window_drag.rs:417), function finders (diag.rs:109 `static_fn`, follow.rs:111,
  party_inventory.rs:1900).
- Proposal: one appended `mpReport(tag, exc)` (diag.rs) with one cap; consider
  rethrow for our own code outside the network path. Move the helpers into lib.rs
  (continues 1102d6f).
- Est.: -200..-400 lines; 0.5-1 day.

### F10 (Low, NEW) forge_mirror and work_mirror each append their own idle tracker
- Files: `mirror.rs` header (`<mirror>IdleOf` / `<mirror>Idle`, one ObjectMap per
  mirror), appended as forgeIdle fn@59994 and workIdle fn@59999 + their maps +
  two Game.dispose cleanups.
- Proposal: mirror.rs appends one IdleOf/Idle and one map (key by worker; the
  mirrors never share a worker at once). Keep the per-mirror Recv.
- Est.: -60..-100 lines; 2 h.

### F11 (Low, NEW) Game.render: two passes on the "no state / loading scene" path
- Files: `loading_draw.rs` (draw loadingS2d when state == null), `colorblind_load.rs`
  (open the colour-blind pass only when state != null && loadingS2d == null).
- Not a counter-patch; both small. Merge into one `render_load` pass only if
  either is touched again. Est.: -50 lines.

### F12 (Low) Re-entry into own hooks, guarded by design
- follow fn@59947 -> Controller.playerGoto (own followCancel hook; `fIssuing` flag).
- force_leave tlUpdate -> Tavern.askLeave__impl -> tlAsk re-checks tlWhy (double
  evaluation, harmless); cpAsk same for toggleCamp__impl.
- all_inv stand-in click -> source slot's onRightClick -> mpAllInvGive: on a
  foreign slot give() only records the target and lets the vanilla take run
  (all_inv.rs:2334-2336, 2777-2781); ctrl-give acts on the own inventory only.
  Side effect: the foreign panel becomes the give target. No loop.
- No action; note for any change to these guards.

### F13 (Low) Ordering guards for own patches
- `jit_names.rs` must run after every pass that appends functions; `harness::patch_harness`
  (feature `harness`) runs after it (`lib.rs:243-248`), so harness-appended
  functions are not covered (test build only).
- `battle_rejoin.rs` Battle.afterEnter `if (!isAuth && world == null)` around
  lockAlives compensates its own replay order (prior M10, Codex AGREE 10-26).
- Proposal: run jit_names after the harness block too; M10 optional.

## Prior audit (2026-10-26) status

| Item | Status at aea5192 |
|---|---|
| H1 debrief | done (096a2da, debrief.rs) |
| H2 direct_lobby + start_transport | open -> F4 |
| H3 net_guard | split done (1171671 alive_bind); swallow scope open |
| H4 inspect lock | open -> F2 |
| M5 window lock | Codex disagreed; no action |
| M6 coop_gates repeat | open -> F3 |
| M7 coop_spectate restore | done (gated writes, spectPost removed) |
| M8 Sing two paths | open, low -> F5 |
| M9 handler admission | open -> F1 |
| M10 battle_rejoin | open -> F13 |
| M11 all_inv | open (Codex: only the drag adapter) |
| L12 hit test | Codex disagreed; no action |
| L13 jit_names | open (rename does not work, Codex) -> F13 |
| L14 diagnostics | open -> F8 |

## Patch -> target map (mechanical, real image)

"Game fns changed" counts edits per pass; targets are `Class.method (file:line)`;
`.String` = a closure (named by its first debug line).

| # | Pass (module::fn) | Game fns changed | New fns / ops | Targets (first 8) |
|---|---|---|---|---|
| 1 | lib::patch_start_choice_item_tips | 1 | 1 / 13 | .String (ui/win/StartChoice.hx:386) |
| 2 | lib::patch_start_choice_unit_tips | 1 | 1 / 41 | .String (ui/win/StartChoice.hx:386) |
| 3 | friendly_fire::patch_enemy_area_friendly_fire | 1 | 0 / 0 | battle.skill.Skill.gatherTargets (battle/skill/Skill.hx:492) |
| 4 | career_plan::patch_career_plan | 2 | 0 / 0 | ui.win.UnitInfo.onBonusAttributeApply (ui/win/UnitInfo.hx:2223); hxbit.closure.Ui_win_UnitInfo_onBonusAttributeApply_L2234_fcde1e.call (hxbit/Closure.hx:168) |
| 5 | customize_slots::patch_customize_slots | 2 | 0 / 0 | LobbyState.initAssignments (Lobby.hx:258); ui.win.CustomizeScreen.initS3d (ui/win/CustomizeScreen.hx:1557) |
| 6 | coop_gates::patch_coop_gates | 9 | 0 / 0 | Game.dispose (Game.hx:1176); .String (Game.hx:1289); .String (ui/comp/Button.hx:91); .String (ui/comp/Button.hx:153); ui.comp.Button.doPadClick (ui/comp/Button.hx:210); ui.win.Dialog.checkAllReady (ui/win/Dialog.hx:1807); ent.Place.setLeaveState__impl (ent/Place.hx:928); ent.Player.canCamp (ent/Player.hx:2243); +1 more |
| 7 | force_leave::patch_force_leave | 11 | 21 / 1339 | st.item.Tool.closeActionWindow__impl (st/item/Tool.hx:552); ui.GameUI.toggleCamp (ui/GameUI.hx:409); world.PlaceContent.update (world/PlaceView.hx:78); st.Controller.toggleCamp__impl (st/Controller.hx:405); st.Controller.closeTavern__impl (st/Controller.hx:411); st.player.Tavern.askLeave__impl (st/player/Tavern.hx:2366); world.PlaceView.update (world/PlaceView.hx:636); world.PlaceView.tryClose (world/PlaceView.hx:980); +3 more |
| 8 | npc_talk::patch_npc_talk | 1 | 1 / 50 | ent.p.Npc.talk (ent/p/Npc.hx:297) |
| 9 | dialog_recruit::patch_dialog_recruit | 2 | 2 / 87 | .String (ui/win/Dialog.hx:1049); ui.win.Dialog.displayChoices (ui/win/Dialog.hx:792) |
| 10 | dialog_inventory::patch_dialog_inventory | 1 | 0 / 0 | ui.win.Dialog.onAlive (ui/win/Dialog.hx:185) |
| 11 | camp_talk::patch_camp_talk | 2 | 0 / 0 | st.Controller.playCampConfession__impl (st/Controller.hx:325); st.Controller.playPersonalQuestReminder__impl (st/Controller.hx:330) |
| 12 | fake_npc::patch_fake_npc | 1 | 0 / 0 | ent.p.FakeNpc.initData (ent/p/Npc.hx:1127) |
| 13 | camp_any_unit::patch_camp_any_unit | 4 | 0 / 0 | .String (world/camp/CampMode.hx:114); .String (world/camp/CampMode.hx:137); st.player.Camp.removeUnit (st/player/Camp.hx:1338); world.camp.CampEntryEntity.canDrag (world/camp/CampEntryEntity.hx:737) |
| 14 | camp_choice::patch_camp_choice | 3 | 0 / 0 | ui.win.Dialog.setClick__impl (ui/win/Dialog.hx:2226); ui.win.Dialog.setClick (hxbit/Macros.hx:2342); st.player.Confession.giveGains (st/player/Confession.hx:757) |
| 15 | hold_speed::patch_hold_speed | 1 | 0 / 0 | ui.BaseUI.holdAction (ui/BaseUI.hx:306) |
| 16 | loot_level::patch_loot_level | 9 | 0 / 0 | .String (battle/Debrief.hx:521) |
| 17 | loot_level::patch_armor_any | 9 | 0 / 0 | .String (battle/Debrief.hx:521) |
| 18 | loot_level::patch_champion_gear | 1 | 0 / 0 | battle.Debrief.genLoot (battle/Debrief.hx:449) |
| 19 | loot_level::patch_loot_order | 1 | 2 / 74 | battle.Debrief.genLoot (battle/Debrief.hx:449) |
| 20 | loot_level::patch_loot_pity | 1 | 1 / 40 | battle.Debrief.genLoot (battle/Debrief.hx:449) |
| 21 | job_xp::patch_job_xp | 2 | 0 / 0 | st.Unit._removeTrait (st/Unit.hx:4129); st.Unit._addTrait (st/Unit.hx:4146) |
| 22 | job_confirm::patch_job_confirm | 1 | 0 / 0 | ui.win.JobSelector.chooseJob (ui/win/UnitInfo.hx:655) |
| 23 | barrier::patch_barrier | 3 | 6 / 303 | .String (Game.hx:367); st.Controller.waitForClients (st/Controller.hx:436); st.Controller.update (st/Controller.hx:474) |
| 24 | ready_start::patch_ready_start | 3 | 9 / 295 | .String (Game.hx:367); Game.startServer (Game.hx:351); Game.update (Game.hx:1675) |
| 25 | battle_rejoin::patch_battle_rejoin | 2 | 1 / 28 | .String (Game.hx:367); battle.Battle.afterEnter (battle/Battle.hx:452) |
| 26 | battle_takeover::patch_battle_takeover | 2 | 0 / 0 | battle.unit.Mount.canPlayMount (battle/unit/Mount.hx:178); st.Unit.isControllable (st/Unit.hx:2024) |
| 27 | drop_in::patch_drop_in | 2 | 0 / 0 | Game.gameplayStart (Game.hx:868); .String (Game.hx:3058) |
| 28 | returning_units::patch_returning_units | 4 | 4 / 110 | .String (Game.hx:405); Game.removingPlayers (Game.hx:1029); Game.dispose (Game.hx:1176); battle.Battle.disposeBattle (battle/Battle.hx:4333) |
| 29 | diag::patch_diag | 17 | 0 / 0 | Game.fadeTo (Game.hx:1375); st.Inventory.netAddItem__impl (st/Inventory.hx:171); st.Inventory._networkOperation (st/Inventory.hx:696); shiro.online.$Log.logError (shiro/online/Log.hx:245); st.Controller.onClientReady__impl (st/Controller.hx:356); st.Controller.onServerReady__impl (st/Controller.hx:421); st.Controller.waitForClients (st/Controller.hx:436); st.Controller.syncEnterMode (st/Controller.hx:506); +9 more |
| 30 | activity_diag::patch_activity_diag | 22 | 0 / 0 | ui.win.BoardPuzzle.checkPuzzleSolved__impl (ui/win/BoardPuzzle.hx:258); ui.win.BoardPuzzle.leaveActivity__impl (ui/win/BoardPuzzle.hx:297); ui.win.BoardPuzzle.getOut__impl (ui/win/BoardPuzzle.hx:307); ui.win.BoardPuzzle.successActivity (ui/win/BoardPuzzle.hx:332); ui.win.CylinderPuzzle.failActivity (ui/win/CylinderPuzzle.hx:330); ui.win.CylinderPuzzle.cancelActivity (ui/win/CylinderPuzzle.hx:352); ui.win.CylinderPuzzle.leaveActivity (ui/win/CylinderPuzzle.hx:361); ui.win.CylinderPuzzle.successActivity (ui/win/CylinderPuzzle.hx:374); +14 more |
| 31 | follow::patch_follow | 6 | 6 / 253 | ent.BasePlayer.updateMoveHold (ent/Player.hx:856); st.Controller.playerGotoEntity (hxbit/Macros.hx:2342); st.Controller.playerGoto (hxbit/Macros.hx:2342); world.World.dispose (world/World.hx:162); world.World.updateSprint (world/World.hx:1308); world.World.update (world/World.hx:1371) |
| 32 | marker_names::patch_marker_names | 2 | 1 / 16 | ui.comp.$PlayerArrow.__constructor__ (ui/comp/PlayerArrow.hx:11); ui.comp.$PlayerMarker.__constructor__ (ui/comp/PlayerMarker.hx:18) |
| 33 | skill_cost::patch_skill_cost | 1 | 0 / 0 | ui.comp.$SkillTip.__constructor__ (ui/comp/SkillIcon.hx:68) |
| 34 | prepared_skill::patch_prepared_skill | 1 | 0 / 0 | battle.Battle.setCurrentSkill (battle/Battle.hx:1620) |
| 35 | skill_sync::patch_skill_sync | 1 | 0 / 0 | battle.Unit.set_apSkillPlayed (hxbit/Macros.hx:2160) |
| 36 | skill_vars::patch_skill_vars | 1 | 0 / 0 | battle.Battle.getOrCreateSkill (battle/Battle.hx:1586) |
| 37 | chest_buttons::patch_chest_buttons | 1 | 5 / 178 | ui.comp.gameUIComp.$GameInventory.__constructor__ (ui/comp/gameUIComp/GameInventory.hx:35) |
| 38 | camp_chest::patch_camp_chest | 1 | 1 / 23 | ui.comp.gameUIComp.GameInventory.update (ui/comp/gameUIComp/GameInventory.hx:57) |
| 39 | party_inventory::patch_party_inventory | 6 | 1 / 158 | lib.$Fmt.formatItemCost (lib/Fmt.hx:227); ui.win.$Dialog.getFormattedItem (ui/win/Dialog.hx:1826); ui.win.$Dialog.getActionCost (ui/win/Dialog.hx:1862); .String (ui/win/Dialog.hx:1520); ui.win.Dialog.displayChoices (ui/win/Dialog.hx:792); st.player._PlayerInventory.$PlayerInventory_Impl_.useList (st/player/PlayerInventory.hx:62) |
| 40 | party_inventory::patch_party_counts | 2 | 0 / 0 | st.player._PlayerInventory.$PlayerInventory_Impl_.countWithChest (st/player/PlayerInventory.hx:42); st.player._PlayerInventory.$PlayerInventory_Impl_.hasItemWithChest (st/player/PlayerInventory.hx:216) |
| 41 | party_inventory::patch_party_lists | 3 | 0 / 0 | battle.ui.comp.StatusIcon.getTipContent (battle/ui/comp/StatusIcon.hx:25); ui.comp.Button.syncText (ui/comp/Button.hx:324); ui.win.InjuryHealPanel.init (ui/win/InjuryHealPanel.hx:126) |
| 42 | party_inventory::patch_party_recipes | 1 | 0 / 0 | ui.win.ItemsRecipe.hasItem (ui/win/Grimoire.hx:41) |
| 43 | party_inventory::patch_party_activities | 8 | 3 / 44 | world.CrimeCaveContent.update (world/CrimeCaveView.hx:60); ui.win.FishingAction.setFishhook (ui/win/FishingAction.hx:165); ui.win.FishingAction.startState (ui/win/FishingAction.hx:361); ui.win.FishingAction.canFishing (ui/win/FishingAction.hx:798); ui.win.FishingAction.updateCounters (ui/win/FishingAction.hx:807); ui.win.LockPick.updateLockPickCount (ui/win/LockPick.hx:198); ui.win.LockPick.startState (ui/win/LockPick.hx:424); .String (ent/p/Chest.hx:50) |
| 44 | loot_all::patch_loot_all | 1 | 0 / 0 | ui.win.Debrief.init (ui/win/Debrief.hx:543) |
| 45 | debrief::patch_debrief | 1 | 0 / 0 | ui.win.Debrief.update (ui/win/Debrief.hx:618) |
| 46 | window_close::patch_window_close | 1 | 0 / 0 | .String (ui/Window.hx:295) |
| 47 | stale_lobby::patch_stale_lobby | 1 | 0 / 0 | .String (Lobby.hx:759) |
| 48 | log_error_cast::patch_log_error_cast | 1 | 0 / 0 | .String (Main.hx:1256) |
| 49 | direct_lobby::patch_direct_lobby | 1 | 0 / 0 | Lobby.isSteamOnly (Lobby.hx:579) |
| 50 | start_transport::patch_start_transport | 1 | 0 / 0 | .String (Lobby.hx:607) |
| 51 | reload_route::patch_reload_route | 1 | 0 / 0 | st.Controller.reload__impl (st/Controller.hx:20) |
| 52 | window_drag::patch_window_drag | 3 | 24 / 922 | Game.dispose (Game.hx:1176); ui.Window.init (ui/Window.hx:29); ui.comp.gameUIComp.$GameInventory.__constructor__ (ui/comp/gameUIComp/GameInventory.hx:35) |
| 53 | take_all::patch_take_all | 1 | 1 / 34 | ui.win.ElementInventory.rebuild (ui/win/Dialog.hx:2937) |
| 54 | tavern_resume::patch_tavern_resume | 1 | 0 / 0 | st.player.Tavern.doService (st/player/Tavern.hx:697) |
| 55 | mod_version::patch_mod_version | 1 | 0 / 0 | $Lobby.checkJoinLobby (Lobby.hx:868) |
| 56 | alive_bind::patch_alive_bind | 1 | 0 / 0 | Game.initAlive (Game.hx:1657) |
| 57 | net_guard::patch_net_guard | 157 | 0 / 0 | Game.start (Game.hx:662); st.State.networkRPC (st/State.hx:131); LobbyState.networkRPC (Lobby.hx:59); battle.BattleMode.networkRPC (hxbit/Macros.hx:2768); ent.Entity.networkRPC (hxbit/Macros.hx:2768); battle.GameplayObject.networkRPC (hxbit/Macros.hx:2768); battle.Unit.networkRPC (hxbit/Macros.hx:2768); battle.Player.networkRPC (hxbit/Macros.hx:2768); +149 more |
| 58 | rpc_cancel::patch_rpc_cancel | 3 | 1 / 40 | Game.dispose (Game.hx:1176); hxbit.NetworkClient.processMessage (hxbit/NetworkHost.hx:78); st.Controller.battleExecuteSkill (hxbit/Macros.hx:2342) |
| 59 | lock_diag::patch_lock_diag | 4 | 1 / 32 | battle.Battle.set_lockCounter (battle/Battle.hx:170); battle.Battle.netExecuteSkill (battle/Battle.hx:2161); battle.Battle.endTurn (battle/Battle.hx:3510); st.Controller.battleUnitEndTurn__impl (st/Controller.hx:1183) |
| 60 | tip_overflow::patch_tip_overflow | 1 | 2 / 80 | ui.comp.TipContent.sync (ui/comp/TipContent.hx:37) |
| 61 | tooltip_input::patch | 1 | 1 / 61 | h2d.Scene.handleEvent (h2d/Scene.hx:533) |
| 62 | censer_tip::patch_censer_tip | 1 | 0 / 0 | ui.comp.$SkillTip.getHelpers (ui/comp/SkillIcon.hx:287) |
| 63 | ping_cell::patch_ping_cell | 2 | 3 / 193 | Game.ping (Game.hx:1947); st.Controller.ping__impl (st/Controller.hx:1284) |
| 64 | forge_mirror::patch_forge_mirror | 5 | 5 / 424 | Game.dispose (Game.hx:1176); ui.win.ForgeAction.init (ui/win/ForgeAction.hx:160); ui.win.ForgeAction.setActionDone (ui/win/ForgeAction.hx:479); ui.win.ForgeAction.endActivity (ui/win/ForgeAction.hx:570); st.Controller.ping__impl (st/Controller.hx:1284) |
| 65 | work_mirror::patch_work_mirror | 8 | 6 / 265 | Game.dispose (Game.hx:1176); ui.win.Archery.init (ui/win/Archery.hx:88); ui.win.Archery.setWorldPosOnShoot (ui/win/Archery.hx:364); ui.win.Activity._cancel__impl (ui/win/Activity.hx:466); ui.win.SingAction.startSong (ui/win/SingAction.hx:765); ui.win.UnitAction.init (ui/win/UnitAction.hx:23); ui.win.UnitAction.click (ui/win/UnitAction.hx:81); st.Controller.ping__impl (st/Controller.hx:1284) |
| 66 | coop_spectate::patch_coop_spectate | 6 | 8 / 136 | .String (ui/win/Activity.hx:408); .String (ui/win/Activity.hx:376); ui.win.Activity._start (ui/win/Activity.hx:329); ui.win.Activity.addHelpIcon__impl (ui/win/Activity.hx:440); ui.win.Activity._cancel__impl (ui/win/Activity.hx:466); .String (ui/win/Activity.hx:732) |
| 67 | timeline_hud::patch_timeline_hud | 2 | 4 / 521 | battle.ui.win.$TimelineEvent.__constructor__ (battle/ui/win/Timeline.hx:54); battle.ui.win.TimelineEvent.sync (battle/ui/win/Timeline.hx:289) |
| 68 | activity_injury::patch_activity_injury | 2 | 2 / 276 | ui.win.FishingAction.startState (ui/win/FishingAction.hx:361); ui.win.Activity.addInjury (ui/win/Activity.hx:751) |
| 69 | style_guard::patch_style_guard | 2 | 0 / 0 | domkit.Properties.applyStyle (domkit/Properties.hx:173); st.Controller.gameOver__impl (st/Controller.hx:61) |
| 70 | all_inv::patch_all_inv | 13 | 20 / 763 | .String (st/Inventory.hx:651); st.Inventory.networkAllow (st/Inventory.hx:1189); .String (ui/comp/Slot.hx:403); ui.comp.Slot.netPick (ui/comp/Slot.hx:500); ui.comp.Slot.drop (ui/comp/Slot.hx:571); ui.comp.ItemSlot.doPick (ui/comp/ItemSlot.hx:105); ui.comp.ItemSlot.allowPick (ui/comp/ItemSlot.hx:113); ui.comp.ItemSlot.getTipContent (ui/comp/ItemSlot.hx:211); +5 more |
| 71 | alt_world::patch_alt_world | 3 | 3 / 60 | ent.p.Element.noOverNoOutline (ent/p/Element.hx:652); ent.p.Element.onUpdateCulling (ent/p/Element.hx:1229); world.World.update (world/World.hx:1371) |
| 72 | battle_camera::patch_battle_camera | 3 | 0 / 0 | battle.Battle.initUI (battle/Battle.hx:912); battle.Battle.onOverlayEvent (battle/Battle.hx:1371); battle.Battle.updateCamera (battle/Battle.hx:3261) |
| 73 | title_version::patch_title_version | 1 | 1 / 6 | ui.win.TitlescreenWindow.init (ui/win/TitleScreen.hx:84) |
| 74 | loading_draw::patch_loading_draw | 1 | 0 / 0 | Game.render (Game.hx:273) |
| 75 | colorblind_load::patch_colorblind_load | 1 | 0 / 0 | Game.render (Game.hx:273) |
| 76 | scroll_hit::patch_scroll_hit | 2 | 1 / 48 | h2d.Scene.getInteractive (h2d/Scene.hx:480); h2d.Scene.handleEvent (h2d/Scene.hx:533) |
| 77 | save_kind::patch_save_kind | 2 | 1 / 36 | ui.win.$LoadGame.getSaveName (ui/win/LoadGame.hx:221); ui.win.LoadGame.getFileName (ui/win/LoadGame.hx:229) |
| 78 | save_order::patch_save_order | 1 | 0 / 0 | ui.win.$LoadGame.loadGames (ui/win/LoadGame.hx:99) |
| 79 | patch_queue::patch_patch_queue | 1 | 0 / 0 | Game.start (Game.hx:662) |
| 80 | jit_names::patch_jit_names | 2 | 0 / 0 | .String (Game.hx:367); .String (ui/win/Activity.hx:408) |

## Codex verdict

`codex exec --sandbox read-only` (gpt-6.1-sol, reasoning medium; the first
run at high died on "model at capacity"), static read of aea5192, nothing run.
Prompt: the findings above with the mechanical evidence.

| F | Verdict | Codex note |
|---|---|---|
| F1 | PARTIAL | replay re-enters the shared handler (barrier.rs:1489, ready_start.rs:1407); drop_in edits another callback (drop_in.rs:26); one admission owner sensible, conflict of every hook not proven |
| F2 | PARTIAL | the timer clears a wider class of locks (force_leave.rs:2864, 2939); the inspect fix helps but does not by itself justify dropping the camp-tool compensation (force_leave.rs:136) |
| F3 | AGREE | forced first click + time compensation (coop_gates.rs:13, 555); after 2 / 5 s a repeat is possible again (:31) |
| F4 | AGREE | both shipped (direct_lobby.rs:127, start_transport.rs:206, lib.rs:212); intentional two-step routing, still one policy in two places |
| F5 | PARTIAL | M7 already fixed (coop_spectate.rs:1316, 1323); Sing camera and animation mirror do different jobs (:89, work_mirror.rs:782) |
| F6 | AGREE | the Nop bypasses the rewritten equip check in all nine closures (loot_level.rs:197, 208, 120); deleting the dead edit justified; merging the genLoot passes is a separate refactor |
| F7 | PARTIAL | three op-0 inserts confirmed, order deterministic spectEnd -> workEnd -> trace (coop_spectate.rs:1288, work_mirror.rs:834, activity_diag.rs:489); no conflict proven |
| F8 | PARTIAL | shipped (lib.rs:177, 192, 193, 222), but diag and lock_diag were kept on purpose (prior L14); only activity_diag / camp_choice are stale |
| F9 | PARTIAL | duplicates exist (all_inv.rs:1506, window_drag.rs:912, alt_world.rs:92, barrier.rs:193); a shared reporter does not fix the swallow policy (net_guard.rs:1030); timeline uses a time limit (timeline_hud.rs:769) |
| F10 | PARTIAL | duplicate emitted Idle/IdleOf confirmed (forge_mirror.rs:1486, work_mirror.rs:719); maps split per mirror on purpose (mirror.rs:22): share the emitter, merging state needs its own reason |
| F11 | PARTIAL | same fn, different fixes (missing draw vs leaked target); composition test exists (colorblind_load.rs:271); no counter-patch |
| F12 | PARTIAL | re-entry exists, ctrl does not intercept that take: source checked foreign (all_inv.rs:2937), foreign branch returns false before the ctrl test (:2383, 2393, 2395) |
| F13 | PARTIAL | harness after jit_names and afterEnter compensation confirmed (lib.rs:243, 248, battle_rejoin.rs:450); no concrete unsafe harness call site (UID injection checks for a preceding NullCheck, harness.rs:3344) |

Codex additions (not verified here):

- `lock_diag.rs:397`: a `CallStack` is captured on every lockCounter 0 -> positive
  transition even after REPORT_CAP is spent (cap checked only in the reporter,
  :253): permanent diagnostic cost in battle, unmeasured. Fix: test the cap first.
- `battle_rejoin.rs:394`: `targetClient` is restored after `flush_send`; an
  exception out of the flush can skip the restore, also on the catch path
  (:418): routing state can leak. Static risk, no repro.

Net: AGREE 3 (F3, F4, F6), PARTIAL 10, DISAGREE 0. Corrections applied above:
F5 (M7 done), F12 (no ctrl interception), F8 (only activity_diag and camp_choice
are stale).
