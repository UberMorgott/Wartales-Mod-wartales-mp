# Builds the two drop-in shims and stages everything install.bat copies.
#
# The export lists are not written by hand: tools/gendef reads the export table
# of the real game binaries and emits a .def in which every name forwards to the
# renamed original, minus the few names the shims implement themselves.
#
#   .\shim\build.ps1 [-GameDir <path>] [-OutDir <path>]
#
# Nothing under -GameDir is modified; the originals are copied out first.

[CmdletBinding()]
param(
    [string]$GameDir = 'D:\Steam\steamapps\common\Wartales',
    [string]$OutDir  = (Join-Path $PSScriptRoot '..\dist')
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

function Assert-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $cmd) { throw "$name not found in PATH" }
    return $cmd.Source
}

$gcc = Assert-Tool gcc
$null = Assert-Tool go

New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$work = Join-Path $OutDir 'obj'
New-Item -ItemType Directory -Force $work | Out-Null

# Read-only snapshot of the originals. The game folder is never written to.
# Name is how the game loads the module, Ship is the file install.bat copies
# into place under that name. They differ so that staging the shims next to the
# game can never clobber an original.
$sources = @(
    @{ Name = 'libhl.dll'; Ship = 'libhl.mp.dll'; Orig = 'libhl_o.dll'; Src = 'libhl\libhl_shim.c'; Skip = 'hl_host_resolve,hlp_host_resolve' }
    @{ Name = 'ssl.hdll';  Ship = 'ssl.mp.hdll';  Orig = 'ssl_o.hdll';  Src = 'ssl\ssl_shim.c';     Skip = 'ssl_conf_set_ca,hlp_conf_set_ca' }
)

foreach ($s in $sources) {
    $game = Join-Path $GameDir $s.Name
    if (-not (Test-Path -LiteralPath $game)) { throw "missing $game" }
    $snapshot = Join-Path $work $s.Name
    Copy-Item -LiteralPath $game -Destination $snapshot -Force

    $target = [IO.Path]::GetFileNameWithoutExtension($s.Orig)
    $def = Join-Path $work ([IO.Path]::GetFileNameWithoutExtension($s.Name) + '.def')

    Push-Location $repo
    try {
        & go run ./tools/gendef -in $snapshot -target $target -library $s.Name -skip $s.Skip -out $def
        if ($LASTEXITCODE -ne 0) { throw "gendef failed for $($s.Name)" }
    } finally { Pop-Location }

    $forwards = (Select-String -Path $def -Pattern '=' -SimpleMatch).Count
    Write-Host ("{0}: {1} forwarded exports, {2} overridden" -f $s.Name, $forwards, ($s.Skip -split ',').Count)

    # Every export the shim itself defines must be missing from the original's
    # call-through set only by name, never by accident: fail loudly if a name we
    # override is not actually exported by the original.
    foreach ($n in ($s.Skip -split ',')) {
        if (-not (Select-String -Path $def -Pattern ("^\s+" + [regex]::Escape($n) + "\s") -Quiet)) {
            throw "$($s.Name): $n is not exported by the original"
        }
    }

    $out = Join-Path $OutDir $s.Ship
    & $gcc -shared -O2 -o $out (Join-Path $PSScriptRoot $s.Src) $def `
        -Wall -Wextra -static-libgcc -s -lkernel32 -luser32
    if ($LASTEXITCODE -ne 0) { throw "gcc failed for $($s.Name)" }
    Write-Host "built $out"
}

Push-Location $repo
try {
    & go build -trimpath -ldflags '-s -w -H=windowsgui' -o (Join-Path $OutDir 'wartales-mp.exe') ./cmd/wartales-mp
    if ($LASTEXITCODE -ne 0) { throw 'go build failed' }
} finally { Pop-Location }

Copy-Item -LiteralPath (Join-Path $repo 'install.bat')   -Destination $OutDir -Force
Copy-Item -LiteralPath (Join-Path $repo 'uninstall.bat') -Destination $OutDir -Force
Write-Host "staged in $OutDir"
