# Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
# Installs or removes the wartales-tips bytecode patch. Works on Windows PowerShell 5 and pwsh 7.
param(
    [string]$GameDir,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$patcher = Join-Path $here 'wartales-tips.exe'

function Find-GameDir {
    $candidates = @()
    try {
        $steam = (Get-ItemProperty 'HKCU:\Software\Valve\Steam' -ErrorAction Stop).SteamPath
        $candidates += Join-Path $steam 'steamapps\common\Wartales'
        $vdf = Join-Path $steam 'steamapps\libraryfolders.vdf'
        if (Test-Path $vdf) {
            foreach ($m in [regex]::Matches((Get-Content $vdf -Raw), '"path"\s+"([^"]+)"')) {
                $candidates += Join-Path ($m.Groups[1].Value -replace '\\\\', '\') 'steamapps\common\Wartales'
            }
        }
    } catch {}
    foreach ($c in $candidates) { if (Test-Path (Join-Path $c 'Wartales.exe')) { return $c } }
    return $null
}

if (-not $GameDir) { $GameDir = Find-GameDir }
if (-not $GameDir -or -not (Test-Path (Join-Path $GameDir 'Wartales.exe'))) {
    Write-Host 'Wartales folder not found. Run: install.ps1 -GameDir "<path to folder with Wartales.exe>"'
    exit 1
}
if (Get-Process Wartales -ErrorAction SilentlyContinue) {
    Write-Host 'Wartales is running. Close the game first.'
    exit 1
}
Write-Host "Game folder: $GameDir"

$files = @('hlboot.dat', 'sdlboot.dat')
if ($Uninstall) {
    foreach ($n in $files) {
        $orig = Join-Path $GameDir "$n.orig"
        if (Test-Path $orig) {
            Move-Item -Force $orig (Join-Path $GameDir $n)
            Write-Host "restored $n"
        } else {
            Write-Host "no backup for $n (nothing to restore)"
        }
    }
    Write-Host 'Done. Vanilla bytecode restored.'
    exit 0
}

if (-not (Test-Path $patcher)) { Write-Host "missing $patcher"; exit 1 }
foreach ($n in $files) {
    $target = Join-Path $GameDir $n
    if (-not (Test-Path $target)) { Write-Host "skip $n (not present)"; continue }
    $orig = Join-Path $GameDir "$n.orig"
    # Always patch from the pristine backup so re-running is idempotent and a game update replaces the backup.
    if (-not (Test-Path $orig)) {
        Copy-Item $target $orig
    } elseif ((Get-Item $target).Length -ne (Get-Item $orig).Length -and -not (Select-String -Path $target -Pattern 'wartales-tips' -Quiet -ErrorAction SilentlyContinue)) {
        # Target differs from the backup and is not ours: the game was updated, refresh the backup.
        Copy-Item $target $orig -Force
    }
    $tmp = "$target.tmp"
    & $patcher patch $orig $tmp
    if ($LASTEXITCODE -ne 0) { Write-Host "patch failed for $n (game version not supported?)"; Remove-Item $tmp -ErrorAction SilentlyContinue; exit 1 }
    Move-Item -Force $tmp $target
    Write-Host "patched $n (backup: $n.orig)"
}
Write-Host 'Done. Start the game; hover the items in the starting-troop preview.'
