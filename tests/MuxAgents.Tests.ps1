$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')

Describe 'MuxAgents T1.1 dispatcher' -Tag 'windows' {
    AfterAll {
        foreach ($verb in @('t11-throw', 't11-refuse', 't11-show', 't11-error', 't11-wrapped', 't11-format', 't11-format-inner')) { $script:MuxAgentVerbs.Remove($verb) }
    }

    It 'A7 usage exits 2 with no rmux call' {
        $fake = New-FakeMuxAgentProviders
        foreach ($line in @('', 'bogus', 'focus', 'list extra', 'spawn -Client', 'spawn -Nope x')) {
            $result = Invoke-MuxAgentCommand -P $fake -Arguments ([string[]]@($line -split ' ' | Where-Object { $_ }))
            "$line => $($result.Code) $(($result.Text | ConvertFrom-Json).check)" | Should Be "$line => 2 usage"
        }
        $fake.Calls.Count | Should Be 0
    }

    It 'A7 unexpected error exits 1 without message text' {
        Register-MuxAgentVerb -Verb 't11-throw' -Handler { param($P, $Request) throw 'SECRET' } -Usage 't11-throw'
        $result = Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @('t11-throw')
        $result.Code | Should Be 1
        ($result.Text | ConvertFrom-Json).check | Should Be 'internal'
        $result.Text | Should Not Match 'SECRET'
    }

    It 'A7 refusal maps code and check' {
        Register-MuxAgentVerb -Verb 't11-refuse' -Handler { param($P, $Request) Exit-MuxAgentCommand 2 'name' 'taken' } -Usage 't11-refuse'
        $result = Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @('t11-refuse')
        $result.Code | Should Be 2
        $result.Text | Should Match '"ok":false'
        $reply = $result.Text | ConvertFrom-Json
        "$($reply.code) $($reply.check) $($reply.message)" | Should Be '2 name taken'
    }

    It 'A7 Text output' {
        Register-MuxAgentVerb -Verb 't11-show' -Handler {
            param($P, $Request)
            [ordered]@{ name = 'alpha ' + [char]0x23F5; agents = @([ordered]@{ name = 'one'; pane = '%9' }, [pscustomobject]@{ name = 'two'; pane = '%10' }) }
        } -Usage 't11-show'
        $fake = New-FakeMuxAgentProviders
        $json = Invoke-MuxAgentCommand -P $fake -Arguments @('t11-show')
        $json.Code | Should Be 0
        $json.Text | Should BeExactly ('{"ok":true,"verb":"t11-show","name":"alpha ' + [char]92 + 'u23f5","agents":[{"name":"one","pane":"%9"},{"name":"two","pane":"%10"}]}')
        $text = Invoke-MuxAgentCommand -P $fake -Arguments @('t11-show', '-Text')
        $text.Code | Should Be 0
        $text.Text | Should BeExactly ('name: alpha ' + [char]0x23F5 + "`none  %9`ntwo  %10")
    }

    It 'A1 every native call gets -N' {
        $fake = New-FakeMuxAgentProviders
        (Invoke-MuxAgentNative -P $fake -Arguments @('list-panes', '-a')).ExitCode | Should Be 0
        $fake.Calls.Count | Should Be 1
        $fake.Calls[0][0] | Should BeExactly '-N'
        ($fake.Calls[0] -join ' ') | Should BeExactly '-N list-panes -a'
    }

    It 'A7 Text by arity' {
        $spawn = ConvertFrom-MuxAgentArgs -Arguments @('spawn', '-Client', 'claude', '-Task', '-Text')
        "$($spawn.Verb) $($spawn.Options.Client) $($spawn.Options.Task) $($spawn.Text)" | Should BeExactly 'spawn claude -Text False'
        (ConvertFrom-MuxAgentArgs -Arguments @('list', '-Text')).Text | Should Be $true
    }

    It 'A7 cold providers build' {
        $pwsh = Join-Path $PSHOME 'pwsh.exe'
        $library = (Join-Path $repoRoot 'scripts\MuxAgents.ps1').Replace("'", "''")
        $output = & $pwsh -NoProfile -Command "`$ErrorActionPreference = 'Stop'; . '$library'; (New-MuxAgentProviders -Entry x).Pwsh" 2>&1
        "$LASTEXITCODE $(($output | Out-String).Trim())" | Should Be "0 $pwsh"
    }

    It 'A7 native failure refuses rmux' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match ' kill-pane ' -ExitCode 1 -StdErr 'SECRET'
        $refusal = $null
        try { $null = Invoke-MuxAgentNative -P $fake -Arguments @('kill-pane', '-t', '%9') } catch { $refusal = $_.Exception }
        $refusal | Should Not BeNullOrEmpty
        "$($refusal.Data['MuxAgentCode']) $($refusal.Data['MuxAgentCheck'])" | Should Be '1 rmux'
        $refusal.Message | Should Not Match 'SECRET'
        (Invoke-MuxAgentNative -P $fake -Arguments @('kill-pane', '-t', '%9') -AllowFailure).ExitCode | Should Be 1
    }

    It 'A7 handler error exits 1 without message text' {
        $ErrorActionPreference = 'Continue'
        Register-MuxAgentVerb -Verb 't11-error' -Handler { param($P, $Request) Write-Error 'SECRET'; [ordered]@{ after = 'error' } } -Usage 't11-error'
        $records = @(Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @('t11-error') *>&1)
        $results = @($records | Where-Object { $_.PSObject.Properties['Code'] })
        $results.Count | Should Be 1
        "$($results[0].Code) $(($results[0].Text | ConvertFrom-Json).check)" | Should Be '1 internal'
        ($records | Out-String) | Should Not Match 'SECRET'
    }

    It 'A1 relative Entry resolves against the PowerShell location' {
        $folder = Join-Path $TestDrive 'entry folder'
        $null = [IO.Directory]::CreateDirectory($folder)
        Push-Location -LiteralPath $folder
        try { $providers = New-MuxAgentProviders -Entry '.\mux-agent.ps1' }
        finally { Pop-Location }
        $providers.Entry | Should BeExactly (Join-Path $folder 'mux-agent.ps1')
        $providers.Launcher | Should BeExactly (Join-Path $folder 'mux-agent-launcher.ps1')
    }

    It 'A7 parser forms' {
        $spawn = ConvertFrom-MuxAgentArgs -Arguments @('spawn', '-Client', 'claude', '-TaskFile', 't.md', '-NoFork', '-Text')
        "$($spawn.Options.Keys -join ',') $($spawn.Options.Client) $($spawn.Options.TaskFile) $($spawn.Options.NoFork) $($spawn.Text)" | Should BeExactly 'Client,TaskFile,NoFork claude t.md True True'
        $focus = ConvertFrom-MuxAgentArgs -Arguments @('focus', 'claude-1')
        "$($focus.Verb) $($focus.Positional -join ',') $($focus.Options.Count)" | Should BeExactly 'focus claude-1 0'
        $repeated = Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @('spawn', '-Client', 'claude', '-Client', 'copilot')
        "$($repeated.Code) $(($repeated.Text | ConvertFrom-Json).check)" | Should Be '2 usage'
        $cased = ConvertFrom-MuxAgentArgs -Arguments @('spawn', '-client', 'claude')
        (@($cased.Options.Keys) -join ',') | Should BeExactly 'Client'
    }

    It 'A7 wrapped refusal maps the inner code' {
        Register-MuxAgentVerb -Verb 't11-wrapped' -Handler {
            param($P, $Request)
            try { Exit-MuxAgentCommand 2 'policy' 'refused inside' } catch { throw [InvalidOperationException]::new('outer wrapper', $_.Exception) }
        } -Usage 't11-wrapped'
        $result = Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @('t11-wrapped')
        $reply = $result.Text | ConvertFrom-Json
        "$($result.Code) $($reply.code) $($reply.check) $($reply.message)" | Should BeExactly '2 2 policy refused inside'
    }

    It 'A1 native success passes the provider result through' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match ' show-options ' -StdOut " value with spaces `n" -StdErr 'warning text'
        $result = Invoke-MuxAgentNative -P $fake -Arguments @('show-options', '-g')
        "$($result.ExitCode)|$($result.StdOut)|$($result.StdErr)" | Should BeExactly "0| value with spaces `n|warning text"
    }

    It 'A1 production providers have the planned members' {
        $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        $planned = @('Env', 'Pid', 'Cwd', 'Home', 'StateRoot', 'Entry', 'Launcher', 'Pwsh', 'Processes', 'Open', 'Kill', 'Rmux', 'RmuxPath', 'Token', 'Now', 'Sleep', 'Admitted', 'Platform', 'Sandbox', 'Endpoint', 'Mux', 'ClientOf', 'IsShell', 'SameStart', 'Argv', 'ClaudeDirectory', 'StopReport', 'StateRootOf', 'PrivateDirectories', 'LockName', 'ClientImage', 'IsUnder')
        (@($providers.PSObject.Properties.Name | Sort-Object) -join ',') | Should BeExactly (@($planned | Sort-Object) -join ',')
        "$($providers.Env['PATH'] -eq $env:Path) $($providers.Env['path'] -eq $env:Path)" | Should Be 'True True'
        "$($providers.Pid -eq $PID) $($providers.Cwd -eq (Get-Location -PSProvider FileSystem).ProviderPath) $($providers.Home -eq $HOME)" | Should Be 'True True True'
        $providers.StateRoot | Should BeExactly (Join-Path $env:LOCALAPPDATA 'mux-agents\state')
        $now = & $providers.Now
        "$($now.Kind) $([Math]::Abs(($now - [DateTime]::UtcNow).TotalSeconds) -lt 5) $($providers.Admitted.GetType().Name) $($providers.Admitted)" | Should BeExactly 'Utc True Boolean False'
        "$(@('Processes', 'Rmux', 'Token', 'Sandbox', 'Endpoint', 'Mux', 'ClientOf', 'IsShell', 'SameStart', 'Argv', 'ClaudeDirectory', 'StopReport', 'StateRootOf', 'PrivateDirectories', 'LockName', 'ClientImage', 'IsUnder') | ForEach-Object { $providers.$_ -is [scriptblock] })" | Should Be ((@(1..17) | ForEach-Object { 'True' }) -join ' ')
        $providers.Platform | Should BeExactly 'windows'
    }

    It 'A1 production Open and Kill' {
        $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        $child = Start-Process -FilePath $providers.Pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 60' -WindowStyle Hidden -PassThru
        try {
            $opened = & $providers.Open $child.Id
            "$($opened.Id) $($opened.Start)" | Should BeExactly "$($child.Id) $($child.StartTime.ToFileTimeUtc())"
            "$(& $providers.Kill $opened) $($child.HasExited)" | Should Be 'True True'
        }
        finally { if (-not $child.HasExited) { $child.Kill() } }
        $unused = 4194303
        while (Get-Process -Id $unused -ErrorAction SilentlyContinue) { $unused -= 4 }
        ($null -eq (& $providers.Open $unused)) | Should Be $true
        # Idle (PID 0) exists, and neither its handle nor its start can be read.
        $idle = Get-Process -Id 0
        "$($null -eq $idle.Handle) $($null -eq $idle.StartTime)" | Should Be 'True True'
        $openedIdle = & $providers.Open 0
        "$($openedIdle.Id) $($null -eq $openedIdle.Start)" | Should Be '0 True'
        $throws = [pscustomobject]@{ HasExited = $false }
        Add-Member -InputObject $throws -MemberType ScriptMethod -Name Kill -Value { throw 'denied' }
        $stuck = [pscustomobject]@{ HasExited = $false; Waits = [Collections.Generic.List[object]]::new() }
        Add-Member -InputObject $stuck -MemberType ScriptMethod -Name Kill -Value { }
        Add-Member -InputObject $stuck -MemberType ScriptMethod -Name WaitForExit -Value { param($Milliseconds) $this.Waits.Add($Milliseconds); $false }
        "$(& $providers.Kill ([pscustomobject]@{ Id = 1; Start = 1; Process = $throws })) $(& $providers.Kill ([pscustomobject]@{ Id = 2; Start = 2; Process = $stuck })) $($stuck.Waits -join ',')" | Should Be 'False False 5000'
        # An exited process is never killed again.
        $exited = [pscustomobject]@{ HasExited = $true; KillCalls = 0 }
        Add-Member -InputObject $exited -MemberType ScriptMethod -Name Kill -Value { $this.KillCalls++; throw 'must not be called' }
        Add-Member -InputObject $exited -MemberType ScriptMethod -Name WaitForExit -Value { param($Milliseconds) $true }
        "$(& $providers.Kill ([pscustomobject]@{ Id = 3; Start = 3; Process = $exited })) $($exited.KillCalls)" | Should Be 'True 0'
    }

    It 'A1 production Open gives no Start without the held handle' {
        $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        # PowerShell turns a failing property getter into $null: here System's handle is unreadable but its start is readable.
        $system = Get-Process -Id 4 -ErrorAction SilentlyContinue
        if ($system -and $null -eq $system.Handle -and $null -ne $system.StartTime) {
            $openedSystem = & $providers.Open 4
            "$($openedSystem.Id) $($null -eq $openedSystem.Start)" | Should Be '4 True'
        }
    }

    It 'A1 fake providers mirror production and expose controls' {
        $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        $fake = New-FakeMuxAgentProviders
        @($providers.PSObject.Properties.Name | Where-Object { -not $fake.PSObject.Properties[$_] }).Count | Should Be 0
        $fake.Admitted | Should Be $true
        $fake.OpenResults[100] = [pscustomobject]@{ Id = 100; Start = $null; Process = $null }
        $overridden = & $fake.Open 100
        "$($overridden.Id) $($null -eq $overridden.Start) $((& $fake.Open 200).Start) $($null -eq (& $fake.Open 4242))" | Should Be '100 True 2000 True'
        $fake.Survivors.Add(150)
        "$(& $fake.Kill ([pscustomobject]@{ Id = 170 })) $(& $fake.Kill ([pscustomobject]@{ Id = 150 })) $($fake.Killed -join ',')" | Should Be 'True False 170,150'
        $before = & $fake.Now
        & $fake.Sleep 1500
        ((& $fake.Now) - $before).TotalMilliseconds | Should Be 1500
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut 'override'
        (Invoke-MuxAgentNative -P $fake -Arguments @('display-message', '-p', 'x')).StdOut | Should BeExactly 'override'
        Add-FakeRmuxRule -Fake $fake -Match ' send-keys ' -StdOut 'static' -Script { param([string[]]$Arguments) [pscustomobject]@{ ExitCode = 3; StdOut = ($Arguments -join '+'); StdErr = 'scripted' } }
        $scripted = Invoke-MuxAgentNative -P $fake -Arguments @('send-keys', '-t', '%9', 'x') -AllowFailure
        "$($scripted.ExitCode)|$($scripted.StdOut)|$($scripted.StdErr)" | Should BeExactly '3|-N+send-keys+-t+%9+x|scripted'
        $unmatched = Invoke-MuxAgentNative -P $fake -Arguments @('select-pane', '-t', '%9')
        "$($unmatched.ExitCode)|$($unmatched.StdOut)|$($unmatched.StdErr)" | Should BeExactly '0||'
    }

    It 'A1 fake callers have the documented rows, variables and fixtures' {
        $modeRow = '' + [char]0x23F5 + [char]0x23F5 + ' bypass permissions on (shift+tab to cycle)'
        $chains = @{ human = '900>4 100>900 200>100'; claude = '900>4 100>900 150>100 170>150 200>170'; copilot = '900>4 100>900 140>100 150>140 170>150 200>170' }
        $leaves = @{ human = 'rmux.exe pwsh.exe pwsh.exe'; claude = 'rmux.exe pwsh.exe claude.exe bash.exe pwsh.exe'; copilot = 'rmux.exe pwsh.exe copilot.exe copilot.exe pwsh.exe pwsh.exe' }
        $shared = @('RMUX=\\.\pipe\rmux-test,900,0', 'RMUX_PANE=%1', 'TMUX=\\.\pipe\rmux-test,900,0', 'TMUX_PANE=%1')
        $environments = @{
            human = $shared
            claude = $shared + @('CLAUDECODE=1', 'CLAUDE_CODE_ENTRYPOINT=cli', 'CLAUDE_CODE_SESSION_ID=11111111-2222-4333-8444-555555555555', 'CLAUDE_PID=150')
            copilot = $shared + @('COPILOT_AGENT_SESSION_ID=66666666-7777-4888-9999-aaaaaaaaaaaa', 'COPILOT_CLI=1')
        }
        foreach ($caller in @('human', 'claude', 'copilot')) {
            $fake = New-FakeMuxAgentProviders -Caller $caller -Root (Join-Path $TestDrive "caller $caller")
            (@($fake.Rows | ForEach-Object { "$($_.Pid)>$($_.ParentPid)" }) -join ' ') | Should BeExactly $chains[$caller]
            @($fake.Rows | Where-Object { $_.Start -ne $_.Pid * 10 }).Count | Should Be 0
            (@($fake.Rows | ForEach-Object { [IO.Path]::GetFileName($_.Path) }) -join ' ') | Should BeExactly $leaves[$caller]
            $fake.Pid | Should Be 200
            (@($fake.Env.Keys | ForEach-Object { "$_=$($fake.Env[$_])" } | Sort-Object) -join ';') | Should BeExactly (@($environments[$caller] | Sort-Object) -join ';')
            if ($caller -eq 'claude') {
                ($fake.Rows | Where-Object Pid -eq 150).Path | Should BeExactly (Join-Path $fake.Home '.local\bin\claude.exe')
                $fake.Fixtures.ClaudeTranscript | Should BeExactly (Join-Path $fake.Home ".claude\projects\fake-work\$($fake.Env['CLAUDE_CODE_SESSION_ID']).jsonl")
                $transcript = [IO.File]::ReadAllText($fake.Fixtures.ClaudeTranscript)
                $transcript.EndsWith("`n") | Should Be $true
                $modes = @($transcript -split "`n" | Where-Object { $_ } | ForEach-Object { ConvertFrom-Json $_ -ErrorAction Stop } | Where-Object type -eq 'permission-mode')
                $modes[-1].permissionMode | Should BeExactly 'bypassPermissions'
                # Exactly one complete object on one LF-terminated line: appended or trailing text fails.
                $sessionLines = [IO.File]::ReadAllText($fake.Fixtures.ClaudeSession).Split("`n")
                "$($sessionLines.Count) $($sessionLines[0].EndsWith('}')) [$($sessionLines[1])]" | Should Be '2 True []'
                $session = @(ConvertFrom-Json $sessionLines[0] -ErrorAction Stop)
                ("$($session.Count) " + (@($session[0].PSObject.Properties | ForEach-Object { "$($_.Name)=$($_.Value):$($_.Value.GetType().Name)" }) -join ' ')) | Should BeExactly "1 pid=150:Int64 sessionId=11111111-2222-4333-8444-555555555555:String cwd=$($fake.Cwd):String procStart=1500:String"
                "$($fake.Env['CLAUDE_PID']) $($fake.Fixtures.ClaudeSession -eq (Join-Path $fake.Home '.claude\sessions\150.json'))" | Should Be '150 True'
            }
            if ($caller -eq 'copilot') {
                ($fake.Rows | Where-Object Pid -eq 150).Path | Should BeExactly (Join-Path $fake.Home 'AppData\Local\Microsoft\WinGet\Links\copilot.exe')
                ($fake.Rows | Where-Object Pid -eq 140).Path | Should BeExactly 'C:\Program Files\Microsoft VS Code\bin\copilot-shim\copilot.exe'
                $fake.Fixtures.CopilotEvents | Should BeExactly (Join-Path $fake.Home ".copilot\session-state\$($fake.Env['COPILOT_AGENT_SESSION_ID'])\events.jsonl")
                @(Get-Content -LiteralPath $fake.Fixtures.CopilotWorkspace | Where-Object { $_ -match '^mc_(task_id|session_id|last_event_id): ' }).Count | Should Be 3
                $events = [IO.File]::ReadAllText($fake.Fixtures.CopilotEvents)
                $events.EndsWith("`n") | Should Be $true
                $lines = @($events -split "`n" | Where-Object { $_ })
                $records = @($lines | ForEach-Object { ConvertFrom-Json $_ -ErrorAction Stop })
                $records.Count | Should Be $lines.Count
                "$($records[1].type) $($records[1].data.allowAllPermissions)" | Should BeExactly 'session.permissions_changed True'
            }
            $probe = (Invoke-MuxAgentNative -P $fake -Arguments @('display-message', '-p', '-t', '%1', 'f')).StdOut
            $panes = (Invoke-MuxAgentNative -P $fake -Arguments @('list-panes', '-a')).StdOut
            $window = (Invoke-MuxAgentNative -P $fake -Arguments @('new-window', '-P')).StdOut
            $split = (Invoke-MuxAgentNative -P $fake -Arguments @('split-window', '-P')).StdOut
            "$probe|$panes|$window|$split" | Should BeExactly "900|100|`$3|@1|`n||%9|@5`n|%9|@5`n"
            # The whole default screen in order: the statusline rows, then the mode row (with the real screen's two
            # leading spaces) as the last non-blank row, then blank rows.
            $expectedScreen = @('> build the dispatcher', '', 'Done.', '', ('-' * 40), '> ', ('-' * 40), '   Time 12:00:00 | Run 2m',
                '   Model Opus 5.5 | Effort high', '   ~\fake work', '   Repo clean | Branch master', ('  ' + $modeRow), '', '') -join "`n"
            (Invoke-MuxAgentNative -P $fake -Arguments @('capture-pane', '-p', '-t', '%1')).StdOut | Should BeExactly $expectedScreen
        }
    }

    It 'A7 parser keys the other spawn value options' {
        $spawn = ConvertFrom-MuxAgentArgs -Arguments @('spawn', '-Client', 'copilot', '-Task', 'x', '-Name', 'c1', '-Split', 'h', '-BriefFile', 'b.md')
        (@($spawn.Options.Keys | ForEach-Object { "$_=$($spawn.Options[$_])" }) -join ' ') | Should BeExactly 'Client=copilot Task=x Name=c1 Split=h BriefFile=b.md'
        "$($spawn.Positional.Count) $($spawn.Text)" | Should Be '0 False'
    }

    It 'A7 unexpected error names the full base exception type' {
        Register-MuxAgentVerb -Verb 't11-format' -Handler { param($P, $Request) throw [System.FormatException]::new('SECRET') } -Usage 't11-format'
        Register-MuxAgentVerb -Verb 't11-format-inner' -Handler { param($P, $Request) throw [InvalidOperationException]::new('outer', [System.FormatException]::new('SECRET')) } -Usage 't11-format-inner'
        foreach ($verb in @('t11-format', 't11-format-inner')) {
            $result = Invoke-MuxAgentCommand -P (New-FakeMuxAgentProviders) -Arguments @($verb)
            $reply = $result.Text | ConvertFrom-Json
            "$verb $($result.Code) $($reply.check) $($reply.message)" | Should BeExactly "$verb 1 internal unexpected error (System.FormatException)"
            $result.Text | Should Not Match 'SECRET'
        }
    }

    It 'A1 production providers snapshot Env and resolve Entry, Pwsh and Sleep' {
        $name = 'RMUX_AGENT_T11_' + [guid]::NewGuid().ToString('N')
        [Environment]::SetEnvironmentVariable($name, 'marker value', 'Process')
        try { $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1') }
        finally { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
        "[$($providers.Env[$name])]" | Should BeExactly '[marker value]'
        $providers.Entry | Should BeExactly (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        $providers.Launcher | Should BeExactly (Join-Path $repoRoot 'scripts\mux-agent-launcher.ps1')
        "$([IO.Path]::GetFileName($providers.Pwsh)) $([IO.File]::Exists($providers.Pwsh))" | Should Be 'pwsh.exe True'
        $watch = [Diagnostics.Stopwatch]::StartNew()
        & $providers.Sleep 50
        $watch.Stop()
        "$($watch.ElapsedMilliseconds -ge 40) $($watch.ElapsedMilliseconds -lt 2000)" | Should Be 'True True'
    }

    It 'A1 fake TokenInfo defaults to an unrestricted Medium token' {
        $fake = New-FakeMuxAgentProviders
        $token = & $fake.Token
        "$($token.IsAppContainer) $($token.IsRestricted) $($token.IntegrityLevel)" | Should BeExactly 'False False S-1-16-8192'
        $fake.TokenInfo = [pscustomobject]@{ IsAppContainer = $true; IsRestricted = $false; IntegrityLevel = 'S-1-16-4096' }
        (& $fake.Token).IntegrityLevel | Should BeExactly 'S-1-16-4096'
    }

    It 'A1 native AllowFailure returns the failed result unchanged' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match ' kill-pane ' -ExitCode 1 -StdOut " partial out `n" -StdErr 'SECRET detail'
        $failed = Invoke-MuxAgentNative -P $fake -Arguments @('kill-pane', '-t', '%9') -AllowFailure
        "$($failed.ExitCode)|$($failed.StdOut)|$($failed.StdErr)" | Should BeExactly "1| partial out `n|SECRET detail"
        $message = $null
        try { $null = Invoke-MuxAgentNative -P $fake -Arguments @('kill-pane', '-t', '%9') } catch { $message = $_.Exception.Message }
        "[$message]" | Should BeExactly '[rmux call failed]'
    }

    It 'A1 production deferred members call their functions' {
        $providers = New-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        # Temporary stand-ins for the T1.2 and T3.1 functions, defined in this It and removed in finally.
        function Get-MuxAgentProcessTable { @([pscustomobject]@{ Pid = 7 }, [pscustomobject]@{ Pid = 8 }) }
        function Invoke-MuxAgentRmux { param($P, [string[]]$Arguments) [pscustomobject]@{ ExitCode = 0; StdOut = 'stored ' + ($Arguments -join ' '); StdErr = '' } }
        function Get-MuxAgentToken { [pscustomobject]@{ IntegrityLevel = 'token stand-in' } }
        try {
            "$(@(& $providers.Processes | ForEach-Object Pid) -join ',')|$((& $providers.Rmux @('-N', 'list-panes', '-a')).StdOut)|$((& $providers.Token).IntegrityLevel)" | Should BeExactly '7,8|stored -N list-panes -a|token stand-in'
        }
        finally {
            foreach ($name in @('Get-MuxAgentProcessTable', 'Invoke-MuxAgentRmux', 'Get-MuxAgentToken')) { Remove-Item -Path "Function:\$name" -ErrorAction SilentlyContinue }
        }
    }

    It 'A1 fake Processes and Token forward the current Rows and TokenInfo' {
        $fake = New-FakeMuxAgentProviders
        $fake.Rows.Add((New-FakeMuxAgentRow 300 200 'C:\added.exe' 'added'))
        (@(& $fake.Processes | ForEach-Object Pid) -join ',') | Should BeExactly '900,100,200,300'
        $fake.Rows = [Collections.Generic.List[object]]::new()
        $fake.Rows.Add((New-FakeMuxAgentRow 400 1 'C:\replaced.exe' 'replaced'))
        (@(& $fake.Processes | ForEach-Object Pid) -join ',') | Should BeExactly '400'
        $fake.TokenInfo = [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $true; IntegrityLevel = 'S-1-16-12288' }
        "$((& $fake.Token).IsRestricted) $((& $fake.Token).IntegrityLevel)" | Should BeExactly 'True S-1-16-12288'
    }

    It 'A1 fake paths lie under Root' {
        $root = Join-Path $TestDrive 'paths root'
        $fake = New-FakeMuxAgentProviders -Root $root
        foreach ($name in @('StateRoot', 'Home', 'Cwd')) {
            "$name $($fake.$name.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase))" | Should BeExactly "$name True"
        }
    }

    It 'A1 fake controls have the documented values' {
        $root = Join-Path $TestDrive 'controls root'
        $fake = New-FakeMuxAgentProviders -Root $root
        "$($fake.Env['rmux']) $($fake.Env['tmux_pane'])" | Should BeExactly '\\.\pipe\rmux-test,900,0 %1'
        "$($fake.StateRoot -eq (Join-Path $root 'fake state')) $($fake.Home -eq (Join-Path $root 'fake home')) $($fake.Cwd -eq (Join-Path $root 'fake work'))" | Should Be 'True True True'
        # Home and Cwd exist; StateRoot does not, so the code under test creates it.
        "$([IO.Directory]::Exists($fake.StateRoot)) $([IO.Directory]::Exists($fake.Home)) $([IO.Directory]::Exists($fake.Cwd))" | Should Be 'False True True'
        $fake.Entry | Should BeExactly (Join-Path $repoRoot 'scripts\mux-agent.ps1')
        $fake.Launcher | Should BeExactly (Join-Path $repoRoot 'scripts\mux-agent-launcher.ps1')
        $fake.Pwsh | Should BeExactly (Join-Path $PSHOME 'pwsh.exe')
        "$($fake.Clock.ToString('o')) $($fake.Clock.Kind)" | Should BeExactly '2026-10-29T12:00:00.0000000Z Utc'
        "$($fake.Calls.Count) $($fake.Killed.Count) $($fake.Survivors.Count) $($fake.OpenResults.Count)" | Should Be '0 0 0 0'
        $opened = & $fake.Open 200
        "$($opened.Id) $($opened.Start) $($null -eq $opened.Process)" | Should Be '200 2000 True'
        $rules = @($fake.Rules | ForEach-Object { "$($_.Match)|$($_.StdOut)|$($_.ExitCode)|$($_.StdErr)|$($null -eq $_.Script)" })
        $expectedRules = @("^-N display-message |900|100|`$3|@1|`n|0||True", '^-N list-panes( |$)||0||True', "^-N (new-window|split-window) |%9|@5`n|0||True")
        ($rules[0..2] -join "`r") | Should BeExactly ($expectedRules -join "`r")
        "$($rules.Count) $($fake.Rules[3].Match) $($fake.Rules[3].ExitCode) [$($fake.Rules[3].StdErr)] $($null -eq $fake.Rules[3].Script)" | Should BeExactly '4 ^-N capture-pane  0 [] True'
    }

    It 'A1 fake rows and fixture files have the documented values' {
        $claudeSession = '11111111-2222-4333-8444-555555555555'
        $copilotSession = '66666666-7777-4888-9999-aaaaaaaaaaaa'
        foreach ($caller in @('human', 'claude', 'copilot')) {
            $fake = New-FakeMuxAgentProviders -Caller $caller -Root (Join-Path $TestDrive "values $caller")
            $pwsh = Join-Path $PSHOME 'pwsh.exe'
            $rows = @("900|4|9000|$(Join-Path $fake.Home 'AppData\Local\rmux\rmux.exe')|rmux.exe", "100|900|1000|$pwsh|`"$pwsh`" -NoLogo")
            $files = [ordered]@{}
            if ($caller -eq 'claude') {
                $claude = Join-Path $fake.Home '.local\bin\claude.exe'
                $rows += @("150|100|1500|$claude|`"$claude`" --dangerously-skip-permissions", '170|150|1700|C:\Program Files\Git\bin\..\usr\bin\bash.exe|/usr/bin/bash -c "pwsh -NoProfile -File mux-agent.ps1"')
                $files[(Join-Path $fake.Home ".claude\projects\fake-work\$claudeSession.jsonl")] = @(
                    ('{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"' + $claudeSession + '"}'),
                    ('{"type":"user","sessionId":"' + $claudeSession + '","message":{"role":"user","content":"build the dispatcher"}}'))
            }
            if ($caller -eq 'copilot') {
                $copilot = Join-Path $fake.Home 'AppData\Local\Microsoft\WinGet\Links\copilot.exe'
                $rows += @('140|100|1400|C:\Program Files\Microsoft VS Code\bin\copilot-shim\copilot.exe|copilot --allow-all', "150|140|1500|$copilot|`"$copilot`" --allow-all",
                    "170|150|1700|$pwsh|`"$pwsh`" -NoProfile -Command pwsh -NoProfile -File mux-agent.ps1")
                $folder = Join-Path $fake.Home ".copilot\session-state\$copilotSession"
                $files[(Join-Path $folder 'workspace.yaml')] = @("id: $copilotSession", "cwd: $($fake.Cwd)", 'client_name: fake-cli', 'name: fake session', 'user_named: false',
                    'summary_count: 0', 'fork_count: 0', 'created_at: 2026-10-29T11:59:00.000Z', 'updated_at: 2026-10-29T12:00:00.000Z', 'remote_steerable: false',
                    'mc_task_id: f0000000-0000-4000-8000-000000000001', 'mc_session_id: f0000000-0000-4000-8000-000000000002', 'mc_last_event_id: e0000000-0000-4000-8000-000000000003')
                $files[(Join-Path $folder 'events.jsonl')] = @(
                    ('{"type":"session.start","data":{"sessionId":"' + $copilotSession + '","startTime":"2026-10-29T11:59:00.000Z"},"id":"e0000000-0000-4000-8000-000000000001","timestamp":"2026-10-29T11:59:00.000Z","parentId":null}'),
                    '{"type":"session.permissions_changed","data":{"previousAllowAllPermissions":false,"allowAllPermissions":true,"previousAllowAllPermissionMode":"off","allowAllPermissionMode":"on"},"id":"e0000000-0000-4000-8000-000000000002","timestamp":"2026-10-29T11:59:01.000Z","parentId":"e0000000-0000-4000-8000-000000000001"}',
                    '{"type":"session.info","data":{"infoType":"status","message":"ready"},"id":"e0000000-0000-4000-8000-000000000003","timestamp":"2026-10-29T11:59:02.000Z","parentId":"e0000000-0000-4000-8000-000000000002"}')
            }
            $rows += "200|$(if ($caller -eq 'human') { 100 } else { 170 })|2000|$pwsh|`"$pwsh`" -NoProfile -File mux-agent.ps1"
            (@($fake.Rows | ForEach-Object { "$($_.Pid)|$($_.ParentPid)|$($_.Start)|$($_.Path)|$($_.CommandLine)" }) -join "`n") | Should BeExactly ($rows -join "`n")
            foreach ($path in $files.Keys) { ("$path`n" + [IO.File]::ReadAllText($path)) | Should BeExactly ("$path`n" + ($files[$path] -join "`n") + "`n") }
        }
    }
}

Describe 'MuxAgents T1.2 caller' -Tag 'windows' {
    BeforeEach { $script:MuxAgentStoreContext = $null }
    AfterAll { $script:MuxAgentStoreContext = $null }

    function Get-T12Outcome {
        # The caller, or 'code check' for a typed refusal; any other error is rethrown.
        param($Fake)
        try { Resolve-MuxAgentCaller -P $Fake }
        catch {
            if (-not $_.Exception.Data.Contains('MuxAgentCode')) { throw }
            "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck'])"
        }
    }

    function Get-T12Kind {
        # The caller's Kind, or 'code check' for a typed refusal.
        param($Fake)
        $outcome = Get-T12Outcome $Fake
        if ($outcome -is [string]) { $outcome } else { $outcome.Kind }
    }

    function Get-T12Refusal {
        # 'code check message' for a typed refusal, or 'resolved'.
        param($Fake)
        try { $null = Resolve-MuxAgentCaller -P $Fake; 'resolved' }
        catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" }
    }

    It 'A1 T1.2 functions resolve to the library after the T1.1 stand-ins' {
        $library = Join-Path $repoRoot 'scripts\MuxAgents.ps1'
        foreach ($name in @('Get-MuxAgentProcessTable', 'Invoke-MuxAgentRmux', 'Resolve-MuxAgentCaller', 'Resolve-MuxAgentExecutable')) {
            "$name $((Get-Command $name -CommandType Function).ScriptBlock.File)" | Should BeExactly "$name $library"
        }
        @(Get-MuxAgentProcessTable | Where-Object Pid -eq $PID).Count | Should Be 1
    }

    It 'A1 human caller resolves' {
        $fake = New-FakeMuxAgentProviders
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Pane) $($caller.Session) $($caller.Window) $($caller.ServerPid) $($caller.ServerStart) $($caller.Socket)" | Should BeExactly 'human %1 $3 @1 900 9000 \\.\pipe\rmux-test'
        $caller.ServerKey | Should MatchExactly '^[0-9a-f]{16}$'
        "[$($caller.AgentTag)] $($null -eq $caller.Client) $($null -eq $caller.Agent) $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly '[] True True 200>100'
        $fake.Calls.Count | Should Be 1
        "$($fake.Calls[0].Count) $($fake.Calls[0] -join ' ')" | Should BeExactly '6 -N display-message -p -t %1 #{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}'
        (@($caller.PSObject.Properties.Name) -join ',') | Should BeExactly 'Kind,Client,Agent,Chain,Pane,Window,Session,ServerPid,ServerStart,Socket,ServerKey,AgentTag'
        "$($caller.ServerPid.GetType().Name) $($caller.Chain.GetType().Name)" | Should BeExactly 'Int32 Object[]'
    }

    It 'A1 the probe targets the caller pane' {
        $fake = New-FakeMuxAgentProviders
        $fake.Env['TMUX_PANE'] = '%7'
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Pane) $($fake.Calls[0][4])" | Should BeExactly '%7 %7'
    }

    It 'A1 a non-default socket, server PID, session and window are returned exactly' {
        $fake = New-FakeMuxAgentProviders
        foreach ($name in @('RMUX', 'TMUX')) { $fake.Env[$name] = '\\.\pipe\rmux-other-7,4242,3' }
        $fake.OpenResults[4242] = [pscustomobject]@{ Id = 4242; Start = [long]42420; Process = $null }
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "4242|100|`$17|@23|`n"
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Socket) $($caller.ServerPid) $($caller.ServerStart) $($caller.Session) $($caller.Window)" | Should BeExactly '\\.\pipe\rmux-other-7 4242 42420 $17 @23'
        $caller.ServerKey | Should BeExactly (Get-MuxAgentHash '\\.\pipe\rmux-other-7|4242|42420').Substring(0, 16)
    }

    It 'A1 an agent at a non-default PID is returned exactly' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        ($fake.Rows | Where-Object Pid -eq 150).Pid = 1234
        ($fake.Rows | Where-Object Pid -eq 170).ParentPid = 1234
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Agent.Pid) $($caller.Agent.Start) $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'agent 1234 1500 200>170>1234>100'
    }

    It 'A1 resolving a caller writes no file' {
        # NTFS updates directory times in the parent's index lazily, so compare paths, file sizes and file contents.
        $snapshot = { param($Root) @(Get-ChildItem -LiteralPath $Root -Recurse -Force | ForEach-Object { if ($_.PSIsContainer) { "dir $($_.FullName)" } else { "file $($_.FullName) $($_.Length) $((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash)" } } | Sort-Object) }
        foreach ($kind in @('human', 'claude', 'copilot')) {
            $root = Join-Path $TestDrive "no write $kind"
            $fake = New-FakeMuxAgentProviders -Caller $kind -Root $root
            $before = @(& $snapshot $root)
            $null = Resolve-MuxAgentCaller -P $fake
            $after = @(& $snapshot $root)
            "$kind $($after.Count) $([IO.Directory]::Exists($fake.StateRoot)) $(($after -join "`n") -ceq ($before -join "`n"))" | Should BeExactly "$kind $($before.Count) False True"
        }
    }

    It 'A1 missing endpoint exits 1 with no rmux call' {
        $pipe = '\\.\pipe\rmux-test'
        $cases = @(
            @{ RMUX = $null; TMUX = $null }, @{ RMUX = ''; TMUX = '' }, @{ TMUX_PANE = $null }, @{ TMUX_PANE = '1' }, @{ TMUX_PANE = '%' }, @{ TMUX_PANE = 'x%1' }, @{ TMUX_PANE = '%1x' },
            @{ TMUX_PANE = '%a' }, @{ RMUX = "$pipe,900" }, @{ RMUX = "$pipe,900," }, @{ RMUX = "$pipe,,0" }, @{ RMUX = "$pipe,x900,0" }, @{ RMUX = "x$pipe,900,0" }, @{ RMUX = "$pipe,900,0x" },
            @{ RMUX = '\\.\pipe\,900,0' }, @{ RMUX = '\\.\pipe\rmux,1,900,0' }, @{ RMUX = '\\?\pipe\rmux-test,900,0' }, @{ RMUX = 'C:\pipe\rmux-test,900,0' }, @{ RMUX = "$pipe`nx,900,0" }, @{ RMUX = "$pipe`rx,900,0" })
        foreach ($case in $cases) {
            $fake = New-FakeMuxAgentProviders
            foreach ($name in @($case.Keys)) { if ($null -eq $case[$name]) { $fake.Env.Remove($name) } else { $fake.Env[$name] = $case[$name] } }
            $label = (@($case.Keys | Sort-Object | ForEach-Object { "$_=[$($case[$_])]" }) -join ' ') -replace "`r", '<CR>' -replace "`n", '<LF>'
            "$label => $(Get-T12Outcome $fake) $($fake.Calls.Count)" | Should BeExactly "$label => 1 endpoint 0"
        }
    }

    It 'A1 RMUX wins over TMUX, TMUX alone is enough and the pipe prefix ignores case' {
        $valid = '\\.\pipe\rmux-test,900,0'
        foreach ($case in @(@($valid, 'bad', 'human'), @('bad', $valid, '1 endpoint'), @($null, $valid, 'human'), @('', $valid, 'human'), @('\\.\PIPE\rmux-test,900,0', $null, 'human'))) {
            $fake = New-FakeMuxAgentProviders
            foreach ($pair in @(@('RMUX', $case[0]), @('TMUX', $case[1]))) { if ($null -eq $pair[1]) { $fake.Env.Remove($pair[0]) } else { $fake.Env[$pair[0]] = $pair[1] } }
            "RMUX=[$($case[0])] TMUX=[$($case[1])] => $(Get-T12Kind $fake)" | Should BeExactly "RMUX=[$($case[0])] TMUX=[$($case[1])] => $($case[2])"
        }
    }

    It 'A1 dead endpoint exits 1 and only probes' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -ExitCode 1 -StdErr 'no server running on \\.\pipe\rmux-test'
        "$(Get-T12Outcome $fake) $($fake.Calls.Count) $(@($fake.Calls | Where-Object { $_ -contains 'new-window' }).Count)" | Should BeExactly '1 endpoint 1 0'
    }

    It 'A1 any probe failure exits 1 endpoint without its text' {
        foreach ($case in @(@{ ExitCode = 1; StdOut = "900|100|`$3|@1|`n" }, @{ ExitCode = 2; StdOut = '' }, @{ Script = { param([string[]]$Arguments) throw 'SECRET timeout' } }, @{ Script = { param([string[]]$Arguments) [pscustomobject]@{ ExitCode = 0; StdOut = $null; StdErr = '' } } })) {
            $fake = New-FakeMuxAgentProviders
            Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' @case
            $message = try { $null = Resolve-MuxAgentCaller -P $fake; 'resolved' } catch { $_.Exception.Message }
            "$(Get-T12Outcome $fake) $($message -match 'SECRET|resolved')" | Should BeExactly '1 endpoint False'
        }
    }

    It 'A1 a Connect refusal inside the probe keeps its message' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -Script { param([string[]]$Arguments) Exit-MuxAgentCommand 1 'endpoint' 'cannot connect to the rmux endpoint' }
        "$(Get-T12Refusal $fake) $($fake.Calls.Count)" | Should BeExactly '1 endpoint cannot connect to the rmux endpoint 1'
    }

    It 'A1 stale server PID exits 1' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "901|100|`$3|@1|`n"
        "$(Get-T12Outcome $fake) $($fake.Calls.Count)" | Should BeExactly '1 endpoint 1'
        # rmux 0.10.0 answers a probe of a gone pane with exit 0 and empty fields.
        foreach ($row in @('', "`n", "||||`n", "900||||`n", "900|||@1|`n")) {
            $fake = New-FakeMuxAgentProviders
            Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut $row
            $label = $row -replace "`n", '<LF>'
            "gone [$label] => $(Get-T12Refusal $fake)" | Should BeExactly "gone [$label] => 1 endpoint rmux endpoint did not answer"
        }
    }

    It 'A1 probe rows need numeric pids, a $ session and an @ window' {
        $bad = @("900|100|3|@1|", "900|100|`$|@1|", "900|100|`$3|1|", "900|100|`$3|@|", "900|x|`$3|@1|", "900||`$3|@1|", "|100|`$3|@1|", "x900|100|`$3|@1|", "900|100|`$3|@1", "900|100|`$3|@1|`nextra")
        foreach ($row in $bad) {
            $fake = New-FakeMuxAgentProviders
            Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut $row
            $label = $row -replace "`n", '<LF>'
            "[$label] => $(Get-T12Refusal $fake)" | Should BeExactly "[$label] => 1 endpoint rmux endpoint did not answer"
        }
        foreach ($case in @(@("900|100|`$3|@1|", ''), @("900|100|`$3|@1|agent-7`r`n", 'agent-7'), @("900|100|`$3|@1|a|b`n", 'a|b'))) {
            $fake = New-FakeMuxAgentProviders
            Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut $case[0]
            $caller = Resolve-MuxAgentCaller -P $fake
            "$($caller.Kind) [$($caller.AgentTag)]" | Should BeExactly "human [$($case[1])]"
        }
    }

    It 'A1 parent started after child breaks the chain' {
        $fake = New-FakeMuxAgentProviders
        ($fake.Rows | Where-Object Pid -eq 100).Start = 5000
        Get-T12Outcome $fake | Should BeExactly '1 caller'
        # A parent that started in the same tick as its child is still its parent.
        ($fake.Rows | Where-Object Pid -eq 100).Start = 2000
        Get-T12Kind $fake | Should BeExactly 'human'
        # Every link is checked, not only the pane link: each parent of the Claude chain in turn starts after its child.
        foreach ($case in @(@(170, 2100), @(150, 1800), @(100, 1600))) {
            $fake = New-FakeMuxAgentProviders -Caller claude
            ($fake.Rows | Where-Object Pid -eq $case[0]).Start = $case[1]
            "$($case[0]) => $(Get-T12Outcome $fake)" | Should BeExactly "$($case[0]) => 1 caller"
        }
    }

    It 'A1 a pane outside the ancestry or a missing row exits 1 caller' {
        foreach ($case in @('pane row', 'caller row', 'other pane')) {
            $fake = New-FakeMuxAgentProviders
            if ($case -eq 'pane row') { $null = $fake.Rows.Remove(($fake.Rows | Where-Object Pid -eq 100)) }
            if ($case -eq 'caller row') { $null = $fake.Rows.Remove(($fake.Rows | Where-Object Pid -eq 200)) }
            if ($case -eq 'other pane') {
                $fake.Rows.Add((New-FakeMuxAgentRow 50 4 (Join-Path $PSHOME 'pwsh.exe') 'pwsh'))
                Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "900|50|`$3|@1|`n"
            }
            "$case => $(Get-T12Outcome $fake)" | Should BeExactly "$case => 1 caller"
        }
    }

    It 'A1 a caller that is the pane process has a one-row chain' {
        $fake = New-FakeMuxAgentProviders
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "900|200|`$3|@1|`n"
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'human 200'
    }

    It 'A1 the walk stops after 64 parents' {
        $pwsh = Join-Path $PSHOME 'pwsh.exe'
        foreach ($case in @(@(63, 'human'), @(64, '1 caller'))) {
            $fake = New-FakeMuxAgentProviders
            $parent = 100
            for ($index = 1; $index -le $case[0]; $index++) {
                $fake.Rows.Add([pscustomobject]@{ Pid = 1000 + $index; ParentPid = $parent; Start = 1000 + $index; Path = $pwsh; CommandLine = 'pwsh' })
                $parent = 1000 + $index
            }
            ($fake.Rows | Where-Object Pid -eq 200).ParentPid = $parent
            "$($case[0] + 1) hops => $(Get-T12Kind $fake)" | Should BeExactly "$($case[0] + 1) hops => $($case[1])"
        }
    }

    It 'A1 a parent cycle ends at the cap' {
        $fake = New-FakeMuxAgentProviders
        $pwsh = Join-Path $PSHOME 'pwsh.exe'
        foreach ($pair in @(@(300, 301), @(301, 300))) { $fake.Rows.Add([pscustomobject]@{ Pid = $pair[0]; ParentPid = $pair[1]; Start = 2000; Path = $pwsh; CommandLine = 'pwsh' }) }
        ($fake.Rows | Where-Object Pid -eq 200).ParentPid = 300
        Get-T12Outcome $fake | Should BeExactly '1 caller'
    }

    It 'A1 node.exe between caller and pane exits 2' {
        $fake = New-FakeMuxAgentProviders
        $fake.Rows.Add((New-FakeMuxAgentRow 150 100 'C:\Program Files\nodejs\node.exe' 'node'))
        ($fake.Rows | Where-Object Pid -eq 200).ParentPid = 150
        Get-T12Outcome $fake | Should BeExactly '2 caller'
    }

    It 'A1 every shell leaf is allowed between a human caller and its pane' {
        $cases = @(@('C:\Program Files\PowerShell\7\pwsh.exe', 'human'), @('C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe', 'human'),
            @('C:\Windows\System32\cmd.exe', 'human'), @('C:\Program Files\Git\usr\bin\bash.exe', 'human'), @('C:\Program Files\Git\usr\bin\sh.exe', 'human'),
            @('C:\WINDOWS\SYSTEM32\CMD.EXE', 'human'), @('C:\Program Files\nodejs\node.exe', '2 caller'), @('C:\Program Files\Git\usr\bin\zsh.exe', '2 caller'), @($null, '2 caller'))
        foreach ($case in $cases) {
            $fake = New-FakeMuxAgentProviders
            $row = New-FakeMuxAgentRow 150 100 'placeholder' 'shell'
            $row.Path = $case[0]
            $fake.Rows.Add($row)
            ($fake.Rows | Where-Object Pid -eq 200).ParentPid = 150
            "[$($case[0])] => $(Get-T12Kind $fake)" | Should BeExactly "[$($case[0])] => $($case[1])"
        }
    }

    It 'A1 a human pane must be a shell; the caller row is neither checked nor an agent' {
        $fake = New-FakeMuxAgentProviders
        ($fake.Rows | Where-Object Pid -eq 100).Path = 'C:\Program Files\nodejs\node.exe'
        Get-T12Outcome $fake | Should BeExactly '2 caller'
        foreach ($path in @('C:\Program Files\nodejs\node.exe', 'C:\Tools\claude.exe')) {
            $fake = New-FakeMuxAgentProviders
            ($fake.Rows | Where-Object Pid -eq 200).Path = $path
            "$path => $(Get-T12Kind $fake)" | Should BeExactly "$path => human"
        }
        $fake = New-FakeMuxAgentProviders -Caller claude
        ($fake.Rows | Where-Object Pid -eq 200).Path = 'C:\Tools\claude.exe'
        "$((Resolve-MuxAgentCaller -P $fake).Agent.Pid)" | Should BeExactly '150'
    }

    It 'A1 claude.exe found by path not name' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        foreach ($row in $fake.Rows) { $row | Add-Member -NotePropertyName Name -NotePropertyValue $null }
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Client) $($caller.Agent.Pid) $($caller.Agent.Start) $($caller.Agent.Path -eq (Join-Path $fake.Home '.local\bin\claude.exe'))" | Should BeExactly 'agent claude 150 1500 True'
        "$(@($caller.Agent.PSObject.Properties.Name) -join ',') $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'Pid,Start,Path 200>170>150>100'
        # A node.exe row named claude.exe, with claude.exe in its command line, is not an agent.
        $fake = New-FakeMuxAgentProviders -Caller claude
        $client = $fake.Rows | Where-Object Pid -eq 150
        $client.Path = 'C:\Program Files\nodejs\node.exe'
        $client | Add-Member -NotePropertyName Name -NotePropertyValue 'claude.exe'
        Get-T12Outcome $fake | Should BeExactly '2 caller'
    }

    It 'A1 claude.exe above a live-like bash chain is the agent' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        $null = $fake.Rows.Remove(($fake.Rows | Where-Object Pid -eq 170))
        $fake.Rows.Add((New-FakeMuxAgentRow 171 150 'C:\Program Files\Git\bin\bash.exe' 'bash'))
        $fake.Rows.Add((New-FakeMuxAgentRow 172 171 'C:\Program Files\Git\bin\..\usr\bin\bash.exe' 'bash'))
        $fake.Rows.Add((New-FakeMuxAgentRow 173 172 'C:\Program Files\Git\usr\bin\bash.exe' 'bash'))
        ($fake.Rows | Where-Object Pid -eq 200).ParentPid = 173
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Client) $($caller.Agent.Pid) $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'agent claude 150 200>173>172>171>150>100'
    }

    It 'A1 leaf names ignore case' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        ($fake.Rows | Where-Object Pid -eq 150).Path = 'C:\Tools\CLAUDE.EXE'
        ($fake.Rows | Where-Object Pid -eq 170).Path = 'C:\Program Files\Git\usr\bin\BASH.EXE'
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Client) $($caller.Agent.Pid)" | Should BeExactly 'agent claude 150'
    }

    It 'A1 copilot caller takes the nearest copilot.exe under an allowed VS Code shim' {
        $fake = New-FakeMuxAgentProviders -Caller copilot
        $caller = Resolve-MuxAgentCaller -P $fake
        $winget = $caller.Agent.Path -eq (Join-Path $fake.Home 'AppData\Local\Microsoft\WinGet\Links\copilot.exe')
        "$($caller.Kind) $($caller.Client) $($caller.Agent.Pid) $winget $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'agent copilot 150 True 200>170>150>140>100'
        # Above the agent any process may run up to the pane; between the caller and the agent only shells may.
        ($fake.Rows | Where-Object Pid -eq 140).Path = 'C:\Program Files\nodejs\node.exe'
        (Resolve-MuxAgentCaller -P $fake).Agent.Pid | Should Be 150
        ($fake.Rows | Where-Object Pid -eq 170).Path = 'C:\Program Files\nodejs\node.exe'
        Get-T12Outcome $fake | Should BeExactly '2 caller'
    }

    It 'A1 an agent that is the pane process is found' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "900|150|`$3|@1|`n"
        $caller = Resolve-MuxAgentCaller -P $fake
        "$($caller.Kind) $($caller.Agent.Pid) $(@($caller.Chain | ForEach-Object Pid) -join '>')" | Should BeExactly 'agent 150 200>170>150'
    }

    It 'A1 agent start comes from Open, not from the CIM row' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        $fake.OpenResults[150] = [pscustomobject]@{ Id = 150; Start = [long]1501; Process = $null }
        $fake.OpenResults[900] = [pscustomobject]@{ Id = 900; Start = [long]9001; Process = $null }
        $caller = Resolve-MuxAgentCaller -P $fake
        "$(($fake.Rows | Where-Object Pid -eq 150).Start) $($caller.Agent.Start) $(($fake.Rows | Where-Object Pid -eq 900).Start) $($caller.ServerStart)" | Should BeExactly '1500 1501 9000 9001'
    }

    It 'A1 an agent Open start from another generation exits 1 caller' {
        # Open keeps 100 ns and CIM whole microseconds: the Open start floored to the microsecond must equal the row Start.
        $changed = '1 caller agent process changed'
        $cases = @(@(1500, 'agent 1500'), @(1501, 'agent 1501'), @(1509, 'agent 1509'), @(1510, $changed), @(1499, $changed), @(1490, $changed), @(1600, $changed), @(1700, $changed), @(5000, $changed))
        foreach ($case in $cases) {
            $fake = New-FakeMuxAgentProviders -Caller claude
            $fake.OpenResults[150] = [pscustomobject]@{ Id = 150; Start = [long]$case[0]; Process = $null }
            $outcome = try { $caller = Resolve-MuxAgentCaller -P $fake; "$($caller.Kind) $($caller.Agent.Start)" } catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" }
            "Open $($case[0]) => $outcome" | Should BeExactly "Open $($case[0]) => $($case[1])"
        }
    }

    It 'A1 unreadable agent start exits 1 caller' {
        foreach ($opened in @($null, [pscustomobject]@{ Id = 150; Start = $null; Process = $null })) {
            $fake = New-FakeMuxAgentProviders -Caller claude
            $fake.OpenResults[150] = $opened
            "$($null -eq $opened) => $(Get-T12Outcome $fake)" | Should BeExactly "$($null -eq $opened) => 1 caller"
        }
    }

    It 'A1 server key changes with the server start' {
        $fake = New-FakeMuxAgentProviders
        $keys = @(foreach ($start in @(9000, 9010)) {
                $fake.OpenResults[900] = [pscustomobject]@{ Id = 900; Start = [long]$start; Process = $null }
                (Resolve-MuxAgentCaller -P $fake).ServerKey
            })
        "$($keys.Count) $($keys[0] -cne $keys[1]) $(@($keys | Where-Object { $_ -cmatch '^[0-9a-f]{16}$' }).Count)" | Should BeExactly '2 True 2'
    }

    It 'A1 server key is the first 16 hex of SHA-256 of socket, server PID and the Open start' {
        $fake = New-FakeMuxAgentProviders
        $fake.OpenResults[900] = [pscustomobject]@{ Id = 900; Start = [long]9010; Process = $null }
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $hex = -join @($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('\\.\pipe\rmux-test|900|9010')) | ForEach-Object { $_.ToString('x2') }) }
        finally { $sha.Dispose() }
        (Resolve-MuxAgentCaller -P $fake).ServerKey | Should BeExactly $hex.Substring(0, 16)
    }

    It 'A1 unreadable server start exits 1' {
        foreach ($opened in @([pscustomobject]@{ Id = 900; Start = $null; Process = $null }, $null)) {
            $fake = New-FakeMuxAgentProviders
            $fake.OpenResults[900] = $opened
            "$($null -eq $opened) => $(Get-T12Outcome $fake) $([IO.Directory]::Exists($fake.StateRoot))" | Should BeExactly "$($null -eq $opened) => 1 endpoint False"
        }
    }

    It 'A1 each caller refusal has its fixed message' {
        $cases = @(
            @('no endpoint', { $fake.Env.Remove('RMUX'); $fake.Env.Remove('TMUX') }, '1 endpoint not inside an rmux pane'),
            @('bad pane', { $fake.Env['TMUX_PANE'] = 'x' }, '1 endpoint not inside an rmux pane'),
            @('dead', { Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -ExitCode 1 }, '1 endpoint rmux endpoint did not answer'),
            @('stale', { Add-FakeRmuxRule -Fake $fake -Match '^-N display-message ' -StdOut "901|100|`$3|@1|`n" }, '1 endpoint rmux server changed'),
            @('server start', { $fake.OpenResults[900] = $null }, '1 endpoint rmux server start is unreadable'),
            @('outside', { ($fake.Rows | Where-Object Pid -eq 100).Start = 5000 }, '1 caller caller is not below its rmux pane'),
            @('non-shell', { ($fake.Rows | Where-Object Pid -eq 170).Path = 'C:\Program Files\nodejs\node.exe' }, '2 caller caller runs under a non-shell process'),
            @('agent start', { $fake.OpenResults[150] = $null }, '1 caller agent start is unreadable'))
        foreach ($case in $cases) {
            $fake = New-FakeMuxAgentProviders -Caller claude
            $null = & $case[1]
            "$($case[0]) => $(Get-T12Refusal $fake)" | Should BeExactly "$($case[0]) => $($case[2])"
        }
    }

    It 'A1 process table reads one CIM row per process' {
        $table = @(Get-MuxAgentProcessTable)
        $rows = @($table | Where-Object Pid -eq $PID)
        $rows.Count | Should Be 1
        $process = Get-Process -Id $PID
        $cim = Get-CimInstance Win32_Process -Filter "ProcessId=$PID"
        $row = $rows[0]
        "$($row.ParentPid -eq $process.Parent.Id) $($row.Path -eq $process.Path) $([Math]::Abs($row.Start - $process.StartTime.ToFileTimeUtc()) -lt 10) $($row.CommandLine -ceq $cim.CommandLine)" | Should BeExactly 'True True True True'
        "$(@($row.PSObject.Properties.Name) -join ',') $($row.Pid.GetType().Name) $($row.ParentPid.GetType().Name) $($row.Start.GetType().Name)" | Should BeExactly 'Pid,ParentPid,Start,Path,CommandLine Int32 Int32 Int64'
        $table.Count | Should BeGreaterThan 10
    }

    It 'A1 process table asks CIM for five properties and keeps the FILETIME start' {
        # It-local stand-ins, not Mock: here a Pester 3.4.0 Mock outlives its It and its Context.
        $t12Log = [Collections.Generic.List[string]]::new()
        function Get-CimInstance {
            param($ClassName, [string[]]$Property)
            $t12Log.Add("$ClassName [$($Property -join ',')] $($args.Count)")
            [pscustomobject]@{ ProcessId = [uint32]42; ParentProcessId = [uint32]7; CreationDate = [DateTime]::new(2026, 10, 29, 11, 0, 0, 123, [DateTimeKind]::Unspecified); ExecutablePath = 'C:\Apps\a.exe'; CommandLine = 'a --flag' }
        }
        $start = [DateTime]::new(2026, 10, 29, 11, 0, 0, 123, [DateTimeKind]::Unspecified).ToFileTimeUtc()
        (@(Get-MuxAgentProcessTable | ForEach-Object { "$($_.Pid)|$($_.ParentPid)|$($_.Start)|$($_.Path)|$($_.CommandLine)" }) -join "`n") | Should BeExactly "42|7|$start|C:\Apps\a.exe|a --flag"
        ($t12Log -join "`n") | Should BeExactly 'Win32_Process [ProcessId,ParentProcessId,CreationDate,ExecutablePath,CommandLine] 0'
    }

}

