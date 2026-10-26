# Shared helpers of the harness scenarios (dot-source it).
Set-Location (Join-Path $PSScriptRoot '..\..\..')
$script:c = '.\tools\harness\coop.ps1'
# One harness command; a failed ack is printed, not thrown (a scenario goes on).
function T([string]$i, [string]$l) {
    try { & $script:c cmd -Inst $i -Line $l | Select-Object -Last 1 }
    catch { "$i '$l' -> $($_.Exception.Message)" }
}
# Leave the current place (Place console `quit`), then teleport both players to
# <place> on the world map (World console `goto`) and enter it with the harness
# `enter` verb (GameState.getPlace with cities: a town's sub-place keeps its town;
# the console `enter` creates a missing one without it, in region Worldwide).
function Go-Place([string]$place, [int]$wait = 22) {
    T A 'console quit'; Start-Sleep 15
    T A "console goto $place"; T B "console goto $place"; Start-Sleep 5
    T A "enter $place"; Start-Sleep $wait
    # both games in the place view, no fade: a mini-game started while the
    # guest is still switching mode makes the host's barrier ask it to rejoin.
    # Not only PlaceView: ruins run in world.Ruins, so match the dump's place id.
    $until = '$s.place.id -eq "' + $place + '" -and $s.mode -ne "world.World" -and -not $s.fading -and -not $s.isLoading'
    foreach ($i in 'A', 'B') {
        try { & $script:c wait -Inst $i -Until $until -TimeoutSec 90 | Select-Object -Last 1 }
        catch { "$i not in the place: $($_.Exception.Message)" }
    }
}
# Both players' last `mp:` activity / spectate lines and harness errors.
function Show-Logs {
    $run = Get-Content 'D:\WartalesTest\runs\current.txt'
    foreach ($i in 'A', 'B') {
        "--$i"
        Get-Content "$run\$i\wartales-mp\shim.log" |
            Select-String 'mp: (activity|spectate|forge|work)|harness: error' |
            Select-Object -Last 8 | ForEach-Object { $_.Line }
    }
    "snaps: $run\<A|B>\wartales-mp\harness\*.png"
}
