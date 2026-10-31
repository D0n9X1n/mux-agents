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
