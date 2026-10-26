# Starts the 2-instance co-op session from the fixture save without any click
# (docs/coop-harness-plan.md): A hosts <Save>, opens a slot, B joins by code,
# A starts, waits for gameplay, then the "Waiting for players" Start (`welcome`).
# Background only: everything goes through the harness command file.
#
#   .\tools\harness\scenarios\up.ps1 [-Save save006]
param([string]$Save = 'save006')
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\..\..')
$c = '.\tools\harness\coop.ps1'
& $c start | Select-Object -Last 1
& $c cmd -Inst A -Line "host $Save" | Select-Object -Last 1
# the co-op lobby needs a few seconds after `host`
for ($k = 0; ; $k++) {
    Start-Sleep 4
    try { & $c cmd -Inst A -Line 'slot' | Select-Object -Last 1; break }
    catch { if ($k -ge 10) { throw } }
}
& $c cmd -Inst B -Line 'join auto' | Select-Object -Last 1
Start-Sleep 10
& $c cmd -Inst A -Line 'start' | Select-Object -Last 1
# isLoading stays true until the Start below; gameplay has started by then
& $c wait -Inst A -Until '$s.game -and $s.hasGameplayStarted' -TimeoutSec 180 | Select-Object -Last 1
Start-Sleep 10
& $c cmd -Inst A -Line 'welcome' | Select-Object -Last 1
Start-Sleep 5
