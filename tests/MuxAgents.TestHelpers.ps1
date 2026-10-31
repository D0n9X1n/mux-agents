# Shared mux-agent test fakes: a providers object with the production members plus mutable
# Rows, Env, Fixtures, Rules, Calls, Clock, Killed, Survivors, OpenResults and TokenInfo.
$script:FakeMuxAgentsDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts'

function Add-FakeRmuxRule {
    # Inserts a rule first. Match is a regex against the space-joined argv (it starts with -N);
    # Script receives the argv and may return the whole result, else the static fields answer.
    param([Parameter(Mandatory)]$Fake, [Parameter(Mandatory)][string]$Match, [string]$StdOut = '', [int]$ExitCode = 0, [string]$StdErr = '', [scriptblock]$Script)
    $Fake.Rules.Insert(0, [pscustomobject]@{ Match = $Match; StdOut = $StdOut; ExitCode = $ExitCode; StdErr = $StdErr; Script = $Script })
}

function New-FakeMuxAgentRow {
    param([int]$ProcessId, [int]$ParentId, [string]$Path, [string]$CommandLine)
    [pscustomobject]@{ Pid = $ProcessId; ParentPid = $ParentId; Start = [long]$ProcessId * 10; Path = $Path; CommandLine = $CommandLine }
}

function Write-FakeMuxAgentFile {
    param([string]$Path, [string[]]$Lines)
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))
    [IO.File]::WriteAllText($Path, (($Lines | ForEach-Object { $_ + "`n" }) -join ''))
}

