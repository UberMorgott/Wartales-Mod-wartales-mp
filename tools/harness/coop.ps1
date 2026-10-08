# Local co-op harness driver (docs/coop-harness-plan.md, slice 1).
#
# Runs the harness game copies D:\WartalesTest\{A,B} (tools\harness\setup.ps1)
# side by side with the TEST winmm.dll (shim\build.ps1 -Test): A is harness
# instance 1, B instance 2. Each gets its own LOCALAPPDATA under the run folder,
# so shim.log, wartales-mp.log, the helper exe and the command channel are
# per instance and land in the run folder by themselves.
#
#   coop.ps1 start   [-Inst A,B]          new run folder, launch, wait for windows
#   coop.ps1 cmd     -Inst A -Line 'dump' send a harness command, wait for its ack
#                                          ('console <line>' runs a debug console command)
#   coop.ps1 shot    [-Inst A,B] [-Tag x] PrintWindow screenshot per window
#   coop.ps1 place   [-Inst A,B]          windows side by side (1280x720 each)
#   coop.ps1 close   -Inst B              WM_CLOSE (Alt+F4, soft)
#   coop.ps1 kill    -Inst B              TerminateProcess (crash)
#   coop.ps1 stop                          close all harness games (kill after 20 s)
#   coop.ps1 status | logs [-Inst A] [-Tail 40]
#
# Only processes this driver started (pids in <run>\run.json) are touched,
# plus the helper exe extracted into that run's own LOCALAPPDATA.

[CmdletBinding()]
param(
    [Parameter(Position = 0, Mandatory)]
    [ValidateSet('start', 'cmd', 'shot', 'place', 'close', 'kill', 'stop', 'status', 'logs')]
    [string]$Action,
    [string[]]$Inst = @('A', 'B'),
    [string]$Line = 'dump',
    [string]$Tag = '',
    [int]$Tail = 40,
    [int]$TimeoutSec = 120,
    [string]$Root = 'D:\WartalesTest'
)

$ErrorActionPreference = 'Stop'
$AppId = '1527950'
$runs = Join-Path $Root 'runs'
$current = Join-Path $runs 'current.txt'

Add-Type -Namespace Harness -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
public struct RECT { public int Left, Top, Right, Bottom; }
'@
Add-Type -AssemblyName System.Drawing

function Instance-No([string]$i) { return [array]::IndexOf(@('A', 'B', 'C', 'D'), $i.ToUpper()) + 1 }

function Run-Dir {
    if (-not (Test-Path -LiteralPath $current)) { throw 'no harness run (coop.ps1 start)' }
    return (Get-Content -LiteralPath $current -Raw).Trim()
}

function Run-State([string]$run) {
    $f = Join-Path $run 'run.json'
    if (-not (Test-Path -LiteralPath $f)) { return @{} }
    $h = @{}
    (Get-Content -LiteralPath $f -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $h[$_.Name] = $_.Value }
    return $h
}

function Game-Proc([string]$run, [string]$i) {
    $st = Run-State $run
    if (-not $st.ContainsKey($i)) { return $null }
    # pid + exe path + start time: a reused pid is never mistaken for our game.
    $e = $st[$i]
    $p = Get-Process -Id $e.pid -ErrorAction SilentlyContinue
    if ($p -and $p.Path -ieq $e.path -and $p.StartTime.ToUniversalTime().Ticks -eq [long]$e.start) { return $p }
    return $null
}

function Mod-Dir([string]$run, [string]$i) { return Join-Path $run "$i\wartales-mp" }

