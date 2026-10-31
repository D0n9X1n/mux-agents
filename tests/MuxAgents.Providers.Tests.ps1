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
Describe 'MuxAgents providers contract' {
    It 'windows has exactly the contract members' {
        $members = Get-MuxAgentPlatformMembers -Platform windows
        (@($members.Keys | Sort-Object) -join ',') | Should BeExactly 'Argv,ClaudeDirectory,ClientImage,ClientOf,Endpoint,IsShell,IsUnder,Kill,LockName,Mux,Open,Platform,PrivateDirectories,Processes,Pwsh,SameStart,Sandbox,StateRootOf,StopReport,Token'
        "$($members.Platform) $([IO.Path]::GetFileName($members.Pwsh)) $(@($members.Keys | Where-Object { $_ -notin 'Platform', 'Pwsh' -and $members[$_] -isnot [scriptblock] }).Count)" | Should BeExactly 'windows pwsh.exe 0'
    }
    It 'refuses every other platform with the platform check' {
        $answers = foreach ($name in 'linux', 'macos', '') { Get-ProvidersRefusal { Get-MuxAgentPlatformMembers -Platform $name } }
        ($answers -join ';') | Should BeExactly ((@(1..3) | ForEach-Object { '2 platform mux-agents 0.1.0 runs on Windows with rmux only' }) -join ';')
    }
    # Pester 4 keeps a Mock until its Describe or Context ends, so each mocking test has its own Context.
    Context 'factory with a spy member set' {
        It 'the factory asks for the running platform, installs every selected member by reference and derives StateRoot and the lock name from it' {
            $global:providersSpySet = Get-MuxAgentPlatformMembers -Platform windows
            $global:providersSpySet.StateRootOf = { param($Environment) 'spy-root' }
            $global:providersAsked = [Collections.Generic.List[string]]::new()
            Mock Get-MuxAgentPlatform { 'spy-os' }
            Mock Get-MuxAgentPlatformMembers { $global:providersAsked.Add("$Platform"); $global:providersSpySet }
            $saved = $script:MuxAgentLockName
            try {
                $built = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts/mux-agent.ps1')
                $moved = @($global:providersSpySet.Keys | Where-Object { $global:providersSpySet[$_] -is [scriptblock] -and -not [object]::ReferenceEquals($built.$_, $global:providersSpySet[$_]) })
                "$($global:providersAsked -join ',')|$($moved -join ',')|$($built.Platform)|$($built.Pwsh -ceq $global:providersSpySet.Pwsh)|$($built.StateRoot)|$([object]::ReferenceEquals($script:MuxAgentLockName, $global:providersSpySet.LockName))" | Should BeExactly 'spy-os||windows|True|spy-root|True'
            }
            finally { $script:MuxAgentLockName = $saved; Remove-Variable -Name providersSpySet, providersAsked -Scope Global }
        }
    }
    Context 'factory on another platform' {
        It 'the real factory refuses a platform other than windows with the platform check' {
            Mock Get-MuxAgentPlatform { 'linux' }
            $saved = $script:MuxAgentLockName
            try { Get-ProvidersRefusal { New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts/mux-agent.ps1') } | Should BeExactly '2 platform mux-agents 0.1.0 runs on Windows with rmux only' }
            finally { $script:MuxAgentLockName = $saved }
        }
    }
    It 'fake providers carry every OS member: fakes for Processes, Open, Kill and Token, production text for the rest' {
        $fake = New-FakeMuxAgentProviders
        $members = Get-MuxAgentPlatformMembers -Platform windows
        (@($members.Keys | Where-Object { -not $fake.PSObject.Properties[$_] -or [string]$fake.$_ -cne [string]$members[$_] } | Sort-Object) -join ',') | Should BeExactly 'Kill,Open,Processes,Token'
    }
}
