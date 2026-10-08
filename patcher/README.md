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

**All or nothing.** Every pass checks the exact shapes it edits first. A pass
(or a part of one) that does not match the game build leaves the bytecode
untouched and logs `<pass> skipped: <why>`; `wartales_tips_patch` then refuses
the whole image, naming every mismatching pass, and the shim serves the game
unpatched: a build the co-op set does not fully fit never runs half-patched.
Only the print-only diagnostics (`diag`, `activity_diag`, `debrief_diag`,
`camp_choice`) are skipped alone. "Skipped (logged)" below means exactly that.

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
match, the fix is skipped (logged).

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
match, the fix is skipped (logged).

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

**Loot level** (`src/loot_level.rs`): `battle.Debrief.genLoot` keeps a dead
enemy's worn item as a drop candidate through one closure per slot (nine
copies, Debrief.hx:521-531). Each one skipped an item whose `requireLevel` is
above `getReferenceLevel(1.0)` (the best unit's level), and for armor called
`Unit.canEquip`, which checks the unit's level too. The `JSGte` level compare
becomes a `JAlways` to the same target, and `canEquip` + `JFalse` becomes
`hasCantEquipReasons` + `JTrue` (armor / helmet kind, animal and item-flag
checks kept, level dropped). Same op count; drop chance, pity counter,
NoEquipDrop, ForceDropWeapon, quality and repair stay vanilla. Loot is
generated on the host.

**Armor drop** (same file, `patch_armor_any`): in the same nine closures the
`JTrue` after `item.isType(Armor)` that leads to the "a unit can wear it" loop
becomes a `Nop`, so armor is pushed as a candidate like weapons and trinkets.
Gear no unit can wear stays an ordinary item (stored, sold, dismantled).

**Champion gear** (same file, `patch_champion_gear`): genLoot skipped the
worn-gear roll for every NoEquipDrop class. With `x = flags & (IsChampion |
NoEquipDrop | ArenaChampion)`, the six-op test becomes `x ^ 8 > 16 -> skip`:
the roll runs for a NoEquipDrop champion that is not an arena champion
(named bosses), and creatures (ghosts, rats, The Beast, sea snake, workmen)
and arena champions keep the flag (vanilla arena masters give that gear as
dialog rewards). Items with `disableLoot` stay out through the candidate
closure; ForceDropWeapon, chance and pity counter unchanged.

**Loot order** (same file, `patch_loot_order`): genLoot's worn-gear loop
(Debrief.hx:500-544) walked `state.allUnits.array` in array order, so the
first dead enemy with an eligible item took the guaranteed drop. Three ops
after the loop's array cast make it walk a sorted copy: `units =
units.copy(); units.sort(lootCmp)`, with two appended functions, `lootCmp(a,
b) = lootKey(b) - lootKey(a)` (typed as ArrayObj.sort's comparator) and
`lootKey(u) = level * 2 + (class IsChampion | IsBoss ? 1 : 0)` (-1 without
data). haxe.ds.ArraySort is stable, so ties keep array order. The boss now
nearly always comes first, and vanilla's `firstEquip = false` after a
ForceDropWeapon drop would then eat the guarantee, so that Mov becomes a Nop:
the forced weapon no longer counts as the guaranteed worn-gear drop. The
per-enemy loot-table rolls run in the new order (independent odds);
state.allUnits is untouched.

**Loot pity** (same file, `patch_loot_pity`): the extra worn-gear roll's
chance (`0.08 + bonuses + equipLootProba * 0.03`, ops 654-700) is multiplied
by `sqrt(8 / N)`, N = dead enemies (at least 1): one `Mul` before the roll's
`JNotLt`, and one call at the worn-gear loop entry to an appended function
that counts the units the loop treats as dead enemies (data, not player side,
not alive, not a captured animal). Expected items per battle at N = 2 / 4 / 6
/ 8 / 10 / 12: 1.10 / 1.32 / 1.57 / 1.81 / 2.04 / 2.26 (vanilla 1.05 / 1.23
/ 1.50 / 1.81 / 2.13 / 2.45). Guarantee, counter and reset unchanged.

**Skill sync** (`src/skill_sync.rs`, issue #2): a round reset
(`battle.Unit.resetTurn`) reaches a client through hxbit `networkSync`, whose
setters never mark the battle UI dirty (`set_skillPersistValues` does, but not
while the battle is locked), so the client's skill bar kept the previous
round's availability (Inhalation greyed) until an action redrew it. On a
client only (`!Game.isAuth`), when the list of `battle.currentUnit` is emptied,
`set_apSkillPlayed` now calls `battle.setUIDirty()` (a flag; the next
`Battle.update` redraws the skill bar, as after an action).
Host and solo are unchanged. Skipped (logged) on mismatch.

**Loading draw** (`src/loading_draw.rs`, issue #3): `Game.render` drew
`Main.loadingS2d` only once `Game.state` exists. A joining client waits for the
host between `Game.init` (which creates the loading scene) and `Game.start`
(which sets the state) while `hxd.System.mainLoop` keeps presenting frames that
nothing draws or clears, so the loading screen flickered. While the state is
null, `Game.render` now draws the loading scene when one is up, the same call
vanilla makes once a state exists. Nothing changes afterwards or without a
loading scene. Skipped (logged) on mismatch.

**Skill vars** (`src/skill_vars.rs`, issue #2): `ScriptedSkill.initScript`
binds `unit.getSkillPersist(id).vars` into the skill script once, and
`Battle.getOrCreateSkill` caches the skill. On a client, hxbit sync replaces
`battle.Unit.skillPersistValues` wholesale, so cached scripts kept reading and
writing an old map's vars (Inhalation's `getCost` used a stale `vars.used`).
On a client only, `getOrCreateSkill` now rebinds a returned ScriptedSkill whose
`interp.variables["vars"]` is not the current object (and its
`ScriptInterp.checkObjUpdate`) to the vars of the existing map entry (a
missing entry is left alone, never created); skills,
scripts and the cache are kept. Host and solo are unchanged. Skipped (logged)
on mismatch.

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
the probes themselves do not change input behavior.

**Transient tooltip input** (`src/tooltip_input.rs`): the helper-column screen
clamp can move an item tooltip over the hovered slot. Decorative tooltip Icons
inherit blocking Interactive handlers; hitting one outs the slot and removes
the tooltip, then the next stationary-pointer check recreates it. Mouse press
and release can consequently reach different Interactive objects. Scene's
candidate dispatch now skips descendants of transient ItemTip, SkillTip and
TipHelper content using its existing cancelled-candidate continuation, so the
underlying control keeps receiving pointer events. Sticky tips (`keepTips`),
gamepad, keyboard events and other tooltip content retain native handling.
The guarded predicate fails open and changes no persistent input fields.
This repairs a source-proven cycle; the reported multiplayer session has not
been reproduced live with the new DLL.

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
switch still running 90 s after a client's own Join disconnects that client instead):
a client full-synced mid-switch would miss the switch's earlier RPCs. Lines in
`shim.log`: `mp: barrier: ...`. Skipped (logged) on mismatch.

**Co-op auto-follow** (`src/follow.rs`): in co-op an open window does not pause
the world map, so `World.update` gets a new `followUpdate` right before
`updateSprint()`. Follow is ON by default for every co-op player; **F** is a
personal opt-out (no world-map binding uses it; a log toast says
"Follow: ON/OFF"; OFF is kept across sessions in
`mpman.Storage` user data `mpFollowOff`, read once per session and written on
F, both under a trap). While following, every 0.5 s the caravan sends the same RPC a ground click
sends, `Controller.playerGoto`, to a point 5 units short of the leader once the
leader is more than 9 away (the leader's own position when that point is not
walkable), then `resetSoftTarget(&true)` like a click without taking the move
priority. Sprint is mirrored with `Controller.playerSetShift` (a moving
leader's synced `flags` bit 8; a resting leader means walk); `updateSprint`
does not send its key-driven shift meanwhile. Leader: only vanilla's synced
`GameState.playerMovePriority`, the player who last moved by their own input
(ground click, mouse hold, pad; an entity click takes it too via
`resetSoftTarget(null)`), or the host while it is unset; but the host while
the host's own move is still going (its target set since it held the priority,
gaps up to 1 s): when several players move by their own input the host leads,
otherwise the most recent mover. Never another follower or just whoever is
moving (that chained the caravans): follow moves never take the priority nor
count as the host's own move. The leader itself does not follow. A manual move
(`Controller.playerGoto` not sent by follow, `playerGotoEntity`,
`BasePlayer.updateMoveHold`) pauses following without touching F: it resumes
1 s after the move arrived (own target seen, then cleared; a client's target
appears only after the host round trip) or, when no target ever shows up
(unwalkable click), 1.5 s + 1 s after the click. No follow in battle, in a
city, on water, in a cutscene, or while the player is locked with an NPC/chest,
waits to enter/camp or has an interaction window; such a gate during a pending
manual move restarts the 1 s idle when it clears. Host logic is untouched: only
vanilla client RPCs; the state is new zero-initialised globals on each machine,
cleared by `World.dispose` (quit or load) except the F choice. Skipped (logged) on mismatch.

**Nicknames on player markers** (`src/marker_names.rs`): the co-op world-map
locator (`ui.comp.PlayerMarker`, clamped to the screen edge) and the minimap
arrow (`ui.comp.PlayerArrow`, also the arrow inside each locator) label the
player with vanilla `Texts.multiplayer.playersArrow[getColor() - 1]` ("P1" /
"И1"). Right after that `playerText.set_text` in both constructors (the call
itself is a jump target, the next op is not), an appended
`markerName(player)` returns `BasePlayer.getUserName()` (plain,
profanity-filtered; `getName` is HTML and `h2d.Text` would print its tags), cut
to 11 chars + `...` when longer than 12, or null when the name is null or
empty; non-null replaces the label through the same `set_text` slot. The colour
is the vanilla `player-<color>` dom class on `playerText`. Local UI only.
Skipped (logged) on mismatch.

**ALT highlight on the world map** (`src/alt_world.rs`): `Element.onOver`
already picks the ALT colour (`isDown("Outlines")` ->
`PREFS.outlineHighlightColor`), but `noOverNoOutline` (Element.hx:652) is true
whenever `game.mode == game.world`, so the colour is wiped for every
non-hovered element; `world.World` has no ALT poll (`PlaceView.update` has
one). (A) that `JEq mode, world -> true` becomes `Call2 r = altWorldGate(mode,
world); JTrue r` (`mode == world && !isDown("Outlines")`); places, ruins and
`DialogOutView` unchanged. (B) after `updateCulling(false)` in `World.update`,
`altWorldEdge(this)` (world mode only): on an ALT edge (state in a new bool
global) it walks `game.state.persistStates` and calls `updateOutline` (vtable
slot, as `PlaceView`) on every `Element` with an `obj`: on press only the
un-culled ones (`obj.flags & 4 == 0`), on release all. (C) the single `Ret` of
`Element.onUpdateCulling` becomes `altWorldCull(this); Ret` (after the lazy
`interact` init): world mode, ALT held, on screen -> `updateOutline`.
Local render state, no network. Places (POI markers) and `ent.Roaming` keep
their own `onOver` and are not covered. Skipped (logged) on mismatch.

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

**Stale lobby** (`src/stale_lobby.rs`): two lobby creations answered in one
tick (StartChoice's start button has no repeat guard) made the second `Lobby`
constructor leave the first (`mpLobby = null`) before the first's
`host.connect` callback ran (Lobby.hx:759-770, one tick later); its
`onConnected` then crashed the host with Null access `.isLoad`
(`LobbyState.initAssignments`). The callback now returns right after its trace
when its lobby was left; the newer lobby's own callback drives the UI.
Skipped (logged) on mismatch.

**logError cast** (`src/log_error_cast.rs`): `mpman.Api.logError` is the
game's `String` logger behind a `Dynamic` field; its wrapper closure's
`SafeCast dyn -> String` threw "Can't cast SysError to String" when mpman
logged a raw socket error (`RelayHost.connect`'s handler), so `onConnect(null)`
never ran and a host whose relay connect failed loaded forever. The cast is now
`Std.string(e)` (same op count): the error text is logged and the failure path
runs. Skipped (logged) on mismatch.

**Direct lobby** (`src/direct_lobby.rs`): lobby members are named by their
games' own ids on both transports (internal/master `idOf`), so a lobby of
Steam players looks Steam-only to `Lobby.isSteamOnly` (only caller
`setupPlatform`). A prologue returns false when `mpLobby.id` ends in 'D', the
mark our master gives every direct-relay lobby (`DirectLobbyMark`): the host
asks `instance/get` and plays over our relay. Skipped (logged) on mismatch.

**Style guard** (`src/style_guard.rs`): domkit's `Properties.applyStyle` saves
the static `APPLY_LOOPS`, counts style passes in it and restores it at the end;
`checkLoop` (run on every node creation, class change and hover) throws
"Infinite loop in apply style" once it passes 100. An exception inside the pass
skipped the restore, so the counter stayed above 100 and every later UI action
on that machine threw the same error until restart (seen in co-op: the
game-over `Pause` window, built when the escort-death popup is confirmed,
threw there; afterwards no button answered on host or guest). Now the pass runs
under a trap that restores `APPLY_LOOPS` and the dirty count, logs
`mp: style: applyStyle threw after <n> passes ...` with the root and the first
12 dirty nodes (component:object; on a runaway loop these are the nodes the
last pass dirtied) plus the full stack, and rethrows. `Controller.gameOver__impl`
runs under a trap too: on an exception it resets `APPLY_LOOPS`, logs
`mp: game over window failed, opening the pause menu instead: ...` with the
stack, closes the half-built window and removes its modal root, and opens the
normal pause menu (Load / Quit) instead, so the host's popup closes and the
session is not bricked. The shim now writes every line of a multi-line game
message (exception + "Called from" stack) and continues long lines instead of
cutting them at 300 characters. Skipped (logged) per part on mismatch.

**Drop-in** (`src/drop_in.rs`): a player who was not in the session can join
a running co-op game by code or Steam invite. Vanilla built the host's in-game
reconnect lobby only when the Pause menu opened (`Game.createLobby`, Pause.hx:133),
and its `getServerID` (Game.hx:3058-3061) refused every user without a
`state.players` entry. Now `Game.gameplayStart` creates that lobby on a co-op
host right away (same guards as Pause, plus `!get_isLocalP2P()`), and
`getServerID` admits an unknown user while `state.players.length` is below the
lobby's `maxPlayers` (4, read from `createLobby`); a full party is still
refused. Log: `mp: drop-in: new player admitted <uid>`. The newcomer owns no
units until someone gives them some: in camp, unit sheet, Stats tab,
Transfer (vanilla `UnitInfo.transferUnit` / `PlayersPanel`; shown in camp
mode only, enabled for the unit's owner; the host owns the units of players
who were absent at load). Transfer clears the unit's camp tool (vanilla). Shapes are validated;
a mismatch skips the pass (logged).

**Returning units** (`src/returning_units.rs`): when a co-op load starts
without some save players, vanilla `Game.removingPlayers` (Game.hx:1029) gives
their units and items to the host and deletes their player, so a player who
drops in later came back with nothing. The host now remembers each absent
player's units (`allUnits` copy, keyed by uid, in a patch-added process-wide
`StringMap`; nothing is saved) and, right after the returning player's
`initContent` in the Join handler (Game.hx:434), `swapOwner`s back every unit
the host still owns and that is still in the troop (units already given away,
dismissed or dead stay as they are). During a battle the hand-back waits:
the battle's unit objects would keep the old owner, so the entry stays pending
and `Battle.disposeBattle` (after `game.battle = null`) runs it for every
player. The map goes with its game (`Game.dispose`), and one left from another
game state is dropped, never used. Items stay with the host. Logs:
`mp: drop-in: remembered units of <uid>`, `mp: drop-in: units returned to
<uid>: <n>`. After a host restart the map is empty; the vanilla Transfer button
still works. Shapes are validated; a mismatch skips the pass (logged).

**Battle takeover** (`src/battle_takeover.rs`): a player who left during a
battle kept their units, and vanilla only marks them `connected = false`
(`BasePlayer.checkConnection`); the battle gate `st.Unit.isControllable`
(Unit.hx:2024, `isOwnedBy(game.me)`) let nobody play them, so a turn the absent
player had begun (`battle.State.startedPlaying`) blocked the battle for good.
On the host only (`game.isAuth`), during a battle, a unit whose owner is not
connected is now controllable (`isControllable` op 0, like vanilla Pit's
`coopIgnoreOwners`), and `Mount.canPlayMount` (Mount.hx:179) counts a rider of
such a player as the host's. Ownership never changes: when the player rejoins,
`connected` is set again and the units are theirs. No new function, no log
line. Shapes are validated; a mismatch skips the pass (logged).

**Camp: any unit** (`src/camp_any_unit.rs`): in co-op camp only a unit's
owner could drag it, assign it to a camp tool or move it between camp and
reserve: four client-side UI gates call `st.Unit.isControllable()`, which
outside battle is `isOwnedBy(game.me)`. Those four calls
(`CampEntryEntity.canDrag`, CampEntryEntity.hx:739; the camp window's reserve
thumbnail tip and click closures, CampMode.hx:115 and :137; `Camp.removeUnit`,
Camp.hx:1338) now call `isPlayer()` (owner != null), so any player may move,
assign or reserve any party unit. The host RPCs behind them
(`GridData.netMoveEntry`, `Camp.sendToCamp` / `sendToReserve`) check no
ownership, so nothing changes on the host side; ownership itself is unchanged.
Single player is unaffected (`me` owns every party unit). `isControllable`
itself, battle control, item actions and the unit sheet keep their owner
checks. Sites are found by debug position; exactly four are required, else the
pass is skipped (logged).

**Draggable windows** (`src/window_drag.rs`): popup windows and the chest /
inventory side panels can be moved with the mouse; positions are kept. Local
UI only, nothing goes over the network. Vanilla already has a window drag
(Window.hx:525-575) that `Window.init` installs only when `canDragWindow()` is
true (just the map); for every other window the test now jumps to an appended
install: the window's own interactive gets an `onPush` that starts a drag when
the window is modal (`modal != WindowModalMode.None`, so HUDs, title / loading
screens and notifications stay put), the push is in the top 70 px of the
window (header strip) and the window is narrower than 90 % of the screen.
Buttons and the unit sheet's 3D portrait keep their clicks (they sit above the
window's interactive). The `GameInventory` constructor gives the chest and
inventory panels the same drag on their "title" header row (each panel moves
on its own; hiding the chest lets the inventory slide up in the column, keeping
its own offset). Drag sets the offset in the parent flow's `FlowProperties`
(applied after layout, like vanilla), the mouse clamped to the screen; a second
push within 0.35 s resets to the layout position. Release saves the offset with
`mpman.Storage.setUserData("mpWinPos:" + key, ((dx + 32768) << 16) | ((dy +
32768) & 0xFFFF))` (key: the window's class name, e.g. `ui.win.UnitInfo`, or
`GameInventory#chest` / `GameInventory#inv`; removed at 0,0); a window or
panel gets it back when it is built. After every reflow (window resize
included) a moved window / panel is pushed back so 48 px of it stay on screen
and its top edge stays reachable. The map keeps its vanilla whole-body drag
(no saving). Windows whose own `init` replaces `interactive.onPush` after the
base init stay fixed. Header-row elements of a window (default cursor, top
70 px) get their `onPush` wrapped (own handler, then the drag) instead of
passing events on, so their hover and tooltips stay intact. Inventory panels
(chest, inventory, co-op AllInv) also get a small resize handle at the
bottom-left corner: dragging it sets the scroll area to whole 53 px rows
(min 2, max what fits below the panel on screen; width unchanged); the top
stays put (the panels are bottom-anchored, so the offset grows with the
height); the rows are saved as `mpWinSize:<key>` and restored when the panel
is built, and its reflow keeps that many grid rows built (vanilla resets to 6
on open). Every appended function runs under a trap (exceptions
logged as `mp: drag: ...`). The shared functions (`mpDragBegin`,
`mpDragRestore`, `mpDragClamp`, `mpDragPanel`) are reusable for more panels.
Each part is skipped (logged) on mismatch.

**All players' inventories** (`src/all_inv.rs`): a bottom-bar button (co-op
only; `WorldButtonsBar` constructor end, a `flow.button` with the
`DialogMerchantTrade` icon, enabled like `btInventory`) opens, inside
`GameInventory`'s dom, one panel per other connected player shaped like the
vanilla `#inventory` one (title with the nickname and a Close icon,
`inventory-content` holding `new ui.comp.Inventory(FoundItems, p.inventory, 6,
content)`), draggable through `mpDragPanel(panel, "AllInv#<player index>")`.
A player's `st.Inventory` belongs to its client and its RPCs are owner-routed,
so every cross-player move is a `SlotOperation.MoveTo` executed by the owner of
the source inventory (consume and `target.addItem` in one step; the add to a
foreign inventory is the `netAddItem` RPC to its owner); no hand op, no
Remove + local add. (N) `st.Inventory.networkAllow`'s dead `networkGetName`
block (ops 41-50) now grants modes 0 (host receive) and 6 (caller pre-check)
of RPCs `networkOperation` (7) and `netAddItem` (0) on an owned inventory to
any client with a BasePlayer; modes 1/2 (routing) and 3-5 (sync, register)
stay owner-only, so a call still runs once, on the owner. (S) the slot API's
`networkOperation` closure sends a MoveTo on an owned inventory raw instead of
vanilla's Remove + local add (whose result never comes back from the owner);
containers keep the conversion. (U) take = the vanilla FoundItems right click
(shift: amount), executed by the owner; `ItemSlot.allowPick` / `allowDrop` /
`doPick` / `drop` refuse a foreign slot (FoundItems slot inside a
`ui.comp.Inventory` whose inventory has an owner other than `Game.me`); give =
`ItemSlot.onRightClick` op 0: while panels are open, ctrl + right click on the
own inventory sends `MoveTo(target, stack)` through the slot API (ctrl +
shift: amount box), target = the panel last hovered (`getTipContent` op 0) or
right-clicked, else the first, connected players only; the amount box keeps
the recipient and item it opened with and gives nothing if that player left
or the slot changed meanwhile. `GameUI.update` op 0
closes the panels in battle, while loading, in an arena fight, in solo or when
the HUD was rebuilt, and drops the panel of a player who disconnected. All
players need this build. Appended functions run under a trap (`mp: allinv:
...`); the whole pass is skipped (logged) if any part mismatches.

