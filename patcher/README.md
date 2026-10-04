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
"+1"), and the host closure applied it verbatim. The client now sends
`-1 - (n + (usedAptitudePoints << 2))`, the offer it saw; the host closure first
requires `aptitudePoints > 0`, then, for a negative `count`, that the encoded
`usedAptitudePoints` is still the unit's (a second confirm sent before the first
one's grant arrived is stale: nothing is granted or spent), and rebuilds `count`
as the host's own `getAttributeUpCounts(unit).get(attr) ?? 0` plus `n`, read
before `usedAptitudePoints++`, the way the regular level-up closure re-checks its
offer. Career Plan may target an attribute outside the offer (every upgradable
attribute gets +/- buttons), so its base is 0 there. `count >= 0` keeps the old
behaviour. All players must run the same
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
show the hold-to-force bar; a repeat click on the same button within 2 s
(same player) or 5 s (another player) of the click that acted is dropped, as the
vote used to swallow it, so a double click or two players clicking Fight/Leave
together run the action once (later clicks act again: re-used buttons work), and none acts while the host's mode-switch barrier runs; a leave request (`Place.setLeaveState__impl`) and a
rest request (`Controller.playerSetRestState__impl`: dropped unless `game.mode`
is a `CampMode`, not forced while already resting) set their state's `forced`
flag; the first player's "next" advances a dialog (`Dialog.checkAllReady` reads the
host's `coopSkipDialogInstanlty` option as on). An open window no longer
locks the others: `Game.getPlayerLocked` and `Player.canCamp` ignore the synced
`hasWindowOpened` (one player reading a character sheet used to block camp,
leaving camp and leaving the tavern for everyone); a player busy with an
NPC/chest/craft (`lockedWith`) still blocks camp (a place leave: see force
leave below). Game modes are global, so a
host transition disposes every client's old mode with its windows. Not
touched: mode-switch load barriers, battle round sync, owner-only confirms,
world-map gathering. Decisions stay on the host; the input (push/pad) and
window-lock changes run on every machine, so all players need the same build.
Each gate is skipped (logged) on mismatch.

