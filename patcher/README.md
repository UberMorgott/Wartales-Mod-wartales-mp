# wartales-tips (patcher/)

The Rust bytecode patcher of wartales-mp. It lives in `patcher/` of this
repository (crate and library name stay `wartales-tips` / `wartales_tips`);
`shim\build.ps1` builds it and links it into `winmm.dll`.

Tooltips on the Wartales new-game **start choice** screen. Hover an item in the
starting-troop preview to see the game's own item tooltip; hover a unit line
("class + trait") to see that class's base skills as the game's skill tooltips.

Nothing new is drawn: the patch rewires the existing `ui.comp.Element` hover
tooltip system onto the preview's `ItemIcon` and `TextFixed` children and adds two
small functions (`ItemTip(icon.item)`, a vertical `Flow` of `SkillTip` per
`unitClass.baseSkills` entry) to the game's HashLink bytecode (`hlboot.dat`).

It also carries one gameplay patch, **enemy friendly fire**: an area attack
cast by a non-player unit also hits that unit's own allies (never the caster),
the way player area attacks already work. Only the area loop of
`Skill.gatherTargets` changes (`src/friendly_fire.rs`): for a skill whose
`allowedTargets` is Enemies it passes `checkValid = false` to
`SkillEval.addTarget` for every targetable unit other than the caster, so
primary-target choice and AI target selection still use the original
`Unit.isValidTarget`. Always on, no toggle.

A co-op fix, **Career Plan extra point** (`src/career_plan.rs`): the client's
`UnitInfo.onBonusAttributeApply` sent `count = offer(attr) + n`, using its own,
possibly stale, level-up offer (a stale offer gives `n` alone, so "+2" became
"+1"), and the host closure applied it verbatim. The client now sends `-1 - n`;
the host closure first requires `aptitudePoints > 0` (else nothing is granted
or spent) and, for a negative `count`, rebuilds it as the host's own
`getAttributeUpCounts(unit).get(attr) ?? 0` plus `n`, read before
`usedAptitudePoints++`, the way the regular level-up closure re-checks its
offer. `count >= 0` keeps the old behaviour. All players must run the same
build: an unpatched host would apply the negative count. If the ops do not
match, the fix is skipped (logged) and the other patches still apply.

**Customize slots** (`src/customize_slots.rs`): the new-game customize scene has
five unit spots (`Model01..05`, `Camera01..05`; the count is taken from the
`characterIndex` allocation in the `CustomizeScreen` constructor), and
`CustomizeScreen.initS3d` threw "Missing prefab Camera06" for a sixth unit.
With `excess = units.length - slots`, a unit is hidden when every spot is taken
or it is an animal and fewer than `excess` animals are hidden so far (humans
keep the spots; the troop's own animal goes before the auto-added Pony).
`initS3d` builds no Character for a hidden unit and numbers the shown ones
1..slots; `LobbyState.initAssignments` (host, new game only) creates a hidden
unit's lobby entry with `p = getUser().id`, so `allUnitsAssigned` and
`makeGroups` give it to the host. Same rule, same createTroop order on host
and clients; troops of `slots` units or fewer are unchanged. If the ops do not
match, the fix is skipped (logged) and the other patches still apply.

**Co-op: nobody waits** (`src/coop_gates.rs`): every co-op consensus in the
world is decided on the host and already has a vanilla force path (hold the
button until the bar fills). A plain click now takes it, so the first player
who clicks decides: wait-all-players buttons (town/location, camp fire, rest,
debriefs, group fight, travel post, trade route, tavern resume, ...) run on the
first click, cast no vote on mouse-down any more (a gamepad press, which has no
release, goes through `Button.doPadClick` straight to the click) and no longer
show the hold-to-force bar; a leave request (`Place.setLeaveState__impl`) and a
rest request (`Controller.playerSetRestState__impl`: dropped unless `game.mode`
is a `CampMode`, not forced while already resting) set their state's `forced`
flag; the first player's "next" advances a dialog (`Dialog.checkAllReady` reads the
host's `coopSkipDialogInstanlty` option as on). An open window no longer
locks the others: `Game.getPlayerLocked` and `Player.canCamp` ignore the synced
`hasWindowOpened` (one player reading a character sheet used to block camp,
leaving camp and leaving the tavern for everyone); a player busy with an
NPC/chest/craft (`lockedWith`) still blocks. Game modes are global, so a
host transition disposes every client's old mode with its windows. Not
touched: mode-switch load barriers, battle round sync, owner-only confirms,
world-map gathering. Decisions stay on the host; the input (push/pad) and
window-lock changes run on every machine, so all players need the same build.
Each gate is skipped (logged) on mismatch.

**Hold speed** (`src/hold_speed.rs`): `BaseUI.holdAction`, the one function
behind every press-and-hold ring, divides its duration by 3 at entry
(NPC/entity trigger 0.45 s -> 0.15 s, gamepad place exit and skill-bar arrow
1.95 s -> 0.65 s). The gamepad long-press binding duration is cdb data
(`Const` `Pad_LongPress_Duration`) and is not changed here.

**Profession experience** (`src/job_xp.rs`): vanilla already keeps each
profession's level across job switches (`st.Unit.jobsLevel`, a networked and
saved `Map<String, Int>`; `_removeTrait` writes `jobsLevel[tid] = t.level`,
`_addTrait` reads it back), but the progress inside the level, `UnitTrait.xp`,
was lost ("will lose all experience gained as ..."). `_removeTrait` now also
writes `jobsLevel[tid + "#xp"] = t.xp`, and `_addTrait` restores it into a trait
it just created (a `getTrait(tid, null)` lookup before its own `getTrait(tid,
&true)` found none, and `xp` is still 0) and resets the stored value to 0. No hxbit schema change:
the extra keys are plain map entries every reader ignores (they look up job ids),
so saves still load without the mod. Both functions run on the host only; the
xp reaches clients through the existing sync. The switch confirm still shows the
game's "will lose all experience" text. A switch made while playing without the
mod leaves the stored value untouched, so it can come back later as stale
progress. Skipped (logged) on mismatch.