function New-FakeMuxAgentProviders {
    # -Native: the running platform's members, paths and endpoint (Unix: TMUX socket, basenames, Unix state folders and
    # lock names); on Windows it is the default fake. Without it the fake is the Windows one on every OS.
    param([ValidateSet('human', 'claude', 'copilot')][string]$Caller = 'human', [string]$Root = $TestDrive, [switch]$Native)
    $homeDirectory = Join-Path $Root 'fake home'
    $work = Join-Path $Root 'fake work'
    foreach ($directory in @($homeDirectory, $work)) { $null = [IO.Directory]::CreateDirectory($directory) }
    $unix = $Native -and -not $IsWindows
    $pwsh = if ($unix) { [IO.Path]::Combine($PSHOME, 'pwsh') } else { Join-Path $PSHOME 'pwsh.exe' }
    $environment = [hashtable]::new([StringComparer]::OrdinalIgnoreCase)
    if ($unix) { $environment['TMUX'] = '/tmp/tmux-test/default,900,0' } else { foreach ($name in @('RMUX', 'TMUX')) { $environment[$name] = '\\.\pipe\rmux-test,900,0' } }
    foreach ($name in @('RMUX_PANE', 'TMUX_PANE')) { $environment[$name] = '%1' }

    $rows = [Collections.Generic.List[object]]::new()
    $rows.Add((New-FakeMuxAgentRow 900 4 (Join-Path $homeDirectory 'AppData\Local\rmux\rmux.exe') 'rmux.exe'))
    $rows.Add((New-FakeMuxAgentRow 100 900 $pwsh ('"' + $pwsh + '" -NoLogo')))
    $callerParent = 100
    if ($Caller -eq 'claude') {
        $claude = Join-Path $homeDirectory '.local\bin\claude.exe'
        $rows.Add((New-FakeMuxAgentRow 150 100 $claude ('"' + $claude + '" --dangerously-skip-permissions')))
        $rows.Add((New-FakeMuxAgentRow 170 150 'C:\Program Files\Git\bin\..\usr\bin\bash.exe' '/usr/bin/bash -c "pwsh -NoProfile -File mux-agent.ps1"'))
        $callerParent = 170
    }
    if ($Caller -eq 'copilot') {
        $copilot = Join-Path $homeDirectory 'AppData\Local\Microsoft\WinGet\Links\copilot.exe'
        $rows.Add((New-FakeMuxAgentRow 140 100 'C:\Program Files\Microsoft VS Code\bin\copilot-shim\copilot.exe' 'copilot --allow-all'))
        $rows.Add((New-FakeMuxAgentRow 150 140 $copilot ('"' + $copilot + '" --allow-all')))
        $rows.Add((New-FakeMuxAgentRow 170 150 $pwsh ('"' + $pwsh + '" -NoProfile -Command pwsh -NoProfile -File mux-agent.ps1')))
        $callerParent = 170
    }
    $rows.Add((New-FakeMuxAgentRow 200 $callerParent $pwsh ('"' + $pwsh + '" -NoProfile -File mux-agent.ps1')))
    # Unix rows carry the image basename without .exe under /x (tmux, pwsh, bash, claude, copilot).
    if ($unix) { foreach ($row in $rows) { $row.Path = '/x/' + [IO.Path]::GetFileNameWithoutExtension(([string]$row.Path).Replace('\', '/')) } }

    # The Claude mode row is the last non-blank screen row, under the statusline rows.
    $screen = @('> build the dispatcher', '', 'Done.', '', ('-' * 40), '> ', ('-' * 40), '   Time 12:00:00 | Run 2m',
        '   Model Opus 5.5 | Effort high', '   ~\fake work', '   Repo clean | Branch master',
        ('  ' + [char]0x23F5 + [char]0x23F5 + ' bypass permissions on (shift+tab to cycle)'), '', '') -join "`n"
    $rules = [Collections.Generic.List[object]]::new()
    foreach ($rule in @(
            @('^-N display-message ', ('900|100|$3|@1|' + "`n")),
            @('^-N list-panes( |$)', ''),
            @('^-N (new-window|split-window) ', ('%9|@5' + "`n")),
            @('^-N capture-pane ', $screen))) {
        $rules.Add([pscustomobject]@{ Match = $rule[0]; StdOut = $rule[1]; ExitCode = 0; StdErr = ''; Script = $null })
    }

    $fake = [pscustomobject]@{
        Env = $environment; Pid = 200; Cwd = $work; Home = $homeDirectory; StateRoot = (Join-Path $Root 'fake state')
        Entry = (Join-Path $script:FakeMuxAgentsDirectory 'mux-agent.ps1'); Launcher = (Join-Path $script:FakeMuxAgentsDirectory 'mux-agent-launcher.ps1')
        Pwsh = $pwsh; Processes = $null; Open = $null; Kill = $null; Rmux = $null; RmuxPath = $null; Token = $null; Now = $null; Sleep = $null; Admitted = $true; Advisory = $null
        Rows = $rows; Rules = $rules; Calls = [Collections.Generic.List[object]]::new(); Clock = [DateTime]::new(2026, 10, 29, 12, 0, 0, [DateTimeKind]::Utc)
        Killed = [Collections.Generic.List[int]]::new(); Survivors = [Collections.Generic.List[int]]::new(); OpenResults = @{}; Fixtures = [ordered]@{}
        TokenInfo = [pscustomobject]@{ IsAppContainer = $false; IsRestricted = $false; IntegrityLevel = 'S-1-16-8192' }
    }
    $fake.Processes = { $fake.Rows }.GetNewClosure()
    $fake.Token = { $fake.TokenInfo }.GetNewClosure()
    $fake.Now = { $fake.Clock }.GetNewClosure()
    $fake.Sleep = { param([int]$Milliseconds) $fake.Clock = $fake.Clock.AddMilliseconds($Milliseconds) }.GetNewClosure()
    $fake.Open = {
        param([int]$Id)
        if ($fake.OpenResults.ContainsKey($Id)) { return $fake.OpenResults[$Id] }
        foreach ($row in $fake.Rows) { if ($row.Pid -eq $Id) { return [pscustomobject]@{ Id = $Id; Start = $row.Start; Process = $null } } }
        $null
    }.GetNewClosure()
    $fake.Kill = { param($Opened) $fake.Killed.Add([int]$Opened.Id); -not $fake.Survivors.Contains([int]$Opened.Id) }.GetNewClosure()
    $fake.Rmux = {
        param([string[]]$Arguments)
        $fake.Calls.Add([string[]]@($Arguments))
        $line = $Arguments -join ' '
        foreach ($rule in $fake.Rules) {
            if ($line -notmatch $rule.Match) { continue }
            if ($rule.Script) { $scripted = & $rule.Script $Arguments; if ($null -ne $scripted) { return $scripted } }
            return [pscustomobject]@{ ExitCode = $rule.ExitCode; StdOut = $rule.StdOut; StdErr = $rule.StdErr }
        }
        [pscustomobject]@{ ExitCode = 0; StdOut = ''; StdErr = '' }
    }.GetNewClosure()
    # The production OS members the fake does not replace; a test swaps one for a spy by assigning it.
    $os = Get-MuxAgentPlatformMembers -Platform $(if ($unix) { Get-MuxAgentPlatform } else { 'windows' })
    foreach ($key in $os.Keys) { if (-not $fake.PSObject.Properties[$key]) { Add-Member -InputObject $fake -NotePropertyName $key -NotePropertyValue $os[$key] } }
    if ($unix) {
        # B3's walk needs the canonical root; locks use the platform name, as New-MuxAgentProviders installs it.
        $fake.StateRoot = Resolve-MuxAgentUnixRealPath -Path $fake.StateRoot
        $private = ${function:New-MuxAgentUnixPrivateDirectories}
        $fake.PrivateDirectories = { param([string[]]$Paths) & $private -Root $fake.StateRoot -Paths $Paths }.GetNewClosure()
        $script:MuxAgentLockName = $os.LockName
    }
    Add-FakeMuxAgentFixtures -Fake $fake -Caller $Caller -Work $work -Unix:$unix
    $fake
}

function Add-FakeMuxAgentFixtures {
    # The calling client's session files, ending in complete records; tests that need partial
    # or invalid lines append them to the paths in $Fake.Fixtures.
    param([Parameter(Mandatory)]$Fake, [string]$Caller, [string]$Work, [switch]$Unix)
    if ($Caller -eq 'claude') {
        $session = '11111111-2222-4333-8444-555555555555'
        foreach ($pair in @(@('CLAUDECODE', '1'), @('CLAUDE_CODE_ENTRYPOINT', 'cli'), @('CLAUDE_CODE_SESSION_ID', $session), @('CLAUDE_PID', '150'))) { $Fake.Env[$pair[0]] = $pair[1] }
        $Fake.Fixtures.ClaudeTranscript = if ($Unix) { [IO.Path]::Combine($Fake.Home, '.claude', 'projects', 'fake-work', "$session.jsonl") } else { Join-Path $Fake.Home ".claude\projects\fake-work\$session.jsonl" }
        $Fake.Fixtures.ClaudeSession = if ($Unix) { [IO.Path]::Combine($Fake.Home, '.claude', 'sessions', '150.json') } else { Join-Path $Fake.Home '.claude\sessions\150.json' }
        Write-FakeMuxAgentFile $Fake.Fixtures.ClaudeTranscript @(
            (ConvertTo-Json -Compress -Depth 5 ([ordered]@{ type = 'permission-mode'; permissionMode = 'bypassPermissions'; sessionId = $session })),
            (ConvertTo-Json -Compress -Depth 5 ([ordered]@{ type = 'user'; sessionId = $session; message = [ordered]@{ role = 'user'; content = 'build the dispatcher' } })))
        Write-FakeMuxAgentFile $Fake.Fixtures.ClaudeSession @(ConvertTo-Json -Compress ([ordered]@{ pid = 150; sessionId = $session; cwd = $Work; procStart = '1500' }))
    }
    if ($Caller -eq 'copilot') {
        $session = '66666666-7777-4888-9999-aaaaaaaaaaaa'
        foreach ($pair in @(@('COPILOT_CLI', '1'), @('COPILOT_AGENT_SESSION_ID', $session))) { $Fake.Env[$pair[0]] = $pair[1] }
        $folder = if ($Unix) { [IO.Path]::Combine($Fake.Home, '.copilot', 'session-state', $session) } else { Join-Path $Fake.Home ".copilot\session-state\$session" }
        $Fake.Fixtures.CopilotWorkspace = Join-Path $folder 'workspace.yaml'
        $Fake.Fixtures.CopilotEvents = Join-Path $folder 'events.jsonl'
        $ids = @('e0000000-0000-4000-8000-000000000001', 'e0000000-0000-4000-8000-000000000002', 'e0000000-0000-4000-8000-000000000003')
        Write-FakeMuxAgentFile $Fake.Fixtures.CopilotWorkspace @("id: $session", "cwd: $Work", 'client_name: fake-cli', 'name: fake session',
            'user_named: false', 'summary_count: 0', 'fork_count: 0', 'created_at: 2026-10-29T11:59:00.000Z', 'updated_at: 2026-10-29T12:00:00.000Z',
            'remote_steerable: false', 'mc_task_id: f0000000-0000-4000-8000-000000000001', 'mc_session_id: f0000000-0000-4000-8000-000000000002', "mc_last_event_id: $($ids[2])")
        # session.permissions_changed is event 2, as Copilot CLI logs it.
        $events = @(
            [ordered]@{ type = 'session.start'; data = [ordered]@{ sessionId = $session; startTime = '2026-10-29T11:59:00.000Z' }; id = $ids[0]; timestamp = '2026-10-29T11:59:00.000Z'; parentId = $null },
            [ordered]@{ type = 'session.permissions_changed'; data = [ordered]@{ previousAllowAllPermissions = $false; allowAllPermissions = $true; previousAllowAllPermissionMode = 'off'; allowAllPermissionMode = 'on' }; id = $ids[1]; timestamp = '2026-10-29T11:59:01.000Z'; parentId = $ids[0] },
            [ordered]@{ type = 'session.info'; data = [ordered]@{ infoType = 'status'; message = 'ready' }; id = $ids[2]; timestamp = '2026-10-29T11:59:02.000Z'; parentId = $ids[1] })
        Write-FakeMuxAgentFile $Fake.Fixtures.CopilotEvents @($events | ForEach-Object { ConvertTo-Json -Compress -Depth 5 $_ })
    }
}
