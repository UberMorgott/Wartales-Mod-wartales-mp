# Proves the built winmm.dll patches the bytecode and carries the SDR
# transport, without launching the game or a Steam client.
#
# Compiles shim\check\shimcheck.c plus three stand-ins (libhl.dll with
# hl_copy_bytes, steam.hdll with the legacy P2P natives that must never run,
# steam_api64.dll as an in-process loopback ISteamNetworkingMessages), loads
# <OutDir>\winmm.dll into the check process the way the Windows loader would,
# and runs it twice: "sdr" (the loopback steam_api64.dll is present: packet
# semantics are asserted end to end) and "nosdr" (no steam_api64.dll: the
# shim must fail closed and say why). The game folder is only read;
# LOCALAPPDATA is redirected to <OutDir>\check-localappdata for the duration.
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
$fakes = Join-Path $OutDir 'check-steam'
New-Item -ItemType Directory -Force $fakes | Out-Null
$src = Join-Path $PSScriptRoot 'check'

$exe = Join-Path $work 'shimcheck.exe'
& $gcc -O2 -o $exe (Join-Path $src 'shimcheck.c') -Wall -Wextra -static-libgcc -lws2_32
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for shimcheck.exe' }

# The stand-ins. steam.hdll is built without optimisation so its six identical
# looking bodies stay distinct functions for MinHook to hook one by one.
& $gcc -shared -O2 -o (Join-Path $fakes 'libhl.dll') (Join-Path $src 'fake_libhl.c') -Wall -Wextra -static-libgcc
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for fake libhl.dll' }
& $gcc -shared -O0 -o (Join-Path $fakes 'steam.hdll') (Join-Path $src 'fake_steam_hdll.c') -Wall -Wextra -static-libgcc
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for fake steam.hdll' }
& $gcc -shared -O2 -o (Join-Path $fakes 'steam_api64.dll') (Join-Path $src 'fake_steam_api64.c') -Wall -Wextra -static-libgcc
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for fake steam_api64.dll' }

$scratch = Join-Path $OutDir 'check-localappdata'
foreach ($mode in 'sdr', 'nosdr') {
    Write-Host "==== shimcheck $mode ===="
    & $exe $dll $Hlboot $scratch $fakes $mode
    if ($LASTEXITCODE -ne 0) { throw "shimcheck $mode failed (exit $LASTEXITCODE)" }
}