**Co-op: force leave** (`src/force_leave.rs`): a leave from a place (town,
tavern, location) or from the owned tavern is never blocked by another
player's business. The Leave button is no longer disabled while another player
is `lockedWith` an entity (`place.lockLeave`, set by scripts and activities,
still hides it). Once a leave is requested (`Place.leaveState.forced`,
replicated), every machine closes its own lock-holding window the way Escape /
Cancel does (`UnitInfo` of an inspected NPC, also the dialog "inspect" choice;
the dialog customize `ChooseUnit` through its cancel, which refunds the cost;
`FiefMandateDetails`, `GarnisonManager`, `CounterChest`; a `Craft` whose
activity has not started, through its own cancel-before-start `onClose`, after
its `Alter` / `Dismantle` sub-window; these three only with no other modal
window over them), never during a fade; their own `onClose` clears the lock. The host polls
`PlaceView.tryClose` every frame and leaves through the normal `syncLeaveMode`
barrier as soon as nothing refuses. A shared dialog is ended by the host with
its own `Dialog.tryClose` (the Leave choice: `allowLeave` -> `leave`) at an
idle choice point only (Leave choice on screen, dialog visible, no leave
running, no modal window over it, no mode switch / pause / fade), then its
choice buttons are reset so a late click cannot leave twice; a scripted
dialog that cannot be left (no Leave choice) keeps the wait. It waits (never
skips) on fades, cinematics, a running mode switch and on started
activities, crafting and gathering, which have no safe cancel and end on their
own. The request stays pending while somebody still asks
(`leaveState.players`; a withdrawn gamepad toggle drops it). The owned tavern
(`TavernMode`): `Tavern.askLeave__impl` (Leave button) and
`Controller.closeTavern__impl` (Escape) no longer drop a refused request: it
stays pending (host global + the replicated, otherwise unused
`Tavern.leaveState.forced`), every machine closes its own lock-holding window,
and the host retries every frame, waiting on a busy player, fade, alive lock
and a running mode switch, then leaves through the vanilla `syncLeaveMode`.
shim.log shows `mp: tryClose refused: <why>`, `mp: leave pending: <why|go>`,
`mp: tavern leave pending: <why|go>` (on change, with the busy player's
`lockedWith`), `mp: leave closes <window>` and `mp: leave ends dialog`.
The camp (`CampMode` -> world map): `Controller.toggleCamp__impl` (Camp
button / Escape, RPC to the host) no longer drops a request that
`GameUI.toggleCamp` would refuse on a busy player (`anyPlayerLocked(true)`),
fade, alive lock, a running mode switch or a modal window on the host: it
stays pending (host global), and `CampMode.update` retries it every frame
until the vanilla body runs (a running rest still refuses, as before). While
a player is locked, the host calls, at most once a second, the vanilla
host-to-all RPC `Tool.closeActionWindow` for every player `lockedWith` a camp
tool; its `__impl` (otherwise only reached from `GridData.removeTool__impl`)
now, in co-op, closes top-down that player's lock-holding windows with the
same guards (the camp tool windows `StrategyTable`, `CampChest`,
`BannerCamp`, `ConverterTool`, `LecternTool`, `Lute`, `Stake` join the list:
no modal window over them; a `Craft` only before its activity starts), each
through its own `close()` -> `onClose`, which clears `lockedWith`. Strategy
toggles apply on click, so closing the strategy table leaves nothing half
done. Camp dialogs are shared DialogOut modes, not camp windows. shim.log
shows `mp: camp leave pending: <why|go>` and `mp: camp leave closes <window>`.
Skipped (logged) on mismatch; the tavern part or the camp part alone when only
it mismatches.
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
client refusing a mode it is not in) is printed once it passes the game's own
online error cap and repeat check (`lastERROR`), and the co-op mode-switch barrier prints its steps (`mp:
syncLeaveMode` / `syncEnterMode lockSync= waitLocks=`, `waitForClients
waitLocks= clients=`, `onClientReady` / `onServerReady left= callbacks=`,
`doLeaveMode` / `doEnterMode lockAlives= fading= fadeParams= onBreak= pending=` (what
`waitAlive` waits on), `leave host faded`, `leave all clients ready`, `leave
client alive`, `leave client faded`, `enter client alive`, `enter client faded`); every read is null-guarded. The
shim copies the game's stdout (`hl_sys_print`) into `shim.log` as `game:`
lines (buffered in memory, written by a background thread every 500 ms), so a stuck black screen shows which step never came. Skipped (logged) on
mismatch.

**Post-battle input diagnostics** (`src/debrief_diag.rs`): records availability
checks that rebuild the Debrief, their counts and button states, and the hovered,
clicked and topmost UI elements after a tooltip opens. Lines start with
`[mp debrief]`. Each window has separate limits for rebuild and input messages.
This distinguishes repeated window rebuilds from a tooltip intercepting input;
it does not claim to fix the remaining reported input failure.

**Camp choice diagnostics** (`src/camp_choice.rs`): records the outgoing
`Dialog.setClick` request, whether the host resolves its button ID, the chosen
confession gain, and the recipient's aptitude points before and after the grant.
Lines start with `mp: dialog choice` or `mp: confession`. In FriendlyFire's
training choice, Target is the attacker and Self is the injured speaker; the
attacker receives the aptitude point with a Strength/Dexterity offer. These
probes preserve the existing choice and reward behavior. Both machines need
the diagnostic DLL to trace the client-to-host boundary; read each machine's
`%LOCALAPPDATA%\wartales-mp\shim.log` after reproducing the issue.

