$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $repoRoot 'scripts\RmuxAgents.ps1')
. (Join-Path $PSScriptRoot 'RmuxAgents.TestHelpers.ps1')

function Invoke-T2Verb {
    param([Parameter(Mandatory)]$Fake, [Parameter(Mandatory)][string[]]$Arguments)
    $result = Invoke-RmuxAgentCommand -P $Fake -Arguments $Arguments
    [pscustomobject]@{ Code = $result.Code; Reply = ($result.Text | ConvertFrom-Json) }
}

function New-T2Fake {
    # Fake providers in their own folder, with the probes the part 2 and part 3 verbs add.
    param([Parameter(Mandatory)][string]$Name, [string]$Caller = 'human')
    $fake = New-FakeRmuxAgentProviders -Caller $Caller -Root (Join-Path $TestDrive $Name)
    Add-FakeRmuxRule -Fake $fake -Match '#\{pane_height\}$' -StdOut "40`n"
    Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$3|`n"
    $fake
}

function New-T2Agent {
    # A committed agent of session $3 with its record; returns the agent folder.
    param([Parameter(Mandatory)]$Fake, [Parameter(Mandatory)]$Caller, [string]$Id = 'aaaaaaaaaaaa', [string]$Name = 'worker', [string]$Pane = '%9', [string]$ParentPane = '%1')
    $state = Get-RmuxAgentStateDirectory -P $Fake -Caller $Caller
    $folder = [IO.Path]::Combine($state, 'agents', $Id)
    $null = [IO.Directory]::CreateDirectory($folder)
    Write-RmuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = $Id; name = $Name; client = 'claude'; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'
            context = 'brief'; reason = 'human'; session = '$3'; pane = $Pane; window = '@5'; parentPane = $ParentPane; parentAgent = $null; directory = $Fake.Cwd; createdAt = 'x' })
    Write-RmuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
    $folder
}

Describe 'RmuxAgents part 3 no widening' {
    It 'C1 every verb refuses a sandboxed or unreadable token before any rmux call' {
        foreach ($token in @(
                [pscustomobject]@{ IsAppContainer = $true; IsRestricted = $false; IntegrityLevel = 'S-1-16-8192' },
                [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $true; IntegrityLevel = 'S-1-16-8192' },
                [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $false; IntegrityLevel = 'S-1-16-4096' },
                $null)) {
            $fake = New-T2Fake 'c1'
            $fake.Admitted = $false
            $fake.TokenInfo = $token
            $run = Invoke-T2Verb $fake @('list')
            "$($run.Code) $($run.Reply.check) $($fake.Calls.Count)" | Should BeExactly '2 sandbox 0'
        }
        $fake = New-T2Fake 'c1 medium'
        $fake.Admitted = $false
        (Invoke-T2Verb $fake @('list')).Code | Should Be 0
    }

    It 'C1 the real token of this process is readable and at least Medium' {
        $token = Get-RmuxAgentToken
        $token.IntegrityLevel | Should Match '^S-1-16-\d+$'
        { Assert-RmuxAgentToken -Token $token } | Should Not Throw
    }

    It 'C3 C2 C5 an admitted Claude agent spawns into its launch directory' {
        $fake = New-T2Fake 'pass' -Caller claude
        $fake.Admitted = $false
        $work = $fake.Cwd
        $fake.Cwd = [IO.Directory]::CreateDirectory((Join-Path $work 'moved')).FullName
        $task = Join-Path $work 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        $brief = Join-Path $work 'brief.md'; [IO.File]::WriteAllText($brief, 'brief')
        $run = Invoke-T2Verb $fake @('spawn', '-Client', 'copilot', '-TaskFile', $task, '-BriefFile', $brief)
        "$($run.Code) $($run.Reply.ok)" | Should BeExactly '0 True'
        $create = @($fake.Calls | Where-Object { $_[1] -eq 'new-window' })[0]
        $create[[Array]::IndexOf($create, '-c') + 1] | Should BeExactly $work
    }

    It 'C3 an unlisted flag on the agent command line refuses spawn and names only the flag' {
        $fake = New-T2Fake 'flags' -Caller claude
        $fake.Admitted = $false
        ($fake.Rows | Where-Object Pid -EQ 150).CommandLine += ' --settings "C:\secret path\s.json"'
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        $run = Invoke-T2Verb $fake @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '2 flags'
        $run.Reply.message | Should Match '--settings'
        $run.Reply.message | Should Not Match 'secret'
        @($fake.Calls | Where-Object { $_[1] -in @('new-window', 'split-window') }).Count | Should Be 0
    }

    It 'C2 a non-bypass transcript mode or a missing mode line refuses spawn' {
        $fake = New-T2Fake 'mode' -Caller claude
        $fake.Admitted = $false
        [IO.File]::AppendAllText($fake.Fixtures.ClaudeTranscript, '{"type":"permission-mode","permissionMode":"default","sessionId":"11111111-2222-4333-8444-555555555555"}' + "`n")
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        (Invoke-T2Verb $fake @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)).Reply.check | Should BeExactly 'policy'

        $fake = New-T2Fake 'screen' -Caller claude
        $fake.Admitted = $false
        Add-FakeRmuxRule -Fake $fake -Match '^-N capture-pane ' -StdOut "> \n  ? for shortcuts`n"
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        (Invoke-T2Verb $fake @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)).Reply.check | Should BeExactly 'policy'
    }

    It 'C2 a Copilot agent whose latest permissions event is not allow-all is refused' {
        $fake = New-T2Fake 'copilot policy' -Caller copilot
        $fake.Admitted = $false
        [IO.File]::AppendAllText($fake.Fixtures.CopilotEvents, '{"type":"session.permissions_changed","data":{"allowAllPermissions":false}}' + "`n")
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        (Invoke-T2Verb $fake @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)).Reply.check | Should BeExactly 'policy'
    }

    It 'C5 a sessions file for another session refuses spawn' {
        $fake = New-T2Fake 'directory' -Caller claude
        $fake.Admitted = $false
        [IO.File]::WriteAllText($fake.Fixtures.ClaudeSession, '{"pid":150,"sessionId":"99999999-2222-4333-8444-555555555555","cwd":"C:\\","procStart":"1500"}')
        $task = Join-Path $fake.Cwd 'task.md'; [IO.File]::WriteAllText($task, 'do it')
        (Invoke-T2Verb $fake @('spawn', '-Client', 'claude', '-TaskFile', $task, '-BriefFile', $task)).Reply.check | Should BeExactly 'directory'
    }
}

