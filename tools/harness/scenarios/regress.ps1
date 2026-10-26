# Release regression cases, background only (docs/coop-harness-plan.md).
#
#   .\tools\harness\scenarios\regress.ps1 -Case chest|rats|confession
#
# chest:      run after up.ps1 (world map). The guest puts 3 of its inventory
#             slot 1 into the camp chest, the host takes them out (`put` / `take`:
#             MoveTo through the slot api's authority branch). shim.log: guest
#             `mp: invOp op=5`, host `mp: invNetAdd count=3` then `mp: invOp op=5`.
# rats:       run after up.ps1. Rat Matriarch fight (`console battle b
#             RatInvasion`), turns ended with `endturn` by the owner of the
#             current unit, tutorials closed, until <Rounds> rounds passed; the
#             host's "There are AvB units" lines show the summons. Then `console
#             win` and both debrief loot pools. Guest: no `Null access` / `threw`.
# confession: run after up.ps1. Camp, a pending confession, its talk (`talk`):
#             both in world.Confession; `console save` (save006.dat, copied to
#             fixtures\coop-save006-confession.dat), stop, up.ps1 again: the guest
#             must reach the camp (issue #9: no `Null access .elementType`).
param(
    [ValidateSet('chest', 'rats', 'confession')][string]$Case = 'chest',
    [int]$Rounds = 2
)
. (Join-Path $PSScriptRoot 'common.ps1')
function Dump([string]$i) { (& $script:c cmd -Inst $i -Line dump | Select-Object -Last 1) | ConvertFrom-Json }
function Errors {
    $run = Get-Content 'D:\WartalesTest\runs\current.txt'
    foreach ($i in 'A', 'B') {
        "--$i"
        Get-Content "$run\$i\wartales-mp\shim.log" |
            Select-String -CaseSensitive 'Null access|threw|Uncaught|harness: error' |
            Where-Object { $_.Line -notmatch 'tips: patched' } |
            Select-Object -Last 5 | ForEach-Object { $_.Line }
    }
}
switch ($Case) {
    'chest' {
        T B 'put 1 3'; Start-Sleep 3
        foreach ($i in 'A', 'B') { $j = Dump $i; "$i inv=$($j.inv)"; "$i chest=$($j.chestItems)" }
        T A 'take 0 3'; Start-Sleep 3
        foreach ($i in 'A', 'B') { $j = Dump $i; "$i inv=$($j.inv)"; "$i chest=$($j.chestItems)" }
        $run = Get-Content 'D:\WartalesTest\runs\current.txt'
        foreach ($i in 'A', 'B') {
            "--$i"; Get-Content "$run\$i\wartales-mp\shim.log" | Select-String 'mp: inv(Op|NetAdd)' | Select-Object -Last 3 | ForEach-Object { $_.Line }
        }
    }
    'rats' {
        T A 'console battle b RatInvasion'; Start-Sleep 30
        $round = 0; $last = ''
        for ($k = 0; $k -lt 120 -and $round -lt $Rounds; $k++) {
            foreach ($i in 'A', 'B') { try { & $script:c cmd -Inst $i -Line tutorial | Out-Null } catch {} }
            $j = Dump A
            if ($j.mode -ne 'battle.Battle') { "left the battle: $($j.mode)"; break }
            $cu = $j.battle.currentUnit
            if ($cu) {
                # the first player unit again: a round has passed
                if ($k -gt 0 -and $cu.name -eq $first -and $last -ne $cu.name) { $round++ }
                if (-not $first) { $first = $cu.name }
                $last = $cu.name
                $who = if ($cu.owner -like 'X1*') { 'A' } else { 'B' }
                try { & $script:c cmd -Inst $who -Line endturn | Out-Null } catch {}
            } else { $last = '' }
            Start-Sleep 5
        }
        $run = Get-Content 'D:\WartalesTest\runs\current.txt'
        Get-Content "$run\A\wartales-mp\shim.log" | Select-String 'There are \d+v\d+ units' | ForEach-Object { $_.Line }
        T A 'console win'; Start-Sleep 25
        foreach ($i in 'A', 'B') { $j = Dump $i; "$i mode=$($j.mode) loot=$($j.loot)" }
        T A 'snap rats-debrief-host'; T B 'snap rats-debrief-guest'
        Errors
    }
    'confession' {
        T A 'camp'; Start-Sleep 25
        foreach ($i in 'A', 'B') { try { & $script:c cmd -Inst $i -Line tutorial | Out-Null } catch {} }
        # the console's generation fails for many ids (cooldowns, conditions):
        # try until one is pending
        $ok = $false
        foreach ($id in 'ConfessionHate', 'ConfessionFriendlyFire', 'ConfessionRandom1', 'ConfessionLevelUp') {
            T A "console confession $id" | Out-Null; Start-Sleep 2
            try { & $script:c cmd -Inst A -Line talk | Out-Null; "talk: $id"; $ok = $true; break } catch {}
        }
        if (-not $ok) { 'no confession could be generated'; return }
        Start-Sleep 8
        foreach ($i in 'A', 'B') { $j = Dump $i; "$i mode=$($j.mode)" }
        T A 'console save'; Start-Sleep 8
        Copy-Item D:\WartalesTest\A\save\save006.dat D:\WartalesTest\fixtures\coop-save006-confession.dat -Force
        & $script:c stop | Select-Object -Last 1
        & (Join-Path $PSScriptRoot 'up.ps1') | Select-Object -Last 1
        Start-Sleep 10
        foreach ($i in 'A', 'B') { $j = Dump $i; "$i mode=$($j.mode) loading=$($j.isLoading) connected=$($j.connectedToHost)" }
        Errors
    }
}
