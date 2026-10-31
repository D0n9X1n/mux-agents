# Provider contract tests. Untagged: they run on every OS. Windows-only natives are mocked or replaced by spies here;
# the windows-tagged suites run them for real. Paths use '/' so GetFileName agrees on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')
function Get-ProvidersRefusal {
    param([Parameter(Mandatory)][scriptblock]$Action)
    try { $null = & $Action; 'none' } catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" }
}
function New-ProvidersAgentFake {
    # The claude fake (agent 150 under bash 170 under pane 100) with '/' paths.
    param([string]$Root = $TestDrive)
    $fake = New-FakeMuxAgentProviders -Caller claude -Root $Root
    foreach ($row in $fake.Rows) { if ($row.Pid -eq 150) { $row.Path = '/x/claude.exe' } elseif ($row.Pid -eq 170) { $row.Path = '/usr/bin/bash.exe' } }
    $fake
}
Describe 'MuxAgents providers platform' {
    It 'maps the OS flags to one platform name' {
        $names = foreach ($flags in @(@($true, $false, $false), @($false, $true, $false), @($false, $false, $true), @($false, $false, $false))) {
            "$(Get-MuxAgentPlatform -OnWindows $flags[0] -OnLinux $flags[1] -OnMacOS $flags[2])"
        }
        ($names -join ',') | Should BeExactly 'windows,linux,macos,'
        Get-MuxAgentPlatform | Should BeExactly $(if ($IsWindows) { 'windows' } elseif ($IsLinux) { 'linux' } else { 'macos' })
    }
}
