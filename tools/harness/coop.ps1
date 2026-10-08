# Local co-op harness driver (docs/coop-harness-plan.md, slice 1).
#
# Runs the harness game copies D:\WartalesTest\{A,B} (tools\harness\setup.ps1)
# side by side with the TEST winmm.dll (shim\build.ps1 -Test): A is harness
# instance 1, B instance 2. Each gets its own LOCALAPPDATA under the run folder,
# so shim.log, wartales-mp.log, the helper exe and the command channel are
# per instance and land in the run folder by themselves.
#
#   coop.ps1 start   [-Inst A,B] [-Keep]  new run folder (-Keep: the current one,
#                                          e.g. relaunch B), launch, wait for windows;
#                                          game stdout -> <run>\<i>\console.log
#   coop.ps1 cmd     -Inst A -Line 'dump' send a harness command, wait for its ack
#                                          (verbs: docs/coop-harness-plan.md section 8;
#                                          'join auto' = join with A's lobby code)
#   coop.ps1 wait    -Inst A -Until '$s.game -and $s.hasGameplayStarted'
#                                          dump every 2 s until the expression on the
#                                          dump ($s) is true (-TimeoutSec)
#   coop.ps1 key     -Inst A -Key Escape  WM_KEYDOWN/UP to the window (Escape, Enter, I, ...)
#   coop.ps1 shot    [-Inst A,B] [-Tag x] PrintWindow screenshot per window
#   coop.ps1 place   [-Inst A,B] [-W 852 -H 480]  windows side by side
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
    [ValidateSet('start', 'cmd', 'wait', 'key', 'shot', 'place', 'close', 'kill', 'stop', 'status', 'logs')]
    [string]$Action,
    [string[]]$Inst = @('A', 'B'),
    [string]$Line = 'dump',
    [string]$Until = '$true',
    [string]$Key = 'Escape',
    [string]$Tag = '',
    [int]$Tail = 40,
    [int]$TimeoutSec = 120,
    [int]$W = 852,
    [int]$H = 480,
    [switch]$Keep,
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

# Sends one harness command line to instance $i, waits for its ack; returns the
# result word, or the dump JSON (saved as <run>\<i>-dump-<seq>.json) for 'dump'.
function Send-Cmd([string]$run, [string]$i, [string]$line) {
    if (-not (Game-Proc $run $i)) { throw "$i is not running" }
    $h = Join-Path (Mod-Dir $run $i) 'harness'
    $seq = [string][DateTimeOffset]::Now.ToUnixTimeMilliseconds()
    $tmp = Join-Path $h 'cmd.tmp'
    [IO.File]::WriteAllText($tmp, "$seq $line")
    Move-Item -LiteralPath $tmp -Destination (Join-Path $h 'cmd.txt') -Force
    $ack = Join-Path $h 'ack.txt'
    $deadline = (Get-Date).AddSeconds([Math]::Min($TimeoutSec, 30))
    while ($true) {
        if (Test-Path -LiteralPath $ack) {
            $a = (Get-Content -LiteralPath $ack -Raw -ErrorAction SilentlyContinue)
            if ($a -and $a.StartsWith("$seq ")) { break }
        }
        if ((Get-Date) -gt $deadline) { throw "${i}: no ack for '$line' (seq $seq)" }
        Start-Sleep -Milliseconds 100
    }
    $result = $a.Substring($seq.Length + 1).Trim()
    if ($result -notin 'ok', 'dispatched') { throw "${i}: '$line' failed: $result (see shim.log)" }
    if ($line -ne 'dump') { return $result }
    $out = Join-Path $run ("{0}-dump-{1}.json" -f $i, $seq)
    Copy-Item -LiteralPath (Join-Path $h 'state.json') -Destination $out
    return (Get-Content -LiteralPath $out -Raw)
}