**Co-op forge mirror** (`src/forge_mirror.rs`): while a player forges, the
other players see that player's worker at the anvil hammer, with each hit's
grade sound and particles and the success / fail gesture at the end; their
camera, UI and input are untouched and no window is created on their side.
Vanilla shares a mini-game only through data.cdb `props.coop` and a window
class with hxbit-synced fields / RPCs (GatherAction); `ui.win.ForgeAction` has
neither, so the others only got the worker parked at the anvil
(`PlaceView.activityUnits.get(element).unitView`). Rather than a data change
plus hand-written hxbit serialization and RPCs on ForgeAction (and a replicated
window taking over every peer's screen), the events ride the existing ping RPC
(`Controller.ping(x, y, z, player)`, any player -> host -> every machine) with
x = -987654321, y = the element's hxbit `__uid`, z = code + (a << 2) + (b << 6):
`ForgeAction.init` sends start (0), `setActionDone` a hit (1; a = EScoreTier
A/B/C = perfect/good/bad, b = 1 + the shard's child index), `endActivity` the
end (2; a = ActivityResult index); co-op only, under a trap. `ping__impl` starts
with `if (forgeRecv(this, x, y, z, player)) return;`: any other x is a vanilla
ping; a sentinel one never shows a marker. Skipped for the sender itself and
outside a PlaceView; start remembers the worker's current anim as its idle;
a hit plays ForgeAction's `animHit` ("Forge") once, back to idle on its end,
and 0.4 s later the grade's sound (`game.ui.sfx`) and the vanilla particle
prefabs at the shard (scene `allShards` child), else the `anvil`, else the
worker, removed after 1.5 s; end plays `animSuccess` ("ForgeYes") on Success,
else `animFail` ("ForgeMeh"). shim.log: `mp: forge send <code> <a> <b> <uid>`
and `mp: forge recv <code> <a> <b> <stage>` (5 start, 6 end, 7 hit); a send
that does not happen prints `mp: forge send skip <code> <step>` (1 no game,
2 no controller, 3 solo, 4 no activity, 5 no target, 0 exception). All
players need this build (an older one shows a far-away ping with its sound).
Archery and the UnitAction kinds: see the work mirror below. Skipped (logged)
on mismatch.

**Co-op spectating** (`src/coop_spectate.rs`): a shared mini-game (data.cdb
`props.coop`: fishing, mining, wood cutting, lock picking, singing, gambling,
the ruins puzzles) no longer takes over the other players' screens. Vanilla
starts a coop activity on the host (`a_startActivity` passes `game.me`), so
`ui.win.Activity` is host-owned (`persist = CurrentMode`) and replicated; its
`setUnit` / `start` RPCs run on every machine and `_start` has no owner test:
every player's inventory was hidden, `mode.lockCamera` / `padCursor.locked`
set, the camera faded to the activity camera, the mini-game window opened
(`setWindow`) with a help icon (the `addHelpIcon` RPC, sent by every
machine), and the end faded everyone (`ctrl.netFade(coop)`). The windows
already support spectators (input only for `unit.owner == game.me`; the
worker's anims / props / effects come from replicated state; the host's copy
stays authoritative), so they are kept but hidden. A spectator is a machine
where `spect(act)`: the activity is replicated (`__host != null`), has a unit,
and `unit.owner != game.me`. `_start` gets `m = spectPre(this)` at its entry
(spectator: `changeCamera = false`, so no fade or camera move;
m = 1 | old lockCamera << 1 | old padCursor.locked << 2) and `spectPost(this,
m)` before its Ret (old lockCamera / padCursor.locked back, `cameraSave =
null` so `_cancel` moves no camera, `showInventory(prevInventory)`, chooseUnit's
full-screen wait Interactive removed); the start cost, GC entry and trait
level stay vanilla. onReady's `showTutorial` -> `spectTut` (no tutorial, the
callback runs at once); the window closure's `setWindow` -> `spectWin` (the
window and its windowRoot invisible: not drawn, no Interactive events, not the
mode's current window, still updated and networked) and `addHelpIcon` ->
`spectHelp` (not sent); `addHelpIcon__impl` returns at once on a spectator;
onActionDone's end `netFade` -> `spectFade` (not replicated when the activity
has a unit with an owner). shim.log: `mp: spectate <id> <m>` on each spectator.
Unit-less activities (BoardPuzzle, a solved NinePuzzle) stay shared as in
vanilla. Not covered: the unit choice before the start (the host's shared
ChooseUnit, the clients' wait Interactive): which player starts it is known on
the host only (`conds` is not synced). All players need this build. Skipped
(logged) on mismatch.

**Co-op work mirror** (`src/work_mirror.rs`, shared parts in `src/mirror.rs`):
while a player does archery or a progress-bar activity (`ui.win.UnitAction`:
Study, MoneyLaundering, Dismantling, Altering, Snaring, Tracking and every
other activity without its own mini-game window), the other players see that
player's worker work. These activities are local-only (no `props.coop`); the
others see the worker parked at the element in a place (`addUnitToElement`,
when the activity prefab's Tool is visible) or the unit's own camp entry
entity in the camp. Vanilla visuals: UnitAction.click plays "Attack" once on
the worker (its progress bar is 2D); Archery is a first-person bow / arrows /
target scene with shot sounds and no worker anim. So the mirror plays the work
anim read from UnitAction.click ("Attack") once per click or shot, then the
idle, and the idle at the end. Events ride the ping RPC like the forge mirror,
with x = -987654322, y = the element's hxbit uid (place) or the unit's (camp),
z = code + (camp << 2) + (kind << 3) (code 0 start, 1 hit, 2 end; kind 0
UnitAction, 1 Archery): `workSend` at the entry of UnitAction.init / click and
Archery.init / setWorldPosOnShoot, co-op only and never for a replicated (coop)
activity, under a trap; start remembers the activity, and
`Activity._cancel__impl` (every end, success or cancel) sends the end for it
(`workEnd`). `ping__impl` gets a second `if (workRecv(...)) return;`: skipped
for the sender; the worker is `mirrorWorker(game, y, camp)` (mirror.rs: the
PlaceView `activityUnits` entry or the CampMode `entryEntities` entity whose
`entry.content` is `Unit(u)`, as `Activity.setUnit__impl` finds them); start
remembers its anim as that worker's idle (one entry per worker, so two players
working at once keep their own; the end forgets it), a hit plays the work anim
once (onEnd: that idle looped), the end loops the idle. `mirror.rs` also holds what both mirrors
validate (types, fields, ping RPC, Entity.play, WaitEvent, logging) and the
shared one-shot / idle anim code. shim.log: `mp: work send <code> <kind>
<camp> <uid>`, `mp: work recv <code> <kind> <camp> <stage>` (3 start, 4 hit, 5
end; 1 no worker). All players need this build (an older one shows a far-away
ping with its sound). Skipped (logged) on mismatch.## Install

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
