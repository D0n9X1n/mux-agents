# Unix (Linux, macOS) bodies of the provider members, dot-sourced after MuxAgents.ps1; dormant until B4 binds them. OS reads go through a kernel object.
function Import-MuxAgentUnixNative { if (-not ('MuxAgentUnixNative' -as [type])) { Add-Type -Path (Join-Path $PSScriptRoot 'MuxAgents.Native.Unix.cs') } }
# Endpoint on Unix: $TMUX only, "<socket>,<server pid>,<session>" with no CR or LF anywhere (\z: $ would let a final LF
# through); the pid stays text as on Windows.
function Get-MuxAgentUnixEndpoint { param($Environment)
    if ("$($Environment['TMUX'])" -match '^(/[^,\r\n]+),(\d+),(-?\d+)\z') { [pscustomobject]@{ Socket = $Matches[1]; ServerPid = $Matches[2] } } }
# IsShell on Unix: the shells allowed between the caller and its agent or pane, by real-path basename.
function Test-MuxAgentUnixShell { param($Row) [IO.Path]::GetFileName("$($Row.Path)") -cin @('sh', 'bash', 'zsh', 'fish', 'dash', 'ksh', 'tcsh', 'pwsh') }
# SameStart on Unix: kernel starts compare exactly (no rounding); an unreadable start is never the same.
function Test-MuxAgentUnixSameStart { param($Opened, $Listed) $null -ne $Opened -and $null -ne $Listed -and [long]$Opened -eq [long]$Listed }
# Argv on Unix: the row's kernel argv; throws when unreadable, so C3 refuses 'flags'.
function Get-MuxAgentUnixArgv { param($Row)
    if ($null -eq $Row.Argv) { throw [InvalidOperationException]::new('mux-agent: argv is unreadable') }
    [string[]]$Row.Argv }
# StopReport on Unix: every candidate tree row stop did not kill (Reason open, start or kill) is a survivor, by pid.
function Get-MuxAgentUnixStopReport { param($Left)
    $pids = [int[]]@($Left | Where-Object { $null -ne $_ } | ForEach-Object { [int]$_.Pid })
    [ordered]@{ survivors = $pids.Count; survivorPids = $pids } }
# The script node runs: the first argument after node that is not an option; -r/--require/--import/--loader take the next
# argument; '--' ends options. $null for -e/-p/--eval/--print (or a short-option cluster holding e or p) or for no script.
function Get-MuxAgentUnixNodeScript { param([string[]]$Argv)
    for ($index = 1; $index -lt $Argv.Count; $index++) {
        $item = $Argv[$index]
        if ($item -ceq '--') { return $(if ($index + 1 -lt $Argv.Count) { $Argv[$index + 1] }) }
        if ($item -cmatch '^(--(eval|print)(=|$)|-[A-Za-z]*[ep][A-Za-z]*$)') { return $null }
        if ($item -cin @('-r', '--require', '--import', '--loader')) { $index++; continue }
        if (-not $item.StartsWith('-')) { return $item } }
    $null }
# ClientOf on Unix, by the real executable path: claude under */claude/versions/* or named claude; copilot named copilot, or node
# whose script resolves to @github/copilot/npm-loader.js. A relative script resolves against that process's own cwd (the C5
# reader); an unreadable cwd or link is no match. Case-sensitive.
function Get-MuxAgentUnixClient { param($Row, [Parameter(Mandatory)]$Kernel)
    $path = "$($Row.Path)"; $file = [IO.Path]::GetFileName($path)
    if ($path -clike '*/claude/versions/*' -or $file -ceq 'claude') { return 'claude' }
    if ($file -ceq 'copilot') { return 'copilot' }
    if ($file -cne 'node' -or $null -eq $Row.Argv) { return }
    $script = Get-MuxAgentUnixNodeScript -Argv $Row.Argv
    if (-not $script) { return }
    $target = try {
        if (-not $script.StartsWith('/')) { $cwd = "$(& $Kernel.ReadCwd ([int]$Row.Pid))"; if (-not $cwd.StartsWith('/')) { throw 'cwd' }; $script = $cwd.TrimEnd('/') + '/' + $script }
        "$(& $Kernel.ResolveFile $script)" } catch { '' }
    if ($target.EndsWith('/@github/copilot/npm-loader.js', [StringComparison]::Ordinal)) { 'copilot' } }