switch ($Action) {
    'start' {
        New-Item -ItemType Directory -Force $runs | Out-Null
        $run = Join-Path $runs (Get-Date -Format 'yyyyMMdd-HHmmss')
        New-Item -ItemType Directory -Force $run | Out-Null
        Set-Content -LiteralPath $current -Value $run -Encoding ascii
        $st = @{}
        foreach ($i in $Inst) {
            $dir = Join-Path $Root $i
            if (-not (Test-Path -LiteralPath (Join-Path $dir '.wartales-harness'))) { throw "$dir is not a harness copy (run setup.ps1)" }
            $la = Join-Path $run $i
            New-Item -ItemType Directory -Force (Join-Path $la 'wartales-mp\harness') | Out-Null
            $psi = [Diagnostics.ProcessStartInfo]::new((Join-Path $dir 'Wartales.exe'))
            $psi.WorkingDirectory = $dir
            $psi.UseShellExecute = $false
            $psi.Environment['SteamAppId'] = $AppId
            $psi.Environment['SteamGameId'] = $AppId
            $psi.Environment['WARTALES_MP_TEST_INSTANCE'] = [string](Instance-No $i)
            $psi.Environment['LOCALAPPDATA'] = $la
            $p = [Diagnostics.Process]::Start($psi)
            $st[$i] = @{ pid = $p.Id; path = $psi.FileName; start = $p.StartTime.ToUniversalTime().Ticks }
            Write-Host "$i started: pid $($p.Id), instance $(Instance-No $i), LOCALAPPDATA $la"
            $st | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'run.json')
            Start-Sleep -Seconds 5 # let A take its helper ports before B starts
        }
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        foreach ($i in $Inst) {
            while ($true) {
                $p = Game-Proc $run $i
                if (-not $p) { throw "$i exited during start (see $(Mod-Dir $run $i)\shim.log)" }
                $p.Refresh()
                if ($p.MainWindowHandle -ne [IntPtr]::Zero) { break }
                if ((Get-Date) -gt $deadline) { throw "$i has no window after $TimeoutSec s" }
                Start-Sleep -Milliseconds 500
            }
            # The harness channel is up once the first frame ran harnessTick.
            $log = Join-Path (Mod-Dir $run $i) 'shim.log'
            while (-not ((Test-Path -LiteralPath $log) -and (Select-String -LiteralPath $log -Pattern 'harness: WMP_TEST_SEAM instance' -Quiet))) {
                if (-not (Game-Proc $run $i)) { throw "$i exited before its first frame" }
                if ((Get-Date) -gt $deadline) { throw "${i}: no harness line in $log after $TimeoutSec s" }
                Start-Sleep -Seconds 1
            }
            Write-Host "$i window up, harness channel active"
        }
        Write-Host "run: $run"
    }
    'cmd' {
        $run = Run-Dir
        foreach ($i in $Inst) {
            if (-not (Game-Proc $run $i)) { throw "$i is not running" }
            $h = Join-Path (Mod-Dir $run $i) 'harness'
            $seq = [string][DateTimeOffset]::Now.ToUnixTimeMilliseconds()
            $tmp = Join-Path $h 'cmd.tmp'
            [IO.File]::WriteAllText($tmp, "$seq $Line")
            Move-Item -LiteralPath $tmp -Destination (Join-Path $h 'cmd.txt') -Force
            $ack = Join-Path $h 'ack.txt'
            $deadline = (Get-Date).AddSeconds([Math]::Min($TimeoutSec, 30))
            while ($true) {
                if (Test-Path -LiteralPath $ack) {
                    $a = (Get-Content -LiteralPath $ack -Raw -ErrorAction SilentlyContinue)
                    if ($a -and $a.StartsWith("$seq ")) { break }
                }
                if ((Get-Date) -gt $deadline) { throw "${i}: no ack for '$Line' (seq $seq)" }
                Start-Sleep -Milliseconds 100
            }
            Write-Host "$i ack: $a"
            $result = $a.Substring($seq.Length + 1).Trim()
            if ($result -notin 'ok', 'dispatched') { throw "${i}: '$Line' failed: $result (see shim.log)" }
            if ($Line -like 'dump*') {
                $state = Join-Path $h 'state.json'
                $out = Join-Path $run ("{0}-dump-{1}.json" -f $i, $seq)
                Copy-Item -LiteralPath $state -Destination $out
                Get-Content -LiteralPath $out -Raw
            }
        }
    }
    'shot' {
        $run = Run-Dir
        foreach ($i in $Inst) {
            $p = Game-Proc $run $i
            if (-not $p) { Write-Host "$i not running"; continue }
            $p.Refresh()
            $hw = $p.MainWindowHandle
            $r = New-Object Harness.Win+RECT
            [void][Harness.Win]::GetWindowRect($hw, [ref]$r)
            $w = $r.Right - $r.Left; $hgt = $r.Bottom - $r.Top
            if ($w -le 0 -or $hgt -le 0) { Write-Host "$i window has no size"; continue }
            $bmp = New-Object Drawing.Bitmap $w, $hgt
            $g = [Drawing.Graphics]::FromImage($bmp)
            $dc = $g.GetHdc()
            [void][Harness.Win]::PrintWindow($hw, $dc, 2) # PW_RENDERFULLCONTENT: DirectX content too
            $g.ReleaseHdc($dc); $g.Dispose()
            $f = Join-Path $run ("{0}-{1}{2}.png" -f $i, (Get-Date -Format 'HHmmss'), $(if ($Tag) { "-$Tag" } else { '' }))
            $bmp.Save($f, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
            Write-Host "$i screenshot: $f (${w}x$hgt)"
        }
    }
    'place' {
        $run = Run-Dir
        $x = 0
        foreach ($i in $Inst) {
            $p = Game-Proc $run $i
            if (-not $p) { continue }
            $p.Refresh()
            [void][Harness.Win]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, $x, 40, 1280, 720, 0x0004) # SWP_NOZORDER
            $x += 1280
        }
    }
    'close' {
        $run = Run-Dir
        foreach ($i in $Inst) {
            $p = Game-Proc $run $i
            if (-not $p) { Write-Host "$i not running"; continue }
            $p.Refresh()
            [void][Harness.Win]::PostMessageW($p.MainWindowHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
            Write-Host "${i}: WM_CLOSE sent (pid $($p.Id))"
        }
    }
    'kill' {
        $run = Run-Dir
        foreach ($i in $Inst) {
            $p = Game-Proc $run $i
            if (-not $p) { Write-Host "$i not running"; continue }
            $p.Kill() # TerminateProcess: a crash, no shutdown code runs
            Write-Host "${i}: terminated (pid $($p.Id))"
        }
    }
    'stop' {
        $run = Run-Dir
        $all = @('A', 'B', 'C', 'D')
        & $PSCommandPath close -Inst $all -Root $Root
        $deadline = (Get-Date).AddSeconds(20)
        while ((Get-Date) -lt $deadline -and ($all | Where-Object { Game-Proc $run $_ })) { Start-Sleep -Milliseconds 500 }
        foreach ($i in $all) { $p = Game-Proc $run $i; if ($p) { $p.Kill(); Write-Host "${i}: killed after WM_CLOSE timeout" } }
        # Helpers watch their game and exit with it; stop any left in this run's folder.
        Start-Sleep -Seconds 3
        Get-Process wartales-mp -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($run, [StringComparison]::OrdinalIgnoreCase) } |
            ForEach-Object { $_.Kill(); Write-Host "helper pid $($_.Id) stopped" }
        Write-Host "stopped; logs in $run"
    }
    'status' {
        $run = Run-Dir
        Write-Host "run: $run"
        foreach ($i in (Run-State $run).Keys) {
            $p = Game-Proc $run $i
            Write-Host ("{0}: {1}" -f $i, $(if ($p) { "pid $($p.Id), $([int]($p.WorkingSet64 / 1MB)) MB" } else { 'not running' }))
        }
    }
    'logs' {
        $run = Run-Dir
        foreach ($i in $Inst) {
            foreach ($f in 'shim.log', 'wartales-mp.log') {
                $p = Join-Path (Mod-Dir $run $i) $f
                if (Test-Path -LiteralPath $p) { Write-Host "==== $i $f"; Get-Content -LiteralPath $p -Tail $Tail }
            }
        }
    }
}
