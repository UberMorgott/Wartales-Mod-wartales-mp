# Patch layering audit (2026-10-26)

Read-only audit of `patcher/src/*.rs` (77 passes in `lib.rs::patch_image`) and
`shim/proxy/hlpatch.h` (7 one-byte patches) for "patch then re-patch" layering:
one edit disables / changes game behaviour and another edit (or later code)
undoes, re-enables or compensates for it. Owner rule (memory "No counter-patches"):
fix or revert the causing patch, never stack an undoing patch on top.

Method: module header comments (each pass documents its edits), cross-module
grep of shared target functions, `git log` per module. Second opinion: Codex
(`codex exec --sandbox read-only`), verdicts in the last section.

Severity: **High** = an own patch undone / compensated by another own patch on
the same behaviour; **Medium** = same behaviour handled in several places with
overlapping policy, or a patch that restores what vanilla just did; **Low** =
hook stacking, guards for own infrastructure, dead diagnostics.

No finding below was changed. coop_spectate.rs is being edited in parallel
(minigame-spectator work); findings 7 and 8 must be reconciled with that work.

---

## H1. Debrief.update: three passes on one rebuild check

- Files: `debrief_cure.rs::patch_debrief_cure`, `debrief_enable.rs::patch_debrief_enable`,
  `debrief_diag.rs::patch`; cause partly `party_inventory.rs::patch_party_counts`
  (Codex correction) and `patch_party_lists` (Debrief cost buttons).
- Game fn: `ui.win.Debrief.update` (Debrief.hx:618-638).
- Layer A (`party_inventory`, 66dbcfe/df45f60 2026-10-01): with-chest counts
  (Button.syncText, Debrief heal/repair) read every player's inventory, so the
  inputs of update's enable check change live under the open window.
- Layer B (`debrief_cure`, 40bcce6 2026-10-02): update's cure loop
  getUnitsToRepair -> getUnitsToHeal so update agrees with init (vanilla mismatch).
- Layer C (`debrief_diag`, e420c80/4685470/5efa517 2026-10-04/05): print-only
  traces for "the remaining hover/click failure"; inserts a trace call between
  the comparison and `rebuild`.
- Layer D (`debrief_enable`, d69bbc0 2026-10-05): both `Call1 rebuild` become
  `Call2 set_enable(btn, desired)`; its matcher searches back 2 ops for the JEq
  because debrief_diag may sit in between (guard for our own patch).
- Consolidation: one `debrief.rs` pass on Debrief.update: heal-list retarget +
  set_enable instead of rebuild. Delete debrief_diag (the failure it chased is the
  rebuild churn D removed). With set_enable the update formula must stay right,
  so B is still needed, but as part of the same pass.
- Risk: low (both edits are one-op swaps, already tested separately).
- Effort: ~0.5 day (merge two plans, drop diag + its ordering guard, tests).

## H2. Steam path disabled, then re-enabled: direct_lobby + start_transport

- Files: `direct_lobby.rs::patch_direct_lobby`, `start_transport.rs::patch_start_transport`.
- Game fns: `Lobby.isSteamOnly`, `Lobby.setupPlatform` instance/get callback (Lobby.hx:607-619).
- Layer A (0844bf2 2026-10-08): isSteamOnly returns false for a lobby id ending
  in 'D'; the master marks every lobby 'D', so the vanilla Steam branch of
  setupPlatform never runs.
- Layer B (c76753a 2026-10-15): the instance/get answer callback gets a Steam
  case `p(r.serverID, null)`, "what setupPlatform's own isSteamOnly branch does":
  the vanilla Steam path, re-added behind the master's answer; plus an Unknown
  case showing the master's reason.
- Consolidation: one edit in `Lobby.setupPlatform`: always ask instance/get when
  our master is in use, and take the platform from the answer (Steam / relay /
  reason). isSteamOnly untouched, direct_lobby deleted, the 'D' id mark becomes
  unnecessary on the game side (master-side cleanup separate).
- Risk: medium (join paths: Steam invite + code join must stay independent;
  harness cannot test Steam invite).
