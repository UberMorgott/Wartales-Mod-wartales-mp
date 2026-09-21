# wartales-mp

A mod for **Wartales co-op through Steam on Windows**.

The mod replaces Wartales' old connection system, which prevented players in different countries from playing together. It removes the dependency on the developers' servers and adds direct connections between players.

[**Download the mod**](https://github.com/UberMorgott/Wartales-Mod-wartales-mp/releases) · [**Описание на русском**](README.ru.md)

## What it does

- Disables the old Steam networking protocol that caused connection problems in many countries. It uses modern Steam networking (SDR) instead.
- Removes the dependency on the developers' servers for creating and finding lobbies. Adds a direct connection to the host without intermediary servers when the host's network allows it.
- Changes how the game's existing join codes work: friends now connect directly to the host, bypassing the developers' servers.
- Keeps familiar Steam invitations. A Steam lobby is created automatically with the in-game room.
- Fixes identified reconnection errors and a crash caused by waiting in the lobby.
- Lets you start even if a player has no human character assigned. For example, four players can use a modded starting party of three humans and one animal.
- Adds hover tooltips to the items shown in the new-game starting-troop preview (bundled `wartales-tips` patch). If the game build differs, this part silently stays off and everything else keeps working.

## Install

1. Download **winmm.dll** from **Assets** in the newest release. You do not need the **Source code** archives.
2. Close Wartales.
3. In Steam, right-click Wartales → **Manage** → **Browse local files**.
4. Copy **winmm.dll** into that folder, next to **Wartales.exe**. Replace the old mod DLL when updating.
5. Launch the game through Steam as usual.

Install the mod on every player's machine. Use **v0.1.5 or newer** for join-code format compatibility.

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

Project code and documentation: Copyright (c) 2026 UberMorgott, CC BY-NC 4.0. Third-party components retain their own licences, including [MinHook](shim/minhook/LICENSE.txt).

This is an unofficial mod, not affiliated with Shiro Games or Valve.