**Co-op barrier never hangs** (`src/barrier.rs`): every mode switch (town,
tavern, place, battle start) waits on the host for each client's
`onClientReady` (`Controller.waitLocks`), with no timeout and no cleanup, so a
client that reconnected (its old `NetworkClient` stays in `waitLocks`) or got
stuck before answering left every screen black. Host only, three hooks:
`waitForClients` records the phase start; `Controller.update` runs a tick that
(1) drops `waitLocks` entries no longer in `host.clients`, (2) after 30 s
without an answer asks each late client to rejoin: the vanilla
`Controller.reload` RPC (what a host save load sends every client), flushed to
that client alone (`flushProps` + `flushSend` first, then `targetClient`), so
its game reloads, reconnects and full-syncs the state the host moved on to,
getting its own `ent.Player` (and units) back through the vanilla Join
handler's `set_ownerObject`; a kicked client still connected 30 s later is
`stop()`ped; (3) runs the parked callbacks exactly like `onClientReady__impl`'s
tail (copied op for op). The host's Join handler parks a Join that arrives
during a switch (`lockSyncMode`, a non-empty wait/callback/queue list, or a
phase begun less than 3 s ago) and the tick replays it when the switch ends (a
switch still running 90 s later disconnects the parked client instead):
a client full-synced mid-switch would miss the switch's earlier RPCs. Lines in
`shim.log`: `mp: barrier: ...`. Skipped (logged) on mismatch.

**Slot 4 diagnostic** (`src/slot4_diag.rs`): prints only. The new-game customize
screen's 4th human slot cannot be hovered (vanilla too), so
`CustomizeScreen.update` calls a probe after `super.update`: when the 2D
hit-test at the cursor (`h2d.Scene.getInteractive`), the event system's over
list or its push list changed (at most every 0.25 s) it prints `mp: slot4 hit:
m=<window x,y> v=<scene x,y> of <W>x<H> 2d=<hit> abs= wh= prop= cancel= < <parent>[hidden]
... | over=<n> ; <entry> ... push=<n> <entry>`. A 3D interactive in the over
list prints as `Interactive(<name>)`, a 2D one as `<name>(h2d.Interactive)`.
Trapped and null-guarded; skipped (logged) on mismatch.

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
does not send its key-driven shift meanwhile. Leader: only vanilla's synced
`GameState.playerMovePriority`, the player who last moved by their own input
(ground click, mouse hold, pad; an entity click takes it too via
`resetSoftTarget(null)`), or the host while it is unset; never another
follower or just whoever is moving (that chained the caravans). The holder
itself does not follow. A manual move (`Controller.playerGoto` not sent by follow, `playerGotoEntity`,
`BasePlayer.updateMoveHold`) turns F off and pauses following until it ends (at
least 1.5 s). No follow in battle, in a
city, on water, in a cutscene, or while the player is locked with an NPC/chest,
waits to enter/camp or has an interaction window. Host logic is untouched: only
vanilla client RPCs; the state is new zero-initialised globals on each machine,
cleared by `World.dispose` (quit or load). Skipped (logged) on mismatch.

**DLC ownership** is never patched: `BasePlayer.hasDlc`, `checkSignature`,
`GameState.hasFeature`, `insertWtdc`, the lobby's `LobbyState.missingDlcs` and
every other DLC check stay byte-identical to the game's (asserted by the
`dlc_untouched` test over the full patch chain).
**Shared chest buttons** (`src/chest_buttons.rs`): the co-op chest panel of the
inventory side panel (`GameInventory.chestInventory`) gets a row of icons at
its top left. Sort: the player panel's sort menu (icon `SortButton`, entries
`Texts.tips.inventory_sort`) applied to the camp chest through the
host-authoritative `st.Inventory.netSortBy` RPC, the call the camp chest window
uses. Quick stack (chest icon, tooltip "Place item in chest"): every stack of
the player's inventory whose item kind the chest already holds moves there;
take similar (`LootAll` icon, "Place item in inventory"): the reverse. Both
use the vanilla slot move `SlotOperation.MoveTo` like the slot UI: local on an
inventory the machine has authority over, else the `networkOperation` RPC (the
host consumes and adds in one step). Built in `GameInventory`'s constructor with
the same domkit calls as the player panel's buttons; skipped (logged) on mismatch.

