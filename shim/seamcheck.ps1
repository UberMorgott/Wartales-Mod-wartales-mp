# Proves a built file does (or does not) carry the local co-op test harness
# seam (`build.ps1 -Test` only: proxy.c WMP_TEST, Go tag `harness`, cargo
# feature `harness`). Every part of the seam carries the marker WMP_TEST_SEAM
# and/or reads WARTALES_MP_TEST_INSTANCE, so their absence (ASCII and UTF-16)
# from the unpacked bytes is the release guarantee. A UPX-packed file is
# unpacked to a temp copy first, and a PE embedded in it (winmm.dll carries the
# helper exe, itself UPX-packed in a release build) is cut out and checked too.
#
#   .\shim\seamcheck.ps1 -Path <winmm.dll|wartales-mp.exe> [-Expect absent|present]

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Path,
    [ValidateSet('absent', 'present')] [string]$Expect = 'absent'
)

$ErrorActionPreference = 'Stop'
$Path = (Resolve-Path -LiteralPath $Path).Path
$latin = [Text.Encoding]::GetEncoding(28591)
$upx = (Get-Command upx -ErrorAction SilentlyContinue)?.Source

# The bytes of a PE image, UPX-unpacked when it is packed.
function Unpacked([byte[]]$b, [string]$ext) {
    if (-not $latin.GetString($b).Contains('UPX!')) { return , $b }
    if (-not $upx) { throw 'a UPX-packed image needs upx in PATH' }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("seamcheck-{0}{1}" -f [guid]::NewGuid(), $ext)
    $out = "$tmp.d$ext"
    try {
        [IO.File]::WriteAllBytes($tmp, $b)
        & $upx -q -t $tmp *> $null
        if ($LASTEXITCODE -ne 0) { return , $b } # 'UPX!' only inside an embedded image
        & $upx -q -d -o $out $tmp *> $null
        if ($LASTEXITCODE -ne 0) { throw "upx -d failed" }
        return , [IO.File]::ReadAllBytes($out)
    } finally { Remove-Item -LiteralPath $tmp, $out -ErrorAction SilentlyContinue }
}

# PE images embedded after offset 0: MZ whose e_lfanew points at "PE\0\0";
# the end is the furthest section's raw data.
function Embedded([byte[]]$b) {
    $res = @()
    $i = 1
    while (($i = [Array]::IndexOf($b, [byte]0x4D, $i)) -ge 0 -and $i -lt $b.Length - 0x40) {
        if ($b[$i + 1] -eq 0x5A) {
            $pe = $i + [BitConverter]::ToInt32($b, $i + 0x3C)
            if ($pe -gt $i -and $pe + 24 -lt $b.Length -and [BitConverter]::ToUInt32($b, $pe) -eq 0x4550) {
                $n = [BitConverter]::ToUInt16($b, $pe + 6)
                $sec = $pe + 24 + [BitConverter]::ToUInt16($b, $pe + 20)
                $end = 0
                for ($k = 0; $k -lt $n; $k++) {
                    $o = $sec + 40 * $k
                    $e = [BitConverter]::ToUInt32($b, $o + 20) + [BitConverter]::ToUInt32($b, $o + 16)
                    if ($e -gt $end) { $end = $e }
                }
                if ($end -gt 0 -and $i + $end -le $b.Length) {
                    $img = New-Object byte[] $end
                    [Array]::Copy($b, $i, $img, 0, $end)
                    $res += , $img
                    $i += $end
                    continue
                }
            }
        }
        $i++
    }
    return , $res
}

function Markers([byte[]]$b) {
    $text = $latin.GetString($b)
    $found = @()
    foreach ($m in 'WMP_TEST_SEAM', 'WARTALES_MP_TEST_INSTANCE') {
        if ($text.Contains($m)) { $found += $m }
        if ($text.Contains($latin.GetString([Text.Encoding]::Unicode.GetBytes($m)))) { $found += "$m (UTF-16)" }
    }
    return , $found
}

$ext = [IO.Path]::GetExtension($Path)
$top = Unpacked ([IO.File]::ReadAllBytes($Path)) $ext
$found = Markers $top
$parts = @()
foreach ($img in (Embedded $top)) {
    $f = Markers (Unpacked $img '.exe')
    $parts += ("embedded PE {0} bytes{1}" -f $img.Length, $(if ($f) { ": $($f -join ', ')" } else { '' }))
    $found += $f
}
$name = Split-Path -Leaf $Path
$detail = @($found | Select-Object -Unique) -join ', '
if ($Expect -eq 'absent' -and $found) { throw "TEST SEAM in $name ($detail): never ship a -Test build" }
if ($Expect -eq 'present' -and -not $found) { throw "no test seam in $name (expected a -Test build)" }
Write-Host ("seamcheck {0}: test seam {1}{2}{3}" -f $name, $Expect, $(if ($detail) { " [$detail]" } else { '' }), $(if ($parts) { "; $($parts -join '; ')" } else { '' }))