# /proc/<pid>/stat: comm may hold spaces, ')' and newlines, so fields are read after the last ')'. Every field through starttime
# (field 22) and the one after it must be present, so a starttime cut short is refused. starttime is raw ticks.
function ConvertFrom-MuxAgentLinuxStat { param([AllowNull()][string]$Text)
    $close = "$Text".LastIndexOf(')'); $open = "$Text".IndexOf(' (')
    if ($open -lt 1 -or $close -lt $open) { return $null }
    $fields = $Text.Substring($close + 1).Trim().Split(' '); $id = 0; $parent = 0; $start = 0L
    if ($fields.Count -lt 21 -or -not [int]::TryParse($Text.Substring(0, $open), [ref]$id) -or -not [int]::TryParse($fields[1], [ref]$parent) -or -not [long]::TryParse($fields[19], [ref]$start)) { return $null }
    [pscustomobject]@{ Pid = $id; ParentPid = $parent; Start = $start } }
# /proc/<pid>/cmdline: NUL-terminated strings. The data must end in NUL; exactly that terminator is dropped, so empty trailing
# arguments survive. No final NUL: unreadable ($null).
function ConvertFrom-MuxAgentLinuxCmdline { param([AllowNull()][byte[]]$Bytes)
    if ($null -eq $Bytes -or $Bytes.Length -eq 0 -or $Bytes[-1] -ne 0) { return $null }
    [Text.Encoding]::UTF8.GetString($Bytes, 0, $Bytes.Length - 1).Split([char]0) }
# One process row. Argv is the kernel argv ($null when unreadable); CommandLine only joins it for display.
function New-MuxAgentUnixRow { param([int]$ProcessId, [int]$ParentId, [long]$Start, $Path, [AllowNull()][string[]]$Argv)
    [pscustomobject]@{ Pid = $ProcessId; ParentPid = $ParentId; Start = $Start; Path = $Path; CommandLine = $(if ($null -ne $Argv) { $Argv -join ' ' }); Argv = $Argv } }
# The kernel argv of one pid as string[] (empty strings kept), or $null when unreadable or empty (a kernel thread).
function Read-MuxAgentUnixArgv { param([Parameter(Mandatory)][scriptblock]$Reader, [int]$Id)
    $argv = $null; try { $argv = [string[]]@(& $Reader $Id | Where-Object { $null -ne $_ }) } catch { }
    if ($argv.Count) { , $argv } }
# Processes on Linux: /proc/[0-9]* stat, exe link, cmdline; a pid that exits mid-read is skipped.
function Get-MuxAgentLinuxProcessTable { param([Parameter(Mandatory)]$Kernel)
    foreach ($id in & $Kernel.ListIds) {
        $stat = try { ConvertFrom-MuxAgentLinuxStat -Text (& $Kernel.ReadStat $id) } catch { $null }
        if (-not $stat) { continue }
        $path = try { & $Kernel.ReadExe $id } catch { $null }
        New-MuxAgentUnixRow -ProcessId $stat.Pid -ParentId $stat.ParentPid -Start $stat.Start -Path $path -Argv (Read-MuxAgentUnixArgv -Reader $Kernel.ReadCmdline -Id $id) } }
function Get-MuxAgentMacProcessTable {
    # Processes on macOS: pid and ppid from ps; start (microseconds) and real path from Process; argv from KERN_PROCARGS2.
    param([Parameter(Mandatory)]$Kernel)
    foreach ($line in "$(& $Kernel.ListPs)" -split "`n") {
        if ($line -notmatch '^\s*(\d+)\s+(\d+)\s*$') { continue }
        $id = [int]$Matches[1]; $parent = [int]$Matches[2]; $start = & $Kernel.ReadStart $id
        if ($null -eq $start) { continue }
        New-MuxAgentUnixRow -ProcessId $id -ParentId $parent -Start $start -Path (& $Kernel.ReadPath $id) -Argv (Read-MuxAgentUnixArgv -Reader $Kernel.ReadArgv -Id $id) } }
# IsGone on Unix: true only when kill(pid, 0) reports ESRCH (no such process). Success, EPERM (alive under another user), any
# other errno, or a probe that cannot run is not shown gone, so a Kill without a pidfd returns $false and the pid is reported.
function Test-MuxAgentUnixGone { param([Parameter(Mandatory)][scriptblock]$Probe, [int]$Id)
    $errno = try { [int](& $Probe $Id) } catch { -1 }
    $errno -eq 3 }