Describe 'MuxAgents T1.3 registry' -Tag 'windows' {
    AfterAll { $script:MuxAgentVerbs.Remove('t13-state') }

    function Get-T13Refusal {
        # 'code check message' for a typed refusal from Command, or 'ran' when it returns; any other error is rethrown.
        param([scriptblock]$Command)
        try { $null = & $Command; 'ran' }
        catch {
            if (-not $_.Exception.Data.Contains('MuxAgentCode')) { throw }
            "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)"
        }
    }

    function Get-T13LockName {
        # The expected mutex name, hashed here rather than through the library's helper.
        param([string]$Key)
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $hex = -join @($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Key)) | ForEach-Object { $_.ToString('x2') }) }
        finally { $sha.Dispose() }
        'Local\MuxAgents-' + $hex.Substring(0, 32)
    }

    function Test-T13LockFree {
        # True when another thread takes the named mutex at once, 'abandoned' when it was abandoned; never waits.
        param([string]$Name)
        $job = Start-ThreadJob -ArgumentList $Name -ScriptBlock {
            param($MutexName)
            $mutex = [Threading.Mutex]::new($false, $MutexName)
            try {
                $held = try { $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { 'abandoned' }
                if ($held) { $mutex.ReleaseMutex() }
                $held
            }
            finally { $mutex.Dispose() }
        }
        try { Receive-Job $job -Wait -ErrorAction Stop } finally { Remove-Job $job -Force }
    }

    function New-T13Agent {
        # An agent folder under State with spec.json and, when Committed, commit.json.
        param([string]$State, [string]$Id, [switch]$Committed)
        $folder = [IO.Directory]::CreateDirectory((Join-Path (Join-Path $State 'agents') $Id)).FullName
        Write-MuxAgentJson -Path (Join-Path $folder 'spec.json') -Value ([ordered]@{ id = $Id })
        if ($Committed) { Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ id = $Id }) }
    }

    It 'A2 state folder is private and per server' {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 private')
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        try { $sid = $identity.User.Value } finally { $identity.Dispose() }
        $allowed = @($sid, 'S-1-5-18', 'S-1-5-32-544') | Sort-Object | ForEach-Object { "$_ Allow FullControl ContainerInherit, ObjectInherit False" }
        foreach ($key in @('0123456789abcdef', 'fedcba9876543210')) {
            $state = Get-MuxAgentStateDirectory -P $fake -Caller ([pscustomobject]@{ ServerKey = $key })
            $state | Should BeExactly (Join-Path $fake.StateRoot $key)
            foreach ($folder in @($state, (Join-Path $state 'agents'), (Join-Path $state 'mail'))) {
                $acl = Get-Acl -LiteralPath $folder
                $rules = $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]) | Sort-Object { $_.IdentityReference.Value } | ForEach-Object { "$($_.IdentityReference.Value) $($_.AccessControlType) $($_.FileSystemRights) $($_.InheritanceFlags) $($_.IsInherited)" }
                (@("$folder $($acl.AreAccessRulesProtected) $($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value)") + @($rules)) -join "`n" | Should BeExactly ((@("$folder True $sid") + @($allowed)) -join "`n")
            }
            Get-MuxAgentStateDirectory -P $fake -Caller ([pscustomobject]@{ ServerKey = $key }) | Should BeExactly $state
        }
        (@(Get-ChildItem -LiteralPath $fake.StateRoot -Name) -join ',') | Should BeExactly '0123456789abcdef,fedcba9876543210'
    }

    It 'A2 atomic JSON round trip' {
        $folder = Join-Path $TestDrive 't13 round trip'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'record.json'
        $value = [ordered]@{ id = '0123456789ab'; name = 'claude-1 ' + [char]0x00E9; argv = @('-a', 'b c'); ok = $true; none = $null; deep = [ordered]@{ one = [ordered]@{ two = [ordered]@{ three = 3 } } } }
        Write-MuxAgentJson -Path $path -Value ([ordered]@{ stale = $true })
        Write-MuxAgentJson -Path $path -Value $value
        $expected = ConvertTo-Json -InputObject $value -Depth 100 -Compress
        ConvertTo-Json -InputObject (Read-MuxAgentJson -Path $path) -Depth 100 -Compress | Should BeExactly $expected
        [BitConverter]::ToString([IO.File]::ReadAllBytes($path)) | Should BeExactly ([BitConverter]::ToString([Text.UTF8Encoding]::new($false).GetBytes($expected)))
        (@(Get-ChildItem -LiteralPath $folder -Force -Name) -join ',') | Should BeExactly 'record.json'
        $null -eq (Read-MuxAgentJson -Path (Join-Path $folder 'absent.json')) | Should Be $true
    }

    It 'A2 live agents are tagged and committed' {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 live')
        $caller = [pscustomobject]@{ Session = '$3'; ServerKey = '0123456789abcdef' }
        $state = Get-MuxAgentStateDirectory -P $fake -Caller $caller
        New-T13Agent $state '0123456789ab' -Committed
        New-T13Agent $state 'abcdef012345'
        # A folder named commit.json does not commit its agent.
        New-T13Agent $state 'fedcba987654'
        $null = [IO.Directory]::CreateDirectory([IO.Path]::Combine($state, 'agents', 'fedcba987654', 'commit.json'))
        $rows = @('%2|@1|300|0123456789ab|claude-1', '%3|@1|310|abcdef012345|claude-2', '%4|@2|320||', '%5|@2|330|0123456789AB|claude-3', '%6|@2|340|fedcba987654|claude-4')
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut (($rows -join "`r`n") + "`r`n")
        $panes = @(Get-MuxAgentPanes -P $fake -Caller $caller)
        (@($panes | ForEach-Object { "$($_.Pane) $($_.Window) $($_.Pid) $($_.Id) $($_.Name)" }) -join ',') | Should BeExactly '%2 @1 300 0123456789ab claude-1'
        (@($panes[0].PSObject.Properties.Name) -join ',') | Should BeExactly 'Pane,Window,Pid,Id,Name'
        $fake.Calls.Count | Should Be 1
        "$($fake.Calls[0].Count) $($fake.Calls[0] -join ' ')" | Should BeExactly '7 -N list-panes -s -t $3 -F #{pane_id}|#{window_id}|#{pane_pid}|#{@agent-id}|#{@agent-name}'
    }

    It 'A2 name pattern reserved taken default' {
        $cases = [ordered]@{ Bad = 'agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter'; parent = 'agent name is reserved'; root = 'agent name is reserved'; all = 'agent name is reserved'; 'claude-1' = 'agent name is taken' }
        foreach ($requested in $cases.Keys) {
            "$requested => $(Get-T13Refusal { Resolve-MuxAgentName -Requested $requested -Client 'claude' -Taken @('claude-1') })" | Should BeExactly "$requested => 2 name $($cases[$requested])"
        }
        Resolve-MuxAgentName -Requested $null -Client 'claude' -Taken @() | Should BeExactly 'claude-1'
        Resolve-MuxAgentName -Requested $null -Client 'claude' -Taken @('claude-1') | Should BeExactly 'claude-2'
        Resolve-MuxAgentName -Requested 'alpha' -Client 'claude' -Taken @('claude-1') | Should BeExactly 'alpha'
    }

    It 'A2 lock returns the action result and releases' {
        $key = 'session|' + [guid]::NewGuid().ToString('N')
        $name = Get-T13LockName $key
        # The action opens the probe after the call created the mutex; it keeps that mutex alive for the checks.
        $probes = [Collections.Generic.List[object]]::new()
        try {
            (Use-MuxAgentLock -LockKey $key -LockAction { $probes.Add([Threading.Mutex]::new($false, $name)); [ordered]@{ answer = 42 } }).answer | Should Be 42
            Test-T13LockFree $name | Should Be $true
            (@(Use-MuxAgentLock -LockKey $key -LockAction { 'a'; 'b' }) -join ',') | Should BeExactly 'a,b'
            Test-T13LockFree $name | Should Be $true
        }
        finally { foreach ($probe in $probes) { $probe.Dispose() } }
    }

    It 'A2 state folder refuses an existing folder with a wider ACL' {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 wider')
        $path = Join-Path $fake.StateRoot '0123456789abcdef'
        $folder = [IO.Directory]::CreateDirectory($path)
        $acl = [IO.FileSystemAclExtensions]::GetAccessControl($folder)
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-1-0'), 'ReadAndExecute', 'Allow'))
        [IO.FileSystemAclExtensions]::SetAccessControl($folder, $acl)
        $failure = $null
        try { $null = Get-MuxAgentStateDirectory -P $fake -Caller ([pscustomobject]@{ ServerKey = '0123456789abcdef' }) } catch { $failure = $_.Exception.Message }
        $failure | Should Match 'shared with other users'
        [IO.Directory]::Exists((Join-Path $path 'agents')) | Should Be $false
        # Through the dispatcher a wider ACL is a typed refusal, 1 state.
        Register-MuxAgentVerb -Verb 't13-state' -Handler { param($P, $Request) $null = Get-MuxAgentStateDirectory -P $P -Caller ([pscustomobject]@{ ServerKey = '0123456789abcdef' }) } -Usage 't13-state'
        $result = Invoke-MuxAgentCommand -P $fake -Arguments @('t13-state')
        $reply = $result.Text | ConvertFrom-Json
        "$($result.Code) $($reply.code) $($reply.check)" | Should BeExactly '1 1 state'
        $result.Text | Should Not Match 'unsafe'
    }

    It 'A2 the user SID lookup disposes its token' {
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        try { $sid = $identity.User.Value } finally { $identity.Dispose() }
        Get-MuxAgentUserSid | Should BeExactly $sid
        # 100 undisposed tokens keep 100 handles open until a collection finalizes them; a collection during the
        # loop could hide them, so such an attempt is repeated.
        $process = [Diagnostics.Process]::GetCurrentProcess()
        $delta = $null
        for ($attempt = 0; $attempt -lt 3 -and $null -eq $delta; $attempt++) {
            [GC]::Collect(); [GC]::WaitForPendingFinalizers()
            $process.Refresh(); $before = $process.HandleCount; $collections = [GC]::CollectionCount(0)
            for ($index = 0; $index -lt 100; $index++) { $null = Get-MuxAgentUserSid }
            $process.Refresh()
            if ([GC]::CollectionCount(0) -eq $collections) { $delta = $process.HandleCount - $before }
        }
        "measured $($null -ne $delta), under 50 $($delta -lt 50)" | Should BeExactly 'measured True, under 50 True'
    }

    It 'A2 the state folder takes its SID from the disposing lookup' {
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        try { $sid = $identity.User.Value } finally { $identity.Dispose() }
        # An It-local stand-in, not Mock: it records the call and returns the real SID.
        $t13Calls = [Collections.Generic.List[string]]::new()
        function Get-MuxAgentUserSid { $t13Calls.Add('sid'); $sid }
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 sid')
        $null = Get-MuxAgentStateDirectory -P $fake -Caller ([pscustomobject]@{ ServerKey = '0123456789abcdef' })
        ($t13Calls -join ',') | Should BeExactly 'sid'
    }

    It 'A2 JSON writes rename a same-folder tmp file into place' {
        $folder = Join-Path $TestDrive 't13 watched'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'record.json'
        $watcher = [IO.FileSystemWatcher]::new($folder)
        $watcher.NotifyFilter = [IO.NotifyFilters]::FileName
        $source = 't13-renamed-' + [guid]::NewGuid().ToString('N')
        $null = Register-ObjectEvent -InputObject $watcher -EventName Renamed -SourceIdentifier $source
        try {
            $watcher.EnableRaisingEvents = $true
            Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 1 })
            Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 2 })
            $into = @()
            $deadline = [DateTime]::UtcNow.AddSeconds(10)
            while ($into.Count -lt 2 -and [DateTime]::UtcNow -lt $deadline) {
                Start-Sleep -Milliseconds 50
                $into = @(Get-Event -SourceIdentifier $source -ErrorAction SilentlyContinue | ForEach-Object SourceEventArgs | Where-Object Name -eq 'record.json')
            }
            $old = @($into | ForEach-Object OldName)
            (@($old | ForEach-Object { $_ -cmatch '^record\.json\.[0-9a-f]{32}\.tmp$' }) -join ',') | Should BeExactly 'True,True'
            $old[0] | Should Not Be $old[1]
            (Read-MuxAgentJson -Path $path).n | Should Be 2
        }
        finally {
            $watcher.EnableRaisingEvents = $false
            Unregister-Event -SourceIdentifier $source
            Get-Event -SourceIdentifier $source -ErrorAction SilentlyContinue | Remove-Event
            $watcher.Dispose()
        }
    }

    It 'A2 JSON reads and overwrites share the file with open readers and a deleter' {
        $folder = Join-Path $TestDrive 't13 shared'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'record.json'
        Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 1 })
        $reader = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::Read -bor [IO.FileShare]::Delete))
        try {
            Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 2 })
            (Read-MuxAgentJson -Path $path).n | Should Be 2
        }
        finally { $reader.Dispose() }
        # A handle with delete access, as another writer holds while it replaces the file, must not make a read fail.
        $deleter = [IO.FileStream]::new($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete), 4096, [IO.FileOptions]::DeleteOnClose)
        try { (Read-MuxAgentJson -Path $path).n | Should Be 2 }
        finally { $deleter.Dispose() }
        $null -eq (Read-MuxAgentJson -Path $path) | Should Be $true
        @(Get-ChildItem -LiteralPath $folder -Force).Count | Should Be 0
    }

    It 'A2 JSON reads refuse more than 1 MB and treat only a missing file or folder as absent' {
        $folder = Join-Path $TestDrive 't13 sizes'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'big.json'
        [IO.File]::WriteAllText($path, '"' + ('a' * 1048574) + '"')
        (Read-MuxAgentJson -Path $path).Length | Should Be 1048574
        [IO.File]::WriteAllText($path, '"' + ('a' * 1048575) + '"')
        $failure = $null
        try { $null = Read-MuxAgentJson -Path $path } catch { $failure = $_.Exception }
        # Oversize is an unexpected error (1 internal through the dispatcher), not a typed refusal.
        "$($failure.GetType().Name) $($failure.Data.Contains('MuxAgentCode')) $($failure.Message)" | Should BeExactly 'InvalidOperationException False mux-agent: state file is larger than 1 MB'
        $null -eq (Read-MuxAgentJson -Path (Join-Path (Join-Path $folder 'gone') 'record.json')) | Should Be $true
        $failure = $null
        try { $null = Read-MuxAgentJson -Path $folder } catch { $failure = $_.Exception.GetBaseException() }
        $failure.GetType().Name | Should Be 'UnauthorizedAccessException'
    }

    It 'A2 a failed JSON write throws and leaves no tmp file' {
        $folder = Join-Path $TestDrive 't13 failed write'
        # A folder at the target path makes the move into place fail.
        $path = [IO.Directory]::CreateDirectory((Join-Path $folder 'record.json')).FullName
        $failure = $null
        try { Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 1 }) } catch { $failure = $_.Exception }
        $null -eq $failure | Should Be $false
        (@(Get-ChildItem -LiteralPath $folder -Force -Name) -join ',') | Should BeExactly 'record.json'
    }

    It 'A2 a failed overwrite keeps the old target' {
        $folder = Join-Path $TestDrive 't13 failed overwrite'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'record.json'
        Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 1 })
        # Before the new tmp file moves into place, another handle opens it without delete sharing.
        $library = Join-Path $repoRoot 'scripts\MuxAgents.ps1'
        $lines = @(Select-String -LiteralPath $library -SimpleMatch 'if ([IO.File]::Exists($Path))')
        $lines.Count | Should Be 1
        $holds = [Collections.Generic.List[object]]::new()
        $breakpoint = Set-PSBreakpoint -Script $library -Line $lines[0].LineNumber -Action { $holds.Add([IO.File]::Open($temporary, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)) }
        $failure = $null
        try { Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 2 }) } catch { $failure = $_.Exception }
        finally { Remove-PSBreakpoint -Breakpoint $breakpoint; foreach ($hold in $holds) { $hold.Dispose() } }
        "$($holds.Count) $($null -ne $failure) $([IO.File]::Exists($path))" | Should BeExactly '1 True True'
        [IO.File]::ReadAllText($path) | Should BeExactly '{"n":1}'
        # The writer cannot delete a tmp file another handle holds, so that one tmp file stays.
        (@(Get-ChildItem -LiteralPath $folder -Force -Name | Sort-Object) -join ',') | Should Match '^record\.json,record\.json\.[0-9a-f]{32}\.tmp$'
    }

    It 'A2 list with -All keeps tagged uncommitted panes, drops malformed ids and touches no state' {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 all')
        $caller = [pscustomobject]@{ Session = '$4'; ServerKey = '0123456789abcdef' }
        $rows = @('%2|@1|300|0123456789ab|claude-1', '%3|@1|310|abcdef012345|copilot-1', '%4|@2|320||', '%5|@2|330|0123456789AB|x', '%6|@2|340|0123456789a|x', '%7|@2|350|0123456789abc|x', '%8|@2|360|0123456789ag|x', '%9|@2|370|x0123456789ab|x', '%10|@2|380|fedcba987654|a|b')
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut (($rows -join "`n") + "`n")
        # Only the first four bars split a row, so a name keeps any bar in it.
        (@(Get-MuxAgentPanes -P $fake -Caller $caller -All | ForEach-Object { "$($_.Pane) $($_.Id) $($_.Name)" }) -join ',') | Should BeExactly '%2 0123456789ab claude-1,%3 abcdef012345 copilot-1,%10 fedcba987654 a|b'
        @(Get-MuxAgentPanes -P $fake -Caller $caller).Count | Should Be 0
        [IO.Directory]::Exists($fake.StateRoot) | Should Be $false
        "$($fake.Calls.Count) $($fake.Calls[1][4])" | Should BeExactly '2 $4'
    }

    It 'A2 a failed list-panes refuses rmux' {
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't13 failed')
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -ExitCode 1 -StdOut "%2|@1|300|0123456789ab|claude-1`n"
        Get-T13Refusal { Get-MuxAgentPanes -P $fake -Caller ([pscustomobject]@{ Session = '$3'; ServerKey = '0123456789abcdef' }) -All } | Should BeExactly '1 rmux rmux call failed'
    }

    It 'A2 names match the whole pattern' {
        foreach ($requested in @('a', 'a-', 'a1-b2', ('a' + ('b' * 23)))) {
            Resolve-MuxAgentName -Requested $requested -Client 'claude' -Taken @() | Should BeExactly $requested
        }
        foreach ($requested in @('', '1a', '-a', 'a_b', 'a b', 'aB', ('a' + ('b' * 24)), "a`n", ('a' + [char]0x00E9))) {
            "[$requested] => $(Get-T13Refusal { Resolve-MuxAgentName -Requested $requested -Client 'claude' -Taken @() })" | Should BeExactly "[$requested] => 2 name agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter"
        }
        # Only a string is a name: any array, even of one valid name, and any other type are refused.
        $refused = '2 name agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter'
        Get-T13Refusal { Resolve-MuxAgentName -Requested @('alpha', 'beta') -Client 'claude' -Taken @() } | Should BeExactly $refused
        Get-T13Refusal { Resolve-MuxAgentName -Requested @('alpha') -Client 'claude' -Taken @() } | Should BeExactly $refused
        Get-T13Refusal { Resolve-MuxAgentName -Requested ([string[]]@('alpha')) -Client 'claude' -Taken @() } | Should BeExactly $refused
        Get-T13Refusal { Resolve-MuxAgentName -Requested ([object[]]@()) -Client 'claude' -Taken @() } | Should BeExactly $refused
        Get-T13Refusal { Resolve-MuxAgentName -Requested 42 -Client 'claude' -Taken @() } | Should BeExactly $refused
    }

    It 'A2 default names take the smallest free number per client' {
        $cases = [ordered]@{ 'claude|' = 'claude-1'; 'claude|claude-2' = 'claude-1'; 'claude|claude-1,claude-3' = 'claude-2'; 'claude|claude-1,claude-2' = 'claude-3'; 'copilot|claude-1,copilot-1' = 'copilot-2'; 'copilot|claude-1' = 'copilot-1'; 'claude|CLAUDE-1' = 'claude-2' }
        foreach ($case in $cases.Keys) {
            $client, $taken = $case -split '\|'
            "$case => $(Resolve-MuxAgentName -Requested $null -Client $client -Taken @($taken -split ',' | Where-Object { $_ }))" | Should BeExactly "$case => $($cases[$case])"
        }
        Resolve-MuxAgentName -Requested $null -Client 'claude' -Taken $null | Should BeExactly 'claude-1'
        Get-T13Refusal { Resolve-MuxAgentName -Requested 'alpha' -Client 'claude' -Taken @('ALPHA') } | Should BeExactly '2 name agent name is taken'
    }

    It 'A2 name refusals check the pattern, then reserved, then taken' {
        Get-T13Refusal { Resolve-MuxAgentName -Requested 'CLAUDE-1' -Client 'claude' -Taken @('claude-1') } | Should BeExactly '2 name agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter'
        Get-T13Refusal { Resolve-MuxAgentName -Requested 'parent' -Client 'claude' -Taken @('parent') } | Should BeExactly '2 name agent name is reserved'
    }

    It 'A2 lock holds the mutex named by the first 32 hex of the key hash and keeps no handle' {
        # The key is hashed as given, case included.
        $key = 'Mail|' + [guid]::NewGuid().ToString('N').ToUpperInvariant()
        $name = Get-T13LockName $key
        Use-MuxAgentLock -LockKey $key -LockAction { "$(Test-T13LockFree $name)" } | Should BeExactly 'False'
        $existing = $null
        [Threading.Mutex]::TryOpenExisting($name, [ref]$existing) | Should Be $false
        if ($existing) { $existing.Dispose() }
    }

    It 'A2 lock timeout refuses lock without running the action' {
        $key = 'session|' + [guid]::NewGuid().ToString('N')
        $ready = [Threading.ManualResetEventSlim]::new($false)
        $release = [Threading.ManualResetEventSlim]::new($false)
        $holder = Start-ThreadJob -ArgumentList (Get-T13LockName $key), $ready, $release -ScriptBlock {
            param($MutexName, $Ready, $Release)
            $mutex = [Threading.Mutex]::new($false, $MutexName)
            try { $null = $mutex.WaitOne(); $Ready.Set(); $null = $Release.Wait(30000); $mutex.ReleaseMutex() }
            finally { $mutex.Dispose() }
        }
        try {
            $ready.Wait(15000) | Should Be $true
            $ran = [Collections.Generic.List[string]]::new()
            $watch = [Diagnostics.Stopwatch]::StartNew()
            Get-T13Refusal { Use-MuxAgentLock -LockKey $key -LockAction { $ran.Add('ran') } -LockTimeoutMilliseconds 300 } | Should BeExactly '2 lock timed out waiting for the agent lock'
            $watch.Stop()
            $ran.Count | Should Be 0
            "$($watch.ElapsedMilliseconds -ge 250) $($watch.ElapsedMilliseconds -lt 5000)" | Should BeExactly 'True True'
        }
        finally {
            $release.Set()
            $null = Wait-Job $holder -Timeout 15
            Remove-Job $holder -Force
            $ready.Dispose()
            $release.Dispose()
        }
    }

    It 'A2 lock waits 15 s by default and its parameters cannot shadow caller variables' {
        $parameters = (Get-Command Use-MuxAgentLock -CommandType Function).ScriptBlock.Ast.Body.ParamBlock.Parameters
        (@($parameters | ForEach-Object { "$($_.Name.VariablePath.UserPath)=$($_.DefaultValue.Extent.Text)" }) -join ' ') | Should BeExactly 'LockKey= LockAction= LockTimeoutMilliseconds=15000'
        $t13Key = 'session|' + [guid]::NewGuid().ToString('N')
        $Key = 'caller key'; $Action = 'caller action'; $Name = 'caller name'; $Mutex = 'caller mutex'; $Held = 'caller held'; $Timeout = 'caller timeout'; $Milliseconds = 'caller ms'; $Result = 'caller result'; $Value = 'caller value'
        Use-MuxAgentLock -LockKey $t13Key -LockAction { "$Key|$Action|$Name|$Mutex|$Held|$Timeout|$Milliseconds|$Result|$Value" } | Should BeExactly 'caller key|caller action|caller name|caller mutex|caller held|caller timeout|caller ms|caller result|caller value'
    }

    It 'A2 lock counts an abandoned mutex as held' {
        $key = 'session|' + [guid]::NewGuid().ToString('N')
        $name = Get-T13LockName $key
        $probe = [Threading.Mutex]::new($false, $name)
        try {
            # The job takes the mutex and its thread ends without releasing it.
            $job = Start-ThreadJob -ArgumentList $name -ScriptBlock { param($MutexName) $mutex = [Threading.Mutex]::new($false, $MutexName); $mutex.WaitOne(0); $mutex.Dispose() }
            try { Receive-Job $job -Wait -ErrorAction Stop | Should Be $true } finally { Remove-Job $job -Force }
            Use-MuxAgentLock -LockKey $key -LockAction { 'ran' } -LockTimeoutMilliseconds 5000 | Should BeExactly 'ran'
            Test-T13LockFree $name | Should Be $true
        }
        finally { $probe.Dispose() }
    }

    It 'A2 lock releases after the action throws or refuses' {
        $key = 'session|' + [guid]::NewGuid().ToString('N')
        $name = Get-T13LockName $key
        $probe = [Threading.Mutex]::new($false, $name)
        try {
            Get-T13Refusal { Use-MuxAgentLock -LockKey $key -LockAction { Exit-MuxAgentCommand 2 'name' 'agent name is taken' } } | Should BeExactly '2 name agent name is taken'
            Test-T13LockFree $name | Should Be $true
            $failure = $null
            try { $null = Use-MuxAgentLock -LockKey $key -LockAction { throw 'action failed' } } catch { $failure = $_.Exception.Message }
            $failure | Should BeExactly 'action failed'
            Test-T13LockFree $name | Should Be $true
        }
        finally { $probe.Dispose() }
    }

    It 'A2 a reader under the session lock never sees a write in progress' {
        $folder = Join-Path $TestDrive 't13 overlap'
        $null = [IO.Directory]::CreateDirectory($folder)
        $path = Join-Path $folder 'record.json'
        Write-MuxAgentJson -Path $path -Value ([ordered]@{ n = 0 })
        $key = 'session|' + [guid]::NewGuid().ToString('N')
        $library = Join-Path $repoRoot 'scripts\MuxAgents.ps1'
        # Without the lock, a read during Replace finds no file; in a probe 233 of 300 unlocked reads failed.
        $body = {
            param($Library, $Path, $Key, $Role)
            . $Library
            $bad = 0
            for ($n = 1; $n -le 300; $n++) {
                $step = if ($Role -eq 'writer') { { Write-MuxAgentJson -Path $Path -Value ([ordered]@{ n = $n }); $true } } else { { $null -ne (Read-MuxAgentJson -Path $Path) } }
                $ok = try { Use-MuxAgentLock -LockKey $Key -LockAction $step } catch { $false }
                if (-not $ok) { $bad++ }
            }
            "$Role bad $bad"
        }
        $jobs = @(foreach ($role in @('writer', 'reader')) { Start-ThreadJob -ArgumentList $library, $path, $key, $role -ScriptBlock $body })
        try { $results = @(Receive-Job $jobs -Wait -ErrorAction Stop) } finally { Remove-Job $jobs -Force }
        (@($results | Sort-Object) -join ', ') | Should BeExactly 'reader bad 0, writer bad 0'
        (Read-MuxAgentJson -Path $path).n | Should Be 300
    }
}

