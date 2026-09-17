# Proves the built winmm.dll patches the bytecode, without launching the game.
#
# Compiles shim\check\shimcheck.c, loads <OutDir>\winmm.dll into it the way the
# Windows loader would, and opens the retail hlboot.dat through the CRT and
# through CreateFileW/ReadFile. The game folder is only read; LOCALAPPDATA is
# redirected to <OutDir>\check-localappdata for the duration of the check.
#
#   .\shim\check.ps1 [-OutDir <path>] [-Hlboot <path to hlboot.dat>]

[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path $PSScriptRoot '..\dist'),
    [string]$Hlboot = 'D:\Steam\steamapps\common\Wartales\hlboot.dat'
)

$ErrorActionPreference = 'Stop'
$gcc = (Get-Command gcc -ErrorAction SilentlyContinue)?.Source
if (-not $gcc) { throw 'gcc not found in PATH' }
$OutDir = (Resolve-Path $OutDir).Path
$dll = Join-Path $OutDir 'winmm.dll'
if (-not (Test-Path -LiteralPath $dll)) { throw "missing $dll (run shim\build.ps1 first)" }
if (-not (Test-Path -LiteralPath $Hlboot)) { throw "missing $Hlboot" }

$work = Join-Path $OutDir 'obj'
New-Item -ItemType Directory -Force $work | Out-Null
$exe = Join-Path $work 'shimcheck.exe'
& $gcc -O2 -o $exe (Join-Path $PSScriptRoot 'check\shimcheck.c') -Wall -Wextra -static-libgcc
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for shimcheck.exe' }

$scratch = Join-Path $OutDir 'check-localappdata'
& $exe $dll $Hlboot $scratch
if ($LASTEXITCODE -ne 0) { throw "shimcheck failed (exit $LASTEXITCODE)" }
