# Unix provider bodies. Untagged: every body runs on every OS against New-UnixFakeKernel and fixtures; the real kernels run in MuxAgents.Unix.Native.Tests.ps1 (tagged linux / macos).
$repoRoot = Split-Path -Parent $PSScriptRoot
foreach ($name in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') { . (Join-Path (Join-Path $repoRoot 'scripts') $name) }
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')
function Get-UnixRefusal { param([Parameter(Mandatory)][scriptblock]$Action) try { $null = & $Action; 'none' } catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" } }
# KERN_PROCARGS2 bytes: int argc, exec path, NUL padding to (4 + 8k), then the strings.
function New-UnixProcArgs([int]$Argc, [string]$Rest) { [byte[]](@([BitConverter]::GetBytes($Argc)) + [Text.Encoding]::UTF8.GetBytes($Rest)) }
Describe 'MuxAgents unix native parsers' {
    Import-MuxAgentUnixNative
    It 'ParseProcArgs reads exactly argc strings from the aligned argv start; any other layout is unreadable' {
        $good = New-UnixProcArgs 2 "/bin/sleep`0`0`0`0`0`0sleep`060`0A=1`0"
        # Each join is parenthesized: -join binds looser than the comma, so without them $answers would hold one string.
        $answers = @((([MuxAgentUnixNative]::ParseProcArgs($good, $good.Length)) -join ' '), (([MuxAgentUnixNative]::ParseProcArgs((New-UnixProcArgs 3 "/bin/sleep`0`0`0`0`0`0sleep`060`0A=1`0"), 33)) -join ' '))
        foreach ($case in @(@(2, "/bin/x`0`0`0--forbidden`0A=1`0B=2`0"), @(2, "/bin/sleep`0`0`0sleep`060`0A=1`0"), @(-1, "/bin/sleep`0`0`0`0`0`0sleep`0"))) {
            $bytes = New-UnixProcArgs $case[0] $case[1]; $answers += "$($null -eq [MuxAgentUnixNative]::ParseProcArgs($bytes, $bytes.Length))" }
        $answers += "$($null -eq [MuxAgentUnixNative]::ParseProcArgs($good, 28))|$($null -eq [MuxAgentUnixNative]::ParseProcArgs($good, 3))"
        ($answers -join ';') | Should BeExactly 'sleep 60;sleep 60 A=1;True;True;True;True|True' }
    It 'PollShowsExit: exit only for one entry with POLLIN and neither POLLERR nor POLLNVAL' {
        ((@(@(1, 1), @(1, 17), @(0, 0), @(1, 32), @(1, 33), @(1, 9), @(1, 16), @(-1, 0)) | ForEach-Object { "$([MuxAgentUnixNative]::PollShowsExit($_[0], $_[1]))" }) -join ',') | Should BeExactly 'True,True,False,False,False,False,False,False' }
    It 'ParseVnodeCwd reads the cdir path at byte 152 of a 2352-byte proc_vnodepathinfo, else nothing' {
        $info = [byte[]]::new(2352); $path = [Text.Encoding]::UTF8.GetBytes('/Users/me/work')
        [Array]::Copy($path, 0, $info, 152, $path.Length); [Array]::Copy($path, 0, $info, 1328, 6)
        "$([MuxAgentUnixNative]::ParseVnodeCwd($info, 2352))|$($null -eq [MuxAgentUnixNative]::ParseVnodeCwd($info, 2351))" | Should BeExactly '/Users/me/work|True' }
    It 'EpochMicros is whole microseconds since 1970 UTC for any DateTime kind' {
        $utc = [DateTime]::new(2026, 10, 30, 0, 0, 0, [DateTimeKind]::Utc).AddTicks(12345679)
        "$([MuxAgentUnixNative]::EpochMicros($utc))|$([MuxAgentUnixNative]::EpochMicros($utc.ToLocalTime()))" | Should BeExactly '1793318401234567|1793318401234567' } }
Describe 'MuxAgents unix pure members' {
    It 'Endpoint takes only TMUX (no CR or LF anywhere) matching ^(/[^,\r\n]+),(\d+),(-?\d+)\z and keeps the server pid as text' {
        $answers = foreach ($env in @(@{ TMUX = '/tmp/tmux-1000/default,0900,-1' }, @{ TMUX = '/tmp/my socket,7,0' }, @{ RMUX = '/tmp/r,7,0' }, @{ TMUX = '\.\pipe\rmux,7,0' }, @{ TMUX = 'tmp/x,7,0' }, @{ TMUX = '/a,b,7,0' }, @{ TMUX = "/a`n/b,7,0" }, @{ TMUX = "/a`r/b,7,0" }, @{ TMUX = "/tmp/t,7,0`r" }, @{ TMUX = "/tmp/t,7,0`n" }, @{ TMUX = '/a,7,x' }, @{})) {
            $parsed = Get-MuxAgentUnixEndpoint -Environment $env; if ($parsed) { "$($parsed.Socket)|$($parsed.ServerPid)" } else { 'none' } }
        ($answers -join ';') | Should BeExactly '/tmp/tmux-1000/default|0900;/tmp/my socket|7;none;none;none;none;none;none;none;none;none;none' }
    It 'IsShell allows exactly sh, bash, zsh, fish, dash, ksh, tcsh and pwsh by basename, case-sensitive' {
        (@(foreach ($p in '/bin/sh', '/usr/bin/bash', '/bin/zsh', '/usr/local/bin/fish', '/bin/dash', '/bin/ksh', '/bin/tcsh', '/opt/microsoft/powershell/7/pwsh', '/bin/Bash', '/bin/pwsh.exe', '/usr/bin/node', '/bin/csh', $null) { "$(Test-MuxAgentUnixShell -Row ([pscustomobject]@{ Path = $p }))" }) -join ',') | Should BeExactly 'True,True,True,True,True,True,True,True,False,False,False,False,False' }
    It 'SameStart compares kernel starts exactly; an unreadable start is never the same' {
        "$(Test-MuxAgentUnixSameStart 1009 1009) $(Test-MuxAgentUnixSameStart 1009 1000) $(Test-MuxAgentUnixSameStart $null 0) $(Test-MuxAgentUnixSameStart 0 $null) $(Test-MuxAgentUnixSameStart 1793318401234567 1793318401234567)" | Should BeExactly 'True False False False True' }
    It 'Argv returns the row argv elements and throws when argv is unreadable, never splitting CommandLine' {
        $one = @(Get-MuxAgentUnixArgv -Row ([pscustomobject]@{ Argv = [string[]]@('claude'); CommandLine = 'x y' }))
        $three = @(Get-MuxAgentUnixArgv -Row ([pscustomobject]@{ Argv = [string[]]@('claude', '--model', 'a b'); CommandLine = 'claude --model a b' }))
        "$($one.Count)|$($three.Count)|$($three[2])|$(Get-UnixRefusal { Get-MuxAgentUnixArgv -Row ([pscustomobject]@{ Argv = $null; CommandLine = 'claude --model m' }) })" | Should BeExactly '1|3|a b|  mux-agent: argv is unreadable' }
    It 'StopReport lists every row stop did not kill, whatever the reason; Windows counts only kill' {
        $left = @(foreach ($pair in @(@(310, 'kill'), @(320, 'start'), @(330, 'open'))) { [pscustomobject]@{ Pid = $pair[0]; Reason = $pair[1]; Row = $null } })
        $full = Get-MuxAgentUnixStopReport -Left $left; $none = Get-MuxAgentUnixStopReport -Left @()
        $windows = & (Get-MuxAgentPlatformMembers -Platform windows).StopReport $left
        "$(@($full.Keys) -join ',')|$($full.survivors)|$($full.survivorPids -join ',')|$($full.survivorPids.GetType().Name)|$($none.survivors)|$($none.survivorPids.Count)|$($none.survivorPids.GetType().Name)|$($windows.survivors)" | Should BeExactly 'survivors,survivorPids|3|310,320,330|Int32[]|0|0|Int32[]|1' } }
Describe 'MuxAgents unix client identity' {
    $loader = '/usr/local/lib/node_modules/@github/copilot/npm-loader.js'
    $links = @{ '/usr/local/bin/copilot' = $loader; '/p/bin/copilot' = '/p/node_modules/@github/copilot/npm-loader.js'; 'bin/copilot' = $loader }
    $cwds = @{ 401 = '/p'; 402 = '/q' }
    $kernel = [pscustomobject]@{ ResolveFile = { param($Path) if ($Path -eq '/broken') { throw 'loop' }; if ($links.ContainsKey($Path)) { $links[$Path] } else { $Path } }.GetNewClosure()
        ReadCwd = { param([int]$Id) $cwds[$Id] }.GetNewClosure() }
    function Get-UnixClient($Path, [string[]]$Argv, [int]$Id = 400) { "$(Get-MuxAgentUnixClient -Row ([pscustomobject]@{ Pid = $Id; Path = $Path; Argv = $Argv }) -Kernel $kernel)" }
    It 'names claude by */claude/versions/* or basename claude, copilot by basename or by node running the loader' {
        (@((Get-UnixClient '/home/u/.local/share/claude/versions/2.1.3' @('claude')), (Get-UnixClient '/opt/claude' @('x')), (Get-UnixClient '/usr/bin/copilot' @('copilot')),
            (Get-UnixClient '/usr/bin/node' @('node', '/usr/local/bin/copilot', '--allow-all')), (Get-UnixClient '/usr/bin/node' @('node', $loader)),
            (Get-UnixClient '/usr/bin/node' @('node', '--no-warnings', $loader)), (Get-UnixClient '/usr/bin/node' @('node', '-r', 'hook.js', '--import', 'x.mjs', '--', $loader)),
            (Get-UnixClient '/usr/bin/node' @('node', 'bin/copilot') 401)) -join ',') | Should BeExactly 'claude,claude,copilot,copilot,copilot,copilot,copilot,copilot' }
    It 'skips the value of --require and --loader: the real loader after either is copilot' {
        "$(Get-UnixClient '/usr/bin/node' @('node', '--require', 'hook.js', $loader)),$(Get-UnixClient '/usr/bin/node' @('node', '--loader', 'hook.mjs', $loader))" | Should BeExactly 'copilot,copilot' }
    It 'names no client when the loader is only the value of --require or --loader and the script is another file' {
        "[$(Get-UnixClient '/usr/bin/node' @('node', '--require', $loader, '/srv/app.js'))],[$(Get-UnixClient '/usr/bin/node' @('node', '--loader', $loader, '/srv/app.js'))]" | Should BeExactly '[],[]' }
    It 'skips an --option=value and the known flags that take no value' {
        "$(Get-UnixClient '/usr/bin/node' @('node', '--max-old-space-size=4096', '--enable-source-maps', '--no-deprecation', $loader))" | Should BeExactly 'copilot' }
    It 'names no client after any other option, which may take the loader as its value' {
        "[$(Get-UnixClient '/usr/bin/node' @('node', '--conditions', $loader, '/srv/app.js'))],[$(Get-UnixClient '/usr/bin/node' @('node', '-C', $loader, '/srv/app.js'))],[$(Get-UnixClient '/usr/bin/node' @('node', '--some-new-flag', $loader))]" | Should BeExactly '[],[],[]' }
    It 'names no client for argv[0] alone, another exe, other case, eval code, an option value, another script, or an unreadable path or cwd' {
        $answers = @((Get-UnixClient '/usr/bin/sleep' @('claude')), (Get-UnixClient '/usr/bin/bash' @('copilot', '--allow-all')), (Get-UnixClient '/usr/bin/claude.exe' @('claude')),
            (Get-UnixClient '/opt/Claude' @('claude')), (Get-UnixClient '/x/claude/versionsX/1' @('claude')), (Get-UnixClient '/usr/bin/copilot-cli' @('copilot')),
            (Get-UnixClient '/usr/bin/node' @('node', '/srv/app.js')), (Get-UnixClient '/usr/bin/node' @('node', '/x/@github/copilot/npm-loader.js.bak')),
            (Get-UnixClient '/usr/bin/bun' @('bun', '/usr/local/bin/copilot')), (Get-UnixClient '/usr/bin/node' @('node')), (Get-UnixClient '/usr/bin/node' @('node', '/broken')), (Get-UnixClient $null @('claude')),
            (Get-UnixClient '/usr/bin/node' @('node', '-e', 'require(1)', $loader)), (Get-UnixClient '/usr/bin/node' @('node', '-p', '1', $loader)), (Get-UnixClient '/usr/bin/node' @('node', '--eval=1', $loader)),
            (Get-UnixClient '/usr/bin/node' @('node', '-r', $loader, '/srv/app.js')), (Get-UnixClient '/usr/bin/node' @('node', 'bin/copilot') 402), (Get-UnixClient '/usr/bin/node' @('node', 'bin/copilot') 403))
        ($answers -join ',') | Should BeExactly (',' * 17) } }
# Fake kernel: Unreadable[pid] makes ReadStart return $null while the pid stays in Live (alive).
function New-UnixFakeKernel {
    $k = [pscustomobject]@{ Stat = @{}; Exe = @{}; Cmdline = @{}; Ps = ''; Path = @{}; Argv = @{}; Live = @{}; Cwd = @{}; Status = "Name:`tpwsh`nNoNewPrivs:`t0`n"; Sandbox = 0
        Pidfd = $true; Fds = @{}; NextFd = 1000; Sent = [Collections.Generic.List[string]]::new(); Closed = [Collections.Generic.List[int]]::new(); Log = [Collections.Generic.List[string]]::new(); Sleeps = 0; OnOpen = $null; OnSleep = $null; Unreadable = @{} }
    $members = [ordered]@{
        ListIds = { @($k.Stat.Keys) }; ReadStat = { param([int]$Id) if ($null -eq $k.Stat[$Id]) { throw 'gone' }; $k.Stat[$Id] }
        ReadExe = { param([int]$Id) $k.Exe[$Id] }; ReadCmdline = { param([int]$Id) $k.Cmdline[$Id] }
        ListPs = { $k.Ps }; ReadPath = { param([int]$Id) $k.Path[$Id] }; ReadArgv = { param([int]$Id) $k.Argv[$Id] }
        ReadStart = { param([int]$Id) $k.Log.Add("start $Id"); if ($k.Unreadable.ContainsKey($Id)) { return $null }; $k.Live[$Id] }; ReadCwd = { param([int]$Id) $k.Log.Add("cwd $Id"); $k.Cwd[$Id] }
        ReadStatus = { if ($null -eq $k.Status) { throw 'denied' }; $k.Status }; SandboxCheck = { if ($null -eq $k.Sandbox) { throw 'no sandbox_check' }; $k.Sandbox }
        ResolveFile = { param($Path) $Path }; IsGone = { param([int]$Id) -not $k.Live.ContainsKey($Id) }
        PidfdOpen = { param([int]$Id) $k.Log.Add("pidfd $Id"); if (-not $k.Pidfd -or -not $k.Live.ContainsKey($Id)) { return -1 }; $fd = $k.NextFd; $k.NextFd = $fd + 1; $k.Fds[$fd] = "$Id/$($k.Live[$Id])"; if ($k.OnOpen) { & $k.OnOpen $Id }; $fd }
        SendKill = { param([int]$Fd) $k.Log.Add("send $Fd"); $id, $start = "$($k.Fds[$Fd])" -split '/'; if ($id -and $k.Live.ContainsKey([int]$id) -and "$($k.Live[[int]$id])" -eq $start) { $k.Sent.Add($k.Fds[$Fd]); $k.Live.Remove([int]$id); $true } else { $false } }
        WaitExit = { param([int]$Fd, [int]$Milliseconds) $id, $start = "$($k.Fds[$Fd])" -split '/'; -not ($id -and $k.Live.ContainsKey([int]$id) -and "$($k.Live[[int]$id])" -eq $start) }
        Close = { param([int]$Fd) $k.Closed.Add($Fd) }; Sleep = { param([int]$Milliseconds) $k.Sleeps++; if ($k.OnSleep) { & $k.OnSleep $k.Sleeps } } }
    foreach ($name in $members.Keys) { Add-Member -InputObject $k -NotePropertyName $name -NotePropertyValue $members[$name].GetNewClosure() }
    $k }
# A /proc/<pid>/stat line: starttime is field 22, the 20th field after the last ')'.
function New-UnixStatLine([int]$Id, [string]$Comm, [int]$Parent, [long]$Start) { "$Id ($Comm) S $Parent $Id $Id 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 $Start 1000000 200 0" }
function Format-UnixRow($Row) { "$($Row.Pid) $($Row.ParentPid) $($Row.Start) $($Row.Path) [$($Row.CommandLine)] $(if ($null -ne $Row.Argv) { "$($Row.Argv.GetType().Name)$($Row.Argv.Count)" } else { 'null' })" }
Describe 'MuxAgents unix linux process table' {
    It 'reads pid, parent and raw start after the last ), the exe link and argv; skips a pid that exited' {
        $k = New-UnixFakeKernel
        $k.Stat[300] = New-UnixStatLine 300 'a) (b' 100 4321; $k.Exe[300] = '/usr/bin/node'; $k.Cmdline[300] = [string[]]@('node', '/x/loader.js')
        $k.Stat[301] = $null; $k.Stat[302] = New-UnixStatLine 302 'sleep' 300 99; $k.Cmdline[302] = [string[]]@('sleep'); $k.Stat[303] = New-UnixStatLine 303 'kworker/0:1' 2 5
        (@(Get-MuxAgentLinuxProcessTable -Kernel $k | Sort-Object Pid | ForEach-Object { Format-UnixRow $_ }) -join ';') | Should BeExactly '300 100 4321 /usr/bin/node [node /x/loader.js] String[]2;302 300 99  [sleep] String[]1;303 2 5  [] null' }
    It 'parses a comm holding ") (" and a newline to the exact pid, ppid and raw start' {
        $stat = ConvertFrom-MuxAgentLinuxStat -Text (New-UnixStatLine 300 "a) (b`nc" 100 4321)
        "$($stat.Pid) $($stat.ParentPid) $($stat.Start)" | Should BeExactly '300 100 4321' }
    It 'rejects stat text cut inside starttime or missing a field: no row and no client' {
        $line = New-UnixStatLine 300 'claude' 100 4321; $cut = $line.Substring(0, $line.IndexOf(' 4321 ') + 3)
        $k = New-UnixFakeKernel; $k.Stat[300] = $cut; $k.Exe[300] = '/opt/claude'; $k.Cmdline[300] = [string[]]@('claude')
        $rows = @(Get-MuxAgentLinuxProcessTable -Kernel $k)
        $bad = @($cut, 'x', '300 (sh) S 100', '', 'abc (sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 1') | ForEach-Object { "$($null -eq (ConvertFrom-MuxAgentLinuxStat -Text $_))" }
        "$($rows.Count)|$(@($rows | Where-Object { Get-MuxAgentUnixClient -Row $_ -Kernel $k }).Count)|$($bad -join ',')" | Should BeExactly '0|0|True,True,True,True,True' }
    It 'the production cmdline splitter splits only on NUL, drops exactly the final terminator and refuses data without it' {
        $enc = [Text.Encoding]::UTF8
        $a = @(ConvertFrom-MuxAgentLinuxCmdline -Bytes $enc.GetBytes("node`0script.js`0`0")); $c = @(ConvertFrom-MuxAgentLinuxCmdline -Bytes $enc.GetBytes("a b`0"))
        "$($a.Count)|$($a -join '/')|$($null -eq (ConvertFrom-MuxAgentLinuxCmdline -Bytes $enc.GetBytes("claude`0--model")))|$($c.Count)|$($c[0])|$($null -eq (ConvertFrom-MuxAgentLinuxCmdline -Bytes ([byte[]]@())))" | Should BeExactly '3|node/script.js/|True|1|a b|True' } }
Describe 'MuxAgents unix macos process table' {
    It 'takes pid and ppid from ps, start and path from Process, argv from KERN_PROCARGS2; skips a pid that exited' {
        $k = New-UnixFakeKernel; $k.Ps = "  PID  PPID`n  400     1`n  401   400`n  402   400`n"; $k.Live[400] = 1793318401234567; $k.Live[402] = 7
        $k.Path[400] = '/usr/local/bin/copilot'; $k.Argv[400] = [string[]]@('copilot')
        (@(Get-MuxAgentMacProcessTable -Kernel $k | ForEach-Object { Format-UnixRow $_ }) -join ';') | Should BeExactly '400 1 1793318401234567 /usr/local/bin/copilot [copilot] String[]1;402 400 7  [] null' }
    It 'an argv layout that does not match argc (empty argv[0], then --forbidden and the environment) is unreadable, so C3 refuses' {
        Import-MuxAgentUnixNative
        $bytes = New-UnixProcArgs 2 "/bin/x`0`0`0--forbidden`0A=1`0B=2`0"
        $k = New-UnixFakeKernel; $k.Ps = "  150   100`n"; $k.Live[150] = 1500; $k.Path[150] = '/x/claude'; $k.Argv[150] = [MuxAgentUnixNative]::ParseProcArgs($bytes, $bytes.Length)
        $row = @(Get-MuxAgentMacProcessTable -Kernel $k)[0]
        $fake = New-FakeMuxAgentProviders -Caller claude
        foreach ($r in $fake.Rows) { if ($r.Pid -eq 150) { $r.Path = '/x/claude'; Add-Member -InputObject $r -NotePropertyName Argv -NotePropertyValue $row.Argv -Force } }
        $fake.ClientOf = { param($Row) if ($Row.Pid -eq 150) { 'claude' } }; $fake.IsShell = { param($Row) $true }; $fake.Argv = { param($Row) Get-MuxAgentUnixArgv -Row $Row }
        "$($null -eq $row.Argv)|$(Get-UnixRefusal { Assert-MuxAgentSpawnAllowed -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) })" | Should BeExactly 'True|2 flags a client command line cannot be read' } }
Describe 'MuxAgents unix kernels' {
    It 'each kernel has exactly its reads, and the fake has every one' {
        $linux = @((New-MuxAgentLinuxKernel).PSObject.Properties.Name | Sort-Object); $mac = @((New-MuxAgentMacKernel).PSObject.Properties.Name | Sort-Object); $fake = @((New-UnixFakeKernel).PSObject.Properties.Name)
        "$($linux -join ',')|$($mac -join ',')|$(@(($linux + $mac) | Where-Object { $_ -notin $fake }).Count)" | Should BeExactly 'Close,IsGone,ListIds,PidfdOpen,ReadCmdline,ReadCwd,ReadExe,ReadStart,ReadStat,ReadStatus,ResolveFile,SendKill,Sleep,WaitExit|Close,IsGone,ListPs,PidfdOpen,ReadArgv,ReadCwd,ReadPath,ReadStart,ResolveFile,SandboxCheck,SendKill,Sleep,WaitExit|0' }
    It 'macOS opens no pidfd and signals nothing' {
        $mac = New-MuxAgentMacKernel
        "$(& $mac.PidfdOpen 1)|$(& $mac.SendKill 3)|$(& $mac.WaitExit 3 0)" | Should BeExactly '-1|False|False' }
    It 'IsGone is true only for ESRCH from kill(pid, 0) on that pid; EPERM, another errno or a probe that throws is not gone' {
        $probes = @({ param($Id) 0 }, { param($Id) 1 }, { param($Id) if ($Id -eq 300) { 3 } else { 0 } }, { param($Id) 22 }, { param($Id) throw [DllNotFoundException]::new('libc') })
        $answers = foreach ($probe in $probes) { "$(Test-MuxAgentUnixGone -Probe $probe -Id 300)" }
        "$($answers -join ',')|$(Test-MuxAgentUnixGone -Probe $probes[2] -Id 301)" | Should BeExactly 'False,False,True,False,False|False' }
    It 'each production IsGone answers not gone for this live process, whether its probe runs (its own OS) or fails (no such libc)' {
        "$(& (New-MuxAgentLinuxKernel).IsGone $PID)|$(& (New-MuxAgentMacKernel).IsGone $PID)" | Should BeExactly 'False|False' } }
Describe 'MuxAgents unix sandbox' {
    It 'linux refuses SANDBOX_RUNTIME=1, unreadable status, no NoNewPrivs line or NoNewPrivs 1, naming the signal' {
        $answers = foreach ($case in @(@('1', "NoNewPrivs:`t0`n"), @('', $null), @('', "Name:`tx`n"), @('', "Name:`tx`nNoNewPrivs:`t1`n"), @('0', "NoNewPrivs:`t0`n"))) {
            $k = New-UnixFakeKernel; $k.Status = $case[1]; Get-UnixRefusal { Assert-MuxAgentLinuxSandbox -Environment @{ SANDBOX_RUNTIME = $case[0] } -Kernel $k } }
        ($answers -join ';') | Should BeExactly '2 sandbox SANDBOX_RUNTIME=1 is set;2 sandbox /proc/self/status is unreadable;2 sandbox NoNewPrivs is unreadable;2 sandbox NoNewPrivs is set;none' }
    It 'macos refuses SANDBOX_RUNTIME=1, a non-zero sandbox_check or a failed one, naming the signal' {
        $answers = foreach ($case in @(@('1', 0), @('', 1), @('', -1), @('', $null), @('', 0))) {
            $k = New-UnixFakeKernel; $k.Sandbox = $case[1]; Get-UnixRefusal { Assert-MuxAgentMacSandbox -Environment @{ SANDBOX_RUNTIME = $case[0] } -Kernel $k } }
        ($answers -join ';') | Should BeExactly '2 sandbox SANDBOX_RUNTIME=1 is set;2 sandbox sandbox_check reports a sandbox;2 sandbox sandbox_check reports a sandbox;2 sandbox sandbox_check failed;none' } }
function New-UnixStopFake {
    # B1 T8's stop scenario (agent 'worker', pane %9, tree 300 -> 310, 320); Unix Open, Kill, SameStart, StopReport over $global:unixStopKernel.
    $fake = New-FakeMuxAgentProviders; $fake.PrivateDirectories = { param([string[]]$Paths) foreach ($path in $Paths) { $null = [IO.Directory]::CreateDirectory($path) } }
    Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$3|`n"
    $folder = [IO.Path]::Combine((Get-MuxAgentStateDirectory -P $fake -Caller (Resolve-MuxAgentCaller -P $fake)), 'agents', 'aaaaaaaaaaaa'); $null = [IO.Directory]::CreateDirectory($folder)
    Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = 'aaaaaaaaaaaa'; name = 'worker'; client = 'claude'; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'; context = 'brief'; reason = 'human'; session = '$3'; pane = '%9'; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $fake.Cwd; createdAt = 'x' })
    Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
    Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%1|@1|100||`n%9|@5|300|aaaaaaaaaaaa|worker`n"
    foreach ($row in @(@(300, 900, '/usr/bin/pwsh'), @(310, 300, '/usr/bin/claude'), @(320, 300, '/usr/bin/sleep'))) { $fake.Rows.Add((New-FakeMuxAgentRow $row[0] $row[1] $row[2] 'x')) }
    foreach ($id in 900, 300, 310, 320) { $global:unixStopKernel.Live[$id] = $id * 10 }   # the fake rows' starts, so the server key is unchanged
    $fake.Open = { param([int]$Id) Open-MuxAgentUnixProcess -Id $Id -Kernel $global:unixStopKernel }; $fake.Kill = { param($Opened) Stop-MuxAgentUnixProcess -Opened $Opened -Kernel $global:unixStopKernel }
    $fake.SameStart = { param($Opened, $Listed) Test-MuxAgentUnixSameStart $Opened $Listed }; $fake.StopReport = { param($Left) Get-MuxAgentUnixStopReport -Left $Left }
    $fake }
Describe 'MuxAgents unix open and kill' {
    It 'Open takes the pidfd first, then the start; a pid gone before or after pidfd_open opens nothing and leaks no fd' {
        $k = New-UnixFakeKernel; $k.Live[300] = 3000; $k.Live[301] = 3010; $k.OnOpen = { param($Id) if ($Id -eq 301) { $k.Live.Remove(301) } }.GetNewClosure()
        $opened = Open-MuxAgentUnixProcess -Id 300 -Kernel $k
        "$($opened.Id) $($opened.Start) $($opened.Fd) $($null -eq $opened.Process)|$($null -eq (Open-MuxAgentUnixProcess -Id 301 -Kernel $k))|$($null -eq (Open-MuxAgentUnixProcess -Id 302 -Kernel $k))|$($k.Log -join ',')|$($k.Closed -join ',')" | Should BeExactly '300 3000 1000 True|True|True|pidfd 300,start 300,pidfd 301,start 301,pidfd 302,start 302|1001' }
    It 'Kill sends SIGKILL on the opened pidfd, waits for exit and closes the fd' {
        $k = New-UnixFakeKernel; $k.Live[300] = 3000; $opened = Open-MuxAgentUnixProcess -Id 300 -Kernel $k
        "$(Stop-MuxAgentUnixProcess -Opened $opened -Kernel $k)|$($k.Sent -join ',')|$($k.Closed -join ',')|$($k.Live.ContainsKey(300))" | Should BeExactly 'True|300/3000|1000|False' }
    It 'a PID reused after the last check is never signalled: the fd stays bound to the old process' {
        $k = New-UnixFakeKernel; $k.Live[300] = 3000; $opened = Open-MuxAgentUnixProcess -Id 300 -Kernel $k; $k.Live[300] = 4444
        "$(Stop-MuxAgentUnixProcess -Opened $opened -Kernel $k)|$($k.Sent.Count)|$($k.Live[300])" | Should BeExactly 'True|0|4444' }
    It 'without a pidfd Kill sends nothing; only a pid the kernel shows gone (ESRCH) counts as exited; a changed or unreadable start does not' {
        $k = New-UnixFakeKernel; $k.Pidfd = $false; foreach ($id in 300, 310, 320, 330) { $k.Live[$id] = $id * 10 }
        $k.OnSleep = { param($Count) if ($Count -eq 3) { $k.Live[300] = 3999 }; if ($Count -eq 4) { $k.Live.Remove(330) } }.GetNewClosure()
        $h = @{}; foreach ($id in 300, 310, 320, 330) { $h[$id] = Open-MuxAgentUnixProcess -Id $id -Kernel $k }; $k.Unreadable[320] = $true
        $answers = foreach ($id in 300, 330, 310, 320) { "$(Stop-MuxAgentUnixProcess -Opened $h[$id] -Kernel $k)/$($k.Sleeps)" }
        "$($h[300].Fd) $($answers -join ' ')|$(@($k.Log | Where-Object { $_ -like 'send*' }).Count)|$($k.Closed.Count)|$($k.Live.ContainsKey(320))" | Should BeExactly '-1 False/50 True/51 False/101 False/151|0|0|True' }
    It 'with a pidfd, a live process that SIGKILL and the exit wait both fail on is not killed; the fd is still closed' {
        $k = New-UnixFakeKernel; $k.Live[300] = 3000; $opened = Open-MuxAgentUnixProcess -Id 300 -Kernel $k
        $k.SendKill = { param([int]$Fd) $k.Log.Add("send $Fd"); $false }.GetNewClosure(); $k.WaitExit = { param([int]$Fd, [int]$Milliseconds) $false }
        "$(Stop-MuxAgentUnixProcess -Opened $opened -Kernel $k)|$(@($k.Log | Where-Object { $_ -like 'send*' }) -join ',')|$($k.Closed -join ',')|$($k.Live[300])" | Should BeExactly 'False|send 1000|1000|3000' } }
Describe 'MuxAgents unix stop' {
    Context 'linux' {
        It 'kills only through pidfds; a start that differs after pidfd_open sends nothing and is reported' {
            $global:unixStopKernel = New-UnixFakeKernel
            try {
                $fake = New-UnixStopFake; $global:unixStopKernel.OnOpen = { param($Id) if ($Id -eq 320) { $global:unixStopKernel.Live[320] = 3999 } }
                $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
                "$(@($global:unixStopKernel.Sent | Sort-Object) -join ',')|$($reply.survivors)|$(@($reply.survivorPids) -join ',')|$($global:unixStopKernel.Live[320])|$(@($reply.PSObject.Properties.Name) -join ',')" | Should BeExactly '300/3000,310/3100|1|320|3999|ok,verb,name,pane,survivors,survivorPids,directory,resume' }
            finally { Remove-Variable -Name unixStopKernel -Scope Global } } }
    Context 'macos and no pidfd' {
        It 'signals nothing and lists only what kill-pane left' {
            $global:unixStopKernel = New-UnixFakeKernel; $global:unixStopKernel.Pidfd = $false
            try {
                $fake = New-UnixStopFake
                Add-FakeRmuxRule -Fake $fake -Match '^-N kill-pane ' -Script { param($Arguments) $global:unixStopKernel.Live.Remove(300); $global:unixStopKernel.Live.Remove(320); $null }
                $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
                "$(@($global:unixStopKernel.Log | Where-Object { $_ -like 'send*' }).Count)|$($reply.survivors)|$(@($reply.survivorPids) -join ',')|$($global:unixStopKernel.Sleeps)" | Should BeExactly '0|1|310|52' }
            finally { Remove-Variable -Name unixStopKernel -Scope Global } } }
    Context 'no pidfd, pid reused' {
        It 'reports a pid that now has another start but is still alive; only ESRCH proves an exit' {
            $global:unixStopKernel = New-UnixFakeKernel; $global:unixStopKernel.Pidfd = $false
            try {
                $fake = New-UnixStopFake
                Add-FakeRmuxRule -Fake $fake -Match '^-N kill-pane ' -Script { param($Arguments) $global:unixStopKernel.Live.Remove(300); $global:unixStopKernel.Live[320] = 7777; $null }
                $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
                "$(@($global:unixStopKernel.Log | Where-Object { $_ -like 'send*' }).Count)|$($reply.survivors)|$(@($reply.survivorPids | Sort-Object) -join ',')|$($global:unixStopKernel.Sleeps)" | Should BeExactly '0|2|310,320|101' }
            finally { Remove-Variable -Name unixStopKernel -Scope Global } } }
    Context 'linux kill fails' {
        It 'reports every row not killed: the rejected start and both failed kills' {
            $global:unixStopKernel = New-UnixFakeKernel
            try {
                $fake = New-UnixStopFake; $global:unixStopKernel.OnOpen = { param($Id) if ($Id -eq 320) { $global:unixStopKernel.Live[320] = 3999 } }
                $global:unixStopKernel.SendKill = { param([int]$Fd) $false }; $global:unixStopKernel.WaitExit = { param([int]$Fd, [int]$Milliseconds) $false }
                $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
                "$($reply.survivors)|$(@($reply.survivorPids) -join ',')|$(@($global:unixStopKernel.Live.Keys | Sort-Object) -join ',')" | Should BeExactly '3|320,300,310|300,310,320,900' }
            finally { Remove-Variable -Name unixStopKernel -Scope Global } } } }
Describe 'MuxAgents unix claude directory' {
    $caller = [pscustomobject]@{ Agent = [pscustomobject]@{ Pid = 150; Start = 1500 } }
    # Each case gets its own fake home root, so no session file leaks between cases.
    function New-UnixC5Home([string]$Name, $Session) {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive $Name)
        if ($null -ne $Session) { $file = [IO.Path]::Combine($fake.Home, '.claude', 'sessions', '150.json'); $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($file)); [IO.File]::WriteAllText($file, $Session) }
        $fake }
    function New-UnixC5Kernel { $k = New-UnixFakeKernel; $k.Live[150] = 1500; $k.Cwd[150] = '/w'; $k }
    It 'with no session file takes the kernel cwd read between two equal starts' {
        $k = New-UnixC5Kernel
        "$(Get-MuxAgentUnixClaudeDirectory -P (New-UnixC5Home 'c5-none' $null) -Caller $caller -Kernel $k)|$($k.Log -join ',')" | Should BeExactly '/w|start 150,cwd 150,start 150' }
    It 'a session file must be an object whose cwd is a non-empty string equal to the kernel cwd; sessionId and procStart are not read' {
        $cases = [ordered]@{ agree = '{"cwd":"/w","sessionId":"s9","procStart":"1"}'; other = '{"cwd":"/x"}'; case = '{"cwd":"/W"}'; 'null' = 'null'; 'false' = 'false'; array = '[]'; wrapped = '[{"cwd":"/w"}]'
            empty = ''; nocwd = '{"sessionId":"s9"}'; number = '{"cwd":7}'; list = '{"cwd":["/w"]}'; blank = '{"cwd":""}'; broken = '{"cwd":' }
        $answers = foreach ($name in $cases.Keys) { "$name=$(Get-MuxAgentUnixClaudeDirectory -P (New-UnixC5Home "c5-$name" $cases[$name]) -Caller $caller -Kernel (New-UnixC5Kernel))" }
        ($answers -join ',') | Should BeExactly 'agree=/w,other=,case=,null=,false=,array=,wrapped=,empty=,nocwd=,number=,list=,blank=,broken=' }
    It 'only a proven missing session file is no session: a folder named <pid>.json or a file it cannot read refuses' {
        $folder = New-UnixC5Home 'c5-folder' $null; $null = [IO.Directory]::CreateDirectory([IO.Path]::Combine($folder.Home, '.claude', 'sessions', '150.json'))
        $locked = New-UnixC5Home 'c5-locked' '{"cwd":"/w"}'; $nofile = New-UnixC5Home 'c5-nofile' $null; $null = [IO.Directory]::CreateDirectory([IO.Path]::Combine($nofile.Home, '.claude', 'sessions'))
        $lock = [IO.File]::Open([IO.Path]::Combine($locked.Home, '.claude', 'sessions', '150.json'), 'Open', 'ReadWrite', 'None')
        try { $answers = foreach ($case in @(@('folder', $folder), @('locked', $locked), @('nofile', $nofile), @('nodir', (New-UnixC5Home 'c5-nodir' $null)))) { "$($case[0])=$(Get-MuxAgentUnixClaudeDirectory -P $case[1] -Caller $caller -Kernel (New-UnixC5Kernel))" } }
        finally { $lock.Dispose() }
        ($answers -join ',') | Should BeExactly 'folder=,locked=,nofile=/w,nodir=/w' }
    It 'refuses when the start changes around the cwd read, belongs to another process, or the cwd is unreadable' {
        $answers = foreach ($case in 'moved', 'other', 'nocwd', 'same') {
            $k = New-UnixC5Kernel; if ($case -eq 'other') { $k.Live[150] = 1501 }; if ($case -eq 'nocwd') { $k.Cwd.Remove(150) }
            if ($case -eq 'moved') { $k.ReadCwd = { param([int]$Id) $k.Live[150] = 1777; '/w' }.GetNewClosure() }
            "$case=$(Get-MuxAgentUnixClaudeDirectory -P (New-UnixC5Home "c5-race-$case" $null) -Caller $caller -Kernel $k)" }
        ($answers -join ',') | Should BeExactly 'moved=,other=,nocwd=,same=/w' } }
