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
