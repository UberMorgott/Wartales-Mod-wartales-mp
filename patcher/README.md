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
