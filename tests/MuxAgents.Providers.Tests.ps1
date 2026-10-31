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
Describe 'MuxAgents providers client identity' {
    It 'windows ClientOf names claude or copilot by image name, any case' {
        $clientOf = (Get-MuxAgentPlatformMembers -Platform windows).ClientOf
        (@(foreach ($path in '/u/claude.exe', '/x/Copilot.EXE', '/x/claude', '/x/claude.exe.bak', '/x/node.exe', $null) { "$(& $clientOf ([pscustomobject]@{ Path = $path }))" }) -join ',') | Should BeExactly 'claude,copilot,,,,'
    }
    It 'a claude.exe row that ClientOf rejects is no client for the caller' {
        $fake = New-ProvidersAgentFake
        $fake.ClientOf = { param($Row) $null }; $fake.IsShell = { param($Row) $true }
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind)|$($caller.Client)|$($null -eq $caller.Agent)" | Should BeExactly 'human||True'
    }
    It 'the agent, its client and the spawn flags check follow ClientOf, not the image name' {
        $fake = New-ProvidersAgentFake
        foreach ($row in $fake.Rows) { if ($row.Pid -eq 150) { $row.Path = '/x/agent-bin' } elseif ($row.Pid -eq 100) { $row.Path = '/x/copilot.exe' } }
        $fake.ClientOf = { param($Row) if ($Row.Pid -eq 150) { 'claude' } }
        $fake.Argv = { param($Row) if ($Row.Pid -eq 150) { @('claude', '--model') } else { @('x', "--argv-of-$($Row.Pid)") } }
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind)|$($caller.Client)|$($caller.Agent.Pid)" | Should BeExactly 'agent|claude|150'
        Get-ProvidersRefusal { Assert-MuxAgentSpawnAllowed -P $fake -Caller $caller } | Should BeExactly "2 policy the caller's live policy is not full bypass"
    }
}
Describe 'MuxAgents providers state and locks' {
    It 'windows StateRootOf and LockName are the Windows code' {
        $members = Get-MuxAgentPlatformMembers -Platform windows
        "$(& $members.StateRootOf @{ LOCALAPPDATA = 'L' })|$(& $members.StateRootOf @{ MUX_AGENTS_STATE = 'S'; LOCALAPPDATA = 'L' })|$(& $members.LockName 'k')" | Should BeExactly "$([IO.Path]::Combine('L', 'mux-agents', 'state'))|S|Local\MuxAgents-$((Get-MuxAgentHash 'k').Substring(0, 32))"
    }
    Context 'windows PrivateDirectories with recorded natives' {
        It 'windows PrivateDirectories reads the SID once and makes each folder private with it' {
            $members = Get-MuxAgentPlatformMembers -Platform windows
            $global:providersCalls = [Collections.Generic.List[string]]::new()
            Mock Get-MuxAgentUserSid { $global:providersCalls.Add('sid'); 'S-1-5-21-9' }
            Mock New-MuxAgentPrivateDirectory { param($Sid, $Path) $global:providersCalls.Add("$Sid|$Path") }
            try { & $members.PrivateDirectories -Paths @('A', 'A/agents'); ($global:providersCalls -join ';') | Should BeExactly 'sid;S-1-5-21-9|A;S-1-5-21-9|A/agents' }
            finally { Remove-Variable -Name providersCalls -Scope Global }
        }
    }
    It 'Get-MuxAgentStateDirectory passes its three folders to PrivateDirectories' {
        $fake = New-FakeMuxAgentProviders
        $seen = [Collections.Generic.List[string]]::new()
        $fake.PrivateDirectories = { param([string[]]$Paths) $seen.AddRange($Paths) }.GetNewClosure()
        $state = Join-Path $fake.StateRoot '0123456789abcdef'
        Get-MuxAgentStateDirectory -P $fake -Caller ([pscustomobject]@{ ServerKey = '0123456789abcdef' }) | Should BeExactly $state
        ($seen -join ';') | Should BeExactly "$state;$(Join-Path $state 'agents');$(Join-Path $state 'mail')"
    }
    It 'spawn makes the new agent folder through PrivateDirectories, and its refusal stops the launch' {
        # The spy creates the state, agents and mail folders and refuses the agent folder itself, so a spawn
        # that still creates that folder on its own launches a pane instead of refusing.
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match '#\{pane_height\}$' -StdOut "40`n"
        Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$3|`n"
        $seen = [Collections.Generic.List[string]]::new()
        # Captured here: a GetNewClosure block cannot see functions dot-sourced into this test file.
        $refuse = ${function:Exit-MuxAgentCommand}
        $fake.PrivateDirectories = {
            param([string[]]$Paths)
            foreach ($path in $Paths) {
                $seen.Add($path)
                if ((Split-Path -Leaf $path) -match '^[0-9a-f]{12}$') { & $refuse 1 'state' 'spy refuses the agent folder' }
                $null = [IO.Directory]::CreateDirectory($path)
            }
        }.GetNewClosure()
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        $result = Invoke-MuxAgentCommand -P $fake -Arguments @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)
        $reply = $result.Text | ConvertFrom-Json
        $leaf = @($seen | Where-Object { (Split-Path -Leaf $_) -match '^[0-9a-f]{12}$' })
        $launches = @($fake.Calls | Where-Object { $_[1] -in @('new-window', 'split-window') }).Count
        "$($result.Code)|$($reply.check)|$($leaf.Count)|$(Split-Path -Leaf (Split-Path -Parent "$($leaf[0])"))|$launches|$(if ($leaf) { [IO.Directory]::Exists($leaf[0]) })" | Should BeExactly '1|state|1|agents|0|False'
    }
    It 'Use-MuxAgentLock names its mutex with the installed LockName' {
        $saved = $script:MuxAgentLockName
        $keys = [Collections.Generic.List[string]]::new()
        $spyName = 'MuxAgentsSpy-' + [guid]::NewGuid().ToString('N')
        $script:MuxAgentLockName = { param([string]$LockKey) $keys.Add($LockKey); $spyName }.GetNewClosure()
        try {
            $held = Use-MuxAgentLock -LockKey 'probe-key' -LockAction { $probe = $null; $found = [Threading.Mutex]::TryOpenExisting($spyName, [ref]$probe); if ($probe) { $probe.Dispose() }; $found }
            "$held|$($keys -join ',')" | Should BeExactly 'True|probe-key'
        }
        finally { $script:MuxAgentLockName = $saved }
    }
}
Describe 'MuxAgents providers start comparison and stop report' {
    function New-ProvidersStopLeft {
        param([int]$ProcessId, [string]$Reason)
        [pscustomobject]@{ Pid = $ProcessId; Reason = $Reason; Row = $null }
    }
    function New-ProvidersStopFake {
        # A human caller and the committed agent 'worker' in pane %9. Its tree is pane 300 (pwsh), 310 (claude) and
        # 320 (node). The listed starts have nonzero units digits, so a rounded start never equals one.
        $fake = New-FakeMuxAgentProviders
        $fake.PrivateDirectories = { param([string[]]$Paths) foreach ($path in $Paths) { $null = [IO.Directory]::CreateDirectory($path) } }
        Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$3|`n"
        $folder = [IO.Path]::Combine((Get-MuxAgentStateDirectory -P $fake -Caller (Resolve-MuxAgentCaller -P $fake)), 'agents', 'aaaaaaaaaaaa'); $null = [IO.Directory]::CreateDirectory($folder)
        Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = 'aaaaaaaaaaaa'; name = 'worker'; client = 'claude'; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'; context = 'brief'; reason = 'human'; session = '$3'; pane = '%9'; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $fake.Cwd; createdAt = 'x' })
        Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%1|@1|100||`n%9|@5|300|aaaaaaaaaaaa|worker`n"
        $fake.Rows.Add((New-FakeMuxAgentRow 300 900 '/x/pwsh' 'pwsh')); $fake.Rows.Add((New-FakeMuxAgentRow 310 300 '/x/claude' 'claude')); $fake.Rows.Add((New-FakeMuxAgentRow 320 300 '/x/node' 'node'))
        foreach ($row in $fake.Rows) { if ($row.Pid -eq 300) { $row.Start = 3002 } elseif ($row.Pid -eq 310) { $row.Start = 3107 } elseif ($row.Pid -eq 320) { $row.Start = 3205 } }
        $fake.OpenResults[300] = [pscustomobject]@{ Id = 300; Start = 3007; Process = $null }; $fake.OpenResults[310] = [pscustomobject]@{ Id = 310; Start = 3104; Process = $null }; $fake.OpenResults[320] = $null
        $fake.Survivors.Add(310)
        $fake
    }
    It 'windows SameStart rounds the Open start to microseconds; StopReport counts only the rows a kill left' {
        $members = Get-MuxAgentPlatformMembers -Platform windows
        $left = @((New-ProvidersStopLeft 310 'kill'), (New-ProvidersStopLeft 320 'start'), (New-ProvidersStopLeft 330 'open'), (New-ProvidersStopLeft 340 'kill'))
        "$(& $members.SameStart 1009 1000) $(& $members.SameStart 1010 1000) $(& $members.SameStart $null 0)|$(@((& $members.StopReport $left).Keys) -join ',') $((& $members.StopReport $left).survivors) $((& $members.StopReport @()).survivors) $((& $members.StopReport $null).survivors)" | Should BeExactly 'True False False|survivors 2 0 0'
    }
    It 'the caller passes the opened start and the listed start to SameStart unchanged and refuses when it says no' {
        $fake = New-ProvidersAgentFake
        foreach ($row in $fake.Rows) { if ($row.Pid -eq 150) { $row.Start = 1503 } }
        $fake.OpenResults[150] = [pscustomobject]@{ Id = 150; Start = 1509; Process = $null }
        $seen = [Collections.Generic.List[string]]::new()
        $fake.SameStart = { param($Opened, $Listed) $seen.Add("$Opened/$Listed"); $true }.GetNewClosure()
        "$((Resolve-MuxAgentCaller -P $fake).Agent.Start)|$($seen -join ',')" | Should BeExactly '1509|1509/1503'
        $fake.SameStart = { param($Opened, $Listed) $false }
        Get-ProvidersRefusal { Resolve-MuxAgentCaller -P $fake } | Should BeExactly '1 caller agent process changed'
    }
    It 'stop passes the opened and listed starts to SameStart unchanged, kills only what it accepts and replies with StopReport' {
        $fake = New-ProvidersStopFake
        $seen = [Collections.Generic.List[string]]::new()
        $fake.SameStart = { param($Opened, $Listed) $seen.Add("$Opened/$Listed"); $Opened -eq 3104 }.GetNewClosure()
        $fake.StopReport = { param($Left) [ordered]@{ survivors = 7; survivorPids = @($Left | ForEach-Object Pid | Sort-Object) } }
        $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
        "$(@($seen | Sort-Object) -join ',')|$(@($fake.Killed) -join ',')|$($reply.survivors)|$(@($reply.survivorPids) -join ',')|$(@($reply.PSObject.Properties.Name) -join ',')" | Should BeExactly '3007/3002,3104/3107|310|7|300,310,320|ok,verb,name,pane,survivors,survivorPids,directory,resume'
    }
    It 'stop hands StopReport every tree row it did not kill: a changed start, an unopenable row and a failed kill' {
        $fake = New-ProvidersStopFake
        # 300 opens with another start, 320 cannot be opened and 310 survives its kill.
        $fake.SameStart = { param($Opened, $Listed) $Opened -eq 3104 }
        $fake.StopReport = { param($Left) [ordered]@{ left = @($Left | Sort-Object Pid | ForEach-Object { "$($_.Pid):$($_.Reason):$($_.Row.Path)" }) } }
        $reply = (Invoke-MuxAgentCommand -P $fake -Arguments @('stop', 'worker')).Text | ConvertFrom-Json
        "$($reply.ok)|$(@($reply.left) -join ',')" | Should BeExactly 'True|300:start:/x/pwsh,310:kill:/x/claude,320:open:/x/node'
    }
}
Describe 'MuxAgents providers sandbox' {
    It 'windows Sandbox refuses an AppContainer, restricted, below-Medium or unreadable token' {
        $sandbox = (Get-MuxAgentPlatformMembers -Platform windows).Sandbox
        $answers = foreach ($token in @([pscustomobject]@{ IsAppContainer = $true; IsRestricted = $false; IntegrityLevel = 'S-1-16-8192' }, [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $true; IntegrityLevel = 'S-1-16-8192' }, [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $false; IntegrityLevel = 'S-1-16-4096' }, $null, [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $false; IntegrityLevel = 'S-1-16-8192' })) {
            $holder = [pscustomobject]@{ Token = { $token }.GetNewClosure() }; Get-ProvidersRefusal { & $sandbox $holder }
        }
        ($answers -join ';') | Should BeExactly (((@(1..4) | ForEach-Object { '2 sandbox the caller runs in a sandbox or below Medium integrity' }) + 'none') -join ';')
    }
    It 'the dispatcher calls Sandbox once, with $P, only for a caller that is not admitted, and keeps its refusal' {
        $fake = New-FakeMuxAgentProviders
        $calls = [Collections.Generic.List[string]]::new()
        $fake.Sandbox = { param($P) $calls.Add([string]$P.Home) }.GetNewClosure()
        Register-MuxAgentVerb -Verb 'tp-ok' -Handler { param($P, $Request) [ordered]@{ ran = 'yes' } } -Usage 'tp-ok'
        try {
            $admitted = (Invoke-MuxAgentCommand -P $fake -Arguments @('tp-ok')).Code; $before = $calls.Count
            $fake.Admitted = $false
            $open = (Invoke-MuxAgentCommand -P $fake -Arguments @('tp-ok')).Code
            "$admitted $before $open $($calls.Count) $($calls[0] -ceq $fake.Home)" | Should BeExactly '0 0 0 1 True'
            $fake.Sandbox = { param($P) Exit-MuxAgentCommand 2 'sandbox' 'spy sandbox' }
            $refused = Invoke-MuxAgentCommand -P $fake -Arguments @('tp-ok')
            "$($refused.Code) $(($refused.Text | ConvertFrom-Json).message) $($fake.Calls.Count)" | Should BeExactly '2 spy sandbox 0'
        }
        finally { $script:MuxAgentVerbs.Remove('tp-ok') }
    }
}
Describe 'MuxAgents providers launcher' {
    It 'windows ClientImage adds .exe; IsUnder compares the full root plus separator, any case' {
        $members = Get-MuxAgentPlatformMembers -Platform windows
        $root = [IO.Path]::GetFullPath($TestDrive).TrimEnd('\') + '\'
        "$(& $members.ClientImage 'claude')|$(& $members.IsUnder $TestDrive ($root + 'a'))|$(& $members.IsUnder $TestDrive ($root.ToUpperInvariant() + 'a'))|$(& $members.IsUnder $TestDrive ($root.TrimEnd('\') + 'x\a'))|$(& $members.IsUnder $TestDrive $root.TrimEnd('\'))" | Should BeExactly 'claude.exe|True|True|False|False'
    }
    It 'Test-MuxAgentSpecPath asks IsUnder with StateRoot and takes only spec.json' {
        $fake = New-FakeMuxAgentProviders
        $roots = [Collections.Generic.List[string]]::new()
        $fake.IsUnder = { param($Root, $Full) $roots.Add($Root); $Full.StartsWith('A/') }.GetNewClosure()
        "$(Test-MuxAgentSpecPath -P $fake -Full 'A/x/spec.json') $(Test-MuxAgentSpecPath -P $fake -Full 'A/x/Spec.json') $(Test-MuxAgentSpecPath -P $fake -Full 'B/spec.json') $($roots[0] -ceq $fake.StateRoot)" | Should BeExactly 'True False False True'
    }
    Context 'launcher with a recorded PATH lookup' {
        It 'the launcher looks the client up on PATH by the name ClientImage returns' {
            # The launcher clears the client identity variables in this process, as it does in a pane.
            $global:providersLooked = [Collections.Generic.List[string]]::new()
            Mock Get-Command { $global:providersLooked.Add("$Name"); $null }
            $fake = New-FakeMuxAgentProviders
            $images = [Collections.Generic.List[string]]::new()
            $fake.ClientImage = { param($Client) $images.Add($Client); 'mux-spy-image' }.GetNewClosure()
            $folder = Join-Path $TestDrive 'launch'
            Write-FakeMuxAgentFile (Join-Path $folder 'spec.json') @('{"client":"claude","args":[]}'); Write-FakeMuxAgentFile (Join-Path $folder 'commit.json') @('{}')
            $errors = [IO.StringWriter]::new(); $console = [Console]::Error
            [Console]::SetError($errors)
            try { $code = Invoke-MuxAgentLauncher -P $fake -Spec (Join-Path $folder 'spec.json') }
            finally { [Console]::SetError($console) }
            try { "$code|$($images -join ',')|$($global:providersLooked -join ',')|$($errors.ToString().Trim())" | Should BeExactly '1|claude|mux-spy-image|mux-agent: mux-spy-image is not on PATH' }
            finally { Remove-Variable -Name providersLooked -Scope Global }
        }
    }
    It 'scripts/mux-agent-launcher.ps1 checks the spec path with Test-MuxAgentSpecPath' {
        # A copy of the real launcher runs in a child pwsh next to a stub library: the real core, then a fake
        # factory and a Test-MuxAgentSpecPath spy that accepts a spec outside the state root, which the old inline
        # check refuses on every OS. The stub launcher body exits 7; a refusal exits 2.
        $bin = Join-Path $TestDrive 'launcher-bin'
        $null = [IO.Directory]::CreateDirectory($bin)
        Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts/mux-agent-launcher.ps1') -Destination $bin
        $record = Join-Path $TestDrive 'spec-check.txt'
        $spec = [IO.Path]::Combine($TestDrive, 'elsewhere', 'spec.json')
        $core = [IO.Path]::Combine($repoRoot, 'scripts', 'MuxAgents.ps1').Replace("'", "''")
        [IO.File]::WriteAllLines((Join-Path $bin 'MuxAgents.ps1'), [string[]]@(
            ". '$core'"
            "function New-MuxAgentProviders { param(`$Entry) [pscustomobject]@{ StateRoot = '$(([IO.Path]::Combine($TestDrive, 'state')).Replace("'", "''"))' } }"
            "function Test-MuxAgentSpecPath { param(`$P, `$Full) [IO.File]::WriteAllText('$($record.Replace("'", "''"))', ""`$(`$P.StateRoot)|`$Full""); `$true }"
            'function Assert-MuxAgentNoReparse { param($Path) }'
            'function Invoke-MuxAgentLauncher { param($P, $Spec) 7 }'
        ))
        $pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
        $null = & $pwsh -NoProfile -File (Join-Path $bin 'mux-agent-launcher.ps1') $spec 2>$null
        $code = $LASTEXITCODE
        $seen = if (Test-Path -LiteralPath $record) { [IO.File]::ReadAllText($record) } else { 'not called' }
        "$code|$seen" | Should BeExactly "7|$([IO.Path]::Combine($TestDrive, 'state'))|$([IO.Path]::GetFullPath($spec))"
    }
    It 'scripts/mux-agent-launcher.ps1 exits 2 and never runs the launcher when Test-MuxAgentSpecPath refuses' {
        # Same harness, with a spy that refuses. The launcher body records a file if it runs at all.
        $bin = Join-Path $TestDrive 'launcher-refuse'
        $null = [IO.Directory]::CreateDirectory($bin)
        Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts/mux-agent-launcher.ps1') -Destination $bin
        $ran = Join-Path $TestDrive 'launcher-ran.txt'
        $spec = [IO.Path]::Combine($TestDrive, 'state', 'agents', 'a', 'spec.json')
        $core = [IO.Path]::Combine($repoRoot, 'scripts', 'MuxAgents.ps1').Replace("'", "''")
        [IO.File]::WriteAllLines((Join-Path $bin 'MuxAgents.ps1'), [string[]]@(
            ". '$core'"
            "function New-MuxAgentProviders { param(`$Entry) [pscustomobject]@{ StateRoot = '$(([IO.Path]::Combine($TestDrive, 'state')).Replace("'", "''"))' } }"
            'function Test-MuxAgentSpecPath { param($P, $Full) $false }'
            'function Assert-MuxAgentNoReparse { param($Path) }'
            "function Invoke-MuxAgentLauncher { param(`$P, `$Spec) [IO.File]::WriteAllText('$($ran.Replace("'", "''"))', 'ran'); 7 }"
        ))
        $pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
        $errors = @(& $pwsh -NoProfile -File (Join-Path $bin 'mux-agent-launcher.ps1') $spec 2>&1 | ForEach-Object { "$_" })
        $code = $LASTEXITCODE
        "$code|$($errors -join ' ')|$(Test-Path -LiteralPath $ran)" | Should BeExactly '2|mux-agent: the launch spec is outside the state folder|False'
    }
}
