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
# The script node runs: the first argument after node that is not an option; '--' ends options. Options are read fail-closed:
# -r/--require/--import/--loader skip the next argument; an allowlisted --option=value or flag without a value, none of which
# changes what node runs, is skipped; any other option gives $null: it may take the next argument as its value (--conditions)
# or run something else (--run=<script>, --check, --test). $null too for -e/-p/--eval/--print (or a short-option cluster holding
# e or p) or for no script.
function Get-MuxAgentUnixNodeScript { param([string[]]$Argv)
    for ($index = 1; $index -lt $Argv.Count; $index++) {
        $item = $Argv[$index]
        if ($item -ceq '--') { return $(if ($index + 1 -lt $Argv.Count) { $Argv[$index + 1] }) }
        if ($item -cmatch '^(--(eval|print)(=|$)|-[A-Za-z]*[ep][A-Za-z]*$)') { return $null }
        if ($item -cin @('-r', '--require', '--import', '--loader')) { $index++; continue }
        if (-not $item.StartsWith('-')) { return $item }
        if ($item -cmatch '^--(max-old-space-size|max-semi-space-size|stack-size|require|import|loader|experimental-loader|conditions|title|unhandled-rejections)=' -or $item -cin @('--no-warnings', '--no-deprecation', '--enable-source-maps', '--trace-warnings', '--preserve-symlinks', '--preserve-symlinks-main', '--expose-gc')) { continue }
        return $null }
    $null }