**Party inventory costs** (`src/party_inventory.rs`): vanilla pays "with chest"
costs (`PlayerInventory.useList(inv, list, checkChest = true)`: crafting,
brewing, repairs, healing, alter, boat work, Confession dialogs) from the
player's own inventory, then the camp chest, then the boat chest, on the host.
A prologue `list = partyPrepare(this, list)` (host only) first checks the whole
list: global items must be held, every other entry must be covered by own +
chest + boat + the other players' inventories; otherwise the list is left to
vanilla (which fails, nothing consumed). Then the part vanilla cannot cover is
taken from the other players in `state.players` order (stolen stacks first) and
vanilla pays the rest. Classic dialogs get the Confession dialogs' scope: a
choice is allowed when the party (global + every player + chest, never equipped
items) holds the item, cost labels count chest + other players, the "same type"
substitution looks at the party, and the cost is paid with `checkChest`.
Tavern event dialogs keep the tavern stock. No new RPC or synced field.
The matching displays (`hasItemWithChest` / `countWithChest`: crafting recipes
and craftable amounts, brewing, repair/heal/alter/boat costs) count the other
players too (GlobalInventory flags `1|2|256` -> `1|2|4|256`); they already
counted the global inventory, the chest and the boat chest on board.
Recipe ingredient rows (`ItemsRecipe`: Grimoire recipe cells and their learn
tooltip, the recipe in item tooltips) called `hasItemWithChest` only when built
with `checkChest`, which those callers never pass; their own-inventory
`hasItem` call now goes to `hasItemWithChest` too (display only: crafting
already pays with `useList(checkChest)`). Grimoire learn costs stay own-only
(the host pays them with `tryUse` on the acting player's inventory).
Activities: fishing hooks (`FishingAction`: counter, cast gate, hook kind) and
lockpicks (`Chest.tryUnlock`'s pick gate, `LockPick` and crime cave counters,
the count before each attempt) count with `countWithChest` / `hasItemWithChest`;
the host's hook wear-out `tryUse` and the per-attempt lockpick `use` go to
`partyTryUse` / `partyUse`: the vanilla call when the own inventory holds
enough, else a one-entry `useList(checkChest = true)` (host: `partyPrepare`,
client: vanilla's RPC to the host). No new RPC or synced field.

**Network handler guard** (`src/net_guard.rs`): in vanilla an exception thrown
while a machine handles network data unwinds through the relay service; on the
host that ends its relay connection and every guest is dropped. Now the game's
own handler bodies run under a trap: every `<name>__impl` call of the 152
generated `networkRPC` functions (701 calls) and the `host.onMessage` calls of
`processMessage`, on the host and on guests. The exception is swallowed only
when hxbit's shared state is provably intact: the handler started and ended
outside any message writer or send (a writer depth counter around every
`NetworkHost` writer and `beginRPC`/`endRPC`) and outside a dispatch, and the
host, its ctx, the input buffer and position, `receivingClient`, the client's
`processID` and a receive generation (bumped per message) are unchanged. Then
`targetClient` is restored, `mp: net: RPC handler <Class.method> threw,
ignored: <error>` and the stack go to shim.log (at most 50 lines a run), and
the impl's result reads as its type's default (an RPC with result answers it).
Otherwise the exception is rethrown (vanilla). Game state the handler changed
before it threw is kept. Not covered, still vanilla: argument decoding,
property sync, object registration, full sync, RPC result callbacks, protocol
errors. Skipped (logged) on mismatch.

**Client window close** (`src/window_close.rs`): the modal backdrop click
(`Window.setModal`'s windowRoot onClick, Window.hx:295-300) closes a window on
a click outside it, but on a client it always called `triggerClose()`, the RPC
stub of a host-replicated window, which returns at once for a window that only
exists on that machine (`__host == null`: UnitInfo and every window a client
opens for itself). The client's call now gets `if (__host == null) close()` in
front, as Escape already does (Game.hx:1725-1727); shared windows keep the RPC.
Skipped (logged) on mismatch.

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
