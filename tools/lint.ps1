# Lint gate for the winmm/ crate only (patcher/ and the rest are not linted here).
#   pwsh tools\lint.ps1
$ErrorActionPreference = 'Stop'
$crate = Join-Path (Split-Path -Parent $PSScriptRoot) 'winmm'

$steps = @(
    @('fmt', '--check'),
    @('clippy', '--all-targets', '--', '-D', 'warnings'),
    @('clippy', '--all-targets', '--features', 'harness', '--', '-D', 'warnings'),
    @('deny', 'check')
)
Push-Location $crate
try {
    foreach ($s in $steps) {
        Write-Host "lint: cargo $($s -join ' ')"
        & cargo @s
        if ($LASTEXITCODE -ne 0) { throw "lint: cargo $($s -join ' ') failed" }
    }
} finally { Pop-Location }
Write-Host 'lint: ok'