# ClientOf on Unix, by the real executable path: claude under */claude/versions/* or named claude; copilot named copilot, or node
# whose script's realpath (every link in every component resolved) ends in @github/copilot/npm-loader.js, so a directory alias
# named @github/copilot is not enough. A relative script resolves against that process's own cwd (the C5 reader); an unreadable
# cwd or a path realpath cannot resolve is no match. Case-sensitive.
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
# The Linux reads: /proc and pidfd. Read members return $null when unreadable, except ReadStat and ReadStatus, which throw.
function New-MuxAgentLinuxKernel { [pscustomobject]@{
    ListIds = { foreach ($d in [IO.Directory]::EnumerateDirectories('/proc')) { $n = 0; if ([int]::TryParse([IO.Path]::GetFileName($d), [ref]$n)) { $n } } }
    ReadStat = { param([int]$Id) [IO.File]::ReadAllText("/proc/$Id/stat") }; ReadStatus = { [IO.File]::ReadAllText('/proc/self/status') }
    ReadExe = { param([int]$Id) try { [IO.File]::ResolveLinkTarget("/proc/$Id/exe", $false).FullName } catch { $null } }
    ReadCmdline = { param([int]$Id) ConvertFrom-MuxAgentLinuxCmdline -Bytes ([IO.File]::ReadAllBytes("/proc/$Id/cmdline")) }
    ReadStart = { param([int]$Id) try { (ConvertFrom-MuxAgentLinuxStat -Text ([IO.File]::ReadAllText("/proc/$Id/stat"))).Start } catch { $null } }
    ReadCwd = { param([int]$Id) try { [IO.Directory]::ResolveLinkTarget("/proc/$Id/cwd", $false).FullName } catch { $null } }
    ResolveFile = { param($Path) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::RealPathLinux($Path) }
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
    SandboxCheck = { Import-MuxAgentUnixNative; [MuxAgentUnixNative]::SandboxCheck() }; ResolveFile = { param($Path) Import-MuxAgentUnixNative; [MuxAgentUnixNative]::RealPathMac($Path) }
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
# Without a pidfd nothing is sent: true only when IsGone shows the exit (ESRCH) within 5 s. A pid that now has another start,
# an unreadable start or a probe that cannot tell is not proof, so the pid is reported.
function Stop-MuxAgentUnixProcess { param([Parameter(Mandatory)]$Opened, [Parameter(Mandatory)]$Kernel)
    if ($Opened.Fd -ge 0) { try { $null = & $Kernel.SendKill $Opened.Fd; return [bool](& $Kernel.WaitExit $Opened.Fd 5000) } finally { $null = & $Kernel.Close $Opened.Fd } }
    for ($round = 0; $round -lt 50; $round++) {
        $null = & $Kernel.Sleep 100
        if (& $Kernel.IsGone $Opened.Id) { return $true } }
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
function Get-MuxAgentUnixLockName {
    # The Unix mutex name for LockKey and the effective uid User (B4: the LockName member). No prefix means Local\,
    # which on Unix is one session, and every tmux pane is its own session; Global\ spans sessions, the uid users.
    param([Parameter(Mandatory)][string]$LockKey, [Parameter(Mandatory)][string]$User)
    "Global\MuxAgents-$User-" + (Get-MuxAgentHash $LockKey).Substring(0, 32)
}
function Invoke-MuxAgentUnixTool {
    # One native call of the Unix state code: stdout without its final newline (other whitespace is data), or a
    # refusal 1 'state' with Message when the call cannot start, times out, exits non-zero or prints nothing.
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string[]]$Arguments, [Parameter(Mandatory)][string]$Message)
    $result = try { Invoke-MuxAgentProcess -FilePath $FilePath -Arguments $Arguments -TimeoutMilliseconds 5000 } catch { $null }
    $text = if ($result) { "$($result.StdOut)" -replace '\n\z', '' } else { '' }
    if (-not $result -or $result.ExitCode -ne 0 -or -not $text) { Exit-MuxAgentCommand 1 'state' $Message }
    $text
}
function Resolve-MuxAgentUnixRealPath {
    # Path with realpath applied to its longest existing prefix; the missing rest is kept. Path must be absolute and
    # free of . and .. segments, so the kept rest cannot leave the resolved prefix. A prefix probe that throws refuses.
    param([Parameter(Mandatory)][string]$Path)
    $parts = @($Path.Split('/', [StringSplitOptions]::RemoveEmptyEntries))
    if (-not $Path.StartsWith('/') -or $Path -match '[\r\n]' -or $parts -contains '.' -or $parts -contains '..') { Exit-MuxAgentCommand 1 'state' 'the state folder path is not absolute' }
    $count = $parts.Count
    try { while ($count -gt 0 -and -not (Test-Path -LiteralPath ('/' + (@($parts | Select-Object -First $count) -join '/')))) { $count-- } }
    catch { Exit-MuxAgentCommand 1 'state' 'the state folder path cannot be resolved' }
    $real = Invoke-MuxAgentUnixTool -FilePath 'realpath' -Arguments @('--', ('/' + (@($parts | Select-Object -First $count) -join '/'))) -Message 'the state folder path cannot be resolved'
    if (-not $real.StartsWith('/') -or $real -match '[\r\n]') { Exit-MuxAgentCommand 1 'state' 'the state folder path cannot be resolved' }
    $rest = @($parts | Select-Object -Skip $count)
    if ($rest.Count) { $real.TrimEnd('/') + '/' + ($rest -join '/') } else { $real }
}
function Get-MuxAgentUnixStateRoot {
    # B4: the StateRootOf member. MUX_AGENTS_STATE (absolute, else refused), else $XDG_STATE_HOME/mux-agents/state (an
    # absolute value only, as the XDG spec says), else $HOME/.local/state/mux-agents/state; canonical via realpath.
    param([Parameter(Mandatory)][Collections.IDictionary]$Environment)
    $state = [string]$Environment['MUX_AGENTS_STATE']; $xdg = [string]$Environment['XDG_STATE_HOME']; $homePath = [string]$Environment['HOME']
    $path = if ($state) { if (-not $state.StartsWith('/')) { Exit-MuxAgentCommand 1 'state' 'MUX_AGENTS_STATE is not an absolute path' }; $state }
    elseif ($xdg.StartsWith('/')) { $xdg.TrimEnd('/') + '/mux-agents/state' }
    elseif ($homePath.StartsWith('/')) { $homePath.TrimEnd('/') + '/.local/state/mux-agents/state' }
    else { Exit-MuxAgentCommand 1 'state' 'HOME is not an absolute path' }
    Resolve-MuxAgentUnixRealPath -Path $path
}
function Assert-MuxAgentUnixPrivate {
    # The Unix privacy decision on gathered facts: refuses 1 'state' at the first failed check, in this order.
    param([Parameter(Mandatory)]$Facts, [Parameter(Mandatory)][string]$User)
    if ($Facts.Kind -ceq 'link') { Exit-MuxAgentCommand 1 'state' 'the state folder path has a link' }
    if ($Facts.Kind -ceq 'file') { Exit-MuxAgentCommand 1 'state' 'the state folder path is a file' }
    if ($Facts.Kind -cne 'directory') { Exit-MuxAgentCommand 1 'state' 'the state folder is missing' }
    if ($Facts.Real -cne $Facts.Path) { Exit-MuxAgentCommand 1 'state' 'the state folder path has a link' }
    if ($Facts.Owner -cne $User) { Exit-MuxAgentCommand 1 'state' 'the state folder has another owner' }
    if ([int]$Facts.Mode -ne 448) { Exit-MuxAgentCommand 1 'state' "the state folder mode is $([Convert]::ToString([int]$Facts.Mode, 8)), not 700" }
    if ("$($Facts.Listing)".Contains('+') -or @($Facts.Aces).Count) { Exit-MuxAgentCommand 1 'state' 'the state folder has an extended ACL' }
}
function Get-MuxAgentUnixEntry {
    # What Path itself is, without following a final link (lstat): link, directory, file or missing.
    param([Parameter(Mandatory)][string]$Path)
    $info = [IO.FileInfo]::new($Path)
    if ($null -ne $info.LinkTarget) { 'link' } elseif ([IO.Directory]::Exists($Path)) { 'directory' } elseif ($info.Exists) { 'file' } else { 'missing' }
}
function Get-MuxAgentUnixUserId {
    # The effective uid as digits (id -u).
    $id = Invoke-MuxAgentUnixTool -FilePath 'id' -Arguments @('-u') -Message 'the user id cannot be read'
    if ($id -cnotmatch '^\d+\z') { Exit-MuxAgentCommand 1 'state' 'the user id cannot be read' }
    $id
}
function Get-MuxAgentUnixStat {
    # One stat of Path itself (lstat semantics on both OSes): dev:inode, the full mode with setuid, setgid and sticky
    # bits, and the owner uid, plus the verbatim Text for comparing two reads. A failed or out-of-form read refuses.
    param([Parameter(Mandatory)][string]$Path, [string]$Platform = (Get-MuxAgentPlatform))
    $message = 'the state folder cannot be checked'
    $statArgs = if ($Platform -ceq 'macos') { @('-f', '%d:%i %Mp%Lp %u', '--', $Path) } else { @('-c', '%d:%i %a %u', '--', $Path) }
    $text = Invoke-MuxAgentUnixTool -FilePath 'stat' -Arguments $statArgs -Message $message
    if ($text -cnotmatch '^(\d+:\d+) ([0-7]{3,4}) (\d+)\z') { Exit-MuxAgentCommand 1 'state' $message }
    [pscustomobject]@{ Identity = $Matches[1]; Mode = [Convert]::ToInt32($Matches[2], 8); Owner = $Matches[3]; Text = $text }
}
function Get-MuxAgentUnixPathFacts {
    # The facts Assert-MuxAgentUnixPrivate decides on, bound to one folder: stat (dev:inode, mode, uid; lstat
    # semantics), ls -ld (macOS ls -lde: ACE lines, inherited ones too) and realpath, then stat again, which must be
    # equal. Any failed or out-of-form read, or a folder that vanishes, refuses: an unreadable signal never passes.
    param([Parameter(Mandatory)][string]$Path, [string]$Platform = (Get-MuxAgentPlatform))
    $message = 'the state folder cannot be checked'
    $kind = try { Get-MuxAgentUnixEntry -Path $Path } catch { Exit-MuxAgentCommand 1 'state' $message }
    $facts = [pscustomobject]@{ Path = $Path; Kind = $kind; Identity = $null; Mode = $null; Owner = $null; Listing = ''; Aces = @(); Real = $null }
    if ($kind -cne 'directory') { return $facts }
    $macos = $Platform -ceq 'macos'
    $first = Get-MuxAgentUnixStat -Path $Path -Platform $Platform
    $facts.Identity = $first.Identity; $facts.Mode = $first.Mode; $facts.Owner = $first.Owner
    $lines = @((Invoke-MuxAgentUnixTool -FilePath 'ls' -Arguments @($(if ($macos) { '-lde' } else { '-ld' }), '--', $Path) -Message $message) -split '\n')
    # The permission field position by position: type d, then r w x per class, with s/S (owner, group) or t/T (other).
    if ($lines[0] -cnotmatch '^(d[r-][w-][xsS-][r-][w-][xsS-][r-][w-][xtT-][+@.]?)\s+\d+\s') { Exit-MuxAgentCommand 1 'state' $message }
    $facts.Listing = $Matches[1]
    $facts.Aces = @($lines | Select-Object -Skip 1)
    $facts.Real = Invoke-MuxAgentUnixTool -FilePath 'realpath' -Arguments @('--', $Path) -Message $message
    if (-not $facts.Real.StartsWith('/') -or $facts.Real -match '[\r\n]') { Exit-MuxAgentCommand 1 'state' $message }
    $second = Get-MuxAgentUnixStat -Path $Path -Platform $Platform
    if ($second.Text -cne $first.Text) { Exit-MuxAgentCommand 1 'state' 'the state folder changed while it was checked' }
    $facts
}
function Assert-MuxAgentUnixParent {
    # A folder above the state root, as OpenSSH StrictModes checks one: owned by root or User, and not group- or
    # other-writable unless the sticky bit is set (as /tmp is). Refuses 1 'state' otherwise.
    param([Parameter(Mandatory)]$Stat, [Parameter(Mandatory)][string]$User)
    if ($Stat.Owner -cne '0' -and $Stat.Owner -cne $User) { Exit-MuxAgentCommand 1 'state' 'a folder above the state folder has another owner' }
    if (($Stat.Mode -band 18) -and -not ($Stat.Mode -band 512)) { Exit-MuxAgentCommand 1 'state' 'a folder above the state folder is writable by others' }
}
function Invoke-MuxAgentUnixMkdir {
    # The one mkdir of the Unix state code: mkdir(2) with the mode, so the folder is never created wider. It throws when
    # anything is already at Path, a link included, so a link put there after the entry check is never followed.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][IO.UnixFileMode]$Mode, [string]$Platform = (Get-MuxAgentPlatform))
    Import-MuxAgentUnixNative
    $errno = if ($Platform -ceq 'macos') { [MuxAgentUnixNative]::MkdirMac($Path, [int]$Mode) } else { [MuxAgentUnixNative]::MkdirLinux($Path, [int]$Mode) }
    if ($errno -ne 0) { throw [IO.IOException]::new("mkdir failed: errno $errno") }
}
function Invoke-MuxAgentUnixChmod {
    # chmod of one folder: restores the bits the umask cleared at mkdir.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][IO.UnixFileMode]$Mode)
    [IO.File]::SetUnixFileMode($Path, $Mode)
}
function New-MuxAgentUnixDirectory {
    # Creates Path when nothing is there (lstat), at 0700 in the mkdir call itself, so it is never wider; then chmod
    # 0700 restores what the umask cleared. Returns whether it created Path. An existing entry, link or not, is left
    # for the caller's check. A failed entry read, mkdir or chmod refuses 1 'state'; mkdir fails on an entry that
    # appeared after the entry read. Once mkdir succeeded, chmod reaches that folder: its parent was checked first
    # (ours at 0700, or sticky), so no other user can rename or replace it.
    param([Parameter(Mandatory)][string]$Path)
    $kind = try { Get-MuxAgentUnixEntry -Path $Path } catch { Exit-MuxAgentCommand 1 'state' 'the state folder cannot be checked' }
    if ($kind -cne 'missing') { return $false }
    $private = [IO.UnixFileMode]'UserRead, UserWrite, UserExecute'
    try { Invoke-MuxAgentUnixMkdir -Path $Path -Mode $private } catch { Exit-MuxAgentCommand 1 'state' 'the state folder cannot be created' }
    try { Invoke-MuxAgentUnixChmod -Path $Path -Mode $private } catch { Exit-MuxAgentCommand 1 'state' 'the state folder cannot be created' }
    $true
}
function New-MuxAgentUnixPrivateDirectories {
    # B4: the PrivateDirectories member, given the selected StateRoot as Root. Walks each path from / down. Every
    # existing folder above the root is checked as Assert-MuxAgentUnixParent says; each missing folder is created at
    # 0700; the root, every folder below it and every folder this call created get the full check. Each folder is
    # checked once per call, before anything is made inside it. Refuses 1 'state' at the first failure.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string[]]$Paths)
    foreach ($path in $Paths) {
        $inside = $path -ceq $Root -or $path.StartsWith($Root + '/', [StringComparison]::Ordinal)
        $parts = @($path.Substring(1).Split('/'))
        if (-not $Root.StartsWith('/') -or -not $inside -or $parts -contains '' -or $parts -contains '.' -or $parts -contains '..') { Exit-MuxAgentCommand 1 'state' 'the state folder path is outside the state root' }
    }
    $user = Get-MuxAgentUnixUserId
    Assert-MuxAgentUnixParent -Stat (Get-MuxAgentUnixStat -Path '/') -User $user
    $checked = [Collections.Generic.HashSet[string]]::new([string[]]@('/'))
    foreach ($path in $Paths) {
        $prefix = ''
        foreach ($part in $path.Substring(1).Split('/')) {
            $prefix += '/' + $part
            if ($checked.Contains($prefix)) { continue }
            $created = New-MuxAgentUnixDirectory -Path $prefix
            if ($created -or $prefix -ceq $Root -or $prefix.StartsWith($Root + '/', [StringComparison]::Ordinal)) {
                Assert-MuxAgentUnixPrivate -Facts (Get-MuxAgentUnixPathFacts -Path $prefix) -User $user
            }
            else { Assert-MuxAgentUnixParent -Stat (Get-MuxAgentUnixStat -Path $prefix) -User $user }
            $null = $checked.Add($prefix)
        }
    }
}
