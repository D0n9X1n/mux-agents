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
Describe 'MuxAgents providers endpoint and mux' {
    It 'windows Endpoint reads RMUX before TMUX, keeps the pid as text, takes only pipes; Mux resolves the server image' {
        $members = Get-MuxAgentPlatformMembers -Platform windows
        $answers = foreach ($env in @(@{ RMUX = '\\.\pipe\a,0900,1'; TMUX = '\\.\pipe\b,7,0' }, @{ TMUX = '\\.\pipe\b,7,0' }, @{ TMUX = '/tmp/tmux-1000/default,7,0' }, @{ RMUX = '\\.\pipe\a,7,-1' }, @{})) {
            $parsed = & $members.Endpoint $env; if ($parsed) { "$($parsed.Socket)|$($parsed.ServerPid)" } else { 'none' }
        }
        ($answers -join ';') | Should BeExactly '\\.\pipe\a|0900;\\.\pipe\b|7;none;none;none'
        $fake = New-FakeMuxAgentProviders
        $fake.Rows[0].Path = '/srv/rmux.exe'
        & $members.Mux $fake | Should BeExactly '/srv/rmux.exe'
    }
    Context 'transport with a recorded native call' {
        It 'Invoke-MuxAgentRmux passes the Endpoint socket as -S and runs the image Mux names' {
            Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments) [pscustomobject]@{ ExitCode = 0; StdOut = "$FilePath $($Arguments -join ' ')"; StdErr = '' } }
            $fake = New-FakeMuxAgentProviders
            $fake.Endpoint = { param($Environment) [pscustomobject]@{ Socket = '/tmp/tmux-1000/default'; ServerPid = '900' } }
            $fake.Mux = { param($P) '/spy/tmux' }
            "$((Invoke-MuxAgentRmux -P $fake -Arguments @('-N', 'list-panes')).StdOut)|$($fake.RmuxPath)" | Should BeExactly '/spy/tmux -S /tmp/tmux-1000/default -N list-panes|/spy/tmux'
        }
    }
    Context 'transport without an endpoint' {
        It 'Invoke-MuxAgentRmux refuses a null Endpoint before resolving the image or making a native call' {
            $global:providersNative = [Collections.Generic.List[string]]::new()
            Mock Invoke-MuxAgentProcess { $global:providersNative.Add("process $FilePath"); [pscustomobject]@{ ExitCode = 0; StdOut = ''; StdErr = '' } }
            Mock Resolve-MuxAgentExecutable { $global:providersNative.Add('resolve'); '/spy/resolved' }
            try {
                $fake = New-FakeMuxAgentProviders
                $fake.Endpoint = { param($Environment) $null }
                $fake.Mux = { param($P) $global:providersNative.Add('mux'); '/spy/tmux' }
                "$(Get-ProvidersRefusal { Invoke-MuxAgentRmux -P $fake -Arguments @('-N', 'list-panes') })|$($global:providersNative -join ',')|$($null -eq $fake.RmuxPath)" | Should BeExactly '1 endpoint not inside an rmux pane||True'
            }
            finally { Remove-Variable -Name providersNative -Scope Global }
        }
    }
    It 'Resolve-MuxAgentCaller keys the server by the Endpoint socket and refuses when Endpoint finds none' {
        $fake = New-FakeMuxAgentProviders
        $fake.Endpoint = { param($Environment) [pscustomobject]@{ Socket = '/tmp/tmux-1000/default'; ServerPid = '900' } }
        "$((Resolve-MuxAgentCaller -P $fake).ServerKey)" | Should BeExactly (Get-MuxAgentHash '/tmp/tmux-1000/default|900|9000').Substring(0, 16)
        $fake.Endpoint = { param($Environment) $null }
        Get-ProvidersRefusal { Resolve-MuxAgentCaller -P $fake } | Should BeExactly '1 endpoint not inside an rmux pane'
    }
    It 'Resolve-MuxAgentCaller takes the server pid from Endpoint, not from the environment' {
        # The fake environment names server 900; the provider names 931, and the probe and Open agree with the provider.
        $fake = New-FakeMuxAgentProviders
        $fake.Endpoint = { param($Environment) [pscustomobject]@{ Socket = '/tmp/tmux-1000/default'; ServerPid = '931' } }
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut ('931|100|$3|@1|' + "`n")
        $fake.OpenResults[931] = [pscustomobject]@{ Id = 931; Start = 9317; Process = $null }
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.ServerPid)|$($caller.ServerStart)|$($caller.ServerKey)" | Should BeExactly "931|9317|$((Get-MuxAgentHash '/tmp/tmux-1000/default|931|9317').Substring(0, 16))"
    }
}
Describe 'MuxAgents providers shells' {
    It 'windows IsShell allows exactly five shell images, any case' {
        $isShell = (Get-MuxAgentPlatformMembers -Platform windows).IsShell
        (@(foreach ($name in 'pwsh.exe', 'PowerShell.exe', 'cmd.exe', 'bash.exe', 'sh.exe', 'fish', 'pwsh', 'node.exe') { "$(& $isShell ([pscustomobject]@{ Path = "/bin/$name" }))" }) -join ',') | Should BeExactly 'True,True,True,True,True,False,False,False'
    }
    It 'Resolve-MuxAgentCaller accepts a shell only IsShell knows and refuses what IsShell rejects' {
        $fake = New-FakeMuxAgentProviders
        $fake.Rows[1].Path = '/usr/bin/fish'
        $fake.IsShell = { param($Row) [IO.Path]::GetFileName($Row.Path) -in @('fish') }
        (Resolve-MuxAgentCaller -P $fake).Kind | Should BeExactly 'human'
        $fake.IsShell = { param($Row) $false }
        Get-ProvidersRefusal { Resolve-MuxAgentCaller -P $fake } | Should BeExactly '2 caller caller runs under a non-shell process'
    }
}
Describe 'MuxAgents providers argv and claude directory' {
    Context 'windows Argv with a recorded split' {
        It 'windows Argv returns the split elements of the row command line as an array' {
            Mock Split-MuxAgentCommandLine { param($CommandLine) @(('<' + $CommandLine + '>'), 'x') }
            $argv = (Get-MuxAgentPlatformMembers -Platform windows).Argv
            $full = & $argv ([pscustomobject]@{ CommandLine = '"C:/c.exe" --model m' })
            $empty = & $argv ([pscustomobject]@{ CommandLine = $null })
            "$($full.Count)|$($full[0])|$($full[1])|$($empty.Count)|$($empty[0])|$($empty[1])" | Should BeExactly '2|<"C:/c.exe" --model m>|x|2|<>|x'
        }
    }
    It 'Assert-MuxAgentSpawnAllowed reads client argv from Argv and refuses when it throws' {
        $fake = New-ProvidersAgentFake
        $fake.Argv = { param($Row) @('claude', "--argv-of-$($Row.Pid)") }
        $caller = Resolve-MuxAgentCaller -P $fake
        Get-ProvidersRefusal { Assert-MuxAgentSpawnAllowed -P $fake -Caller $caller } | Should BeExactly '2 flags the flag --argv-of-150 is not allowed for a spawning agent'
        $fake.Argv = { param($Row) throw 'unreadable' }
        Get-ProvidersRefusal { Assert-MuxAgentSpawnAllowed -P $fake -Caller $caller } | Should BeExactly '2 flags a client command line cannot be read'
    }
    It 'windows ClaudeDirectory takes cwd from sessions/<pid>.json only on equal microsecond start and session id' {
        $fake = New-FakeMuxAgentProviders
        Write-FakeMuxAgentFile ([IO.Path]::Combine($fake.Home, '.claude', 'sessions', '150.json')) @('{"sessionId":"s1","cwd":"/w","procStart":"1503"}')
        $dir = (Get-MuxAgentPlatformMembers -Platform windows).ClaudeDirectory
        $answers = foreach ($pair in @(@(1509, 's1'), @(1500, 's1'), @(1510, 's1'), @(1500, 's2'))) {
            $fake.Env['CLAUDE_CODE_SESSION_ID'] = $pair[1]; "$(& $dir $fake ([pscustomobject]@{ Agent = [pscustomobject]@{ Pid = 150; Start = $pair[0] } }))"
        }
        ($answers -join ',') | Should BeExactly '/w,/w,,'
    }
    It 'Get-MuxAgentLaunchDirectory takes the Claude directory from ClaudeDirectory' {
        $fake = New-FakeMuxAgentProviders
        $fake.ClaudeDirectory = { param($P, $Caller) if ($Caller.Agent.Pid -eq 4242) { [string]$TestDrive } }
        $caller = [pscustomobject]@{ Client = 'claude'; Agent = [pscustomobject]@{ Pid = 4242; Start = 1 } }
        "$(Get-MuxAgentLaunchDirectory -P $fake -Caller $caller)" | Should BeExactly ([string]$TestDrive)
        $caller.Agent.Pid = 4243
        Get-ProvidersRefusal { Get-MuxAgentLaunchDirectory -P $fake -Caller $caller } | Should BeExactly "2 directory the caller's launch directory cannot be confirmed"
    }
}