Describe 'RmuxAgents part 2 messages and stop' {
    It 'B2 B4 a message is sanitized, prefixed and read exactly once' {
        $fake = New-T2Fake 'mail'
        $send = Invoke-T2Verb $fake @('send', '%1', '-Body', ("a" + [char]27 + "[31mb`r`nc" + [char]7))
        "$($send.Code) $($send.Reply.pane)" | Should BeExactly '0 %1'
        $first = Invoke-T2Verb $fake @('read')
        @($first.Reply.messages).Count | Should Be 1
        $first.Reply.messages[0].text | Should BeExactly "[rmux-agent from root (%1) #$($send.Reply.id)]`na[31mb`nc"
        @((Invoke-T2Verb $fake @('read')).Reply.messages).Count | Should Be 0
        @($fake.Calls | Where-Object { $_[1] -in @('send-keys', 'paste-buffer') }).Count | Should Be 0
    }

    It 'B2 a pane of another session, an unknown name and an agent -Body are refused' {
        $fake = New-T2Fake 'targets'
        Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$4|`n"
        (Invoke-T2Verb $fake @('send', '%5', '-Body', 'x')).Reply.check | Should BeExactly 'target'
        (Invoke-T2Verb $fake @('send', 'nobody', '-Body', 'x')).Reply.check | Should BeExactly 'target'
        $agent = New-T2Fake 'agent body' -Caller claude
        (Invoke-T2Verb $agent @('send', '%1', '-Body', 'x')).Reply.check | Should BeExactly 'usage'
    }

    It 'B4 read -Wait returns empty at its deadline and refuses more than 600 s' {
        $fake = New-T2Fake 'wait'
        $start = $fake.Clock
        @((Invoke-T2Verb $fake @('read', '-Wait', '3')).Reply.messages).Count | Should Be 0
        ($fake.Clock - $start).TotalSeconds -ge 3 | Should Be $true
        (Invoke-T2Verb $fake @('read', '-Wait', '601')).Code | Should Be 2
    }

    It 'B1 a gone agent gets exactly one notice: exited with its code, else lost' {
        $fake = New-T2Fake 'notice'
        $caller = Resolve-RmuxAgentCaller -P $fake
        $exited = New-T2Agent -Fake $fake -Caller $caller -Id 'aaaaaaaaaaaa' -Name 'done'
        Write-RmuxAgentJson -Path (Join-Path $exited 'launcher.json') -Value ([ordered]@{ pid = 1; exitCode = 7 })
        $null = New-T2Agent -Fake $fake -Caller $caller -Id 'bbbbbbbbbbbb' -Name 'gone'
        $texts = @((Invoke-T2Verb $fake @('read')).Reply.messages | ForEach-Object { ($_.text -split "`n")[1] } | Sort-Object)
        ($texts -join '|') | Should BeExactly 'done exited with code 7.|gone is gone: its pane closed without an exit record.'
        $null = Invoke-T2Verb $fake @('list')
        @((Invoke-T2Verb $fake @('read')).Reply.messages).Count | Should Be 0
    }

    It 'B1 a notice whose mailbox append fails is posted on the next read, and a lost marker never posts twice' {
        $fake = New-T2Fake 'notice retry'
        $caller = Resolve-RmuxAgentCaller -P $fake
        $folder = New-T2Agent -Fake $fake -Caller $caller -Name 'done'
        Write-RmuxAgentJson -Path (Join-Path $folder 'launcher.json') -Value ([ordered]@{ pid = 1; exitCode = 7 })
        $box = [IO.Path]::Combine((Get-RmuxAgentStateDirectory -P $fake -Caller $caller), 'mail', 'pane-1.jsonl')
        $held = [IO.File]::Open($box, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        try { (Invoke-T2Verb $fake @('list')).Code | Should Be 1 }
        finally { $held.Dispose() }
        [IO.File]::Exists((Join-Path $folder 'notice.json')) | Should Be $false
        $null = Invoke-T2Verb $fake @('list')
        Remove-Item -LiteralPath (Join-Path $folder 'notice.json')
        $null = Invoke-T2Verb $fake @('list')
        $messages = @((Invoke-T2Verb $fake @('read')).Reply.messages)
        "$($messages.Count) $(($messages[0].text -split "`n")[1])" | Should BeExactly '1 done exited with code 7.'
    }

    It 'B1 a torn mailbox line is sealed, skipped and never counts as the notice' {
        $fake = New-T2Fake 'torn'
        $caller = Resolve-RmuxAgentCaller -P $fake
        $folder = New-T2Agent -Fake $fake -Caller $caller -Name 'done'
        Write-RmuxAgentJson -Path (Join-Path $folder 'launcher.json') -Value ([ordered]@{ pid = 1; exitCode = 7 })
        $box = [IO.Path]::Combine((Get-RmuxAgentStateDirectory -P $fake -Caller $caller), 'mail', 'pane-1.jsonl')
        [IO.File]::WriteAllText($box, '{"id":"exit-aaaaaaaaaaaa"')
        (Invoke-T2Verb $fake @('list')).Code | Should Be 0
        $messages = @((Invoke-T2Verb $fake @('read')).Reply.messages)
        "$($messages.Count) $(($messages[0].text -split "`n")[1])" | Should BeExactly '1 done exited with code 7.'
        $null = Invoke-T2Verb $fake @('send', '%1', '-Body', 'after')
        (@((Invoke-T2Verb $fake @('read')).Reply.messages) | ForEach-Object { ($_.text -split "`n")[1] }) -join '|' | Should BeExactly 'after'
    }

    It 'B5 stop closes the pane, stops the recorded tree and posts stopped once' {
        $fake = New-T2Fake 'stop'
        $caller = Resolve-RmuxAgentCaller -P $fake
        $null = New-T2Agent -Fake $fake -Caller $caller
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%1|@1|100||`n%9|@5|300|aaaaaaaaaaaa|worker`n"
        $fake.Rows.Add((New-FakeRmuxAgentRow 300 900 $fake.Pwsh 'pwsh launcher'))
        $fake.Rows.Add((New-FakeRmuxAgentRow 310 300 'C:\x\claude.exe' 'claude'))
        $fake.Rows.Add((New-FakeRmuxAgentRow 320 100 'C:\x\other.exe' 'unrelated'))
        $fake.Survivors.Add(310)
        $run = Invoke-T2Verb $fake @('stop', 'worker')
        "$($run.Code) $($run.Reply.survivors) $($run.Reply.resume)" | Should BeExactly '0 1 claude --resume 0f11fc2f-2032-48ef-a920-4af2a8e5af50'
        (@($fake.Killed | Sort-Object) -join ',') | Should BeExactly '300,310'
        @($fake.Calls | Where-Object { ($_ -join ' ') -eq '-N kill-pane -t %9' }).Count | Should Be 1
        $messages = @((Invoke-T2Verb $fake @('read')).Reply.messages)
        "$($messages.Count) $(($messages[0].text -split "`n")[1])" | Should BeExactly '1 worker was stopped.'
    }

    It 'B5 an agent that is not the parent cannot stop' {
        $fake = New-T2Fake 'stop parent' -Caller claude
        $caller = Resolve-RmuxAgentCaller -P $fake
        $null = New-T2Agent -Fake $fake -Caller $caller -ParentPane '%7'
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
        (Invoke-T2Verb $fake @('stop', 'worker')).Reply.check | Should BeExactly 'parent'
        @($fake.Calls | Where-Object { $_[1] -eq 'kill-pane' }).Count | Should Be 0
    }
}

Describe 'RmuxAgents launcher and transport' {
    It 'A4 the launcher runs no client on abort and records the client exit code otherwise' {
        $fake = New-T2Fake 'launcher'
        $folder = [IO.Directory]::CreateDirectory((Join-Path $TestDrive 'launcher\agents\cccccccccccc')).FullName
        $spec = Join-Path $folder 'spec.json'
        Write-RmuxAgentJson -Path $spec -Value ([ordered]@{ schema = 1; client = 'claude'; args = @('--session-id', 's', 'the prompt') })
        $calls = [Collections.Generic.List[object]]::new()
        $client = { param([string[]]$ClientArguments) $calls.Add($ClientArguments); $global:LASTEXITCODE = 7 }
        Write-RmuxAgentJson -Path (Join-Path $folder 'abort.json') -Value @{ x = 1 }
        Invoke-RmuxAgentLauncher -P $fake -Spec $spec -Client $client | Should Be 1
        "$($calls.Count) $((Read-RmuxAgentJson -Path (Join-Path $folder 'launcher.json')).aborted)" | Should BeExactly '0 True'

        Remove-Item -LiteralPath (Join-Path $folder 'abort.json')
        Write-RmuxAgentJson -Path (Join-Path $folder 'commit.json') -Value @{ x = 1 }
        Invoke-RmuxAgentLauncher -P $fake -Spec $spec -Client $client | Should Be 7
        ($calls[0] -join ' ') | Should BeExactly '--dangerously-skip-permissions --effort high --session-id s the prompt'
        (Read-RmuxAgentJson -Path (Join-Path $folder 'launcher.json')).exitCode | Should Be 7
        Invoke-RmuxAgentLauncher -P $fake -Spec $spec -Client { throw 'boom' } | Should Be 1
    }

    It 'A4 the launcher gives up after 30 s with no commit' {
        $fake = New-T2Fake 'commit wait'
        $folder = [IO.Directory]::CreateDirectory((Join-Path $TestDrive 'commit wait\agent')).FullName
        $start = $fake.Clock
        Wait-RmuxAgentCommit -P $fake -AgentDirectory $folder | Should Be $false
        [int]($fake.Clock - $start).TotalSeconds | Should Be 30
    }

    It 'A4 the scrub removes exactly the client identity variables' {
        $names = @($script:RmuxAgentScrub) + @('ANTHROPIC_BASE_URL')
        $saved = @{}
        foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name); [Environment]::SetEnvironmentVariable($name, 'x') }
        try {
            Clear-RmuxAgentIdentity
            @($script:RmuxAgentScrub | Where-Object { [Environment]::GetEnvironmentVariable($_) }).Count | Should Be 0
            [Environment]::GetEnvironmentVariable('ANTHROPIC_BASE_URL') | Should BeExactly 'x'
            $script:RmuxAgentScrub.Count | Should Be 12
        }
        finally { foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) } }
    }

    It 'P1 state root, hash vector and lock names carry no Terminal Profiles name' {
        Get-RmuxAgentStateRoot -Environment @{ RMUX_AGENTS_STATE = 'D:\s'; LOCALAPPDATA = 'C:\l' } | Should BeExactly 'D:\s'
        Get-RmuxAgentStateRoot -Environment @{ LOCALAPPDATA = 'C:\l' } | Should BeExactly 'C:\l\rmux-agents\state'
        Get-RmuxAgentHash 'abc' | Should BeExactly 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'
        foreach ($file in Get-ChildItem (Join-Path $repoRoot 'scripts') -Filter *.ps1) { [IO.File]::ReadAllText($file.FullName) | Should Not Match 'TerminalProfiles' }
    }

    It 'A1 rmux calls need -N and run the server image with -S' {
        $fake = New-T2Fake 'transport'
        $fake.Rows[0].Path = 'C:\rmux here\rmux.exe'
        Resolve-RmuxAgentExecutable -P $fake | Should BeExactly 'C:\rmux here\rmux.exe'
        $failure = $null
        try { $null = Invoke-RmuxAgentRmux -P $fake -Arguments @('list-panes') } catch { $failure = $_.Exception.Message }
        $failure | Should BeExactly 'rmux-agent: every rmux call must start with -N'
    }

    It 'A7 native calls round-trip UTF-8 and quoted arguments and time out' {
        $stub = Join-Path $TestDrive 'echo.ps1'
        [IO.File]::WriteAllText($stub, '[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); [Console]::Out.Write($args -join "|"); if ($args[0] -eq "sleep") { Start-Sleep 5 }')
        $pwsh = Join-Path $PSHOME 'pwsh.exe'
        (Invoke-RmuxAgentProcess -FilePath $pwsh -Arguments @('-NoProfile', '-File', $stub, ([string][char]0xE9), 'a "b" c\')).StdOut | Should BeExactly ([string][char]0xE9 + '|a "b" c\')
        $failure = $null
        try { $null = Invoke-RmuxAgentProcess -FilePath $pwsh -Arguments @('-NoProfile', '-File', $stub, 'sleep') -TimeoutMilliseconds 500 } catch { $failure = $_.Exception.GetType().Name }
        $failure | Should BeExactly 'TimeoutException'
    }
}