function Get-MuxAgentUnixLinkTarget { param([string]$Path) $target = [IO.File]::ResolveLinkTarget($Path, $true); if ($target) { $target.FullName } else { [IO.Path]::GetFullPath($Path) } }
# The Linux reads: /proc and pidfd. Read members return $null when unreadable, except ReadStat and ReadStatus, which throw.
function New-MuxAgentLinuxKernel { [pscustomobject]@{
    ListIds = { foreach ($d in [IO.Directory]::EnumerateDirectories('/proc')) { $n = 0; if ([int]::TryParse([IO.Path]::GetFileName($d), [ref]$n)) { $n } } }
    ReadStat = { param([int]$Id) [IO.File]::ReadAllText("/proc/$Id/stat") }; ReadStatus = { [IO.File]::ReadAllText('/proc/self/status') }
    ReadExe = { param([int]$Id) try { [IO.File]::ResolveLinkTarget("/proc/$Id/exe", $false).FullName } catch { $null } }
    ReadCmdline = { param([int]$Id) ConvertFrom-MuxAgentLinuxCmdline -Bytes ([IO.File]::ReadAllBytes("/proc/$Id/cmdline")) }
    ReadStart = { param([int]$Id) try { (ConvertFrom-MuxAgentLinuxStat -Text ([IO.File]::ReadAllText("/proc/$Id/stat"))).Start } catch { $null } }
    ReadCwd = { param([int]$Id) try { [IO.Directory]::ResolveLinkTarget("/proc/$Id/cwd", $false).FullName } catch { $null } }
    ResolveFile = { param($Path) Get-MuxAgentUnixLinkTarget -Path $Path }
    IsGone = { param([int]$Id) Test-MuxAgentUnixGone -Id $Id -Probe { param([int]$Id) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::ProbeLinux($Id) } }
    PidfdOpen = { param([int]$Id) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::PidfdOpen($Id) }; SendKill = { param([int]$Fd) [MuxAgentUnixNative]::PidfdKill($Fd) }
    WaitExit = { param([int]$Fd, [int]$Milliseconds) [MuxAgentUnixNative]::PidfdExited($Fd, $Milliseconds) }; Close = { param([int]$Fd) [MuxAgentUnixNative]::Close($Fd) }
    Sleep = { param([int]$Milliseconds) Start-Sleep -Milliseconds $Milliseconds } } }
# The macOS reads: ps, Process, KERN_PROCARGS2, sandbox_check, proc_pidinfo, kill(pid, 0). No pidfd exists: PidfdOpen is -1 and nothing is signalled.
function New-MuxAgentMacKernel { [pscustomobject]@{
    ListPs = { $run = Invoke-MuxAgentProcess -FilePath '/bin/ps' -Arguments @('-A', '-o', 'pid=,ppid='); if ($run.ExitCode -ne 0) { throw 'mux-agent: ps failed' }; $run.StdOut }
    ReadStart = { param([int]$Id) try { Import-MuxAgentUnixNative; [MuxAgentUnixNative]::EpochMicros([Diagnostics.Process]::GetProcessById($Id).StartTime) } catch { $null } }
    ReadPath = { param([int]$Id) try { [Diagnostics.Process]::GetProcessById($Id).MainModule.FileName } catch { $null } }
    ReadArgv = { param([int]$Id) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::ProcArgs($Id) }
    ReadCwd = { param([int]$Id) try { Import-MuxAgentUnixNative; [MuxAgentUnixNative]::Cwd($Id) } catch { $null } }
    SandboxCheck = { Import-MuxAgentUnixNative; [MuxAgentUnixNative]::SandboxCheck() }; ResolveFile = { param($Path) Get-MuxAgentUnixLinkTarget -Path $Path }
    IsGone = { param([int]$Id) Test-MuxAgentUnixGone -Id $Id -Probe { param([int]$Id) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::ProbeMac($Id) } }
    PidfdOpen = { param([int]$Id) -1 }; SendKill = { param([int]$Fd) $false }; WaitExit = { param([int]$Fd, [int]$Milliseconds) $false }; Close = { param([int]$Fd) }
    Sleep = { param([int]$Milliseconds) Start-Sleep -Milliseconds $Milliseconds } } }
# Sandbox (C1) on Linux: refuses 2 'sandbox' for SANDBOX_RUNTIME=1, an unreadable /proc/self/status or NoNewPrivs set, naming the signal.
function Assert-MuxAgentLinuxSandbox { param($Environment, [Parameter(Mandatory)]$Kernel)
    if ("$($Environment['SANDBOX_RUNTIME'])" -ceq '1') { Exit-MuxAgentCommand 2 'sandbox' 'SANDBOX_RUNTIME=1 is set' }
    $status = try { "$(& $Kernel.ReadStatus)" } catch { Exit-MuxAgentCommand 2 'sandbox' '/proc/self/status is unreadable' }
    if ($status -notmatch '(?m)^NoNewPrivs:[ \t]*(\d+)[ \t]*$') { Exit-MuxAgentCommand 2 'sandbox' 'NoNewPrivs is unreadable' }
    if ($Matches[1] -ne '0') { Exit-MuxAgentCommand 2 'sandbox' 'NoNewPrivs is set' } }
