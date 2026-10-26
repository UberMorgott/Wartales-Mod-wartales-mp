# Patch consolidation: in-game verification (branch refactor/patch-consolidation)

Scenarios for the changes of `refactor/patch-consolidation` (worktree
`E:\DEV\Wartales\mp-consolidate`, from main e2e82fb), per the layering audit
`docs/patch-layering-audit.md`. Not run yet: the harness rig `D:\WartalesTest`
was busy. Unit tests (`cargo test --release`, real `hlboot.dat`) and the shim
build passed for every commit.

Logs: `%LOCALAPPDATA%\wartales-mp\shim.log` on each machine (harness: the run
folder's `A`/`B` copies). `tips:` lines are the patcher at start-up, `game:`
lines the game's stdout.

## Every run: start-up

- [ ] Both machines: no `skipped` line from the patcher and the game starts
      patched (the image is refused as a whole on any skip).
- [ ] `tips: patched debrief fn@28424: co-op cure check counts the units to heal (op 73), repair/cure availability sets the button, no rebuild (ops [62, 151])`
- [ ] `tips: patched alive bind fn@3288 op 10: initAlive binds every pending battle entity first (18 ops)`
- [ ] `tips: patched network guard: 701 RPC handler calls and 4 message handler calls trapped, writer depth in 16 functions`
- [ ] No `tips: patched debrief cure ...`, `patched debrief enable ...` or
      `[mp debrief]` lines any more (passes merged / diagnostics removed).

## 1. Debrief (H1: `debrief.rs`, replaces debrief_cure + debrief_enable + debrief_diag)

Setup: co-op (host + guest), win a battle in which a player unit ends with
damaged armour and one curable injury; keep at least one Remedy and some Raw
Materials in a player inventory or the camp chest.

1. [ ] Debrief opens on both. Hover loot items and the Take all button for
       10 s: the cursor/hover state stays put (no flicker, no repeated item
       hover sound). Take one item by click, then Take all: both work, on host
       and guest.
2. [ ] Guest moves all Remedies out of reach (sell/drop or into a slot the
       party counts do not see), or another player takes the last Remedy from
       the loot: the Cure all button turns disabled within a frame, the loot
       grid does not flash/rebuild, an open hover tooltip stays.
3. [ ] Give a Remedy back: Cure all enables again in place.
4. [ ] Same with Raw Materials for Repair all.
5. [ ] A unit with an injury but intact armour and a Remedy in the party: Cure
       all is enabled and stays enabled (vanilla rebuilt the window every
       frame in this case). Click it: the unit is cured, the window refreshes
       once (cureAll's own netRebuild), costs/labels correct.
- Pass: points 1-5 on both machines; no `game:` exception lines; no
  `[mp debrief]` lines.

## 2. Battle entity batch (H3 split: `alive_bind.rs`, was net_guard part B)

Behaviour is unchanged by construction (patched image byte-identical before
and after the split, SHA-256 8CF50EB9...184F with the debrief change), so this
is a regression check of issue #8.

1. [ ] Co-op battle against a Rat Matriarch (ThroatyHowl summons several rats
       in one host flush), guest connected. After the summon the guest sees
       all rats with health bars, the battle continues to the next turn on
       both machines.
- Pass: the guest's log has no `Null access .grid` and no
  `mp: net: RPC handler ... threw, ignored:` line for feedbackDamages /
  battleNextTurn; the battle ends normally.

## Not changed (no in-game check needed)

Kept as they are, each with Codex agreeing: H2 (the transport choice lives in
the master; option A breaks the synchronous Steam path, B moves the code but
leaves the layering), H3 narrowing / Game.start carve-out (not provable
statically), H4, M5, M6, M7/M8 (coop_spectate owner), M9 + barrier waitLocks,
M10, M11, L12, L13, L15, battle_camera, timeline_hud, L14 activity_diag /
camp_choice (harness R2 S7 uses camp_choice lines).