Describe 'MuxAgents T1.4 context' -Tag 'windows' {
    $T14New = '12345678-9abc-4def-8123-456789abcdef'
    $T14Parent = '11111111-2222-4333-8444-555555555555'

    function Get-T14Plan {
        # 'Context|Reason|ParentSession' for a stand-in caller and an environment built from Values.
        param([string]$Kind = 'agent', [string]$From = 'claude', [string]$To = 'claude', [hashtable]$Values = @{}, [switch]$NoFork)
        $environment = [hashtable]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($key in $Values.Keys) { $environment[$key] = $Values[$key] }
        $caller = [pscustomobject]@{ Kind = $Kind; Client = if ($Kind -eq 'agent') { $From } }
        $plan = Get-MuxAgentContextPlan -Caller $caller -Client $To -Env $environment -NoFork:$NoFork
        "$($plan.Context)|$($plan.Reason)|$($plan.ParentSession)"
    }

    function ConvertTo-T14Json {
        param($Value)
        ConvertTo-Json -Compress -InputObject @($Value)
    }

    It 'A3 cli with id gives fork argv' {
        $fake = New-FakeMuxAgentProviders -Caller claude
        $caller = Resolve-MuxAgentCaller -P $fake
        $plan = Get-MuxAgentContextPlan -Caller $caller -Client 'claude' -Env $fake.Env
        "$($plan.Context)|$($plan.Reason)|$($plan.ParentSession)" | Should BeExactly "fork|same-client|$T14Parent"
        $argv = Get-MuxAgentClientArgs -Client 'claude' -Plan $plan -SessionId $T14New -Prompt 'read the brief'
        ConvertTo-T14Json $argv | Should BeExactly (ConvertTo-T14Json @('--resume', $T14Parent, '--fork-session', '--session-id', $T14New, 'read the brief'))
        # The client name matches without case, as its ValidateSet does.
        $plan = Get-MuxAgentContextPlan -Caller $caller -Client 'Claude' -Env $fake.Env
        "$($plan.Context)|$($plan.Reason)|$($plan.ParentSession)" | Should BeExactly "fork|same-client|$T14Parent"
        ConvertTo-T14Json (Get-MuxAgentClientArgs -Client 'Claude' -Plan $plan -SessionId $T14New -Prompt 'p') | Should BeExactly (ConvertTo-T14Json @('--resume', $T14Parent, '--fork-session', '--session-id', $T14New, 'p'))
    }

    It 'A3 sdk-cli gives brief without --resume' {
        $values = @{ CLAUDE_CODE_ENTRYPOINT = 'sdk-cli'; CLAUDE_CODE_SESSION_ID = $T14Parent }
        Get-T14Plan -Values $values | Should BeExactly 'brief|not-interactive|'
        # No entrypoint at all is not interactive either, and the match is exact.
        Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'CLI'; CLAUDE_CODE_SESSION_ID = $T14Parent } | Should BeExactly 'brief|not-interactive|'
        Get-T14Plan -To 'Claude' -Values $values | Should BeExactly 'brief|not-interactive|'
        Get-T14Plan -Values @{ CLAUDE_CODE_SESSION_ID = $T14Parent } | Should BeExactly 'brief|not-interactive|'
        $plan = [pscustomobject]@{ Context = 'brief'; Reason = 'not-interactive'; ParentSession = $null }
        $argv = Get-MuxAgentClientArgs -Client 'claude' -Plan $plan -SessionId $T14New -Prompt 'read the brief'
        ConvertTo-T14Json $argv | Should BeExactly (ConvertTo-T14Json @('--session-id', $T14New, 'read the brief'))
        $argv -contains '--resume' | Should Be $false
    }

    It 'A3 missing id gives brief' {
        Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'cli' } | Should BeExactly 'brief|no-session-id|'
        # no-session-id comes before not-interactive.
        Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'sdk-cli' } | Should BeExactly 'brief|no-session-id|'
        # Each client reads only its own variable.
        Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'cli'; COPILOT_AGENT_SESSION_ID = $T14Parent } | Should BeExactly 'brief|no-session-id|'
        Get-T14Plan -From 'copilot' -To 'copilot' -Values @{ CLAUDE_CODE_SESSION_ID = $T14Parent } | Should BeExactly 'brief|no-session-id|'
    }

    It 'A3 human NoFork and cross-client give brief' {
        $full = @{ CLAUDE_CODE_ENTRYPOINT = 'cli'; CLAUDE_CODE_SESSION_ID = $T14Parent; COPILOT_AGENT_SESSION_ID = $T14Parent }
        Get-T14Plan -Kind 'human' -Values $full | Should BeExactly 'brief|human|'
        Get-T14Plan -Kind 'human' -Values $full -NoFork | Should BeExactly 'brief|no-fork|'
        Get-T14Plan -Values $full -NoFork | Should BeExactly 'brief|no-fork|'
        Get-T14Plan -To 'copilot' -Values $full | Should BeExactly 'brief|cross-client|'
        Get-T14Plan -From 'copilot' -To 'claude' -Values $full | Should BeExactly 'brief|cross-client|'
        # cross-client comes before no-session-id.
        Get-T14Plan -To 'copilot' -Values @{ CLAUDE_CODE_ENTRYPOINT = 'cli' } | Should BeExactly 'brief|cross-client|'
        # An unknown client is an error, not a brief.
        $outcome = try { $null = Get-T14Plan -To 'other' -Values $full; 'ran' } catch { 'threw' }
        $outcome | Should BeExactly 'threw'
    }

    It 'A3 Copilot caller forks without a Claude entrypoint' {
        Get-T14Plan -From 'copilot' -To 'copilot' -Values @{ COPILOT_AGENT_SESSION_ID = $T14Parent } | Should BeExactly "fork|same-client|$T14Parent"
        Get-T14Plan -From 'copilot' -To 'copilot' -Values @{ COPILOT_AGENT_SESSION_ID = $T14Parent; CLAUDE_CODE_ENTRYPOINT = 'sdk-cli' } | Should BeExactly "fork|same-client|$T14Parent"
    }

    It 'A3 malformed session id gives brief' {
        foreach ($id in @('not-a-guid', "..\$T14Parent", "$T14Parent`n", "${T14Parent}0", (' ' + $T14Parent), 'gggggggg-gggg-gggg-gggg-gggggggggggg')) {
            Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'cli'; CLAUDE_CODE_SESSION_ID = $id } | Should BeExactly 'brief|no-session-id|'
        }
        # Letter case does not matter.
        $upper = $T14New.ToUpperInvariant()
        Get-T14Plan -Values @{ CLAUDE_CODE_ENTRYPOINT = 'cli'; CLAUDE_CODE_SESSION_ID = $upper } | Should BeExactly "fork|same-client|$upper"
    }

    function Get-T14Hashes {
        # 'relative=hash' for every file below Folder, sorted, so a changed, added or removed file shows.
        param([string]$Folder)
        @(Get-ChildItem -LiteralPath $Folder -Recurse -File | ForEach-Object { $_.FullName.Substring($Folder.Length) + '=' + (Get-FileHash -LiteralPath $_.FullName).Hash } | Sort-Object) -join ';'
    }

    function New-T14Copilot {
        # A Copilot fake under its own root, since TestDrive is shared by every It in this Describe.
        param([string]$Name)
        $fake = New-FakeMuxAgentProviders -Caller copilot -Root (Join-Path $TestDrive $Name)
        $old = $fake.Env['COPILOT_AGENT_SESSION_ID']
        [pscustomobject]@{
            Fake = $fake; Old = $old; Home = $fake.Home; Events = $fake.Fixtures.CopilotEvents; Workspace = $fake.Fixtures.CopilotWorkspace
            Source = Join-Path $fake.Home ".copilot\session-state\$old"; Target = Join-Path $fake.Home ".copilot\session-state\$T14New"
        }
    }

    It 'A3 Copilot copy drops partial line rewrites ids removes mc keys' {
        $copilot = New-T14Copilot 't14 partial'
        $complete = [IO.File]::ReadAllText($copilot.Events)
        [IO.File]::AppendAllText($copilot.Events, '{"type":"assistant.message","data":{"sessionId":"' + $copilot.Old + '","content":"half')
        $before = Get-T14Hashes $copilot.Source
        $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New
        ConvertTo-T14Json $result | Should BeExactly '[true]'
        $events = [IO.File]::ReadAllText((Join-Path $copilot.Target 'events.jsonl'))
        $events | Should BeExactly $complete.Replace($copilot.Old, $T14New)
        foreach ($line in $events.TrimEnd("`n").Split("`n")) { $null = ConvertFrom-Json -InputObject $line }
        $events.Contains('half') | Should Be $false
        $workspace = [IO.File]::ReadAllText((Join-Path $copilot.Target 'workspace.yaml'))
        $workspace | Should BeExactly ((@([IO.File]::ReadAllText($copilot.Workspace).Split("`n") | Where-Object { $_ -notmatch '^mc_' }) -join "`n").Replace($copilot.Old, $T14New))
        @($workspace.Split("`n")) -ccontains "id: $T14New" | Should Be $true
        $workspace -match '(?m)^mc_' | Should Be $false
        foreach ($file in Get-ChildItem -LiteralPath $copilot.Target -File) { [IO.File]::ReadAllText($file.FullName).Contains($copilot.Old) | Should Be $false }
        Get-T14Hashes $copilot.Source | Should BeExactly $before
        # Every written file is UTF-8 without a BOM.
        foreach ($file in Get-ChildItem -LiteralPath $copilot.Target -File) {
            $bytes = [IO.File]::ReadAllBytes($file.FullName)
            "$($file.Name) $($bytes.Count -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)" | Should BeExactly "$($file.Name) False"
        }
    }

    It 'A3 Copilot copy failure rolls back' {
        $copilot = New-T14Copilot 't14 invalid'
        [IO.File]::AppendAllText($copilot.Events, '{"type":broken}' + "`n")
        $before = Get-T14Hashes $copilot.Source
        $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New
        ConvertTo-T14Json $result | Should BeExactly '[false]'
        [IO.Directory]::Exists($copilot.Target) | Should Be $false
        Get-T14Hashes $copilot.Source | Should BeExactly $before
    }

    It 'A3 Copilot copy keeps complete unterminated records and YAML' {
        $copilot = New-T14Copilot 't14 unterminated'
        $complete = [IO.File]::ReadAllText($copilot.Events)
        [IO.File]::WriteAllText($copilot.Events, $complete.TrimEnd("`n"))
        $yaml = [IO.File]::ReadAllText($copilot.Workspace) + 'branch: main'
        [IO.File]::WriteAllText($copilot.Workspace, $yaml)
        $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New
        ConvertTo-T14Json $result | Should BeExactly '[true]'
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'events.jsonl')) | Should BeExactly $complete.Replace($copilot.Old, $T14New)
        # The YAML gets no LF added; only its ids and mc_ lines change.
        $workspace = [IO.File]::ReadAllText((Join-Path $copilot.Target 'workspace.yaml'))
        $workspace | Should BeExactly ((@($yaml.Split("`n") | Where-Object { $_ -notmatch '^mc_' }) -join "`n").Replace($copilot.Old, $T14New))
        $workspace.EndsWith("`nbranch: main") | Should Be $true
    }

    It 'A3 Copilot argv' {
        $fork = [pscustomobject]@{ Context = 'fork'; Reason = 'same-client'; ParentSession = $T14Parent }
        $brief = [pscustomobject]@{ Context = 'brief'; Reason = 'human'; ParentSession = $null }
        ConvertTo-T14Json (Get-MuxAgentClientArgs -Client 'copilot' -Plan $fork -SessionId $T14New -Prompt 'read the brief') | Should BeExactly (ConvertTo-T14Json @('--resume', $T14New, '-i', 'read the brief'))
        ConvertTo-T14Json (Get-MuxAgentClientArgs -Client 'copilot' -Plan $brief -SessionId $T14New -Prompt 'read the brief') | Should BeExactly (ConvertTo-T14Json @('--session-id', $T14New, '-i', 'read the brief'))
        # An unknown context or client is an error, not a brief.
        foreach ($case in @(@('claude', 'other'), @('copilot', 'other'), @('other', 'brief'))) {
            $plan = [pscustomobject]@{ Context = $case[1]; Reason = 'x'; ParentSession = $T14Parent }
            $outcome = try { $null = Get-MuxAgentClientArgs -Client $case[0] -Plan $plan -SessionId $T14New -Prompt 'p'; 'ran' } catch { 'threw' }
            "$($case -join ' ') $outcome" | Should BeExactly "$($case -join ' ') threw"
        }
    }

    It 'A3 Copilot copy refuses an existing target and leaves it alone' {
        $copilot = New-T14Copilot 't14 existing'
        Write-FakeMuxAgentFile (Join-Path $copilot.Target 'keep.txt') @('mine')
        $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New
        ConvertTo-T14Json $result | Should BeExactly '[false]'
        @(Get-ChildItem -LiteralPath $copilot.Target -Force).Name -join ',' | Should BeExactly 'keep.txt'
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'keep.txt')) | Should BeExactly "mine`n"
        # A file in the target's place is kept too.
        $copilot = New-T14Copilot 't14 existing file'
        [IO.File]::WriteAllText($copilot.Target, 'mine')
        ConvertTo-T14Json (Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New) | Should BeExactly '[false]'
        [IO.File]::ReadAllText($copilot.Target) | Should BeExactly 'mine'
        # No temp folder is left beside it.
        (@(Get-ChildItem -LiteralPath (Split-Path $copilot.Target) -Force -Name | Sort-Object) -join ',') | Should BeExactly ((@($copilot.Old, $T14New) | Sort-Object) -join ',')
    }

    It 'A3 Copilot copy matches file names without case and drops only leading mc_' {
        $copilot = New-T14Copilot 't14 case'
        $yaml = [IO.File]::ReadAllText($copilot.Workspace) + "summary: mc_task_id stays`n"
        [IO.File]::Delete($copilot.Workspace)
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'Workspace.YAML'), $yaml)
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'Upper.JSONL'), "{`"ref`":`"$($copilot.Old)`"}`n{`"half")
        ConvertTo-T14Json (Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New) | Should BeExactly '[true]'
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'Upper.JSONL')) | Should BeExactly "{`"ref`":`"$T14New`"}`n"
        $workspace = [IO.File]::ReadAllText((Join-Path $copilot.Target 'Workspace.YAML'))
        $workspace | Should BeExactly ((@($yaml.Split("`n") | Where-Object { $_ -notmatch '^mc_' }) -join "`n").Replace($copilot.Old, $T14New))
        $workspace.Contains("`nsummary: mc_task_id stays`n") | Should Be $true
    }

    It 'A3 Copilot copy reads files a writer holds open and copies subfolders unchanged' {
        $copilot = New-T14Copilot 't14 subfolders'
        # Subfolder files keep the old id, mc_ lines and partial lines.
        Write-FakeMuxAgentFile (Join-Path $copilot.Source 'checkpoints\index.jsonl') @("{`"id`":`"$($copilot.Old)`"}")
        [IO.File]::AppendAllText((Join-Path $copilot.Source 'checkpoints\index.jsonl'), '{"half')
        Write-FakeMuxAgentFile (Join-Path $copilot.Source 'files\workspace.yaml') @("id: $($copilot.Old)", 'mc_task_id: x')
        $null = [IO.Directory]::CreateDirectory((Join-Path $copilot.Source 'rewind\empty'))
        $writer = [IO.File]::Open($copilot.Events, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
        try { $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New }
        finally { $writer.Dispose() }
        ConvertTo-T14Json $result | Should BeExactly '[true]'
        foreach ($folder in @('checkpoints', 'files')) { Get-T14Hashes (Join-Path $copilot.Target $folder) | Should BeExactly (Get-T14Hashes (Join-Path $copilot.Source $folder)) }
        [IO.Directory]::Exists((Join-Path $copilot.Target 'rewind\empty')) | Should Be $true
        # No source handle stays open.
        [IO.Directory]::Delete($copilot.Source, $true)
    }

    It 'A3 Copilot copy rewrites every top-level jsonl and yaml and drops mc_ only from workspace.yaml' {
        $copilot = New-T14Copilot 't14 top level'
        # A case-variant id is not the id, and only lines that start with lower-case mc_ are dropped.
        $upper = $copilot.Old.ToUpperInvariant()
        [IO.File]::AppendAllText($copilot.Workspace, "alias: $upper`nMC_keep: 1`n  mc_keep: 1`n")
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'other.jsonl'), "{`"ref`":`"$($copilot.Old)`",`"alias`":`"$upper`"}`n{`"half")
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'keep.jsonl'), "{`"ref`":`"$($copilot.Old)`"}`n{`"complete`":`"keep-me`"}")
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'extra.yaml'), "mc_keep: 1`nref: $($copilot.Old)`ntail: last")
        # Non-ASCII text survives as UTF-8 (the characters are built from code points so this file stays ASCII).
        $kept = [string]::new([char[]]@(0x4FDD, 0x7559))
        $utf8 = [Text.UTF8Encoding]::new($false)
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'unicode.jsonl'), "{`"content`":`"$kept`",`"ref`":`"$($copilot.Old)`"}`n", $utf8)
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'unicode.yaml'), "note: $kept`nref: $($copilot.Old)`n", $utf8)
        [IO.File]::WriteAllText((Join-Path $copilot.Source 'notes.txt'), "ref: $($copilot.Old)`n")
        $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New
        ConvertTo-T14Json $result | Should BeExactly '[true]'
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'other.jsonl')) | Should BeExactly "{`"ref`":`"$T14New`",`"alias`":`"$upper`"}`n"
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'keep.jsonl')) | Should BeExactly "{`"ref`":`"$T14New`"}`n{`"complete`":`"keep-me`"}`n"
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'extra.yaml')) | Should BeExactly "mc_keep: 1`nref: $T14New`ntail: last"
        $unicode = [ordered]@{ 'unicode.jsonl' = "{`"content`":`"$kept`",`"ref`":`"$T14New`"}`n"; 'unicode.yaml' = "note: $kept`nref: $T14New`n" }
        foreach ($name in $unicode.Keys) {
            $bytes = [IO.File]::ReadAllBytes((Join-Path $copilot.Target $name))
            "$name $($utf8.GetString($bytes))" | Should BeExactly "$name $($unicode[$name])"
            "$name $([Convert]::ToHexString($bytes))" | Should BeExactly "$name $([Convert]::ToHexString($utf8.GetBytes($unicode[$name])))"
        }
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'workspace.yaml')).EndsWith("`nalias: $upper`nMC_keep: 1`n  mc_keep: 1`n") | Should Be $true
        [IO.File]::ReadAllText((Join-Path $copilot.Target 'notes.txt')) | Should BeExactly "ref: $($copilot.Old)`n"
    }

    It 'A3 Copilot copy rolls back on every failed check and exception' {
        $leftover = '12345678-9abc-4def-8123-456766666666'
        $cases = [ordered]@{
            'no id line'        = { param($c) [IO.File]::WriteAllText($c.Workspace, ([IO.File]::ReadAllText($c.Workspace) -replace '(?m)^id: .*\n', '')) }
            'upper id key'      = { param($c) [IO.File]::WriteAllText($c.Workspace, ([IO.File]::ReadAllText($c.Workspace) -creplace '(?m)^id: ', 'ID: ')) }
            'quoted id'         = { param($c) [IO.File]::WriteAllText($c.Workspace, ([IO.File]::ReadAllText($c.Workspace) -replace '(?m)^id: (.*)$', 'id: "$1"')) }
            'events without id' = { param($c) [IO.File]::WriteAllText($c.Events, "{`"type`":`"x`"}`n") }
            'missing events'    = { param($c) [IO.File]::Delete($c.Events) }
            'missing workspace' = { param($c) [IO.File]::Delete($c.Workspace) }
            'missing source'    = { param($c) [IO.Directory]::Delete($c.Source, $true) }
            'blank line'        = { param($c) [IO.File]::WriteAllText($c.Events, ([IO.File]::ReadAllText($c.Events) -replace '\n', "`n`n")) }
            # One pass of Replace leaves a new old id where the target's tail meets the rest of the text.
            'leftover id'       = { param($c) [IO.File]::AppendAllText($c.Workspace, "note: $($c.Old)$($c.Old.Substring(8))`n") }
            'other invalid line' = { param($c) [IO.File]::WriteAllText((Join-Path $c.Source 'other.jsonl'), "{`"a`":1}`n{`"type`":broken}`n") }
            'other blank line'  = { param($c) [IO.File]::WriteAllText((Join-Path $c.Source 'other.jsonl'), "{`"a`":1}`n`n{`"b`":2}`n") }
            'leftover id jsonl' = { param($c) [IO.File]::WriteAllText((Join-Path $c.Source 'other.jsonl'), "{`"note`":`"$($c.Old)$($c.Old.Substring(8))`"}`n") }
            'spaced id line'    = { param($c) [IO.File]::WriteAllText($c.Workspace, ([IO.File]::ReadAllText($c.Workspace) -replace '(?m)^id: (.*)$', ' id: $1 ')) }
            'crlf workspace'    = { param($c) [IO.File]::WriteAllText($c.Workspace, ([IO.File]::ReadAllText($c.Workspace) -replace "`n", "`r`n")) }
            'locked file'       = { param($c) Write-FakeMuxAgentFile (Join-Path $c.Source 'z\locked.bin') @('x'); [IO.File]::Open((Join-Path $c.Source 'z\locked.bin'), 'Open', 'Read', 'None') }
        }
        foreach ($name in $cases.Keys) {
            $copilot = New-T14Copilot "t14 rollback $name"
            $target = if ($name -like 'leftover id*') { $leftover } else { $T14New }
            $handles = @(& $cases[$name] $copilot)
            try { $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $target }
            finally { foreach ($handle in $handles) { if ($handle -is [IDisposable]) { $handle.Dispose() } } }
            "$name $(ConvertTo-T14Json $result)" | Should BeExactly "$name [false]"
            "$name $([IO.Directory]::Exists((Join-Path $copilot.Home ".copilot\session-state\$target")))" | Should BeExactly "$name False"
            "$name $(@(Get-ChildItem -LiteralPath (Join-Path $copilot.Home '.copilot\session-state') -Force -Name | Where-Object { $_ -like '*.tmp' }).Count)" | Should BeExactly "$name 0"
        }
    }
    function Get-T14Tree {
        # Every folder and file below Folder with its size and hash, sorted, so any change shows.
        param([string]$Folder)
        @(Get-ChildItem -LiteralPath $Folder -Recurse -Force | ForEach-Object {
                $relative = $_.FullName.Substring($Folder.Length)
                if ($_.PSIsContainer) { "$relative\" } else { "$relative=$($_.Length)=$((Get-FileHash -LiteralPath $_.FullName).Hash)" }
            } | Sort-Object) -join ';'
    }

    It 'A3 a brief has no ParentSession' {
        $full = [hashtable]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($pair in @(@('CLAUDE_CODE_ENTRYPOINT', 'sdk-cli'), @('CLAUDE_CODE_SESSION_ID', $T14Parent), @('COPILOT_AGENT_SESSION_ID', $T14Parent))) { $full[$pair[0]] = $pair[1] }
        $cases = @(@('human', 'claude', 'claude', $false), @('agent', 'claude', 'claude', $true), @('agent', 'claude', 'copilot', $false), @('agent', 'claude', 'claude', $false))
        foreach ($case in $cases) {
            $caller = [pscustomobject]@{ Kind = $case[0]; Client = if ($case[0] -eq 'agent') { $case[1] } }
            $plan = Get-MuxAgentContextPlan -Caller $caller -Client $case[2] -Env $full -NoFork:$case[3]
            "$($plan.Reason) $($null -eq $plan.ParentSession)" | Should BeExactly "$($plan.Reason) True"
        }
        $empty = [hashtable]::new([StringComparer]::OrdinalIgnoreCase)
        $null -eq (Get-MuxAgentContextPlan -Caller ([pscustomobject]@{ Kind = 'agent'; Client = 'copilot' }) -Client 'copilot' -Env $empty).ParentSession | Should Be $true
    }

    It 'A3 Copilot copy refuses ids that are not GUIDs and touches nothing' {
        $copilot = New-T14Copilot 't14 bad ids'
        # A usable session outside session-state, which SourceId '../outside' would reach.
        $outside = Join-Path $copilot.Home '.copilot\outside'
        Write-FakeMuxAgentFile (Join-Path $outside 'workspace.yaml') @('id: ../outside')
        Write-FakeMuxAgentFile (Join-Path $outside 'events.jsonl') @('{"sessionId":"../outside"}')
        $absolute = Join-Path $TestDrive 't14 bad ids absolute'
        $before = Get-T14Tree $copilot.Home
        foreach ($bad in @('../outside', '..\x', $absolute, '', 'not-a-guid')) {
            foreach ($role in @('source', 'target')) {
                $sourceId = if ($role -eq 'source') { $bad } else { $copilot.Old }
                $targetId = if ($role -eq 'target') { $bad } else { $T14New }
                $result = Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $sourceId -TargetId $targetId
                "$role '$bad' $(ConvertTo-T14Json $result)" | Should BeExactly "$role '$bad' [false]"
                "$role '$bad' $(Get-T14Tree $copilot.Home)" | Should BeExactly "$role '$bad' $before"
            }
        }
        Test-Path -LiteralPath $absolute | Should Be $false
        # Letter case does not matter.
        ConvertTo-T14Json (Copy-MuxAgentCopilotSession -HomeDirectory $copilot.Home -SourceId $copilot.Old -TargetId $T14New.ToUpperInvariant()) | Should BeExactly '[true]'
        $upperCopilot = New-T14Copilot 't14 upper source'
        $upperOld = $upperCopilot.Old.ToUpperInvariant()
        foreach ($path in @($upperCopilot.Workspace, $upperCopilot.Events)) { [IO.File]::WriteAllText($path, [IO.File]::ReadAllText($path).Replace($upperCopilot.Old, $upperOld)) }
        ConvertTo-T14Json (Copy-MuxAgentCopilotSession -HomeDirectory $upperCopilot.Home -SourceId $upperOld -TargetId $T14New) | Should BeExactly '[true]'
    }

    It 'A3 Copilot copy builds in a unique temp folder and never replaces a competing target' {
        $library = Join-Path $repoRoot 'scripts\MuxAgents.ps1'
        $lines = @(Select-String -LiteralPath $library -SimpleMatch '[IO.Directory]::Move(')
        $lines.Count | Should Be 1
        $raceTemps = [Collections.Generic.List[string]]::new()
        foreach ($raceCompetitor in @('none', 'folder', 'file')) {
            $raceCopilot = New-T14Copilot "t14 race $raceCompetitor"
            $raceRoot = Split-Path $raceCopilot.Target
            # Just before the move, record the temp folder and let a competitor take the target.
            $breakpoint = Set-PSBreakpoint -Script $library -Line $lines[0].LineNumber -Action {
                $raceTemps.Add((@(Get-ChildItem -LiteralPath $raceRoot -Force -Name | Where-Object { $_ -ne $raceCopilot.Old }) -join ','))
                if ($raceCompetitor -eq 'folder') { Write-FakeMuxAgentFile (Join-Path $raceCopilot.Target 'theirs.txt') @('theirs') }
                if ($raceCompetitor -eq 'file') { [IO.File]::WriteAllText($raceCopilot.Target, 'theirs') }
            }
            try { $result = Copy-MuxAgentCopilotSession -HomeDirectory $raceCopilot.Home -SourceId $raceCopilot.Old -TargetId $T14New }
            finally { Remove-PSBreakpoint -Breakpoint $breakpoint }
            $expected = if ($raceCompetitor -eq 'none') { '[true]' } else { '[false]' }
            "$raceCompetitor $(ConvertTo-T14Json $result)" | Should BeExactly "$raceCompetitor $expected"
            $left = @(Get-ChildItem -LiteralPath $raceRoot -Force -Name | Where-Object { $_ -like '*.tmp' }).Count
            "$raceCompetitor temps left $left" | Should BeExactly "$raceCompetitor temps left 0"
            if ($raceCompetitor -eq 'folder') { "$(Get-T14Tree $raceCopilot.Target) $([IO.File]::ReadAllText((Join-Path $raceCopilot.Target 'theirs.txt')))" | Should Match "^\\theirs\.txt=7=[0-9A-F]{64} theirs`n$" }
            if ($raceCompetitor -eq 'file') { [IO.File]::ReadAllText($raceCopilot.Target) | Should BeExactly 'theirs' }
            if ($raceCompetitor -eq 'none') { [IO.File]::Exists((Join-Path $raceCopilot.Target 'events.jsonl')) | Should Be $true }
        }
        foreach ($temp in $raceTemps) { $temp | Should Match ('^\.' + [regex]::Escape($T14New) + '\.[0-9a-f]{32}\.tmp$') }
        @($raceTemps | Sort-Object -Unique).Count | Should Be 3
    }

    It 'A3 Copilot copy creates nothing when the session is missing' {
        $homeDirectory = Join-Path $TestDrive 't14 no copilot'
        $null = [IO.Directory]::CreateDirectory($homeDirectory)
        ConvertTo-T14Json (Copy-MuxAgentCopilotSession -HomeDirectory $homeDirectory -SourceId $T14Parent -TargetId $T14New) | Should BeExactly '[false]'
        @(Get-ChildItem -LiteralPath $homeDirectory -Force).Count | Should Be 0
    }

    It 'A3 Copilot copy returns false when the temp folder cannot be made' {
        # The dispatcher runs verbs with Stop, so a cleanup error would surface here instead of false.
        $ErrorActionPreference = 'Stop'
        $library = Join-Path $repoRoot 'scripts\MuxAgents.ps1'
        $lines = @(Select-String -LiteralPath $library -SimpleMatch 'CreateDirectory($temp)')
        $lines.Count | Should Be 1
        $deniedCopilot = New-T14Copilot 't14 denied'
        $deniedRoot = Split-Path $deniedCopilot.Target
        $before = Get-T14Tree $deniedCopilot.Home
        # Just before the temp folder is made, a file takes session-state's place, so CreateDirectory fails.
        $breakpoint = Set-PSBreakpoint -Script $library -Line $lines[0].LineNumber -Action { [IO.Directory]::Move($deniedRoot, "$deniedRoot.away"); [IO.File]::WriteAllText($deniedRoot, 'x') }
        try { $result = Copy-MuxAgentCopilotSession -HomeDirectory $deniedCopilot.Home -SourceId $deniedCopilot.Old -TargetId $T14New }
        finally {
            Remove-PSBreakpoint -Breakpoint $breakpoint
            [IO.File]::Delete($deniedRoot)
            [IO.Directory]::Move("$deniedRoot.away", $deniedRoot)
        }
        ConvertTo-T14Json $result | Should BeExactly '[false]'
        Get-T14Tree $deniedCopilot.Home | Should BeExactly $before
    }
}

Describe 'MuxAgents T1.5 header' -Tag 'windows' {
    $T15Id = '0123456789ab'
    $T15Claude = '11111111-2222-4333-8444-555555555555'
    $T15Copilot = '66666666-7777-4888-9999-aaaaaaaaaaaa'

    function New-T15Header {
        # The header for a stand-in caller (human, or an agent of From) and a plan of Context and Reason.
        param([string]$From = 'human', [string]$Client = 'claude', [string]$Context = 'fork', [string]$Reason = 'same-client', [string]$Pane = '%1', [string]$Brief, [string]$Transcript, $Fake)
        if (-not $Fake) { $Fake = New-FakeMuxAgentProviders }
        $caller = [pscustomobject]@{ Kind = if ($From -eq 'human') { 'human' } else { 'agent' }; Client = if ($From -ne 'human') { $From }; Pane = $Pane }
        $plan = [pscustomobject]@{ Context = $Context; Reason = $Reason; ParentSession = $null }
        @(New-MuxAgentHeader -P $Fake -Caller $caller -Name "$Client-2" -Id $T15Id -Client $Client -Plan $plan -BriefPath $Brief -TranscriptPath $Transcript)
    }

    function Get-T15Refusal {
        # 'binding' when parameter binding refuses the command, the base exception type (and a typed refusal's code
        # and check) for any other refusal, else 'ran'.
        param([scriptblock]$Command)
        try { $null = & $Command; 'ran' }
        catch [Management.Automation.ParameterBindingException] { 'binding' }
        catch {
            $exception = $_.Exception.GetBaseException()
            $typed = if ($exception.Data.Contains('MuxAgentCode')) { " $($exception.Data['MuxAgentCode']) $($exception.Data['MuxAgentCheck'])" }
            $exception.GetType().FullName + $typed
        }
    }

    function Invoke-T15Probe {
        # Runs the Reply and Listen lines of a header for the probe entry in Shell (pwsh or bash) and returns
        # each run's JSON output; <file> becomes body.md.
        param([string[]]$Header, [string]$Shell)
        foreach ($line in @($Header[3].Substring('Reply: '.Length).Replace('<file>', 'body.md'), $Header[4].Substring('Listen: '.Length))) {
            $output = if ($Shell -eq 'bash') { & 'C:\Program Files\Git\bin\bash.exe' -c $line } else { & ([scriptblock]::Create($line)) }
            @($output) -join ''
        }
    }

    $T15Bash = [IO.File]::Exists('C:\Program Files\Git\bin\bash.exe')

    function New-T15Probe {
        # A probe entry under a folder with spaces, parentheses and CJK characters; it prints its full path and
        # arguments as ASCII JSON. Expected holds the Reply and Listen outputs.
        param([string]$Folder)
        $directory = Join-Path $TestDrive ("$Folder rmux agents (x86) " + [string][char]0x4E2D + [char]0x6587)
        $entry = Join-Path $directory 'mux-agent.ps1'
        Write-FakeMuxAgentFile $entry @('ConvertTo-Json -Compress -EscapeHandling EscapeNonAscii -InputObject ([string[]](@([IO.Path]::GetFullPath($PSCommandPath)) + $args))')
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive $Folder)
        $fake.Entry = $entry
        $expected = @(@($entry, 'send', 'parent', '-BodyFile', 'body.md'), @($entry, 'read', '-Wait', '600')) | ForEach-Object { ConvertTo-Json -Compress -EscapeHandling EscapeNonAscii -InputObject ([string[]]$_) }
        [pscustomobject]@{ Fake = $fake; Expected = $expected }
    }

    It 'A5 header per context and client' {
        $fake = New-FakeMuxAgentProviders
        $entry = $fake.Entry.Replace('\', '/')
        $reply = "Reply: pwsh -NoProfile -File `"$entry`" send parent -BodyFile <file>"
        $listen = "Listen: pwsh -NoProfile -File `"$entry`" read -Wait 600"
        # A fork names neither the brief nor a transcript, even when given.
        $header = New-T15Header -From 'claude' -Brief 'C:\b\brief.md' -Transcript 'C:\t\x.jsonl' -Fake $fake
        ConvertTo-Json -Compress $header | Should BeExactly (ConvertTo-Json -Compress @(
                '[mux-agent] You are claude-2 (claude, id 0123456789ab), started in this RMUX session.',
                'Your parent is claude agent (%1); report to it with send parent; free text only in files.',
                'Context: forked from the parent session (same-client)', $reply, $listen))
        $header = New-T15Header -Client 'copilot' -Context 'brief' -Reason 'human' -Brief 'C:\b dir\brief.md' -Fake $fake
        ConvertTo-Json -Compress $header | Should BeExactly (ConvertTo-Json -Compress @(
                '[mux-agent] You are copilot-2 (copilot, id 0123456789ab), started in this RMUX session.',
                'Your parent is root (%1); report to it with send parent; free text only in files.',
                'Context: brief-only (human); brief: C:/b dir/brief.md; parent transcript: none', $reply, $listen))
        foreach ($from in @('human', 'claude', 'copilot')) {
            foreach ($client in @('claude', 'copilot')) {
                foreach ($context in @('fork', 'brief')) {
                    $case = "$from $client $context"
                    $header = New-T15Header -From $from -Client $client -Context $context -Reason 'r-x' -Pane '%7' -Brief 'C:\b\f.md' -Transcript 'C:\t\p.jsonl' -Fake $fake
                    $parent = if ($from -eq 'human') { 'root' } else { "$from agent" }
                    $line = if ($context -eq 'fork') { 'Context: forked from the parent session (r-x)' } else { 'Context: brief-only (r-x); brief: C:/b/f.md; parent transcript: C:/t/p.jsonl' }
                    "$case $(ConvertTo-Json -Compress $header)" | Should BeExactly ("$case " + (ConvertTo-Json -Compress @(
                                "[mux-agent] You are $client-2 ($client, id $T15Id), started in this RMUX session.",
                                "Your parent is $parent (%7); report to it with send parent; free text only in files.", $line, $reply, $listen)))
                    "$case $(($header -join "`n").Contains('\'))" | Should BeExactly "$case False"
                }
            }
        }
        Get-T15Refusal { New-T15Header -Context 'other' -Fake $fake } | Should BeExactly 'System.ArgumentException'
        # Any caller that is not an agent is root, even with a Client.
        foreach ($kind in @('human', 'other')) {
            $header = @(New-MuxAgentHeader -P $fake -Caller ([pscustomobject]@{ Kind = $kind; Client = 'claude'; Pane = '%1' }) -Name 'claude-2' -Id $T15Id -Client 'claude' -Plan ([pscustomobject]@{ Context = 'fork'; Reason = 'r' }))
            "$kind $($header[1])" | Should BeExactly "$kind Your parent is root (%1); report to it with send parent; free text only in files."
        }
        # The identity line names exactly the given Name and Id.
        $header = @(New-MuxAgentHeader -P $fake -Caller ([pscustomobject]@{ Kind = 'agent'; Client = 'claude'; Pane = '%1' }) -Name 'reviewer' -Id 'abcdef012345' -Client 'copilot' -Plan ([pscustomobject]@{ Context = 'fork'; Reason = 'r' }))
        $header[0] | Should BeExactly '[mux-agent] You are reviewer (copilot, id abcdef012345), started in this RMUX session.'
        # Every identity input is required, so a missing one is refused while binding.
        $caller = [pscustomobject]@{ Kind = 'human'; Client = $null; Pane = '%1' }
        $plan = [pscustomobject]@{ Context = 'fork'; Reason = 'same-client'; ParentSession = $null }
        $inputs = @{ P = $fake; Caller = $caller; Name = 'claude-2'; Id = $T15Id; Client = 'claude'; Plan = $plan }
        Get-T15Refusal { New-MuxAgentHeader @inputs } | Should BeExactly 'ran'
        foreach ($name in @('P', 'Caller', 'Name', 'Id', 'Client', 'Plan')) {
            $missing = $inputs.Clone()
            $missing[$name] = if ($name -in @('P', 'Caller', 'Plan')) { $null } else { '' }
            "$name $(Get-T15Refusal { New-MuxAgentHeader @missing })" | Should BeExactly "$name binding"
        }
    }

    It 'A5 brief-only header names the brief and parent transcript' {
        $expected = @{ claude = { param($f) ($f.Home + "/.claude/projects/fake-work/$T15Claude.jsonl").Replace('\', '/') }; copilot = { param($f) ($f.Home + "/.copilot/session-state/$T15Copilot/events.jsonl").Replace('\', '/') } }
        foreach ($from in @('human', 'claude', 'copilot')) {
            $fake = New-FakeMuxAgentProviders -Caller $from -Root (Join-Path $TestDrive "t15 brief $from")
            $caller = Resolve-MuxAgentCaller -P $fake
            $plan = Get-MuxAgentContextPlan -Caller $caller -Client 'claude' -Env $fake.Env -NoFork:($from -ne 'human')
            $transcript = Get-MuxAgentParentTranscript -P $fake -Caller $caller
            $brief = Join-Path $fake.Home 'brief dir\brief.md'
            $header = @(New-MuxAgentHeader -P $fake -Caller $caller -Name 'claude-2' -Id $T15Id -Client 'claude' -Plan $plan -BriefPath $brief -TranscriptPath $transcript)
            $named = if ($from -eq 'human') { 'none' } else { & $expected[$from] $fake }
            $reason = if ($from -eq 'human') { 'human' } else { 'no-fork' }
            "$from $($header[2])" | Should BeExactly "$from Context: brief-only ($reason); brief: $($brief.Replace('\', '/')); parent transcript: $named"
        }
        # A human caller has no parent transcript even when a client's variables and files are there.
        foreach ($client in @('claude', 'copilot')) {
            $fake = New-FakeMuxAgentProviders -Caller $client -Root (Join-Path $TestDrive "t15 human $client")
            $human = [pscustomobject]@{ Kind = 'human'; Client = $null; Pane = '%1' }
            "$client $($null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $human))" | Should BeExactly "$client True"
        }
        # Only a well-formed session id is looked up, so a crafted one cannot point elsewhere.
        $fake = New-FakeMuxAgentProviders -Caller copilot -Root (Join-Path $TestDrive 't15 crafted copilot')
        $caller = Resolve-MuxAgentCaller -P $fake
        foreach ($id in @("x\..\$T15Copilot", "$T15Copilot\..\$T15Copilot", " $T15Copilot", "$T15Copilot ")) {
            $fake.Env['COPILOT_AGENT_SESSION_ID'] = $id
            "[$id] $($null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $caller))" | Should BeExactly "[$id] True"
        }
        Get-T15Refusal { Get-MuxAgentParentTranscript -P $fake -Caller $null } | Should BeExactly 'binding'
        Get-T15Refusal { Get-MuxAgentParentTranscript -P $null -Caller ([pscustomobject]@{ Kind = 'human' }) } | Should BeExactly 'binding'
        # A Copilot session without events.jsonl has no transcript.
        $fake.Env['COPILOT_AGENT_SESSION_ID'] = $T15Copilot
        [IO.File]::Delete($fake.Fixtures.CopilotEvents)
        $null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $caller) | Should Be $true
        $fake = New-FakeMuxAgentProviders -Caller claude -Root (Join-Path $TestDrive 't15 crafted claude')
        $caller = Resolve-MuxAgentCaller -P $fake
        $fake.Env['CLAUDE_CODE_SESSION_ID'] = "..\fake-work\$T15Claude"
        $null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $caller) | Should Be $true
        # Without a brief file the brief is none.
        $header = New-T15Header -Context 'brief' -Reason 'human'
        $header[2] | Should BeExactly 'Context: brief-only (human); brief: none; parent transcript: none'
    }

    It 'A5 multi-line task follows the header' {
        $header = New-T15Header -From 'claude'
        $task = "  build the parser`r`n  keep -Text`n`tthen test it`n"
        $prompt = Join-MuxAgentPrompt -Header $header -Task $task
        $prompt | Should BeExactly (($header -join "`n") + "`n`n" + $task)
        $lines = $prompt -split "`n"
        $lines[0].StartsWith('[mux-agent]') | Should Be $true
        $lines.IndexOf('') | Should Be 5
        Get-T15Refusal { Join-MuxAgentPrompt -Header $header -Task '' } | Should BeExactly 'binding'
        Get-T15Refusal { Join-MuxAgentPrompt -Header @() -Task $task } | Should BeExactly 'binding'
    }

    It 'A5 transcript lookup' {
        $home15 = Join-Path $TestDrive 't15 lookup'
        $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId $T15Claude) | Should Be $true
        $projects = Join-Path $home15 '.claude\projects'
        $null = [IO.Directory]::CreateDirectory((Join-Path $projects "folder only\$T15Claude.jsonl"))
        Write-FakeMuxAgentFile (Join-Path $projects "a\nested\$T15Claude.jsonl") @('{}')
        Write-FakeMuxAgentFile (Join-Path $projects "b\other.jsonl") @('{}')
        $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId $T15Claude) | Should Be $true
        Write-FakeMuxAgentFile (Join-Path $projects "c dir\$T15Claude.jsonl") @('{}')
        Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId $T15Claude | Should BeExactly (Join-Path $projects "c dir\$T15Claude.jsonl")
        Write-FakeMuxAgentFile (Join-Path $projects "d\$T15Claude.jsonl") @('{}')
        $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId $T15Claude) | Should Be $true
        Get-T15Refusal { Find-MuxAgentClaudeTranscript -HomeDirectory '' -SessionId $T15Claude } | Should BeExactly 'binding'
        Get-T15Refusal { Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId '' } | Should BeExactly 'binding'
        # Only a well-formed id builds a path, so a file named for a malformed one is never returned.
        Write-FakeMuxAgentFile (Join-Path $projects 'project-a\not-a-guid.jsonl') @('{}')
        $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $home15 -SessionId 'not-a-guid') | Should Be $true
        $other = Join-Path $TestDrive 't15 lookup other'
        $id = 'abcdefab-cdef-4abc-8def-abcdefabcdef'
        Write-FakeMuxAgentFile (Join-Path $other ".claude\projects\p\$id.jsonl") @('{}')
        Find-MuxAgentClaudeTranscript -HomeDirectory $other -SessionId $id | Should BeExactly (Join-Path $other ".claude\projects\p\$id.jsonl")
        # A malformed id never lists the projects folder: this one is a junction to a deleted folder, so it exists
        # but listing it throws.
        $dangling = Join-Path $TestDrive 't15 lookup dangling'
        $gone = Join-Path $dangling 'gone'
        $null = [IO.Directory]::CreateDirectory($gone)
        $null = [IO.Directory]::CreateDirectory((Join-Path $dangling '.claude'))
        $junction = Join-Path $dangling '.claude\projects'
        $null = New-Item -ItemType Junction -Path $junction -Target $gone
        [IO.Directory]::Delete($gone)
        try {
            Get-T15Refusal { Find-MuxAgentClaudeTranscript -HomeDirectory $dangling -SessionId $T15Claude } | Should BeExactly 'System.IO.DirectoryNotFoundException'
            foreach ($id in @('not-a-guid', '../invalid', "$T15Claude`n", "x\..\$T15Claude")) {
                "[$id] $(Get-T15Refusal { $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $dangling -SessionId $id) })" | Should BeExactly "[$id] ran"
                "[$id] $($null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $dangling -SessionId $id))" | Should BeExactly "[$id] True"
            }
        }
        finally { [IO.Directory]::Delete($junction) }
    }

    It 'A5 the Entry allowlist accepts exactly its characters and refuses the rest before any line' {
        $caller = [pscustomobject]@{ Kind = 'human'; Client = $null; Pane = '%1' }
        $plan = [pscustomobject]@{ Context = 'fork'; Reason = 'same-client' }
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't15 allowlist')
        $deployed = $fake.Entry
        $build = { param($Entry) $fake.Entry = $Entry; $lines = [Collections.Generic.List[object]]::new(); $outcome = Get-T15Refusal { New-MuxAgentHeader -P $fake -Caller $caller -Name 'claude-2' -Id $T15Id -Client 'claude' -Plan $plan | ForEach-Object { $lines.Add($_) } }; "$outcome $($lines.Count)" }
        $accepted = 'ran 5'
        $refused = 'System.InvalidOperationException 1 entry 0'
        # The expected sets are written out here, never derived from the production pattern. Each case puts one
        # character inside a folder name of an otherwise valid Entry.
        $allowed = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 _\/:.()-'
        $cases = [Collections.Generic.List[object]]::new()
        for ($code = 0x20; $code -le 0x7E; $code++) { $cases.Add(@($code, $(if ($allowed.IndexOf([char]$code) -ge 0) { $accepted } else { $refused }))) }
        # Control characters; numbers that are not decimal digits; a no-break space; a combining mark; Unicode quotes and dashes.
        foreach ($code in @(0x00, 0x07, 0x09, 0x0A, 0x0D, 0x7F, 0x00B2, 0x2162, 0x00A0, 0x0301, 0x201C, 0x201D, 0x201E, 0x2018, 0x2019, 0x2013, 0x2014)) { $cases.Add(@($code, $refused)) }
        # CJK, Cyrillic and Greek letters, Arabic-Indic and fullwidth decimal digits and a fullwidth letter.
        foreach ($code in @(0x4E2D, 0x0416, 0x03A9, 0x0661, 0xFF11, 0xFF21)) { $cases.Add(@($code, $accepted)) }
        $cases.Count | Should Be 118
        # Wrong cases are labelled by code point, so no control character is printed.
        $wrong = foreach ($case in $cases) {
            $outcome = & $build ('C:\a' + [char]$case[0] + 'b\mux-agent.ps1')
            if ($outcome -cne $case[1]) { 'U+{0:X4} gave {1}' -f $case[0], $outcome }
        }
        @($wrong) -join ', ' | Should BeExactly ''
        # The empty Entry and an Entry ending in LF are refused.
        & $build '' | Should BeExactly $refused
        & $build ($deployed + "`n") | Should BeExactly $refused
        # The reviewer's repro: a closing U+201D ends the quoted argument in PowerShell.
        & $build ('C:\a' + [char]0x201D + '; Write-Output header-injected; #\mux-agent.ps1') | Should BeExactly $refused
        # Accepted: every allowed class (letters, digits, space, _, \, /, :, ., parentheses and -), CJK included.
        foreach ($entry in @($deployed, 'C:/Program Files (x86)/rmux_agents/v2/mux-agent.ps1', ('D:\' + [char]0x7528 + [char]0x6237 + '\rmux agents\mux-agent.ps1'))) {
            $fake.Entry = $entry
            $header = @(New-MuxAgentHeader -P $fake -Caller $caller -Name 'claude-2' -Id $T15Id -Client 'claude' -Plan $plan)
            "[$entry] $($header.Count) $($header[4])" | Should BeExactly "[$entry] 5 Listen: pwsh -NoProfile -File `"$($entry.Replace('\', '/'))`" read -Wait 600"
        }
        # The allowlist is one script-scope variable that the check reads.
        $shared = $script:MuxAgentEntryPattern
        try {
            $script:MuxAgentEntryPattern = '^C:\\only\z'
            & $build 'C:\only' | Should BeExactly 'ran 5'
            & $build 'C:\Program Files\mux-agent.ps1' | Should BeExactly 'System.InvalidOperationException 1 entry 0'
        }
        finally { $script:MuxAgentEntryPattern = $shared }
    }

    It 'A5 Reply and Listen run the entry exactly from pwsh' {
        $probe = New-T15Probe 't15 pwsh'
        $header = New-T15Header -Fake $probe.Fake
        ConvertTo-Json -Compress @(Invoke-T15Probe -Header $header -Shell 'pwsh') | Should BeExactly (ConvertTo-Json -Compress $probe.Expected)
    }

    It 'A5 Reply and Listen run the entry exactly from Git Bash' -Skip:(-not $T15Bash) {
        $probe = New-T15Probe 't15 bash'
        $header = New-T15Header -Fake $probe.Fake
        $bash = ConvertTo-Json -Compress @(Invoke-T15Probe -Header $header -Shell 'bash')
        $bash | Should BeExactly (ConvertTo-Json -Compress $probe.Expected)
        $bash | Should BeExactly (ConvertTo-Json -Compress @(Invoke-T15Probe -Header $header -Shell 'pwsh'))
    }

    It 'A5 a malformed parent session id is never looked up' {
        # An It-local stand-in counts lookups; the library function is not mocked.
        $lookups = [Collections.Generic.List[string]]::new()
        function Find-MuxAgentClaudeTranscript { param($HomeDirectory, $SessionId) $lookups.Add($SessionId); 'found' }
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 't15 stand-in')
        $caller = [pscustomobject]@{ Kind = 'agent'; Client = 'claude'; Pane = '%1' }
        foreach ($id in @('../invalid', "$T15Claude`n")) {
            $fake.Env['CLAUDE_CODE_SESSION_ID'] = $id
            "[$id] $($null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $caller))" | Should BeExactly "[$id] True"
        }
        $lookups.Count | Should Be 0
        $fake.Env['CLAUDE_CODE_SESSION_ID'] = $T15Claude
        Get-MuxAgentParentTranscript -P $fake -Caller $caller | Should BeExactly 'found'
        ConvertTo-Json -Compress @($lookups) | Should BeExactly (ConvertTo-Json -Compress @($T15Claude))
    }

    It 'A5 transcript checks follow the shared session-id pattern' {
        # Both checks read $script:MuxAgentSessionIdPattern, so a stand-in shared pattern changes what both accept and
        # a private copy of the regex would not follow it.
        $fake = New-FakeMuxAgentProviders -Caller claude -Root (Join-Path $TestDrive 't15 shared')
        $caller = Resolve-MuxAgentCaller -P $fake
        $other = Join-Path $fake.Home '.claude\projects\fake-work\not-a-guid.jsonl'
        Write-FakeMuxAgentFile $other @('{}')
        $shared = $script:MuxAgentSessionIdPattern
        try {
            $script:MuxAgentSessionIdPattern = '^not-a-guid\z'
            Find-MuxAgentClaudeTranscript -HomeDirectory $fake.Home -SessionId 'not-a-guid' | Should BeExactly $other
            $null -eq (Find-MuxAgentClaudeTranscript -HomeDirectory $fake.Home -SessionId $T15Claude) | Should Be $true
            $fake.Env['CLAUDE_CODE_SESSION_ID'] = 'not-a-guid'
            Get-MuxAgentParentTranscript -P $fake -Caller $caller | Should BeExactly $other
            $fake.Env['CLAUDE_CODE_SESSION_ID'] = $T15Claude
            $null -eq (Get-MuxAgentParentTranscript -P $fake -Caller $caller) | Should Be $true
        }
        finally { $script:MuxAgentSessionIdPattern = $shared }
        Get-MuxAgentParentTranscript -P $fake -Caller $caller | Should BeExactly $fake.Fixtures.ClaudeTranscript
    }
}

Describe 'MuxAgents T1.6 spawn' -Tag 'windows' {
    function Get-T16Hash {
        param([string]$Text)
        $sha = [Security.Cryptography.SHA256]::Create()
        try { -join @($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Text)) | ForEach-Object { $_.ToString('x2') }) } finally { $sha.Dispose() }
    }

    function New-T16Fake {
        # A fake whose pipe, and so its server key and spawn lock, is unique; State is its state folder (the fake
        # server 900 starts at 9000).
        param([string]$Caller = 'human', [string]$Root = (Join-Path $TestDrive ('t16 ' + [guid]::NewGuid().ToString('N'))))
        $fake = New-FakeMuxAgentProviders -Caller $Caller -Root $Root
        $pipe = '\\.\pipe\rmux-t16-' + [guid]::NewGuid().ToString('N')
        foreach ($name in @('RMUX', 'TMUX')) { $fake.Env[$name] = "$pipe,900,0" }
        $fake | Add-Member -NotePropertyName State -NotePropertyValue (Join-Path $fake.StateRoot (Get-T16Hash "$pipe|900|9000").Substring(0, 16))
        $fake | Add-Member -NotePropertyName Pipe -NotePropertyValue $pipe
        $fake
    }

    function Invoke-T16Spawn {
        # Code and the parsed reply of one spawn.
        param($Fake, [string[]]$Arguments)
        $result = Invoke-MuxAgentCommand -P $Fake -Arguments (@('spawn') + $Arguments)
        [pscustomobject]@{ Code = $result.Code; Reply = ($result.Text | ConvertFrom-Json) }
    }

    function Get-T16Calls {
        # The rmux calls whose command (after -N) is Verb, each as one compressed JSON array, joined by newlines.
        param($Fake, [string]$Verb)
        @(foreach ($call in $Fake.Calls) { if ($call[1] -eq $Verb) { ConvertTo-Json -Compress -InputObject ([string[]]$call) } }) -join "`n"
    }

    function Get-T16Launch {
        # The expected new-window or split-window argv for the agent Id, as compressed JSON.
        param($Fake, [string]$Id, [string[]]$Target)
        $spec = [IO.Path]::Combine($Fake.State, 'agents', $Id, 'spec.json')
        ConvertTo-Json -Compress -InputObject ([string[]](@('-N') + $Target + @('-c', $Fake.Cwd, $Fake.Pwsh, '-NoProfile', '-File', $Fake.Launcher, $spec)))
    }

    function Get-T16Agents {
        # The names of the folders under State\agents, joined by commas.
        param($Fake)
        $agents = Join-Path $Fake.State 'agents'
        if (-not [IO.Directory]::Exists($agents)) { return '' }
        @([IO.Directory]::GetDirectories($agents) | ForEach-Object { [IO.Path]::GetFileName($_) }) -join ','
    }

    function Set-T16Panes {
        # list-panes answers with Count agents named claude-<n>; Committed of them have commit.json.
        param($Fake, [int]$Count, [int]$Committed = $Count)
        # Spawn refuses a state folder that other users can read, so the fixture builds it the way spawn does
        # instead of inheriting the ACL of the test drive's parent (a shared temp folder on CI runners).
        New-MuxAgentPrivateDirectory (Get-MuxAgentUserSid) (Join-Path $Fake.State 'agents')
        $lines = foreach ($n in 1..$Count) {
            $id = '{0:x12}' -f (0xa00 + $n)
            if ($n -le $Committed) {
                $null = [IO.Directory]::CreateDirectory([IO.Path]::Combine($Fake.State, 'agents', $id))
                Write-MuxAgentJson -Path ([IO.Path]::Combine($Fake.State, 'agents', $id, 'commit.json')) -Value ([ordered]@{ committedAt = 'x' })
            }
            "%$(20 + $n)|@$(20 + $n)|$(300 + $n)|$id|claude-$n"
        }
        Add-FakeRmuxRule $Fake '^-N list-panes ' -StdOut ((@($lines) -join "`n") + "`n")
    }

    It 'A2 default window argv' {
        $fake = New-T16Fake
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.name)" | Should BeExactly '0 claude-1'
        Get-T16Calls $fake 'new-window' | Should BeExactly (Get-T16Launch $fake $run.Reply.id @('new-window', '-d', '-P', '-F', '#{pane_id}|#{window_id}', '-t', '$3:', '-n', 'claude-1'))
        $fake.StateRoot | Should Match ' '
        [IO.File]::Exists([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'spec.json')) | Should Be $true
        Get-T16Calls $fake 'split-window' | Should BeExactly ''
    }

    It 'A2 split h and v argv' {
        foreach ($split in @('h', 'v')) {
            $fake = New-T16Fake
            $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-Task', 'hi', '-Split', $split)
            "$split $($run.Code)" | Should BeExactly "$split 0"
            Get-T16Calls $fake 'split-window' | Should BeExactly (Get-T16Launch $fake $run.Reply.id @('split-window', '-d', "-$split", '-P', '-F', '#{pane_id}|#{window_id}', '-t', '%1'))
            Get-T16Calls $fake 'new-window' | Should BeExactly ''
        }
    }

    It 'A2 human child starts in current directory' {
        $fake = New-T16Fake
        $fake.Cwd | Should Match ' '
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        $call = @($fake.Calls | Where-Object { $_[1] -eq 'new-window' })[0]
        $call[[array]::IndexOf($call, '-c') + 1] | Should BeExactly $fake.Cwd
        (Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'record.json'))).directory | Should BeExactly $fake.Cwd
    }

    It 'A2 tagged caller pane refused' {
        foreach ($tag in @('0123456789ab', 'x')) {
            $fake = New-T16Fake
            Add-FakeRmuxRule $fake '^-N display-message ' -StdOut "900|100|`$3|@1|$tag`n"
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
            "$tag $($run.Code) $($run.Reply.check)" | Should BeExactly "$tag 2 tagged"
            Get-T16Calls $fake 'new-window' | Should BeExactly ''
            [IO.Directory]::Exists($fake.StateRoot) | Should Be $false
        }
    }

    It 'A2 cap of 6 live refused' {
        $fake = New-T16Fake
        Set-T16Panes $fake 6
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '2 cap'
        Get-T16Calls $fake 'new-window' | Should BeExactly ''
        # Uncommitted panes count too: a launch in progress holds its place.
        $fake = New-T16Fake
        Set-T16Panes $fake 6 -Committed 0
        "$((Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Reply.check)" | Should BeExactly 'cap'
        # Five live agents leave room for a sixth, named after the taken ones.
        $fake = New-T16Fake
        Set-T16Panes $fake 5
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.name)" | Should BeExactly '0 claude-6'
    }

    It 'A2 two simultaneous spawns at 5 live' {
        $root = Join-Path $TestDrive 't16 race'
        $pipe = '\\.\pipe\rmux-t16-' + [guid]::NewGuid().ToString('N')
        $panes = Join-Path $TestDrive 't16 race panes.txt'
        [IO.File]::WriteAllText($panes, (@(foreach ($n in 1..5) { "%$(20 + $n)|@$(20 + $n)|$(300 + $n)|$('{0:x12}' -f (0xa00 + $n))|claude-$n" }) -join "`n") + "`n")
        $body = {
            param($RepoRoot, $Root, $Pipe, $Panes)
            . (Join-Path $RepoRoot 'scripts\MuxAgents.ps1')
            . (Join-Path $RepoRoot 'tests\MuxAgents.TestHelpers.ps1')
            $fake = New-FakeMuxAgentProviders -Root $Root
            foreach ($name in @('RMUX', 'TMUX')) { $fake.Env[$name] = "$Pipe,900,0" }
            Add-FakeRmuxRule $fake '^-N list-panes ' -Script ({ param($Arguments) [pscustomobject]@{ ExitCode = 0; StdOut = [IO.File]::ReadAllText($Panes); StdErr = '' } }.GetNewClosure())
            Add-FakeRmuxRule $fake '^-N new-window ' -Script ({
                    param($Arguments)
                    Start-Sleep -Milliseconds 300
                    $id = [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($Arguments[-1]))
                    [IO.File]::AppendAllText($Panes, "%9|@5|309|$id|x`n")
                    [pscustomobject]@{ ExitCode = 0; StdOut = "%9|@5`n"; StdErr = '' }
                }.GetNewClosure())
            $result = Invoke-MuxAgentCommand -P $fake -Arguments @('spawn', '-Client', 'claude', '-Task', 'hi')
            "$($result.Code) $(($result.Text | ConvertFrom-Json).check)"
        }
        $jobs = @(foreach ($n in 1..2) { Start-ThreadJob -ArgumentList $repoRoot, $root, $pipe, $panes -ScriptBlock $body })
        try { $results = @(Receive-Job $jobs -Wait -ErrorAction Stop) } finally { Remove-Job $jobs -Force }
        (@($results | Sort-Object) -join ',') | Should BeExactly '0 ,2 cap'
    }

    It 'A2 failed list-panes refuses before new-window' {
        $fake = New-T16Fake
        Add-FakeRmuxRule $fake '^-N list-panes ' -ExitCode 1
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '1 rmux'
        Get-T16Calls $fake 'new-window' | Should BeExactly ''
        Get-T16Agents $fake | Should BeExactly ''
        # server.json is written under the lock before the panes are listed.
        [IO.File]::ReadAllText((Join-Path $fake.State 'server.json')) | Should BeExactly (ConvertTo-Json -Compress -InputObject ([ordered]@{ socket = $fake.Pipe; serverPid = 900; serverStart = 9000 }))
    }

    It 'A2 NoFork gives brief' {
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'do the task')
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), "brief text`n")
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-NoFork', '-TaskFile', 't.md', '-BriefFile', 'b.md')
        "$($run.Code) $($run.Reply.context) $($run.Reply.reason)" | Should BeExactly '0 brief no-fork'
        $folder = [IO.Path]::Combine($fake.State, 'agents', $run.Reply.id)
        [IO.File]::ReadAllText((Join-Path $folder 'brief.md')) | Should BeExactly "brief text`n"
        $spec = Read-MuxAgentJson -Path (Join-Path $folder 'spec.json')
        $transcript = $fake.Fixtures.ClaudeTranscript.Replace('\', '/')
        $lines = $spec.prompt -split "`n"
        $lines[0] | Should BeExactly "[mux-agent] You are claude-1 (claude, id $($run.Reply.id)), started in this RMUX session."
        $lines[1] | Should BeExactly 'Your parent is claude agent (%1); report to it with send parent; free text only in files.'
        $lines[2] | Should BeExactly "Context: brief-only (no-fork); brief: $((Join-Path $folder 'brief.md').Replace('\', '/')); parent transcript: $transcript"
        ($lines[5..6] -join '|') | Should BeExactly '|do the task'
        $lines.Count | Should Be 7
        ConvertTo-Json -Compress @($spec.args) | Should BeExactly (ConvertTo-Json -Compress @('--session-id', $run.Reply.sessionId, $spec.prompt))
        ConvertTo-Json -Compress ([ordered]@{ schema = $spec.schema; stateDirectory = $spec.stateDirectory; id = $spec.id; client = $spec.client; parentPane = $spec.parentPane }) |
            Should BeExactly (ConvertTo-Json -Compress ([ordered]@{ schema = 1; stateDirectory = $fake.State; id = $run.Reply.id; client = 'claude'; parentPane = '%1' }))
        (@($spec.PSObject.Properties.Name) -join ',') | Should BeExactly 'schema,stateDirectory,id,client,parentPane,args,prompt'
    }

    It 'A2 agent callers pass free text only in files' {
        foreach ($caller in @('claude', 'copilot')) {
            $fake = New-T16Fake -Caller $caller
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'x')
            "$caller $($run.Code) $($run.Reply.check)" | Should BeExactly "$caller 2 usage"
            Get-T16Calls $fake 'new-window' | Should BeExactly ''
            [IO.Directory]::Exists($fake.StateRoot) | Should Be $false
        }
    }

    It 'A2 record tag and result fields' {
        $fake = New-T16Fake
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        $reply = $run.Reply
        "$($run.Code) $($reply.name) $($reply.pane) $($reply.window) $($reply.client) $($reply.context) $($reply.reason)" | Should BeExactly '0 claude-1 %9 @5 claude brief human'
        $reply.id | Should MatchExactly '^[0-9a-f]{12}$'
        $reply.sessionId | Should MatchExactly '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        (@($reply.PSObject.Properties.Name) -join ',') | Should BeExactly 'ok,verb,name,id,pane,window,client,sessionId,context,reason'
        Get-T16Calls $fake 'set-option' | Should BeExactly (ConvertTo-Json -Compress -InputObject ([string[]]@('-N',
                    'set-option', '-p', '-t', '%9', '@agent-id', $reply.id, ';', 'set-option', '-p', '-t', '%9', '@agent-name', 'claude-1', ';',
                    'set-option', '-p', '-t', '%9', '@agent-parent', '%1', ';', 'set-option', '-p', '-t', '%9', '@agent-context', 'brief:human')))
        $expected = [ordered]@{ schema = 1; id = $reply.id; name = 'claude-1'; client = 'claude'; sessionId = $reply.sessionId; context = 'brief'; reason = 'human'
            session = '$3'; pane = '%9'; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $fake.Cwd; taskLine = 'hi'; createdAt = '2026-10-29T12:00:00.0000000Z' }
        [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $reply.id, 'record.json')) | Should BeExactly (ConvertTo-Json -Compress -InputObject $expected)
        # A Claude caller records its client, pid, exact start and session.
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'hi')
        $reply = (Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md', '-BriefFile', 't.md')).Reply
        $record = [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $reply.id, 'record.json'))
        $parent = ConvertTo-Json -Compress -InputObject ([ordered]@{ client = 'claude'; pid = 150; start = 1500; sessionId = '11111111-2222-4333-8444-555555555555' })
        $record | Should Match ([regex]::Escape('"parentAgent":' + $parent + ','))
        $record | Should Match ([regex]::Escape('"context":"brief","reason":"cross-client"'))
    }

    It 'A2 commit.json written after tag and record' {
        $fake = New-T16Fake
        $log = [Collections.Generic.List[string]]::new()
        $agents = Join-Path $fake.State 'agents'
        $seen = { param($Name) @([IO.Directory]::GetFiles($agents, $Name, [IO.SearchOption]::AllDirectories)).Count }.GetNewClosure()
        Add-FakeRmuxRule $fake '^-N new-window ' -StdOut "%9|@5`n" -Script ({ param($Arguments) $log.Add("launch spec $(& $seen 'spec.json')"); $null }.GetNewClosure())
        Add-FakeRmuxRule $fake '^-N set-option ' -Script ({ param($Arguments) $log.Add("tag commit $(& $seen 'commit.json') record $(& $seen 'record.json')"); $null }.GetNewClosure())
        $fake.Now = { $log.Add("now commit $(& $seen 'commit.json') record $(& $seen 'record.json')"); $fake.Clock }.GetNewClosure()
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        $run.Code | Should Be 0
        ($log -join ', ') | Should BeExactly 'launch spec 1, tag commit 0 record 0, now commit 0 record 0, now commit 0 record 1'
        $folder = Join-Path $agents $run.Reply.id
        [IO.File]::ReadAllText((Join-Path $folder 'commit.json')) | Should BeExactly '{"committedAt":"2026-10-29T12:00:00.0000000Z"}'
        [IO.File]::Exists((Join-Path $folder 'record.json')) | Should Be $true
    }

    It 'A2 pane killed and abort.json written when tagging fails' {
        foreach ($kill in @(0, 1)) {
            $fake = New-T16Fake
            Add-FakeRmuxRule $fake '^-N set-option ' -ExitCode 1
            Add-FakeRmuxRule $fake '^-N kill-pane ' -ExitCode $kill
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
            "$kill $($run.Code) $($run.Reply.check)" | Should BeExactly "$kill 1 launch"
            $id = Get-T16Agents $fake
            $folder = [IO.Path]::Combine($fake.State, 'agents', $id)
            [IO.File]::ReadAllText((Join-Path $folder 'abort.json')) | Should BeExactly '{"abortedAt":"2026-10-29T12:00:00.0000000Z","pane":"%9"}'
            "$([IO.File]::Exists((Join-Path $folder 'commit.json'))) $([IO.File]::Exists((Join-Path $folder 'record.json')))" | Should BeExactly 'False False'
            Get-T16Calls $fake 'kill-pane' | Should BeExactly '["-N","kill-pane","-t","%9"]'
        }
    }

    It 'A2 a failed or garbled launch writes abort.json and kills nothing' {
        $answers = @(@(1, "%9|@5`n"), @(0, ''), @(0, "%9|@5`n%10|@6`n"), @(0, "%9|@5;kill-server`n"), @(0, "#{pane_id}|@5`n"), @(0, "%9 |@5`n"))
        foreach ($answer in $answers) {
            $fake = New-T16Fake
            Add-FakeRmuxRule $fake '^-N new-window ' -ExitCode $answer[0] -StdOut $answer[1]
            # A stale match in the caller's scope must not name the pane.
            $null = '%7|@8' -match '^(%\d+)\|(@\d+)$'
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
            $case = ConvertTo-Json -Compress $answer
            "$case $($run.Code) $($run.Reply.check)" | Should BeExactly "$case 1 launch"
            $folder = [IO.Path]::Combine($fake.State, 'agents', (Get-T16Agents $fake))
            "$case $([IO.File]::ReadAllText((Join-Path $folder 'abort.json')))" | Should BeExactly "$case {`"abortedAt`":`"2026-10-29T12:00:00.0000000Z`",`"pane`":null}"
            "$case $(Get-T16Calls $fake 'kill-pane')$(Get-T16Calls $fake 'set-option')" | Should BeExactly "$case "
        }
        # A CRLF line end is not garbled.
        $fake = New-T16Fake
        Add-FakeRmuxRule $fake '^-N new-window ' -StdOut "%9|@5`r`n"
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.pane) $($run.Reply.window)" | Should BeExactly '0 %9 @5'
    }

    It 'A3 failed copy verification gives brief in JSON tag and header' {
        $fake = New-T16Fake -Caller 'copilot'
        $folder = [IO.Path]::GetDirectoryName($fake.Fixtures.CopilotEvents)
        [IO.File]::WriteAllText((Join-Path $folder 'extra.jsonl'), "{`"ok`":1}`n{not json`n")
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), 'brief')
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md', '-BriefFile', 'b.md')
        "$($run.Code) $($run.Reply.context) $($run.Reply.reason)" | Should BeExactly '0 brief copy-verification-failed'
        (Get-T16Calls $fake 'set-option') | Should Match ([regex]::Escape('"@agent-context","brief:copy-verification-failed"]'))
        $spec = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'spec.json'))
        ($spec.prompt -split "`n")[2] | Should Match '^Context: brief-only \(copy-verification-failed\); brief: '
        ConvertTo-Json -Compress @($spec.args) | Should BeExactly (ConvertTo-Json -Compress @('--session-id', $run.Reply.sessionId, '-i', $spec.prompt))
        (@([IO.Directory]::GetDirectories([IO.Path]::GetDirectoryName($folder))) -join ',') | Should BeExactly $folder
    }

    It 'A3 agent caller without BriefFile exits 2' {
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path ([IO.Path]::GetDirectoryName($fake.Fixtures.CopilotEvents)) 'extra.jsonl'), "{not json`n")
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '2 brief'
        Get-T16Calls $fake 'new-window' | Should BeExactly ''
        @([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count | Should Be 1
        Get-T16Agents $fake | Should BeExactly ''
        # A cross-client agent caller is brief from the start and needs it too.
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        "$((Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')).Reply.check)" | Should BeExactly 'brief'
    }

    It 'A2 same-client agents fork' {
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.context) $($run.Reply.reason)" | Should BeExactly '0 fork same-client'
        $folder = [IO.Path]::Combine($fake.State, 'agents', $run.Reply.id)
        [IO.File]::Exists((Join-Path $folder 'brief.md')) | Should Be $false
        $spec = Read-MuxAgentJson -Path (Join-Path $folder 'spec.json')
        ConvertTo-Json -Compress @($spec.args) | Should BeExactly (ConvertTo-Json -Compress @('--resume', '11111111-2222-4333-8444-555555555555', '--fork-session', '--session-id', $run.Reply.sessionId, $spec.prompt))
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.context) $($run.Reply.reason)" | Should BeExactly '0 fork same-client'
        [IO.File]::Exists([IO.Path]::Combine($fake.Home, '.copilot', 'session-state', $run.Reply.sessionId, 'events.jsonl')) | Should Be $true
        $spec = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'spec.json'))
        ConvertTo-Json -Compress @($spec.args) | Should BeExactly (ConvertTo-Json -Compress @('--resume', $run.Reply.sessionId, '-i', $spec.prompt))
    }

    It 'A2 a failed launch deletes the Copilot copy' {
        foreach ($verb in @('new-window', 'set-option')) {
            $fake = New-T16Fake -Caller 'copilot'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
            Add-FakeRmuxRule $fake "^-N $verb " -ExitCode 1
            $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
            "$verb $($run.Code) $($run.Reply.check)" | Should BeExactly "$verb 1 launch"
            "$verb $(@([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count)" | Should BeExactly "$verb 1"
        }
    }

    It 'A5 a refused entry launches nothing and leaves nothing' {
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $fake.Entry = 'C:\x"$(y)\mux-agent.ps1'
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '1 entry'
        Get-T16Calls $fake 'new-window' | Should BeExactly ''
        @([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count | Should Be 1
        Get-T16Agents $fake | Should BeExactly ''
    }

    It 'A2 names and session ids from outside are validated' {
        foreach ($name in @('../x', 'a;b', '#{pane_id}', 'parent', 'Claude-1')) {
            $fake = New-T16Fake
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi', '-Name', $name)
            "$name $($run.Code) $($run.Reply.check) $(Get-T16Calls $fake 'new-window')" | Should BeExactly "$name 2 name "
            "$name $(Get-T16Agents $fake)" | Should BeExactly "$name "
        }
        $fake = New-T16Fake
        Set-T16Panes $fake 1
        "$((Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi', '-Name', 'claude-1')).Reply.check)" | Should BeExactly 'name'
        $fake = New-T16Fake
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-Task', 'hi', '-Name', 'w-2')
        "$($run.Code) $($run.Reply.name)" | Should BeExactly '0 w-2'
        (Get-T16Calls $fake 'new-window') | Should Match '"-n","w-2","-c"'
        foreach ($session in @('../x', 'a;b', '#{pane_id}')) {
            foreach ($caller in @('claude', 'copilot')) {
                $fake = New-T16Fake -Caller $caller
                $fake.Env[$(if ($caller -eq 'claude') { 'CLAUDE_CODE_SESSION_ID' } else { 'COPILOT_AGENT_SESSION_ID' })] = $session
                [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
                $run = Invoke-T16Spawn $fake @('-Client', $caller, '-TaskFile', 't.md', '-BriefFile', 't.md')
                "$session $caller $($run.Code) $($run.Reply.reason)" | Should BeExactly "$session $caller 0 no-session-id"
                $record = [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'record.json'))
                $copilot = Join-Path $fake.Home '.copilot'
                $entries = if ([IO.Directory]::Exists($copilot)) { @([IO.Directory]::GetFileSystemEntries($copilot, '*', 'AllDirectories')).Count } else { 0 }
                "$session $caller $($record.Contains($session)) $entries" | Should BeExactly "$session $caller False $(if ($caller -eq 'copilot') { 4 } else { 0 })"
                $record | Should Match '"sessionId":null},'
            }
        }
    }

    It 'A2 spawn usage refusals launch nothing' {
        $work = (New-T16Fake).Cwd
        $big = Join-Path $work 'big.md'
        [IO.File]::WriteAllText($big, ('x' * 16385))
        $brief = Join-Path $work 'brief.md'
        [IO.File]::WriteAllBytes($brief, [byte[]]::new(1MB + 1))
        [IO.File]::WriteAllBytes((Join-Path $work 'bad.md'), [byte[]]@(0x61, 0xC3, 0x28))
        [IO.File]::WriteAllText((Join-Path $work 'empty.md'), '')
        $cases = @(
            @('-Task', 'hi'), @('-Client', 'Claude', '-Task', 'hi'), @('-Client', '../x', '-Task', 'hi'), @('-Client', 'a;b', '-Task', 'hi'),
            @('-Client', 'claude'), @('-Client', 'claude', '-Task', 'hi', '-TaskFile', $big), @('-Client', 'claude', '-Task', ''),
            @('-Client', 'claude', '-Task', 'hi', '-Split', 'H'), @('-Client', 'claude', '-Task', 'hi', '-Split', 'h;kill-server'), @('-Client', 'claude', '-Task', 'hi', '-Split', '#{pane_id}'),
            @('-Client', 'claude', '-TaskFile', (Join-Path $work 'missing.md')), @('-Client', 'claude', '-TaskFile', $work), @('-Client', 'claude', '-TaskFile', $big),
            @('-Client', 'claude', '-TaskFile', 'bad.md'), @('-Client', 'claude', '-TaskFile', 'empty.md'), @('-Client', 'claude', '-Task', 'hi', '-BriefFile', $brief),
            @('-Client', 'claude', '-Task', 'hi', '-BriefFile', 'missing.md'))
        foreach ($case in $cases) {
            $fake = New-T16Fake -Root ([IO.Path]::GetDirectoryName($work))
            foreach ($name in @('RMUX', 'TMUX')) { $fake.Env[$name] = '\\.\pipe\rmux-t16-' + [guid]::NewGuid().ToString('N') + ',900,0' }
            $run = Invoke-T16Spawn $fake $case
            $line = $case -join ' '
            "$line => $($run.Code) $($run.Reply.check) $(Get-T16Calls $fake 'new-window') $([IO.Directory]::Exists($fake.StateRoot))" | Should BeExactly "$line => 2 usage  False"
        }
    }

    It 'A2 task and brief files are UTF-8 up to 16 KB and 1 MB' {
        $fake = New-T16Fake
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), ('x' * 16384))
        [IO.File]::WriteAllBytes((Join-Path $fake.Cwd 'b.md'), [byte[]]::new(1MB))
        "$((Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md', '-BriefFile', 'b.md')).Code)" | Should BeExactly '0'
        $fake = New-T16Fake
        $task = Join-Path $TestDrive ('t16 task ' + [guid]::NewGuid().ToString('N') + '.md')
        [IO.File]::WriteAllText($task, ('caf' + [char]0xE9), [Text.UTF8Encoding]::new($true))
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), ('br' + [char]0xE9), [Text.UTF8Encoding]::new($true))
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', $task, '-BriefFile', 'b.md')
        $folder = [IO.Path]::Combine($fake.State, 'agents', $run.Reply.id)
        (Read-MuxAgentJson -Path (Join-Path $folder 'spec.json')).prompt | Should Match ("`n`ncaf" + [char]0xE9 + '\z')
        [IO.File]::ReadAllBytes((Join-Path $folder 'brief.md')) -join ',' | Should BeExactly '98,114,195,169'
        # The file is closed after reading, also when it is refused; deleted at once, before a collection could close it.
        [IO.File]::Delete($task)
        foreach ($limit in @(100, 1)) {
            $path = Join-Path $fake.Cwd "closed $limit.md"
            [IO.File]::WriteAllText($path, 'hi')
            $outcome = try { Read-MuxAgentText -Path $path -MaxBytes $limit } catch { $_.Exception.Data['MuxAgentCheck'] }
            [IO.File]::Delete($path)
            "$limit $outcome $([IO.File]::Exists($path))" | Should BeExactly "$limit $(if ($limit -eq 1) { 'usage' } else { 'hi' }) False"
        }
        # A file another process is still writing is read; a UTF-16 file is not UTF-8.
        $fake = New-T16Fake
        $open = [IO.File]::Open((Join-Path $fake.Cwd 't.md'), 'Create', 'Write', 'ReadWrite')
        try {
            $open.Write([byte[]]@(0x68, 0x69), 0, 2)
            $open.Flush()
            "$((Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md')).Code)" | Should BeExactly '0'
        }
        finally { $open.Dispose() }
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'u.md'), 'hi', [Text.UnicodeEncoding]::new($false, $true))
        "$((Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 'u.md')).Reply.check)" | Should BeExactly 'usage'
        # An empty brief is a brief.
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'e.md'), '')
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-NoFork', '-TaskFile', 't.md', '-BriefFile', 'e.md')
        "$($run.Code) $([IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'brief.md')).Length)" | Should BeExactly '0 0'
    }

    It 'A2 the spawn lock is per server and session' {
        $fake = New-T16Fake
        $key = (Get-T16Hash "$($fake.Pipe)|900|9000").Substring(0, 16)
        $lock = 'Local\MuxAgents-' + (Get-T16Hash "spawn|$key|`$3").Substring(0, 32)
        $held = [Collections.Generic.List[string]]::new()
        $probe = {
            param($Arguments)
            $job = Start-ThreadJob -ArgumentList $lock -ScriptBlock {
                param($Name)
                $mutex = [Threading.Mutex]::new($false, $Name)
                try { $free = $mutex.WaitOne(0); if ($free) { $mutex.ReleaseMutex() }; $free } finally { $mutex.Dispose() }
            }
            try { $held.Add("$($Arguments[1]) free $(Receive-Job $job -Wait)") } finally { Remove-Job $job -Force }
            $null
        }.GetNewClosure()
        Add-FakeRmuxRule $fake '^-N list-panes ' -Script $probe
        Add-FakeRmuxRule $fake '^-N set-option ' -Script $probe
        (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Code | Should Be 0
        ($held -join ', ') | Should BeExactly 'list-panes free False, set-option free False'
    }

    It 'A2 a failure before the launch removes the agent folder and the Copilot copy' {
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), 'brief')
        # An It-local stand-in fails the spec.json write, after brief.md is written and before the launch.
        $writeJson = ${function:Write-MuxAgentJson}
        $seen = [Collections.Generic.List[string]]::new()
        function Write-MuxAgentJson {
            param($Path, $Value)
            if ([IO.Path]::GetFileName($Path) -eq 'spec.json') { $seen.Add("brief $([IO.File]::Exists((Join-Path ([IO.Path]::GetDirectoryName($Path)) 'brief.md')))"); throw 'stand-in failure' }
            & $writeJson -Path $Path -Value $Value
        }
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md', '-BriefFile', 'b.md')
        "$($run.Code) $($run.Reply.check) $($seen -join ',')" | Should BeExactly '1 internal brief True'
        Get-T16Calls $fake 'new-window' | Should BeExactly ''
        Get-T16Agents $fake | Should BeExactly ''
        @([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count | Should Be 1
    }

    It 'A2 command line length follows .NET argument quoting' {
        $script = Join-Path $TestDrive 't16 echo line.ps1'
        [IO.File]::WriteAllText($script, '[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); [Environment]::CommandLine')
        $arguments = [string[]]@('-NoProfile', '-NonInteractive', '-File', $script, '', 'plain', 'two words', "tab`there", 'a"b', '"', '\"', '\\"', 'trail\', 'a b\', 'a b\\',
            'run\\\"x', '\\a\\', ('ideo' + [char]0x3000 + 'space'), ([string][char]0x4E2D + [char]0x6587 + ' ' + [char]0x4E2D))
        $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
        foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
        $start.RedirectStandardOutput = $true
        $start.UseShellExecute = $false
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $process = [Diagnostics.Process]::Start($start)
        try { $line = $process.StandardOutput.ReadToEnd().TrimEnd("`r`n"); $process.WaitForExit() } finally { $process.Dispose() }
        # The child's command line starts with its quoted host path; the rest is .NET's quoting of the arguments.
        $tail = $line.Substring($line.IndexOf('" ', 1) + 2)
        $tail | Should Match '^-NoProfile -NonInteractive -File "'
        Get-MuxAgentCommandLineLength -Arguments $arguments | Should Be $tail.Length
        foreach ($case in @(@('', 2), @('x', 1), @('"', 4), @('\"', 6), @('\\"', 8), @('a\b', 3), @('a b\', 7), @('a b\\', 9))) {
            "$($case[0]) $(Get-MuxAgentCommandLineLength -Arguments @($case[0]))" | Should BeExactly "$($case[0]) $($case[1])"
        }
        Get-MuxAgentCommandLineLength -Arguments @('a', 'b') | Should Be 3
    }

    It 'A2 a task too long for the client command line is refused before anything is left' {
        $flags = @{ claude = @('--dangerously-skip-permissions', '--effort', 'high'); copilot = @('--no-sandbox', '--allow-all') }
        foreach ($client in @('claude', 'copilot')) {
            # The full client command line: a 1024-character reserve for the executable, the wrapper flags, the client args.
            $fake = New-T16Fake
            $run = Invoke-T16Spawn $fake @('-Client', $client, '-Task', 'x')
            $spec = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'spec.json'))
            $base = 1024 + (Get-MuxAgentCommandLineLength -Arguments ([string[]]($flags[$client] + @($spec.args))))
            # Each '"' in the prompt costs 2; the task 'x' it replaces cost 1. Exactly 32767 fits; one more refuses.
            $fit = ('"' * [math]::Floor((32768 - $base) / 2)) + ('x' * ((32768 - $base) % 2))
            foreach ($case in @(@($fit, 0), @(($fit + 'x'), 2))) {
                $fake = New-T16Fake
                [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), $case[0])
                $run = Invoke-T16Spawn $fake @('-Client', $client, '-TaskFile', 't.md')
                "$client $($case[0].Length) $($run.Code) $($run.Reply.check)" | Should BeExactly "$client $($case[0].Length) $($case[1]) $(if ($case[1]) { 'usage' })"
            }
            $case[0].Length | Should BeLessThan 16385
            $run.Reply.message | Should BeExactly 'task is too long for the client command line'
            (@($fake.Calls | ForEach-Object { $_[1] }) -join ',') | Should BeExactly 'display-message,list-panes'
            Get-T16Agents $fake | Should BeExactly ''
        }
        # The review's repros: 16384 '"', and 8192 '\"' pairs, which fit in bytes; a long plain task still fits.
        foreach ($case in @(@(('"' * 16384), 'usage'), @(('\"' * 8192), 'usage'), @(('x' * 16000), ''))) {
            $fake = New-T16Fake
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), $case[0])
            $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md')
            "$($case[0].Substring(0, 2)) $($run.Reply.check) $(Get-T16Calls $fake 'new-window' | ForEach-Object { $_.Length -gt 0 })" | Should BeExactly "$($case[0].Substring(0, 2)) $($case[1]) $(-not $case[1])"
        }
        # A Copilot fork refused for length keeps no copy and no agent folder.
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), ('"' * 16384))
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.check) $(Get-T16Agents $fake)" | Should BeExactly '2 usage '
        @([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count | Should Be 1
    }

    It 'A2 a failing cleanup step still ends in 1 launch' {
        # abort: abort.json is blocked by a folder; kill: kill-pane throws; both: both, so kill-pane runs after a failed
        # abort write; copy: a file in the Copilot copy is held open, so deleting the copy fails.
        foreach ($failure in @('abort', 'kill', 'both', 'copy')) {
            $fake = New-T16Fake -Caller 'copilot'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
            $agents = Join-Path $fake.State 'agents'
            $sessions = Join-Path $fake.Home '.copilot\session-state'
            $source = [IO.Path]::GetDirectoryName($fake.Fixtures.CopilotEvents)
            $block = $failure -in @('abort', 'both')
            $held = [Collections.Generic.List[IDisposable]]::new()
            $hold = $failure -eq 'copy'
            Add-FakeRmuxRule $fake '^-N set-option ' -ExitCode 1 -Script ({
                    param($Arguments)
                    if ($block) { $null = [IO.Directory]::CreateDirectory((Join-Path ([IO.Directory]::GetDirectories($agents)[0]) 'abort.json')) }
                    if ($hold) {
                        $copy = @([IO.Directory]::GetDirectories($sessions) | Where-Object { $_ -ne $source })[0]
                        $held.Add([IO.File]::Open((Join-Path $copy 'events.jsonl'), 'Open', 'Read', 'Read'))
                    }
                    $null
                }.GetNewClosure())
            if ($failure -in @('kill', 'both')) { Add-FakeRmuxRule $fake '^-N kill-pane ' -Script { param($Arguments) throw [TimeoutException]::new('stand-in timeout') } }
            try { $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md') } finally { foreach ($stream in $held) { $stream.Dispose() } }
            "$failure $($run.Code) $($run.Reply.check)" | Should BeExactly "$failure 1 launch"
            "$failure $(Get-T16Calls $fake 'kill-pane')" | Should BeExactly "$failure [`"-N`",`"kill-pane`",`"-t`",`"%9`"]"
            "$failure $(@([IO.Directory]::GetFileSystemEntries($sessions)).Count)" | Should BeExactly "$failure $(if ($hold) { 2 } else { 1 })"
            $folder = [IO.Directory]::GetDirectories($agents)[0]
            "$failure $([IO.File]::Exists((Join-Path $folder 'commit.json'))) $([IO.Directory]::Exists((Join-Path $folder 'abort.json')))" | Should BeExactly "$failure False $block"
        }
    }

    It 'A2 a failure between record and commit still aborts' {
        $fake = New-T16Fake
        $count = [ref]0
        # The second clock read is commit.json's committedAt.
        $fake.Now = { $count.Value++; if ($count.Value -eq 2) { throw 'stand-in clock failure' }; $fake.Clock }.GetNewClosure()
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '1 launch'
        $folder = [IO.Path]::Combine($fake.State, 'agents', (Get-T16Agents $fake))
        (@('record.json', 'commit.json', 'abort.json') | ForEach-Object { "$_ $([IO.File]::Exists((Join-Path $folder $_)))" }) -join ', ' | Should BeExactly 'record.json True, commit.json False, abort.json True'
        Get-T16Calls $fake 'kill-pane' | Should BeExactly '["-N","kill-pane","-t","%9"]'
    }

    It 'A2 the state folder and server.json are written under the spawn lock' {
        $fake = New-T16Fake
        $lock = 'Local\MuxAgents-' + (Get-T16Hash ('spawn|' + (Get-T16Hash "$($fake.Pipe)|900|9000").Substring(0, 16) + '|$3')).Substring(0, 32)
        $log = [Collections.Generic.List[string]]::new()
        $free = {
            $job = Start-ThreadJob -ArgumentList $lock -ScriptBlock {
                param($Name)
                $mutex = [Threading.Mutex]::new($false, $Name)
                try { $taken = $mutex.WaitOne(0); if ($taken) { $mutex.ReleaseMutex() }; $taken } finally { $mutex.Dispose() }
            }
            try { Receive-Job $job -Wait } finally { Remove-Job $job -Force }
        }
        $writeJson = ${function:Write-MuxAgentJson}
        $userSid = ${function:Get-MuxAgentUserSid}
        # It-local stand-ins log whether another thread could take the spawn lock, then run the library's own functions.
        function Get-MuxAgentUserSid { $log.Add("state free $(& $free)"); & $userSid }
        function Write-MuxAgentJson { param($Path, $Value) $log.Add("$([IO.Path]::GetFileName($Path)) free $(& $free)"); & $writeJson -Path $Path -Value $Value }
        (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Code | Should Be 0
        ($log -join ', ') | Should BeExactly 'state free False, server.json free False, state free False, spec.json free False, record.json free False, commit.json free False'
    }

    It 'A2 parentAgent holds a Copilot caller session and the exact start' {
        $fake = New-T16Fake -Caller 'copilot'
        # The client runs as PID 157, not the fake default 150. Open keeps 100 ns; the CIM row (1570) keeps whole microseconds.
        $row = @($fake.Rows | Where-Object Pid -eq 150)[0]
        $row.Pid = 157
        $row.Start = 1570
        @($fake.Rows | Where-Object Pid -eq 170)[0].ParentPid = 157
        $fake.OpenResults[157] = [pscustomobject]@{ Id = 157; Start = 1577; Process = $null }
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $reply = (Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md', '-BriefFile', 't.md')).Reply
        $record = [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $reply.id, 'record.json'))
        $parent = ConvertTo-Json -Compress -InputObject ([ordered]@{ client = 'copilot'; pid = 157; start = 1577; sessionId = '66666666-7777-4888-9999-aaaaaaaaaaaa' })
        $record | Should Match ([regex]::Escape('"parentAgent":' + $parent + ','))
    }

    It 'A2 each spawn gets a fresh session id' {
        $fake = New-T16Fake
        $first = (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Reply.sessionId
        $second = (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Reply.sessionId
        foreach ($id in @($first, $second)) { $id | Should MatchExactly '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' }
        $first | Should Not Be $second
    }

    It 'A2 brief.md is written for a fork too' {
        $fake = New-T16Fake -Caller 'claude'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), 'fork brief')
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md', '-BriefFile', 'b.md')
        "$($run.Code) $($run.Reply.context)" | Should BeExactly '0 fork'
        [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'brief.md')) | Should BeExactly 'fork brief'
    }

    It 'A5 the header and prompt are built before the agent folder exists' {
        $log = [Collections.Generic.List[string]]::new()
        $header = ${function:New-MuxAgentHeader}
        $join = ${function:Join-MuxAgentPrompt}
        $fake = New-T16Fake
        # It-local stand-ins log the agent folders present when each runs, then run the library's own functions.
        function New-MuxAgentHeader { param($P, $Caller, $Name, $Id, $Client, $Plan, $BriefPath, $TranscriptPath) $log.Add("header [$(Get-T16Agents $fake)]"); & $header @PSBoundParameters }
        function Join-MuxAgentPrompt { param($Header, $Task) $log.Add("prompt [$(Get-T16Agents $fake)]"); & $join @PSBoundParameters }
        $length = ${function:Get-MuxAgentCommandLineLength}
        function Get-MuxAgentCommandLineLength { param($Arguments) $log.Add("length [$(Get-T16Agents $fake)]"); & $length @PSBoundParameters }
        (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Code | Should Be 0
        ($log -join ', ') | Should BeExactly 'header [], prompt [], length []'
    }

    It 'A2 parentAgent sessionId is checked with the shared session id pattern' {
        $shared = $script:MuxAgentSessionIdPattern
        try {
            $script:MuxAgentSessionIdPattern = '^not-a-guid\z'
            foreach ($session in @('not-a-guid', '11111111-2222-4333-8444-555555555555')) {
                $fake = New-T16Fake -Caller 'claude'
                $fake.Env['CLAUDE_CODE_SESSION_ID'] = $session
                [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
                $reply = (Invoke-T16Spawn $fake @('-Client', 'claude', '-NoFork', '-TaskFile', 't.md', '-BriefFile', 't.md')).Reply
                $record = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $reply.id, 'record.json'))
                "$session [$($record.parentAgent.sessionId)]" | Should BeExactly "$session [$(if ($session -eq 'not-a-guid') { $session })]"
            }
        }
        finally { $script:MuxAgentSessionIdPattern = $shared }
    }

    It 'A2 the task keeps its line breaks in the prompt' {
        $fake = New-T16Fake
        $task = "one`r`ntwo`n`nthree"
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), $task)
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-TaskFile', 't.md')
        $spec = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'spec.json'))
        $spec.prompt.EndsWith("`n`n" + $task) | Should Be $true
        @($spec.args)[-1] | Should BeExactly $spec.prompt
    }

    It 'A2 seven listed live agents are refused too' {
        $fake = New-T16Fake
        Set-T16Panes $fake 7
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.check) $(Get-T16Calls $fake 'new-window')" | Should BeExactly '2 cap '
    }

    It 'A2 server.json holds the resolved server pid and start' {
        $fake = New-T16Fake
        foreach ($name in @('RMUX', 'TMUX')) { $fake.Env[$name] = "$($fake.Pipe),901,0" }
        Add-FakeRmuxRule $fake '^-N display-message ' -StdOut "901|100|`$3|@1|`n"
        $row = New-FakeMuxAgentRow 901 4 'C:\fake\rmux.exe' 'rmux.exe'
        $row.Start = 9017
        $fake.Rows.Add($row)
        (Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')).Code | Should Be 0
        $state = Join-Path $fake.StateRoot (Get-T16Hash "$($fake.Pipe)|901|9017").Substring(0, 16)
        [IO.File]::ReadAllText((Join-Path $state 'server.json')) | Should BeExactly (ConvertTo-Json -Compress -InputObject ([ordered]@{ socket = $fake.Pipe; serverPid = 901; serverStart = 9017 }))
    }

    It 'A2 a commit.json that cannot be written aborts after tag and record' {
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $agents = Join-Path $fake.State 'agents'
        Add-FakeRmuxRule $fake '^-N set-option ' -Script ({
                param($Arguments)
                $null = [IO.Directory]::CreateDirectory((Join-Path ([IO.Directory]::GetDirectories($agents)[0]) 'commit.json'))
                $null
            }.GetNewClosure())
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '1 launch'
        $folder = [IO.Directory]::GetDirectories($agents)[0]
        "$([IO.File]::Exists((Join-Path $folder 'record.json'))) $([IO.File]::Exists((Join-Path $folder 'abort.json'))) $([IO.File]::Exists((Join-Path $folder 'commit.json')))" | Should BeExactly 'True True False'
        Get-T16Calls $fake 'kill-pane' | Should BeExactly '["-N","kill-pane","-t","%9"]'
        @([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))).Count | Should Be 1
    }

    It 'A2 a record.json that cannot be written aborts after the tag' {
        $fake = New-T16Fake -Caller 'copilot'
        [IO.File]::WriteAllText((Join-Path $fake.Cwd 't.md'), 'task')
        $agents = Join-Path $fake.State 'agents'
        # The tag succeeds and leaves a folder named record.json, so the record write fails.
        Add-FakeRmuxRule $fake '^-N set-option ' -Script ({
                param($Arguments)
                $null = [IO.Directory]::CreateDirectory((Join-Path ([IO.Directory]::GetDirectories($agents)[0]) 'record.json'))
                $null
            }.GetNewClosure())
        $run = Invoke-T16Spawn $fake @('-Client', 'copilot', '-TaskFile', 't.md')
        "$($run.Code) $($run.Reply.check)" | Should BeExactly '1 launch'
        $folder = [IO.Directory]::GetDirectories($agents)[0]
        "$([IO.File]::Exists((Join-Path $folder 'abort.json'))) $([IO.File]::Exists((Join-Path $folder 'commit.json')))" | Should BeExactly 'True False'
        Get-T16Calls $fake 'kill-pane' | Should BeExactly '["-N","kill-pane","-t","%9"]'
        (@([IO.Directory]::GetFileSystemEntries((Join-Path $fake.Home '.copilot\session-state'))) -join ',') | Should BeExactly ([IO.Path]::GetDirectoryName($fake.Fixtures.CopilotEvents))
    }

    It 'A2 the pane and window come from the new-window answer' {
        $fake = New-T16Fake
        Add-FakeRmuxRule $fake '^-N new-window ' -StdOut "%23|@17`n"
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.pane) $($run.Reply.window)" | Should BeExactly '0 %23 @17'
        $tag = @($fake.Calls | Where-Object { $_[1] -eq 'set-option' })[0]
        (@(for ($index = 0; $index -lt $tag.Count; $index++) { if ($tag[$index] -eq '-t') { $tag[$index + 1] } }) -join ',') | Should BeExactly '%23,%23,%23,%23'
        $record = Read-MuxAgentJson -Path ([IO.Path]::Combine($fake.State, 'agents', $run.Reply.id, 'record.json'))
        "$($record.pane) $($record.window)" | Should BeExactly '%23 @17'
        $fake = New-T16Fake
        Add-FakeRmuxRule $fake '^-N new-window ' -StdOut "%23|@17`n"
        Add-FakeRmuxRule $fake '^-N set-option ' -ExitCode 1
        $run = Invoke-T16Spawn $fake @('-Client', 'claude', '-Task', 'hi')
        "$($run.Code) $($run.Reply.check) $(Get-T16Calls $fake 'kill-pane')" | Should BeExactly '1 launch ["-N","kill-pane","-t","%23"]'
        [IO.File]::ReadAllText([IO.Path]::Combine($fake.State, 'agents', (Get-T16Agents $fake), 'abort.json')) | Should Match '"pane":"%23"'
    }
}
