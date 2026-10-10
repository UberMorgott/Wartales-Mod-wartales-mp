# wartales-mp

A mod for **Wartales co-op through Steam on Windows**.

The mod replaces Wartales' old connection system, which prevented players in different countries from playing together. It removes the dependency on the developers' servers and adds direct connections between players.

[**Download the mod**](https://github.com/UberMorgott/Wartales-Mod-wartales-mp/releases) · [**Описание на русском**](README.ru.md)

## What it does

Everything below is in the one **winmm.dll**. The game patches are applied in memory when the game starts; the game files on disk are not changed. Each patch first checks that the game code looks as expected: if any patch does not match (a different game build), none of them is applied and the game runs its original code, while the connection part of the mod (Steam transport, join by code, helper) still works.

### Network and lobby

- Disables the old Steam networking protocol that caused connection problems in many countries. It uses modern Steam networking (SDR) instead.
- Removes the dependency on the developers' servers for creating and finding lobbies. Adds a direct connection to the host without intermediary servers when the host's network allows it.
- Changes how the game's existing join codes work: friends now connect directly to the host, bypassing the developers' servers.
- Keeps familiar Steam invitations. A Steam lobby is created automatically with the in-game room.
- Fixes identified reconnection errors and a crash caused by waiting in the lobby.
- Mod-file check on join: a player whose mod files (winmm.dll, and res1.pak if any) differ from the host's is refused at the join with a clear message instead of crashing later in the game.
- Join by code (direct connection) now works for loaded co-op saves, and a host can start the game over the direct connection without hanging. Players are now identified by the game's own player id on both Steam and direct connections.

### Co-op stability

- The mode-switch wait (leaving or entering a town, the tavern or a place, starting a battle) no longer leaves every screen black forever: the host drops a player who has not answered after 30 seconds and reloads them into the running game; a player who joins during the switch is let in after it.
- An error in the game's handling of a network message is logged instead of ending the whole co-op session.
- DLC ownership checks are untouched (vanilla): DLC content and the save's DLC markers follow the game's own rules.
- Fixes the post-battle screen rebuilding itself every frame in co-op when the remedy count and the injured units disagreed.
- Post-battle loot with a damaged or injured squad: the screen no longer tears itself down and rebuilds every frame while the "repair all" / "cure all" availability changes, which could leave loot items and **Take all** dead (hover sound repeating, clicks lost). The buttons now just turn on and off. *(Vanilla bug.)*
- A co-op load no longer waits forever for a player who disconnected before they were ready to start, and a reconnecting player is no longer counted twice. *(Vanilla bug.)*
- Loading a save, restarting a battle or reloading from the pause menu in a co-op game no longer hangs on the loading screen when some players of the save are not in the session: the host keeps the lobby's list of absent players across the reload. *(Vanilla bug.)*
- When the host loads a save inside a running co-op session, the other players no longer hang on an endless loading screen: the mod keeps the Steam connection to each player open while the game restarts its own session, so the reload also works on slow or unstable networks.
- A player who reconnects during a battle no longer stays on the loading screen forever: the host replays the battle's start to that player. *(Vanilla bug.)*
- A guest no longer freezes at the end of a battle after rejoining it mid-battle.
- A player who leaves during a battle no longer blocks it: the host can play that player's units (also a turn they had already begun) until they come back; the units stay theirs and they take them over again when they rejoin. *(Vanilla bug.)*
- The host no longer crashes when two lobby creations finish at the same moment (a left lobby's late connect result is ignored).
- Clients no longer freeze when an enemy summons units and hits them in the same moment (for example the Rat Matriarch's howl spawning rats): the client makes the new units alive before it runs the host's battle messages. *(Vanilla bug.)*
- A joining player's loading screen no longer flickers while they wait for the host: it is drawn before the game state exists. *(Vanilla bug.)*
- The join gate no longer deadlocks when a save is loaded while a player is joining.
- The owned tavern's daily report on the other players' screens no longer shows every value as a loss (all red, as if the tavern had been reset): the host now sends the day's report only after it is filled in.
- A guest whose host left or crashed is no longer stuck on a black screen (seen after the host left the owned tavern and quit): the mod notices that the host's session is gone, and the game shows its normal disconnect message and returns to the title. A leaving player's own goodbye is now sent before their connection is closed.
- A runaway style pass while the game-over window is built (for example after an escort dies and you press Continue) no longer leaves every button dead on every screen: the UI recovers, the error is logged to **shim.log**, and the normal pause menu opens instead. *(Vanilla bug.)*
- A failed activity of a client (a lost fish or a broken lockpick on Extreme, or a failed activity that injures) no longer freezes fishing or the activity with a "Not allowed" error: the unit's injury is applied by the host and synced to everyone.
- A co-op client no longer runs out of memory after several loads: the game kept every loaded world in a queue that only the host ever empties, so each save load, battle restart or reload on a client added a whole world to memory. *(Vanilla bug.)*
- A battle skill request that the host cancels no longer leaves the client's battle locked (clicks, **End Turn** and damage previews dead): the client handles the cancel as a refused request.
- A client watching another player's fishing, gathering, lock picking or singing no longer gets a stuck camera, cursor or inventory: only the host builds that mini-game window, so the watching client no longer hits an error that left its controls locked.
- Watching forging, archery and work now reaches the other players: the marker values of these events were changed by the network's float precision, so they used to see an idle worker and a far-away ping.
- A guest joining or rejoining while a camp confession scene runs no longer loads forever: the joining guest now rebuilds the confession character from the synced confession instead of hitting an error, and an error while a guest's game starts is no longer ignored (it used to leave a half-loaded world on an endless loading screen).

### Co-op play

- Drop-in: a new player can join a running co-op game by code or Steam invite while the party has fewer than 4 players. A newcomer owns no units until someone gives them some (camp, unit sheet, Stats tab, **Transfer**). A player who was absent when the save was loaded gets their own units back when they join (units already given away, dismissed or dead stay as they are; after a host restart use **Transfer**).
- No more "waiting for the other players" after the world is loaded: the first player's click decides, as holding the button used to. A burst of clicks runs the action once.
- Leaving a town, the tavern, a place or the owned tavern is never blocked by another player's business: windows and confirms tied to that place are closed, and the shared dialog is ended once no choice is being resolved. Personal windows (unit sheet, inventory) stay open and do not block. Only a running mini-game still holds the leave.
- Leaving the camp for the world map is no longer silently refused while another player is busy (strategy table, camp chest, banner editor, an unstarted craft): the request stays pending, their camp window is closed the way its X / Escape does (a strategy choice already made stays, nothing half-applied), and everyone switches to the world map together. Started crafts and a modal window open on the host are waited for; a rest still refuses the leave.
- A player reading a recruit's (or any NPC's) info no longer stops the others from talking to a different NPC. The same NPC, chests, crafts and activities still wait, and so does every NPC while a dialog is open.
- Talking to a unit from the camp reserve or a party portrait waits, like a click on a unit in the camp, while another player is busy (banner editor, chest, craft): it no longer pulls everyone into the dialog and leaves that player on a black screen.
- Pings: in battle the pinged cell, or the whole footprint of the unit standing on it, blinks for three seconds on every player's screen in the pinging player's nickname colour, and a pinged unit's outline blinks too; the old ripple rings are gone. A ping now plays an audible chat sound (at most every 0.3 s). Everywhere, the ping marker now lands where the pinging player's cursor is, instead of floating or sinking into the scenery for the others (its depth was read at UI coordinates, not render pixels).
- Battle timeline: while a player unit acts, the first diamond (bottom left) shows its portrait instead of the crossed swords, with the nickname of the player who controls it above, in that player's colour. Back to the swords when nobody acts; blank during the enemy turn.
- Co-op battle status list above and left of that diamond, during your side's round: one row per player with units still alive, nickname in the player's colour; `» acting`, a green check (already took their turn this round), a red cross (still waiting). Long nicknames are cut with `...`.
- Camp: any player may drag any party unit, assign it to a camp tool or move it between the camp and the reserve, not only their own units. Battle control, items and the unit sheet keep the owner rules.
- Caravan follow on the world map, on by default: your caravan follows the player who last moved by their own input (the host when several move at once). Your own click, mouse hold or pad move pauses it; it resumes 1 s after you arrive. Press **F** to turn it off for yourself (remembered) and again to turn it back on.
- Co-op player markers (the screen-edge locator and the minimap arrows) show the player's nickname instead of P1..P4, in the player's colour. Nicknames longer than 12 characters are cut with `...`.
- ALT highlight on the world map: holding the Outlines key (ALT) outlines the chests, gather nodes, treasure, tracks and other interactive elements on screen, as inside places; release clears them. Local only. Map places and roaming parties are not covered.
- Party-wide inventory: dialog, crafting, repair and healing costs, fishing hooks and lockpicks are paid from your inventory, the chests and then the other players' inventories, and the counts on screen include the other players' items (fishing and lock picking no longer stop at 0 hooks / lockpicks while the chest or another player has some). The injury heal panel lists remedies the other players carry, and recipe ingredients in the Grimoire and item tooltips are no longer shown missing (red) when the chest or another player has them.
- Shared chest panel: sort, quick stack (move your items the chest already holds) and take similar buttons.
- The shared camp chest is available in a co-op game started from a solo save (players who joined later); before, the chest button and panel were missing on the world map until the host made camp. *(Vanilla bug.)*
- During a confession or another shared dialog the inventory panels are hidden for every player, not only for the one who started it. *(Vanilla bug.)*
- Other players' inventories: a new bottom-bar button (co-op only) (the inventory chest with a companions badge) opens one panel per other connected player at the left edge of the screen, draggable (positions kept). Full access, both ways: right click an item in a player's panel to take it (shift: choose the amount); while the panels are open, ctrl + right click an item in your own inventory to give it to the player whose panel you last hovered or clicked (ctrl + shift: choose the amount). To equip someone else's item, take it first. Dragging items onto or out of another player's panel is blocked. Every player needs this version: an older one refuses the transfer.
- Forging is visible to the other players: while a player forges, everyone else in that place sees the worker at the anvil hammer, with each hit's particles and sound (perfect / good / bad) and the success or fail gesture at the end. Their camera, UI and controls are not touched. Every player needs this version: an older one shows a far-away ping and its sound instead.
- Shared mini-games (fishing, mining, wood cutting, lock picking, singing, gambling, the ruins puzzles) no longer take over the other players' screens: only the player doing it gets the mini-game window and camera; everyone else keeps their own camera, inventory and controls and watches that player's character do it in the scene. Every player needs this version.
- Archery and the progress-bar activities (studying, money laundering, dismantling, altering, snaring, tracking, ...) are visible to the other players too: the character doing it makes a work motion for each click or shot, in a place or in the camp. Their camera, UI and controls are not touched. Every player needs this version: an older one shows a far-away ping and its sound instead.
- The bard's tavern song is visible to the other players: everyone else in the tavern sees the singer's song animation. Every player needs this version.
- **Take all** button on the post-battle loot screen, and a take-all icon on searched barrels, crates and chests and on dialog item grids.
- Item tooltips no longer swallow mouse input: a tooltip pushed over the hovered slot could make loot items unclickable and keep re-opening itself.
- Career Plan fix: a client could see "+2" on level-up and get only "+1". The host now grants the extra point on top of its own current offer, ignores a grant when the unit has no aptitude point left, and drops a stale second request.
- Lets you start even if a player has no human character assigned. For example, four players can use a modded starting party of three humans and one animal.

### Gameplay and interface

- The load list shows the kind of each save next to its name: (Autosave), (Quicksave) or (Manual), so auto, quick and manual saves of one campaign no longer look alike. The list is sorted newest first, the autosave included (before, the autosave was always on top, even when older).
- Enemy friendly fire: area attacks cast by enemies also hit their own allies (never the caster), as player area attacks already do.
- Switching a unit's profession keeps the experience earned in the old one, and applies at once without the confirm window.
- Hold-to-confirm rings fill three times faster.
- Enemies of a higher level than your squad can drop their worn equipment (weapons, armor, trinkets); before, anything above your best unit's level never dropped. Armor drops even if none of your units' classes can wear it (keep it for a later recruit, or sell it).
- Named champions and bosses can drop their worn equipment too, with the same chance as other enemies (their signature weapon still always drops). Their unique items marked as not lootable stay excluded, and arena champions keep their gear, since the arena gives it as a reward.
- The battle's guaranteed piece of enemy equipment comes from a random fallen enemy instead of whichever enemy the game listed first. Stronger enemies are more likely (the odds grow with the square of the level and are doubled for a champion or boss), so it no longer comes from the same enemy every battle. A boss's always-dropped signature weapon no longer uses up that guaranteed piece.
- The chance of extra enemy equipment beyond the guaranteed piece scales with the battle size: small fights give a bit more, large fights a bit less than before, about one extra piece per ten fallen enemies at any size (the counter that raises the chance after each miss works as before).
- The title screen shows the installed mod version ("Co-op Fix v…") on its own line above the game version, so players can compare versions before joining.
- Popup windows (unit sheet, shops, crafting, ...) and the inventory panels can be dragged by their title row. Positions are kept per window; a double click on the title row puts the window back. Dragging is local only and never clicks the world under the cursor.
- In co-op a client closes its own window (unit sheet and other windows it opened itself) by clicking outside it, as the host always could; before, that click did nothing on a client.
- Skill tooltips show the Valor point cost outside battle for every unit, including the new-game screens.
- In co-op a client's skill bar is up to date at round start (Inhalation and similar skills were greyed out until any action), and a client's skills read the current synced skill values (Inhalation's cost no longer used a stale count). *(Vanilla bug.)*
- The chest panel opens where the game puts it, instead of displaced or snapping back on hover. A resized inventory panel scrolls with its scrollbar again.
- Resized inventory panels: the scrollbar works and the rows are in order (the top row stays under the header); a resize stops above the HUD bottom bar; a double click on the title row restores the vanilla size and position; the chest panel is no longer an empty collapsed box after loading a save, and it draws above the inventory panel instead of behind it.
- Clicks only reach the slots a scrolling list shows: a slot scrolled under the chest header no longer takes the click or drag. *(Vanilla bug.)*
- Battle camera: rotating with the right mouse button while panning with the keyboard no longer speeds the camera up.
- Tooltip keyword panels ("Poison", "Vigilance", ...) wrap into columns instead of running off the screen, and are not shown twice.
- A censer that grants Purge (Remastered) shows its tooltip in chests, shops and other inventories again; before, its tooltip failed every frame, which froze the moved item between cells and dropped the FPS.
- A boss's prepared two-turn skill that its script refuses when it fires (Matthias Lund's Lucilla Vengeance in Remastered) is cancelled and the turn goes on as usual; before, the battle froze on the boss's turn.
- Starting troops larger than the new-game customize screen (it has five spots): instead of crashing ("Missing prefab Camera06"), the extra animals are left off the screen and belong to the host; humans always get a spot. Troops of five or fewer look exactly as before.

### Start-screen tooltips

- Hover tooltips on the items shown in the new-game starting-troop preview.
- Class tooltips on the unit lines of the start choices.

### Diagnostics

- Writes game errors, co-op mode-switch steps and forced leaves to **shim.log**, to help with bug reports. This only adds log lines.
- Writes the end steps of every activity and mini-game (ruins puzzles, lock picking, fishing, dice) and every refused co-op network call to **shim.log**, to find where a mini-game freezes. This only adds log lines.
- The start-up window shows how many co-op patches were applied ("Applied N of M") with a progress bar, and says so when some were skipped.
- **shim.log** is no longer flooded by network SYNC lines when the game's network log is on: they are counted instead, so later lines are not lost to the log size limit.
- Writes post-battle loot-window steps (window rebuilds, button states, hovered and clicked elements) and camp-dialog choice / confession rewards to **shim.log**. This only adds log lines.
- Writes battle requests refused while the battle is locked, and the call that took the lock, to **shim.log**, to find what leaves a battle dead. This only adds log lines.

## Install

1. Download **winmm.dll** from **Assets** in the newest release. You do not need the **Source code** archives.
2. Close Wartales.
3. In Steam, right-click Wartales → **Manage** → **Browse local files**.
4. Copy **winmm.dll** into that folder, next to **Wartales.exe**. Replace the old mod DLL when updating.
5. Launch the game through Steam as usual. For the first few seconds a small **Wartales Co-op Fix** window shows that the mod is preparing the game; it closes by itself when the game window appears. No console windows open. If the mod cannot apply its patches or its helper stops, a message box says so and names the log.

Install the mod on every player's machine. **All players need the same version**: a guest whose mod files differ from the host's cannot join.

## Play with a friend

**Through Steam:** the host creates a co-op game. Once Steam is ready, friends can select **Join Game** in the Steam friends list without waiting for an invitation. Normal invitations still work. Try this first; it usually needs no router configuration.

**By code:** the host creates a game and sends its code to the friend, who enters it in the game. Codes normally have **8 symbols**, or 11 with custom settings. This method needs a reachable direct connection to the host. If it fails, join through Steam instead.

## Update or remove

To update, close the game and replace **winmm.dll** with the new version. To uninstall, close the game and remove that file from its folder. The mod does not overwrite the original game files.

## Known issues

- Joining **by code** needs the host's port to be reachable from the internet (an open / forwarded port or UPnP on the host's router). Joining through a Steam invite or the friends list is not affected.
- Not yet confirmed in a live co-op session: the guest's exit when the host vanished, any unit in the camp, the other players' inventories, and watching forging, work and mini-games. If one of them misbehaves, send **shim.log** from every player.
- The post-battle loot fix (dead loot items / **Take all** with a damaged squad) is defensive: it removes the rebuild cause found in the game code but has not yet been confirmed in a live co-op session. If loot still does not react, send **shim.log** from every player (the loot-window diagnostics above are included).

## If it does not work

The mod is still being tested and is not guaranteed to work on every network. If the problem remains after everyone updates, [report it here](https://github.com/UberMorgott/Wartales-Mod-wartales-mp/issues). Say whether you joined through Steam or a code, and include the error message.

The logs, **wartales-mp.log** and **shim.log**, are in `%LOCALAPPDATA%\wartales-mp`. You can paste that path into File Explorer's address bar.

Antivirus software may flag the DLL. If that happens, include the antivirus name and detection name in your report.

[Technical notes and build instructions](docs/TECHNICAL.md) · [CC BY-NC 4.0 licence](LICENSE)

Project code and documentation: Copyright (c) 2026 UberMorgott, CC BY-NC 4.0. Third-party components retain their own licences, including [MinHook](shim/minhook/LICENSE.txt) and [hlbc](patcher/vendor/NOTICE.md).

This is an unofficial mod, not affiliated with Shiro Games or Valve.