switch ($Action) {
    'start' {
        if ($Keep) { $run = Run-Dir } else {
            New-Item -ItemType Directory -Force $runs | Out-Null
            $run = Join-Path $runs (Get-Date -Format 'yyyyMMdd-HHmmss')
            New-Item -ItemType Directory -Force $run | Out-Null
            Set-Content -LiteralPath $current -Value $run -Encoding ascii
        }
        $st = Run-State $run
        foreach ($i in $Inst) {
            if (Game-Proc $run $i) { throw "$i is already running" }
            $dir = Join-Path $Root $i
            if (-not (Test-Path -LiteralPath (Join-Path $dir '.wartales-harness'))) { throw "$dir is not a harness copy (run setup.ps1)" }
            $la = Join-Path $run $i
            New-Item -ItemType Directory -Force (Join-Path $la 'wartales-mp\harness') | Out-Null
            # The game's stdout/stderr go to a per-instance file, not this console.
            $exe = Join-Path $dir 'Wartales.exe'
            $envs = @{ SteamAppId = $AppId; SteamGameId = $AppId; WARTALES_MP_TEST_INSTANCE = [string](Instance-No $i); LOCALAPPDATA = $la }
            $p = Start-Process -FilePath $exe -WorkingDirectory $dir -Environment $envs -PassThru -NoNewWindow `
                -RedirectStandardOutput (Join-Path $la 'console.log') -RedirectStandardError (Join-Path $la 'console.err.log')
            $st[$i] = @{ pid = $p.Id; path = $exe; start = $p.StartTime.ToUniversalTime().Ticks }
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
            # The harness channel is up once this process's first frame ran
            # harnessTick (shim.log lines carry the pid; -Keep reuses the log).
            $log = Join-Path (Mod-Dir $run $i) 'shim.log'
            $seam = '^\[{0} .*harness: WMP_TEST_SEAM instance' -f $st[$i].pid
            while (-not ((Test-Path -LiteralPath $log) -and (Select-String -LiteralPath $log -Pattern $seam -Quiet))) {
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
            $l = $Line
            if ($l -eq 'join auto') {
                $code = (Send-Cmd $run 'A' 'dump' | ConvertFrom-Json).lobby.code
                if (-not $code) { throw 'A has no lobby code (host first)' }
                $l = "join $code"
            }
            $r = Send-Cmd $run $i $l
            if ($l -eq 'dump') { $r } else { Write-Host "${i}: '$l' -> $r" }
        }
    }
    'wait' {
        $run = Run-Dir
        $cond = [scriptblock]::Create($Until)
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        foreach ($i in $Inst) {
            while ($true) {
                $s = Send-Cmd $run $i 'dump' | ConvertFrom-Json
                if (& $cond) { Write-Host "${i}: $Until"; break }
                if ((Get-Date) -gt $deadline) { throw "${i}: not '$Until' after $TimeoutSec s" }
                Start-Sleep -Seconds 2
            }
        }
    }
    'key' {
        $run = Run-Dir
        $vk = switch ($Key) { 'Escape' { 0x1B } 'Enter' { 0x0D } 'Space' { 0x20 } 'Tab' { 0x09 } default { [int][char]$Key.ToUpper() } }
        foreach ($i in $Inst) {
            $p = Game-Proc $run $i
            if (-not $p) { Write-Host "$i not running"; continue }
            $p.Refresh()
            [void][Harness.Win]::PostMessageW($p.MainWindowHandle, 0x0100, [IntPtr]$vk, [IntPtr]1) # WM_KEYDOWN
            Start-Sleep -Milliseconds 80
            [void][Harness.Win]::PostMessageW($p.MainWindowHandle, 0x0101, [IntPtr]$vk, [IntPtr]0xC0000001L) # WM_KEYUP
            Write-Host "${i}: key $Key"
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
            [void][Harness.Win]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, $x, 40, $W, $H, 0x0004) # SWP_NOZORDER
            $x += $W
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
            foreach ($f in 'wartales-mp\shim.log', 'wartales-mp\wartales-mp.log', 'console.log') {
                $p = Join-Path (Join-Path $run $i) $f
                if (Test-Path -LiteralPath $p) { Write-Host "==== $i $f"; Get-Content -LiteralPath $p -Tail $Tail }
            }
        }
    }
}
