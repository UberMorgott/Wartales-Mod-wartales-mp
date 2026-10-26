# Shared mini-game spectating, both players in the scene, background only.
# Each case starts the mini-game the vanilla way (Controller.startActivity on the
# host, the unit picked by `choose`), then snaps both games (Game.screenshot into
# <run>\<A|B>\wartales-mp\harness\<name>.png).
#
#   .\tools\harness\scenarios\spectate.ps1 -Case lockpick|woodcut|ruins|forge|sing
# Run after scenarios\up.ps1. shim.log: `mp: spectate <id> 1` (watching from its
# own camera) or `0` (camera activity: LockPick, NinePuzzle, CylinderPuzzle, Sing).
param([ValidateSet('lockpick', 'woodcut', 'ruins', 'forge', 'sing')][string]$Case = 'lockpick')
. (Join-Path $PSScriptRoot 'common.ps1')
switch ($Case) {
    'lockpick' {
        # TowerBad2's GearCrate is a locked chest (the debug `activity` cheat's spot)
        Go-Place 'TowerBad2'
        T A 'console item LockPick 5'; T B 'console item LockPick 5'
        # the host picks, the guest watches
        T A 'job Thief'
        T A 'play LockPick GearCrate'; Start-Sleep 4
        T A 'choose'; Start-Sleep 6
        T A 'tutorial'; Start-Sleep 3
        T A 'snap lp-host'; T B 'snap lp-guest'
        T A 'lpstop'; Start-Sleep 6
        T A 'snap lp-end-host'; T B 'snap lp-end-guest'
    }
    'woodcut' {
        # a GatherAction: the spectator keeps its own camera (spectate ... 1)
        Go-Place 'PoorHamlet'
        T A 'job Woodcutter'
        T B 'snap wc-before-guest'
        T A 'play WoodCutting WoodCutting'; Start-Sleep 4
        T A 'choose'; Start-Sleep 6
        T A 'tutorial'; Start-Sleep 4
        T A 'snap wc-host'; T B 'snap wc-guest'
    }
    'ruins' {
        # the tile-sliding puzzle; the ruins need torches
        T A 'console item Torch 5'; T B 'console item Torch 5'
        Go-Place 'E1Ruins' 25
        T A 'job Scholar'
        T A 'play NinePuzzle NinePuzzle'; Start-Sleep 4
        T A 'choose'; Start-Sleep 7
        T A 'tutorial'; Start-Sleep 3
        T A 'snap np-host'; T B 'snap np-guest'
    }
    'sing' {
        # the bard's song in Cortia's tavern (T2Tavern): a camera activity, the
        # guest sees the stage; at the end its stage is hidden again (spectEnd)
        # load the region first (T2Forge, same town): a long teleport + enter can
        # outlast the host's 30 s mode-switch barrier on the guest
        Go-Place 'T2Forge'
        Go-Place 'T2Tavern'
        T A 'job Bard'
        T B 'snap sg-before-guest'
        T A 'play Sing SingSpot'; Start-Sleep 4
        T A 'choose'; Start-Sleep 6
        T A 'tutorial'; Start-Sleep 3
        T A 'song'; Start-Sleep 8
        T A 'snap sg-host'; T B 'snap sg-guest'
        Start-Sleep 60
        T B 'snap sg-end-guest'
    }
    'forge' {
        # Cortia's forge (T2Forge). Not a coop activity: forge_mirror shows the
        # worker's hits, the sparks and the metal shards on the anvil.
        Go-Place 'T2Forge'
        foreach ($it in 'IronOre 5', 'Wood 5', 'Leather 3') { T A "console item $it" }
        T A 'job Blacksmith'
        T B 'snap fg-before-guest'
        T A 'play Forge Anvil'; Start-Sleep 4
        T A 'choose'; Start-Sleep 5
        T A 'craft'; Start-Sleep 2
        T A 'confirm'; Start-Sleep 6
        T A 'tutorial'; Start-Sleep 3
        T A 'snap fg-host'; T B 'snap fg-start-guest'
        foreach ($k in 0..3) { T A "forgehit $k"; Start-Sleep -Milliseconds 400; T B "snap fg-hit$k-guest"; Start-Sleep 2 }
        Start-Sleep 8
        T B 'snap fg-end-guest'
    }
}
Show-Logs
