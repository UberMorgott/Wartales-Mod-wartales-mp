# Builds the one file wartales-mp ships: winmm.dll (Rust crate winmm\, msvc).
#
#   .\tools\build.ps1 [-Test]
#
# Release: dist\winmm.dll, stripped, no packer, and proven free of the harness
# seam. -Test: the local co-op harness build (cargo feature harness) in
# dist-test\winmm.dll, for D:\WartalesTest only (tools\harness\setup.ps1);
# never ship it.

[CmdletBinding()]
param([switch]$Test)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$crate = Join-Path $repo 'winmm'
$out = Join-Path $repo $(if ($Test) { 'dist-test' } else { 'dist' })

# Shown by the start-up splash and in wartales-mp.log (modver build stamp).
$env:WMP_VERSION = (Get-Content -LiteralPath (Join-Path $repo 'VERSION') -Raw).Trim()
if ($env:WMP_VERSION -notmatch '^[0-9A-Za-z.+-]+$') { throw "bad VERSION '$env:WMP_VERSION'" }
$env:WARTALES_MP_BUILD = (git -C $repo rev-parse --short=12 HEAD).Trim()

$cargo = @('build', '--release', '--manifest-path', (Join-Path $crate 'Cargo.toml'))
if ($Test) { $cargo += @('--features', 'harness', '--target-dir', (Join-Path $crate 'target-test')) }
& cargo @cargo
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
$built = Join-Path $crate ('{0}\release\winmm.dll' -f $(if ($Test) { 'target-test' } else { 'target' }))

# Every part of the harness seam reads WARTALES_MP_TEST_INSTANCE or carries
# WMP_TEST_SEAM; a release file must contain neither (ASCII or UTF-16).
$latin = [Text.Encoding]::GetEncoding(28591)
$text = $latin.GetString([IO.File]::ReadAllBytes($built))
$hit = foreach ($m in 'WMP_TEST_SEAM', 'WARTALES_MP_TEST_INSTANCE') {
    if ($text.Contains($m) -or $text.Contains(($m.ToCharArray() -join "`0"))) { $m }
}
if ($Test -and -not $hit) { throw "test build carries no harness seam: $built" }
if (-not $Test -and $hit) { throw "release build carries the harness seam ($($hit -join ', ')): $built" }

New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item -LiteralPath $built -Destination (Join-Path $out 'winmm.dll') -Force
$dll = Get-Item (Join-Path $out 'winmm.dll')
$sha = (Get-FileHash -LiteralPath $dll.FullName -Algorithm SHA256).Hash
Write-Host ("built {0}: {1} bytes, SHA256 {2}{3}" -f $dll.FullName, $dll.Length, $sha, $(if ($Test) { ' (TEST build: D:\WartalesTest only, never ship)' } else { '' }))
