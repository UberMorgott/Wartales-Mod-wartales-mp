# Builds the single drop-in proxy DLL (winmm.dll) that wartales-mp ships.
#
# The game folder is never read from or written to here: the proxy stands in
# for C:\Windows\System32\winmm.dll, so the export list is generated from the
# real System32 copy by tools/gendef (per-export C thunks + an aliasing .def --
# see the tool for why forwarder strings cannot be used for a same-named proxy).
#
# Build order (the exe is embedded into the DLL, so it must exist first):
#   1. gendef reads System32\winmm.dll  -> winmm_stubs.c + winmm.def
#      hlpatchgen (internal/hlpatch)    -> proxy\hlpatch.h
#   2. go build                          -> wartales-mp.exe   (the final helper)
#   3. objcopy wraps that exe            -> embed.o           (a linkable blob)
#   4. gcc links proxy.c + stubs + MinHook + embed.o + .def -> winmm.dll
# The single file to drop into the game folder is <OutDir>\winmm.dll.
#
#   .\shim\build.ps1 [-OutDir <path>] [-SystemDll <path to real winmm.dll>]

[CmdletBinding()]
param(
    [string]$OutDir    = (Join-Path $PSScriptRoot '..\dist'),
    [string]$SystemDll = (Join-Path $env:WINDIR 'System32\winmm.dll')
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$minhook = Join-Path $PSScriptRoot 'minhook'

function Assert-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $cmd) { throw "$name not found in PATH" }
    return $cmd.Source
}

$gcc = Assert-Tool gcc
$objcopy = Assert-Tool objcopy
$null = Assert-Tool go

if (-not (Test-Path -LiteralPath $SystemDll)) { throw "missing $SystemDll" }

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$work = Join-Path $OutDir 'obj'
New-Item -ItemType Directory -Force $work | Out-Null

# 1. Generate the forwarder thunks and the aliasing .def from the real winmm.
$stubs = Join-Path $work 'winmm_stubs.c'
$def   = Join-Path $work 'winmm.def'
Push-Location $repo
try {
    & go run ./tools/gendef -in $SystemDll -library winmm.dll -stubs $stubs -out $def
    if ($LASTEXITCODE -ne 0) { throw 'gendef failed' }
} finally { Pop-Location }
$forwards = (Select-String -Path $def -Pattern '=' -SimpleMatch).Count
Write-Host ("winmm.dll: {0} forwarded exports" -f $forwards)

# 1b. Regenerate the HashLink bytecode patch table from internal/hlpatch, so the
#     committed header can never drift from the tested Go table.
Push-Location $repo
try {
    & go run ./tools/hlpatchgen -out (Join-Path $PSScriptRoot 'proxy\hlpatch.h')
    if ($LASTEXITCODE -ne 0) { throw 'hlpatchgen failed' }
} finally { Pop-Location }

# 2. Build the helper exe first: it is embedded into the DLL below.
$exe = Join-Path $OutDir 'wartales-mp.exe'
Push-Location $repo
try {
    & go build -trimpath -ldflags '-s -w -H=windowsgui' -o $exe ./cmd/wartales-mp
    if ($LASTEXITCODE -ne 0) { throw 'go build failed' }
} finally { Pop-Location }

# 3. Wrap the final exe as a linkable object. objcopy derives the symbol names
#    (_binary_wartales_mp_exe_start/_end) from the input file name, so run it
#    from the exe's directory with the bare name.
$embed = Join-Path $work 'embed.o'
Push-Location $OutDir
try {
    & $objcopy -I binary -O pe-x86-64 -B i386:x86-64 'wartales-mp.exe' $embed
    if ($LASTEXITCODE -ne 0) { throw 'objcopy failed' }
} finally { Pop-Location }

# 4. Link the proxy DLL: our code + generated thunks + MinHook + embedded exe.
$mhsrc = @(
    (Join-Path $minhook 'src\hook.c')
    (Join-Path $minhook 'src\buffer.c')
    (Join-Path $minhook 'src\trampoline.c')
    (Join-Path $minhook 'src\hde\hde64.c')
)
$out = Join-Path $OutDir 'winmm.dll'
& $gcc -shared -O2 -o $out `
    (Join-Path $PSScriptRoot 'proxy\proxy.c') (Join-Path $PSScriptRoot 'proxy\sdr.c') (Join-Path $PSScriptRoot 'proxy\bridge.c') `
    $stubs $mhsrc $embed $def `
    "-I$(Join-Path $minhook 'include')" -DNDEBUG `
    -Wall -Wextra -static-libgcc -s -lkernel32 -lws2_32
if ($LASTEXITCODE -ne 0) { throw 'gcc failed for winmm.dll' }
Write-Host "built $out"
Write-Host "drop this one file into the game folder: $out"
