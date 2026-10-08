# Builds the local co-op harness game copies (docs/coop-harness-plan.md):
# D:\WartalesTest\A and \B, each a tree of HARDLINKS to the real game folder
# (same volume, no extra disk) plus its own copies of everything the game or
# the mod writes: save\, prefs.sav, console.json, cache\ and the TEST winmm.dll.
# The real game folder is only read; its winmm.dll hash is printed before and
# after so a run can prove it.
#
# Never hardlink a file the game writes in place: a write through the link
# would change the real game's file. The copy list below is everything the game
# folder shows modified after install (prefs.sav, save\*, cache\*, console.json).
#
#   .\tools\harness\setup.ps1 [-Game <dir>] [-Root D:\WartalesTest] [-Dll <dist-test\winmm.dll>] [-Saves] [-Force]
#
# -Saves copies the real save\ into each copy (a game to host from); default is
# an empty save\. -Force rebuilds existing copies (their saves are lost).

[CmdletBinding()]
param(
    [string]$Game = 'D:\Steam\steamapps\common\Wartales',
    [string]$Root = 'D:\WartalesTest',
    [string]$Dll = (Join-Path $PSScriptRoot '..\..\dist-test\winmm.dll'),
    [string[]]$Instances = @('A', 'B'),
    [switch]$Saves,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$Game = (Resolve-Path -LiteralPath $Game).Path
$Dll = (Resolve-Path -LiteralPath $Dll).Path
if ((Split-Path -Qualifier $Game) -ne (Split-Path -Qualifier $Root)) { throw "hardlinks need $Root on the same volume as $Game" }
if ($Root.TrimEnd('\') -ieq $Game.TrimEnd('\') -or $Root.StartsWith($Game, [StringComparison]::OrdinalIgnoreCase)) { throw "refusing: $Root is inside the real game folder" }
& (Join-Path $PSScriptRoot '..\..\shim\seamcheck.ps1') -Path $Dll -Expect present

$realDll = Join-Path $Game 'winmm.dll'
$before = if (Test-Path -LiteralPath $realDll) { (Get-FileHash -LiteralPath $realDll).Hash } else { 'absent' }
Write-Host "real game winmm.dll SHA256 before: $before"

# Copied, never linked (relative to the game folder); skipped entirely.
$copy = @('prefs.sav', 'console.json')
$copyDirs = @('cache')
$skipTop = @('save', 'winmm.dll', 'Wartales.zip')
$skipLike = @('_backup-*')

foreach ($inst in $Instances) {
    $dst = Join-Path $Root $inst
    if (Test-Path -LiteralPath $dst) {
        if (-not $Force) {
            Copy-Item -LiteralPath $Dll -Destination (Join-Path $dst 'winmm.dll') -Force
            Write-Host "$dst exists: test winmm.dll refreshed (-Force rebuilds the copy)"
            continue
        }
        # Only ever delete our own copy: a hardlink tree never owns the data.
        if (-not (Test-Path -LiteralPath (Join-Path $dst '.wartales-harness'))) { throw "$dst is not a harness copy (no .wartales-harness marker); not deleting" }
        Remove-Item -LiteralPath $dst -Recurse -Force
    }
    New-Item -ItemType Directory -Force $dst | Out-Null
    Set-Content -LiteralPath (Join-Path $dst '.wartales-harness') -Value "harness copy of $Game" -Encoding ascii
    $links = 0; $copies = 0
    Get-ChildItem -LiteralPath $Game -Force | ForEach-Object {
        $name = $_.Name
        if ($skipTop -contains $name) { return }
        foreach ($p in $skipLike) { if ($name -like $p) { return } }
        if ($_.PSIsContainer) {
            if ($copyDirs -contains $name) {
                Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $dst $name) -Recurse -Force
                $script:copies++
                return
            }
            Get-ChildItem -LiteralPath $_.FullName -Recurse -Force | ForEach-Object {
                $rel = $_.FullName.Substring($Game.Length + 1)
                $t = Join-Path $dst $rel
                if ($_.PSIsContainer) { New-Item -ItemType Directory -Force $t | Out-Null }
                else {
                    New-Item -ItemType Directory -Force (Split-Path $t) | Out-Null
                    New-Item -ItemType HardLink -Path $t -Target $_.FullName | Out-Null
                    $script:links++
                }
            }
            return
        }
        $t = Join-Path $dst $name
        if ($copy -contains $name) { Copy-Item -LiteralPath $_.FullName -Destination $t; $script:copies++ }
        else { New-Item -ItemType HardLink -Path $t -Target $_.FullName | Out-Null; $script:links++ }
    }
    New-Item -ItemType Directory -Force (Join-Path $dst 'save') | Out-Null
    if ($Saves) {
        Get-ChildItem -LiteralPath (Join-Path $Game 'save') -File | Where-Object { $_.Extension -in '.dat', '.sav' } |
            Copy-Item -Destination (Join-Path $dst 'save')
    }
    Copy-Item -LiteralPath $Dll -Destination (Join-Path $dst 'winmm.dll')
    Write-Host ("{0}: {1} hardlinks, {2} copies, test winmm.dll, save\ {3}" -f $dst, $script:links, $script:copies, $(if ($Saves) { 'copied' } else { 'empty' }))
}

$after = if (Test-Path -LiteralPath $realDll) { (Get-FileHash -LiteralPath $realDll).Hash } else { 'absent' }
Write-Host "real game winmm.dll SHA256 after:  $after"
if ($before -ne $after) { throw 'the real game winmm.dll changed' }