**Diagnostics** (`src/diag.rs`): prints only, changes no behaviour. Every
`shiro.online.Log.logError` message (dropped fades, refused mode switches, a
client refusing a mode it is not in) is printed at entry, before the online
error cap, and the co-op mode-switch barrier prints its steps (`mp:
syncLeaveMode` / `syncEnterMode lockSync= waitLocks=`, `waitForClients
waitLocks= clients=`, `onClientReady` / `onServerReady left= callbacks=`,
`doLeaveMode lockAlives= fading= fadeParams= onBreak= pending=` (what
`waitAlive` waits on), `leave host faded`, `leave all clients ready`, `leave
client alive`, `leave client faded`); every read is null-guarded. The
shim copies the game's stdout (`hl_sys_print`) into `shim.log` as `game:`
lines, so a stuck black screen shows which step never came. Skipped (logged) on
mismatch.

**Co-op auto-follow** (`src/follow.rs`): in co-op an open window does not pause
the world map, so `World.update` gets a new `followUpdate` right before
`updateSprint()`. While the local player has a window open
(`GameUI.hasWindowOpened`: character sheet, inventory, ...) or has toggled
follow with **F** (no world-map binding uses it; a log toast says
"Follow: ON/OFF"), every 0.5 s their caravan sends the same RPC a ground click
sends, `Controller.playerGoto`, to a point 5 units short of the leader once the
leader is more than 9 away (the leader's own position when that point is not
walkable), then `resetSoftTarget(&true)` like a click without taking the move
priority. Sprint is mirrored with `Controller.playerSetShift` (a moving
leader's synced `flags` bit 8; a resting leader means walk); `updateSprint`
does not send its key-driven shift meanwhile. Leader: vanilla's synced
`GameState.playerMovePriority` (the player who last clicked), else a moving
player, else the host; a player in a menu or in vanilla soft regroup comes last.
A manual move (`Controller.playerGoto` not sent by follow, `playerGotoEntity`,
`BasePlayer.updateMoveHold`) turns F off and pauses following until it ends (at
least 1.5 s). No follow in battle, in a
city, on water, in a cutscene, or while the player is locked with an NPC/chest,
waits to enter/camp or has an interaction window. Host logic is untouched: only
vanilla client RPCs; the state is new zero-initialised globals on each machine,
cleared by `World.dispose` (quit or load). Skipped (logged) on mismatch.

## Install

Two ways, pick one:

1. **Bundled in wartales-mp** (recommended, see the [top-level README](../README.md)):
   the co-op mod's `winmm.dll` links this patcher and applies it
   to the copy of the bytecode it already hands the game. Nothing in the game
   folder is modified. If the game build differs, the tooltips are silently
   skipped and everything else keeps working (`shim.log`: `tips: not applied`).
2. **Standalone** (`patcher\dist\`, next to a built `wartales-tips.exe`): run
   `install.cmd`. It finds the Steam install, keeps `hlboot.dat.orig` / `sdlboot.dat.orig` backups and writes the patched files.
   `uninstall.cmd` restores the backups. Re-run `install.cmd` after a game update.

## Build

From `patcher\` (`..\shim\build.ps1` runs the gnu-target build itself;
override the location with `-PatcherDir`):

```powershell
cargo build --release                                   # wartales-tips.exe (CLI)
cargo build --release --target x86_64-pc-windows-gnu    # libwartales_tips.a for winmm.dll
.\target\release\wartales-tips.exe patch <in.dat> <out.dat>
.\target\release\wartales-tips.exe inspect <in.dat> ui.comp.Element
```

`vendor\hlbc` is hlbc 0.7.0 with two fixes so that reading and writing the
bytecode is byte-identical (see `vendor\NOTICE.md`). Every lookup in the patcher
is by name (types, fields, methods), never by index, so a game update either
patches cleanly or fails with a clear message.

## Licence

CC BY-NC 4.0, Copyright (c) 2026 Morgott. hlbc is MIT, Copyright Guillaume Anthouard.
Wartales is a trademark of Shiro Games; this is an unofficial mod.
