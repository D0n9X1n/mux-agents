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
