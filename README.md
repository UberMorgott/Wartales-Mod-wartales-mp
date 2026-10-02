# wartales-mp

A mod for **Wartales co-op through Steam on Windows**.

The mod replaces Wartales' old connection system, which prevented players in different countries from playing together. It removes the dependency on the developers' servers and adds direct connections between players.

[**Download the mod**](https://github.com/UberMorgott/Wartales-Mod-wartales-mp/releases) · [**Описание на русском**](README.ru.md)

## What it does

Everything below is in the one **winmm.dll**. The game patches are applied in memory when the game starts; the game files on disk are not changed. Each patch first checks that the game code looks as expected: on a different game build that patch silently stays off and everything else keeps working.

### Network and lobby

- Disables the old Steam networking protocol that caused connection problems in many countries. It uses modern Steam networking (SDR) instead.
- Removes the dependency on the developers' servers for creating and finding lobbies. Adds a direct connection to the host without intermediary servers when the host's network allows it.
- Changes how the game's existing join codes work: friends now connect directly to the host, bypassing the developers' servers.
- Keeps familiar Steam invitations. A Steam lobby is created automatically with the in-game room.
- Fixes identified reconnection errors and a crash caused by waiting in the lobby.
- Mod-file check on join: a player whose mod files (winmm.dll, and res1.pak if any) differ from the host's is refused at the join with a clear message instead of crashing later in the game.

### Co-op stability

- The mode-switch wait (leaving or entering a town, the tavern or a place, starting a battle) no longer leaves every screen black forever: the host drops a player who has not answered after 30 seconds and reloads them into the running game; a player who joins during the switch is let in after it.
- An error in the game's handling of a network message is logged instead of ending the whole co-op session.
- The in-game DLC check never marks a player who owns a DLC as missing it (which locked DLC content for the whole party); a real mismatch is written to the log as a warning.
- Fixes the post-battle screen rebuilding itself every frame in co-op when the remedy count and the injured units disagreed.

### Co-op play

- No more "waiting for the other players" after the world is loaded: the first player's click decides, as holding the button used to. A burst of clicks runs the action once.
- Leaving a town, the tavern, a place or the owned tavern is never blocked by another player's business: their inspect windows and unstarted crafts are closed, and the shared dialog is ended. Started crafts and activities are waited for.
- Caravan follow on the world map: press **F** to make your caravan follow the player who last moved. It also follows on its own while you have a window open. Your own click cancels it.
- Party-wide inventory: dialog, crafting, repair and healing costs are paid from your inventory, the chests and then the other players' inventories, and the counts on screen include the other players' items. The injury heal panel lists remedies the other players carry.
- Shared chest panel: sort, quick stack (move your items the chest already holds) and take similar buttons.
- **Take all** button on the post-battle loot screen, and a take-all icon on searched barrels, crates and chests and on dialog item grids.
- Career Plan fix: a client could see "+2" on level-up and get only "+1". The host now grants the extra point on top of its own current offer, ignores a grant when the unit has no aptitude point left, and drops a stale second request.
- Lets you start even if a player has no human character assigned. For example, four players can use a modded starting party of three humans and one animal.

### Gameplay and interface

- Enemy friendly fire: area attacks cast by enemies also hit their own allies (never the caster), as player area attacks already do.
- Switching a unit's profession keeps the experience earned in the old one, and applies at once without the confirm window.
- Hold-to-confirm rings fill three times faster.
- Skill tooltips show the Valor point cost outside battle for every unit, including the new-game screens.
- Tooltip keyword panels ("Poison", "Vigilance", ...) wrap into columns instead of running off the screen, and are not shown twice.
- Starting troops larger than the new-game customize screen (it has five spots): instead of crashing ("Missing prefab Camera06"), the extra animals are left off the screen and belong to the host; humans always get a spot. Troops of five or fewer look exactly as before.

### Start-screen tooltips

- Hover tooltips on the items shown in the new-game starting-troop preview.
- Class tooltips on the unit lines of the start choices.

### Diagnostics

- Writes game errors, co-op mode-switch steps, forced leaves and DLC warnings to **shim.log**, to help with bug reports. This only adds log lines.
## Install

1. Download **winmm.dll** from **Assets** in the newest release. You do not need the **Source code** archives.
2. Close Wartales.
3. In Steam, right-click Wartales → **Manage** → **Browse local files**.
4. Copy **winmm.dll** into that folder, next to **Wartales.exe**. Replace the old mod DLL when updating.
5. Launch the game through Steam as usual.

Install the mod on every player's machine. **All players need the same version**: a guest whose mod files differ from the host's cannot join.

## Play with a friend

**Through Steam:** the host creates a co-op game. Once Steam is ready, friends can select **Join Game** in the Steam friends list without waiting for an invitation. Normal invitations still work. Try this first; it usually needs no router configuration.

**By code:** the host creates a game and sends its code to the friend, who enters it in the game. Codes normally have **8 symbols**, or 11 with custom settings. This method needs a reachable direct connection to the host. If it fails, join through Steam instead.

## Update or remove

To update, close the game and replace **winmm.dll** with the new version. To uninstall, close the game and remove that file from its folder. The mod does not overwrite the original game files.

## If it does not work

The mod is still being tested and is not guaranteed to work on every network. If the problem remains after everyone updates, [report it here](https://github.com/UberMorgott/Wartales-Mod-wartales-mp/issues). Say whether you joined through Steam or a code, and include the error message.

The logs, **wartales-mp.log** and **shim.log**, are in `%LOCALAPPDATA%\wartales-mp`. You can paste that path into File Explorer's address bar.

Antivirus software may flag the DLL. If that happens, include the antivirus name and detection name in your report.

[Technical notes and build instructions](docs/TECHNICAL.md) · [CC BY-NC 4.0 licence](LICENSE)

Project code and documentation: Copyright (c) 2026 UberMorgott, CC BY-NC 4.0. Third-party components retain their own licences, including [MinHook](shim/minhook/LICENSE.txt) and [hlbc](patcher/vendor/NOTICE.md).

This is an unofficial mod, not affiliated with Shiro Games or Valve.