# Sandbox (C1) on macOS: refuses 2 'sandbox' for SANDBOX_RUNTIME=1, or sandbox_check(getpid(), NULL, 0) non-zero or failing.
function Assert-MuxAgentMacSandbox { param($Environment, [Parameter(Mandatory)]$Kernel)
    if ("$($Environment['SANDBOX_RUNTIME'])" -ceq '1') { Exit-MuxAgentCommand 2 'sandbox' 'SANDBOX_RUNTIME=1 is set' }
    $result = try { [int](& $Kernel.SandboxCheck) } catch { Exit-MuxAgentCommand 2 'sandbox' 'sandbox_check failed' }
    if ($result -ne 0) { Exit-MuxAgentCommand 2 'sandbox' 'sandbox_check reports a sandbox' } }
# Open on Unix: the pidfd first (-1 on macOS or without pidfd), then the start, so the start belongs to the process the fd holds.
# Pidfd lifetime: a row that SameStart rejects keeps the fd this returns. B1's Invoke-MuxAgentStop passes only the two starts to
# SameStart and drops the handle, so no Unix member sees it; closing it needs a core Release member that B2 does not add. The fd
# ends when mux-agent exits (one verb per process); a pidfd pins no pid number and signals nothing. The server identity
# (& $P.Open $serverPid) and the agent check in Resolve-MuxAgentCaller likewise hold their fd, never signalled, until exit.
function Open-MuxAgentUnixProcess { param([int]$Id, [Parameter(Mandatory)]$Kernel)
    $fd = [int](& $Kernel.PidfdOpen $Id); $start = & $Kernel.ReadStart $Id
    if ($null -eq $start) { if ($fd -ge 0) { $null = & $Kernel.Close $fd }; return $null }
    [pscustomobject]@{ Id = $Id; Start = $start; Process = $null; Fd = $fd } }
# Kill on Unix: SIGKILL only through the pidfd bound at Open, which cannot reach a reused PID; true when it exited within 5 s.
# Without a pidfd nothing is sent: true only when the exit is shown within 5 s (IsGone: ESRCH, or the pid now has another
# start). An unreadable start or a probe that cannot tell is not an exit, so the pid is reported.
function Stop-MuxAgentUnixProcess { param([Parameter(Mandatory)]$Opened, [Parameter(Mandatory)]$Kernel)
    if ($Opened.Fd -ge 0) { try { $null = & $Kernel.SendKill $Opened.Fd; return [bool](& $Kernel.WaitExit $Opened.Fd 5000) } finally { $null = & $Kernel.Close $Opened.Fd } }
    for ($round = 0; $round -lt 50; $round++) {
        $null = & $Kernel.Sleep 100; $now = & $Kernel.ReadStart $Opened.Id
        if (($null -ne $now -and $now -ne $Opened.Start) -or (& $Kernel.IsGone $Opened.Id)) { return $true } }
    $false }
# ClaudeDirectory (C5) on Unix: the agent's kernel cwd, read between two starts equal to the caller's. ~/.claude/sessions/<pid>.json
# is "no session" only when the read proves it missing (FileNotFoundException or DirectoryNotFoundException); File.Exists is not
# used, as it is false on access errors and for a folder. A file that is present must parse to an object whose cwd is a non-empty
# string equal to that cwd; anything else (unreadable, a folder, over 1 MB, malformed) refuses. sessionId/procStart stay
# Windows-only (claude-code#36213).
function Get-MuxAgentUnixClaudeDirectory { param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)]$Kernel)
    $id = [int]$Caller.Agent.Pid; $want = $Caller.Agent.Start
    if (-not (Test-MuxAgentUnixSameStart (& $Kernel.ReadStart $id) $want)) { return $null }
    $cwd = try { "$(& $Kernel.ReadCwd $id)" } catch { '' }
    if (-not $cwd -or -not (Test-MuxAgentUnixSameStart (& $Kernel.ReadStart $id) $want)) { return $null }
    $file = [IO.Path]::Combine($P.Home, '.claude', 'sessions', "$id.json")
    $missing = $false; $text = $null
    try {
        $stream = [IO.File]::OpenRead($file)
        try { if ($stream.Length -le 1MB) { $text = [IO.StreamReader]::new($stream).ReadToEnd() } } finally { $stream.Dispose() } }
    catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] { $missing = $true }
    catch { $text = $null }
    if ($missing) { return $cwd }
    $session = try { ConvertFrom-Json -InputObject $text -NoEnumerate } catch { $null }
    if ($session -is [System.Management.Automation.PSCustomObject] -and $session.cwd -is [string] -and $session.cwd -ceq $cwd) { $cwd } }