- Effort: ~1 day incl. master change and both join paths retest.

## H3. net_guard swallows, then un-swallows; root-cause fixes bundled in

- File: `net_guard.rs::patch_net_guard`.
- Game fns: every generated `networkRPC` `__impl` call, `processMessage`
  `host.onMessage`, hxbit.NetworkHost writers, `Game.start`, `Game.initAlive`.
- Layer A (4ffe0bc 2026-10-01): traps I/M swallow any handler exception.
- Layer B (a57102f 2026-10-10): the writer-depth counter W also counts
  `Game.start`, so an exception out of it is rethrown again (the swallow left a
  half-built world: `Null access .getRegion` every frame).
- Layer C (d3dfba6 2026-10-05, e1963c1 2026-10-18): parts A/B
  (initAlive before handlers, bind pending battle.Entity.battle) are root-cause
  fixes for exceptions the swallow was hiding (issue #8, rat matriarch).
- style_guard.rs (cb6c966) notes the game-over exception was swallowed by
  net_guard on the guest while it bricked the host: the swallow masks bugs.
- Consolidation: move the initAlive / entity-bind fixes into their own pass
  (they stand alone); narrow the swallow to an explicit allow-list or log-and-
  rethrow, or drop it once the root causes are fixed, which removes the
  Game.start carve-out and most of W/G bookkeeping.
- Risk: high (session-drop behaviour on unstable RU networks; a narrower trap
  can turn a logged glitch back into a disconnect).
- Effort: 1-2 days (split is mechanical; deciding the swallow scope needs logs).

## H4. Npc.inspect lock special-cased in four passes

- Files: `npc_talk.rs` (Npc.talk `anyPlayerLocked` -> `npcTalkLocked`),
  `dialog_recruit.rs` (displayChoices `getPlayerLockedWith` -> `dlgLockedWith`;
  recruitUnit -> `dlgRecruit` which closes the inspect UnitInfo),
  `force_leave.rs` (F2 guard 3: closes the NPC-inspect UnitInfo; flRelease clears
  `me.lockedWith` after RELEASE_AFTER s), and in the opposite direction
  `camp_talk.rs` (adds `anyPlayerLocked(null)` to `playCampConfession__impl` /
  `playPersonalQuestReminder__impl`).
- Root: `Npc.inspect` sets `me.lockedWith = npc` until its UnitInfo closes, and
  every party gate (`Game.anyPlayerLocked` / `getPlayerLocked` /
  `getPlayerLockedWith`) treat that read-only sheet like a busy action.
- Layers: 902819a (10-02) npc_talk, b7dd29f/1e4f7e7 (10-01) force_leave,
  ab1f33d (10-07) dialog_recruit + edits in npc_talk/force_leave, df940e6 (10-02)
  camp_talk. Each new fix re-derived "is this lock an inspect?" at its own call site.
- Consolidation: one change at the source: in multi, Npc.inspect does not take a
  party-blocking lock (or `getPlayerLocked` skips a lock whose only holder is an
  inspect UnitInfo). Then npc_talk's wrapper, dialog_recruit edit 1 and
  force_leave guard 3 become unnecessary; dialog_recruit edit 2 (close own sheet
  before recruit) may stay as UI cleanup.
- Risk: medium (vanilla uses the lock to keep two players off one NPC; keep the
  same-NPC rule).
- Effort: ~1-1.5 days.

## M5. Window-lock removed twice for the camp leave

- Files: `coop_gates.rs` G6 (`hasWindowOpened` read in the getPlayerLocked filter
  and Player.canCamp becomes false), `force_leave.rs` part C `gtc_hooks`
  (`go` flag skips GameUI.toggleCamp's canToggleButton / canLeaveCamp window tests).
- Game fns: `Game.getPlayerLocked` filter, `Player.canCamp`, `GameUI.toggleCamp`.
- Layer A 5543625 (09-30), layer B 3f4f8d8 (10-02).
- Consolidation: one policy "windows never block a party action": either G6
  also covers GameUI.toggleCamp's window tests, or C owns the camp path and G6
  shrinks. Drop the `go` global if G6 covers it.
- Risk: low-medium (host-side windows only). Effort: ~0.5 day.

## M6. coop_gates G1: forced click, then a repeat guard for the forcing

- File: `coop_gates.rs` G1 (Button.waitAllPlayers, onPush, doPadClick).
- Layer A (5543625 09-30): the `forced` branch is always taken: first click acts.
- Layer B (7f651dd 10-01, c93dac9 10-07, eaec4ed 10-08): per-button 2 s / 5 s
  repeat guard in two global ObjectMaps + newest-time global + Game.dispose
  cleanup + drop while the barrier runs, because the forced path ran an action
  twice (two startAttack, two leave requests). force_leave.rs relies on it.
- Consolidation: make the forced click one-shot on the vote itself: on the host,
  run cb() once and mark `bd.forced`/clear `players` so later votes for the same
  ButtonData are no-ops until the window re-arms it, instead of a time window.
  Removes both maps and the timing constants.
- Risk: medium (buttons re-used while the window stays open must re-arm).
- Effort: ~1 day.

## M7. coop_spectate: let vanilla lock, then restore

- File: `coop_spectate.rs` (spectPre / spectPost, spectWin, spectTut). Being
  edited in parallel.
- Game fn: `ui.win.Activity._start` and its onReady / window closures.
- Layer A: vanilla `_start` sets `mode.lockCamera`, `padCursor.locked`, hides the
  inventory, adds the chooseUnit full-screen Interactive.
- Layer B (d75dda5 10-05, 76bafd7 10-09): spectPost restores lockCamera /
  padCursor from a bit-packed `m`, `showInventory(prevInventory)`, removes the
  Interactive; spectWin builds then hides the host window.
- Consolidation: skip the vanilla sets on a spectator (gate each with
  `spect(act)`) instead of restoring afterwards; no `m` packing.
- Risk: medium (CAMERA_ACTIVITIES exception must keep vanilla). Effort: ~1 day,
  do it inside the running spectator work.

## M8. Sing handled by two mechanisms

- Files: `coop_spectate.rs` (Sing in CAMERA_ACTIVITIES; client spectator gets no
  window via spectTut), `work_mirror.rs` kind 2 (a4c9c6a 10-09: SingAction.startSong
  sends start/hit over the ping mirror, under a trap).
- Sing is a replicated (coop) activity, yet its show on spectators comes from the
  non-coop mirror because the replicated window throws on clients.
- Consolidation: pick one path for Sing (mirror only: drop Sing from the spectate
  camera list, or spectate only: play the vanilla startSing/stopSing on the
  host-owned window) so a coop activity is not both suppressed and re-mirrored.
- Risk: low-medium. Effort: ~0.5 day.

## M9. Host Join / ReadyToStart handler hooked by five passes, with undo logic

- Files: `barrier.rs` (joinGate at Join entry, replay), `ready_start.rs`
  (readyReset/readyTick/readyMark/readyJoined/readyHold/readySendTo/readyRemoved),
  `returning_units.rs` R2 (after `p.initContent()`), `battle_rejoin.rs` (late
  joiner branch), `drop_in.rs` E2 (getServerID admits new players).
- Game fns: Game.startServer onMessage closure, Game.update, gameplayStart.
- Layering: ready_start's header says the wait-forever is common "with three or
  more players and the mod's barrier"; readyHold undoes a counted ReadyToStart
  (`playersReady--`) and replays it later because joinGate (barrier) parks Joins
  and drop_in lets unknown users in (44158a1 10-07); readyRemoved puts back
  `playersRemoved` that a host reload drops (8ceadb0 10-08).
- Consolidation: one host "session admission" pass owning the handler: a single
  table of clients (joined / synced / ready / parked) replacing barrier's parked
  list, ready_start's four lists and the replay of both. Barrier timeouts and
  ready start read the same table.
- Risk: high (central co-op start/reconnect path). Effort: 3-4 days + harness runs.

## M10. battle_rejoin compensates its own replay order

- File: `battle_rejoin.rs` (late-joiner replay; Battle.afterEnter edit).
- Layer A (e6b9fe5 10-08): replay netInitGrid / clientBeginBattle / initClients /
  netAfterGen to the late joiner.
- Layer B (71c58e7 10-08): `afterEnter` re-sets `lockAlives` after the replay, so
  afterEnter gets `if (!isAuth && world == null)` around it.
- Consolidation: send the replay after the late joiner's afterEnter (e.g. trigger
  from the joiner's first battle frame / a client ack) so vanilla order holds and
  afterEnter stays vanilla. Or accept B as the minimal fix (it changes nothing in
  a vanilla start).
- Risk: medium. Effort: ~1 day. Optional.

## M11. all_inv: vanilla pick/drop refused, then re-implemented

- File: `all_inv.rs` (U: ItemSlot.allowPick / allowDrop / doPick / drop refused on
  foreign slots; D: Slot.netPick + slot click hooks with a stand-in hand).
- Layer A 2fd1dae (10-05), layer B aa573b6 (10-07).
- Consolidation: one interception point (the slot click / netPick) that turns a
  foreign drag into the owner-routed MoveTo; drop the four op-0 refusals if D
  already never lets a foreign hand reach them.
- Risk: medium (item loss/dup paths). Effort: ~1 day; verify with the harness.

## L12. Scene hit test: two candidate filters

- Files: `scroll_hit.rs` (handleEvent + getInteractive: skip a candidate outside a
  Scroll/Hidden flow's clip), `tooltip_input.rs` (handleEvent: skip passive tip
  descendants).
- Consolidation: one appended `mpHitOk(cand, x, y)` used at the visible test of
  both hit loops.
- Risk: low. Effort: ~0.5 day.

## L13. jit_names: guard for our own appended functions

- File: `jit_names.rs::patch_jit_names` (Nop after NullCheck before a call to a
  nameless appended function).
- Root: appended functions have no `obj` / `field.ref`, so the JIT's null-access
  message lookup hashes NULL.
- Consolidation: give appended functions a name at creation (`push_fn` registers
  them as a method / field of one holder class), remove jit_names.
- Risk: medium (module-init ordering). Effort: ~1 day. Low priority.

## L14. Dead / superseded diagnostics

- `debrief_diag.rs` (see H1), `activity_diag.rs` (82bf914 10-02: the freeze it
  traced was fixed by activity_injury 19f36e4 and coop_spectate), `camp_choice.rs`
  (e420c80 10-04, print-only reward trace). Each still costs a pass, a skip-on-
  mismatch risk (a skipped diag refuses the whole image only if it calls
  `skipped`) and ordering guards (H1).
- Keep: `diag.rs` (extended today, e2e82fb), `lock_diag.rs` (open item: battle-lock
  logging).
- Consolidation: delete the three, or fold any still-wanted line into diag.rs.
- Risk: none for gameplay. Effort: ~1 h.

## L15. camp_chest: late chest add plus a grid fix for it

- File: `camp_chest.rs` (mpCampChest in GameInventory.update; then
  `tryInitChestInventory()` at the head of the chestTool branch, 5f4dfd2 10-09).
- The grid fix covers both our late add and the vanilla replicated-later case,
  so it is not a pure counter-patch. Kept as one pass already; no action unless
  the chest add moves to Camp creation (then the grid edit may still be needed
  for replication).
- Risk/effort: n/a.

## Checked, not layering

- camp_any_unit (isControllable -> isPlayer at 4 camp sites) vs battle_takeover
  (isControllable prologue for absent players in battle): disjoint.
- hold_speed vs coop_gates G1: G1 removes only the wait-all hold bar; hold_speed
  scales the other rings.
- ping_cell / mirror / forge_mirror / work_mirror on Controller.ping__impl: one
  shared receiver hook in mirror.rs; ping_cell replaces the vanilla fx by design.
- window_drag: two same-day reverts (7a2ae8a, 8e77759) left no residue.
- skill_sync / skill_vars: two distinct client root causes for issue #2.
- shim hlpatch.h (join-code length, allPlayersAssigned, flush timeouts) vs
  customize_slots (allUnitsAssigned): different functions.
- style_guard: symptom guard for a domkit loop, not undoing our patch (cause not
  found; window_drag's onAfterReflow is a suspect worth one check).

## Codex verdict

`codex exec --sandbox read-only` on the candidate list (2026-10-26; read current
sources, patch_image order, cited commit diffs; nothing run).

| # | Verdict | Codex note |
|---|---------|------------|
| H1 | PARTIAL | merge + drop diag sound; cure fixes a vanilla list bug, enable keeps it right without rebuild; the live counts come from `patch_party_counts`, not `patch_party_lists` |
| H2 | PARTIAL | reads it as moving the transport choice to the master, not disable/re-enable; one policy is fine but `setupPlatform` still needs the callback branches |
| H3 | PARTIAL | Game.start carve-out does compensate a too-wide swallow; split A/B + narrow trap right; that the rest of the swallow is unneeded is not provable statically |
| H4 | PARTIAL | fix the inspect lock at the source; a general NPC filter is too wide; keep dialog_recruit's UnitInfo close and camp_talk's mode-switch gate |
| M5 | DISAGREE | G6 drops the replicated `hasWindowOpened`; gtc_hooks bypasses other local tests during an agreed leave: sequential vanilla gates, merging saves no hook |
| M6 | AGREE | immediate run + repeat_guard compensation; doPadClick redirect after disabling onPush is another one; a permanent once-per-button block would break reused buttons, use explicit completion/reset |
| M7 | PARTIAL | gating the writes beats restoring; chooseUnit Interactive exists before `_start`; the hidden host window must stay (authoritative logic); only the client window may be suppressed |
| M8 | PARTIAL | the song mirror also covers the vanilla ownership-registration failure; unify the spectator/mirror decision but do not just drop the mirror |
| M9 | PARTIAL | readyHold undo/replay confirmed, one admission/readiness owner simpler; readyRemoved restores a vanilla reload loss on its own; four passes hook the handler (drop_in edits gameplayStart/getServerID) |
| M10 | AGREE | 71c58e7 diff confirms the causality; a fewer-hook alternative not proven |
| M11 | PARTIAL | the refusals keep unsafe vanilla hand transfer out; the stand-in calls the safe owner-routed MoveTo; only the local drag adapter can be simplified |
| L12 | DISAGREE | geometric clip in two hit tests vs event-dispatch filter with sticky/gamepad exceptions: different contracts, no causal compensation |
| L13 | PARTIAL | guard confirmed, but a name alone does not help: hlbc `Function::write` does not serialize it and `field.ref` is built at runtime; simpler: emit safe op neighbourhoods in the calling patch |
| L14 | PARTIAL | diagnostics, not counter-patches; make them opt-in / drop from release; no proof every traced failure is gone |
| L15 | PARTIAL | grid init also needed for the independent late replication; current idempotent pass already simple |

Codex additions:

- **AGREE** `barrier.rs::add_tick` removes waitLocks entries whose Join its own
  `joinGate` parked: compensation for our own gate. Simpler: put only admitted /
  synced clients into the barrier to begin with. Folded into M9.
- **PARTIAL** `battle_camera.rs::apply_pan`: Rotate jumps into the pan block, then
  two extra `dragMode == Rotate` tests exclude the edge-scroll and the
  CameraSpeedInstant store that jump dragged in. Simpler: jump straight to the
  keyboard-pan part. (Low.)
- **PARTIAL** `timeline_hud.rs::add_hud`: re-sets the portrait after the style
  system resets it (two writers of one bitmap); fix at the Icon / style source.
  (Low.) The Player-mask on/off pair is not a separate counter-patch.

Net after Codex: AGREE/PARTIAL on 13 of 15 items; M5 and L12 disputed (keep as
"no action" unless a cheaper merge shows up); two new Low items (battle_camera,
timeline_hud) and one addition to M9 (barrier waitLocks).
