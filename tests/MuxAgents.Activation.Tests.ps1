# Activation tests (B4). Untagged: every OS. Natives are mocked; the integration suite runs them for real.
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')
function Get-ActivationRefusal {
    param([Parameter(Mandatory)][scriptblock]$Action)
    try { $null = & $Action; 'none' } catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" } }
function New-ActivationMuxFake {
    # A fake whose Endpoint parses a Unix TMUX; pid 900 is the server row.
    param([string]$ServerPath = '/opt/tmux-3.4/bin/tmux', [string]$Tmux = '/tmp/tmux-1000/default,900,0')
    $fake = New-FakeMuxAgentProviders
    $fake.Env['TMUX'] = $Tmux; $fake.Env.Remove('RMUX')
    $fake.Endpoint = { param($Environment) if ("$($Environment['TMUX'])" -match '^(/[^,\r\n]+),(\d+),(-?\d+)$') { [pscustomobject]@{ Socket = $Matches[1]; ServerPid = $Matches[2] } } }
    $fake.Rows[0].Path = $ServerPath
    $fake }
Describe 'MuxAgents activation tmux' {
    Context 'image and version' {
        $global:activationCalls = [Collections.Generic.List[string]]::new()
        $global:activationVersion = [pscustomobject]@{ ExitCode = 0; StdOut = "tmux 3.4`n"; StdErr = '' }
        Mock Find-MuxAgentApplication { param($Name) $global:activationCalls.Add("path:$Name"); '/usr/bin/tmux' }
        Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments, $TimeoutMilliseconds) $global:activationCalls.Add("$FilePath $($Arguments -join ' ')"); if ($global:activationVersion -is [scriptblock]) { & $global:activationVersion } else { $global:activationVersion } }
        It 'takes tmux from the TMUX server pid, else from PATH, and asks it -V' {
            $answers = foreach ($case in @(@('/opt/tmux-3.4/bin/tmux', '/tmp/s,900,0'), @('/usr/bin/bash', '/tmp/s,900,0'), @('/opt/tmux-3.4/bin/tmux', '/tmp/s,901,0'), @('/opt/tmux-3.4/bin/tmux', 'relative/s,900,0'))) {
                $global:activationCalls.Clear()
                $image = Resolve-MuxAgentUnixMux -P (New-ActivationMuxFake -ServerPath $case[0] -Tmux $case[1])
                "$image<$($global:activationCalls -join ',')>" }
            ($answers -join ';') | Should BeExactly '/opt/tmux-3.4/bin/tmux</opt/tmux-3.4/bin/tmux -V>;/usr/bin/tmux<path:tmux,/usr/bin/tmux -V>;/usr/bin/tmux<path:tmux,/usr/bin/tmux -V>;/usr/bin/tmux<path:tmux,/usr/bin/tmux -V>' }
        It 'accepts tmux 3.2 and later and refuses endpoint for anything older or unreadable' {
            $answers = foreach ($case in @(@(0, "tmux 3.2`n"), @(0, 'tmux 3.10'), @(0, 'tmux next-3.6'), @(0, 'tmux 4.0'), @(0, 'tmux 3.1c'), @(0, 'tmux 2.9a'), @(0, 'tmux master'), @(0, ''), @(1, 'tmux 3.4'))) {
                $global:activationVersion = [pscustomobject]@{ ExitCode = $case[0]; StdOut = $case[1]; StdErr = '' }
                Get-ActivationRefusal { Resolve-MuxAgentUnixMux -P (New-ActivationMuxFake) } }
            $old = '1 endpoint tmux 3.2 or later is required'
            ($answers -join ';') | Should BeExactly ((@('none', 'none', 'none', 'none') + @(1..5 | ForEach-Object { $old })) -join ';')
            $global:activationVersion = { throw [TimeoutException]::new('slow') }
            Get-ActivationRefusal { Resolve-MuxAgentUnixMux -P (New-ActivationMuxFake) } | Should BeExactly $old } }
    Context 'no tmux' {
        Mock Find-MuxAgentApplication { $null }
        Mock Invoke-MuxAgentProcess { throw 'must not run' }
        It 'refuses endpoint when neither the server nor PATH has tmux, without running anything' {
            Get-ActivationRefusal { Resolve-MuxAgentUnixMux -P (New-ActivationMuxFake -ServerPath '/usr/bin/bash') } | Should BeExactly '1 endpoint tmux was not found' } } }
Describe 'MuxAgents activation unix members' {
    It 'linux and macos carry the windows keys; ClientImage, Pwsh and Token are the Unix ones' {
        $keys = (@((Get-MuxAgentPlatformMembers -Platform windows).Keys | Sort-Object) -join ',')
        $answers = foreach ($name in 'linux', 'macos') {
            $m = Get-MuxAgentPlatformMembers -Platform $name
            "$($m.Platform) $((@($m.Keys | Sort-Object) -join ',') -ceq $keys) $(& $m.ClientImage 'claude') $(& $m.ClientImage 'copilot') $($m.Pwsh -ceq [IO.Path]::Combine($PSHOME, 'pwsh')) $($null -eq (& $m.Token))" }
        ($answers -join ';') | Should BeExactly 'linux True claude copilot True True;macos True claude copilot True True'
    }
    It 'IsUnder compares ordinally below the root plus one slash' {
        $answers = foreach ($name in 'linux', 'macos') {
            $isUnder = (Get-MuxAgentPlatformMembers -Platform $name).IsUnder
            (@(foreach ($pair in @(@('/s/state', '/s/state/k/a/spec.json'), @('/s/state/', '/s/state/k'), @('/s/state', '/S/state/k'), @('/s/state', '/s/statex/k'), @('/s/state', '/s/state'))) { "$(& $isUnder $pair[0] $pair[1])" }) -join ',') }
        ($answers -join ';') | Should BeExactly 'True,True,False,False,False;True,True,False,False,False'
    }
    It 'StopReport reports every row stop left, whatever its Reason, with survivors and survivorPids' {
        $left = @(foreach ($pair in @(@(310, 'open'), @(320, 'start'), @(330, 'kill'))) { [pscustomobject]@{ Pid = $pair[0]; Reason = $pair[1]; Row = [pscustomobject]@{ Pid = $pair[0] } } })
        $answers = foreach ($name in 'linux', 'macos') {
            $report = & (Get-MuxAgentPlatformMembers -Platform $name).StopReport $left; $none = & (Get-MuxAgentPlatformMembers -Platform $name).StopReport @()
            "$(@($report.Keys) -join ',') $($report.survivors) $(@($report.survivorPids) -join ',') $($none.survivors) [$(@($none.survivorPids) -join ',')]" }
        ($answers -join ';') | Should BeExactly 'survivors,survivorPids 3 310,320,330 0 [];survivors,survivorPids 3 310,320,330 0 []'
    }
    Context 'wiring' {
        $global:activationWired = [Collections.Generic.List[string]]::new()
        Mock New-MuxAgentLinuxKernel { 'K-linux' }; Mock New-MuxAgentMacKernel { 'K-macos' }
        Mock Get-MuxAgentLinuxProcessTable { $global:activationWired.Add("Processes linux $Kernel") }; Mock Get-MuxAgentMacProcessTable { $global:activationWired.Add("Processes macos $Kernel") }
        Mock Open-MuxAgentUnixProcess { $global:activationWired.Add("Open $Id $Kernel") }; Mock Stop-MuxAgentUnixProcess { $global:activationWired.Add("Kill $Opened $Kernel") }
        Mock Assert-MuxAgentLinuxSandbox { $global:activationWired.Add("Sandbox linux $($Environment['K']) $Kernel") }; Mock Assert-MuxAgentMacSandbox { $global:activationWired.Add("Sandbox macos $($Environment['K']) $Kernel") }
        Mock Get-MuxAgentUnixEndpoint { $global:activationWired.Add("Endpoint $($Environment['K'])") }; Mock Resolve-MuxAgentUnixMux { $global:activationWired.Add("Mux $P") }
        Mock Get-MuxAgentUnixClient { $global:activationWired.Add("ClientOf $Row $Kernel") }; Mock Test-MuxAgentUnixShell { $global:activationWired.Add("IsShell $Row") }
        Mock Test-MuxAgentUnixSameStart { $global:activationWired.Add("SameStart $Opened $Listed") }; Mock Get-MuxAgentUnixArgv { $global:activationWired.Add("Argv $Row") }
        Mock Get-MuxAgentUnixClaudeDirectory { $global:activationWired.Add("ClaudeDirectory $P $Caller $Kernel") }; Mock Get-MuxAgentUnixStopReport { $global:activationWired.Add("StopReport $(@($Left) -join '/')") }
        Mock Get-MuxAgentUnixStateRoot { $global:activationWired.Add("StateRootOf $($Environment['K'])") }; Mock New-MuxAgentUnixPrivateDirectories { $global:activationWired.Add("PrivateDirectories $Root $($Paths -join '/')") }
        Mock Get-MuxAgentUnixUserId { $global:activationWired.Add('uid'); '501' }; Mock Get-MuxAgentUnixLockName { $global:activationWired.Add("LockName $LockKey $User") }
        It 'each Unix member calls its own function with its arguments and the platform kernel; the uid is read once' {
            $saved = $script:MuxAgentProviders
            try {
                $script:MuxAgentProviders = [pscustomobject]@{ StateRoot = '/root' }
                foreach ($name in 'linux', 'macos') {
                    $global:activationWired.Clear(); $script:MuxAgentUnixUser = $null
                    $m = Get-MuxAgentPlatformMembers -Platform $name
                    $null = & $m.Processes; $null = & $m.Open 7; $null = & $m.Kill 'o'; $null = & $m.Sandbox ([pscustomobject]@{ Env = @{ K = 'e' } }); $null = & $m.Endpoint @{ K = 'e' }
                    $null = & $m.Mux 'p'; $null = & $m.ClientOf 'r1'; $null = & $m.IsShell 'r2'; $null = & $m.SameStart 5 5; $null = & $m.Argv 'r3'; $null = & $m.ClaudeDirectory 'p' 'c'
                    $null = & $m.StopReport @('a', 'b'); $null = & $m.StateRootOf @{ K = 's' }; $null = & $m.PrivateDirectories @('a', 'b'); $null = & $m.LockName 'k'; $null = & $m.LockName 'j'
                    "$name|$($global:activationWired -join ',')" | Should BeExactly "$name|Processes $name K-$name,Open 7 K-$name,Kill o K-$name,Sandbox $name e K-$name,Endpoint e,Mux p,ClientOf r1 K-$name,IsShell r2,SameStart 5 5,Argv r3,ClaudeDirectory p c K-$name,StopReport a/b,StateRootOf s,PrivateDirectories /root a/b,uid,LockName k 501,LockName j 501"
                } }
            finally { $script:MuxAgentProviders = $saved; $script:MuxAgentUnixKernel = $null; $script:MuxAgentUnixUser = $null } }
    }
}
Describe 'MuxAgents activation gone pane' {
    It 'only a clean probe that answers an empty pane id and tag is gone' {
        $answers = foreach ($probe in @(@(0, "|`n"), @(0, '|'), @(1, "|`n"), @(0, "%9|aaaaaaaaaaaa`n"), @(0, "|x`n"), @(0, ''))) { "$(Test-MuxAgentPaneGone ([pscustomobject]@{ ExitCode = $probe[0]; StdOut = $probe[1] }))" }
        "$($answers -join ',') $(Test-MuxAgentPaneGone $null)" | Should BeExactly 'True,True,False,False,False,False False' }
    It 'the notice sweep posts lost only on a gone answer, never on a failed or thrown probe' {
        $answers = foreach ($case in @(@(0, "|`n"), @(1, "|`n"), @(1, "can't find pane: %9`n"), @(0, "|x`n"), @(0, 'throw'))) {
            $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive ([guid]::NewGuid().ToString('N')))
            $fake.PrivateDirectories = { param([string[]]$Paths) foreach ($path in $Paths) { $null = [IO.Directory]::CreateDirectory($path) } }
            $caller = Resolve-MuxAgentCaller -P $fake
            $folder = [IO.Path]::Combine((Get-MuxAgentStateDirectory -P $fake -Caller $caller), 'agents', 'aaaaaaaaaaaa'); $null = [IO.Directory]::CreateDirectory($folder)
            Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = 'aaaaaaaaaaaa'; name = 'worker'; client = 'claude'; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'; context = 'brief'; reason = 'human'; session = '$3'; pane = '%9'; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $fake.Cwd; createdAt = 'x' })
            Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
            if ($case[1] -ceq 'throw') { Add-FakeRmuxRule -Fake $fake -Match '#\{pane_id\}\|#\{@agent-id\}$' -Script { throw [TimeoutException]::new('slow') } }
            else { Add-FakeRmuxRule -Fake $fake -Match '#\{pane_id\}\|#\{@agent-id\}$' -ExitCode $case[0] -StdOut $case[1] }
            $ran = try { Update-MuxAgentNotices -P $fake -Caller $caller; 'ran' } catch { 'threw' }
            "$ran/$([IO.File]::Exists((Join-Path $folder 'notice.json')))" }
        ($answers -join ',') | Should BeExactly 'ran/True,ran/False,ran/False,ran/False,threw/False' } }
Describe 'MuxAgents activation entry' {
    It 'refuses every platform but windows, linux and macos with one platform line' {
        $answers = foreach ($name in 'windows', 'linux', 'macos', 'freebsd', 'Windows', '') { "$(Get-MuxAgentEntryRefusal -Platform $name)" }
        $line = '{"ok":false,"verb":null,"code":2,"check":"platform","message":"mux-agents 0.1.0 runs on Windows, Linux and macOS only"}'
        ($answers -join ';') | Should BeExactly ";;;$line;$line;$line" }
    It 'the entry asks the gate before it builds providers and tests no OS flag itself' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'scripts/mux-agent.ps1'))
        $gate = [array]::IndexOf($lines, '$refusal = Get-MuxAgentEntryRefusal -Platform (Get-MuxAgentPlatform)')
        $build = [array]::FindIndex($lines, [Predicate[string]] { param($l) $l.Contains('Initialize-MuxAgentProviders') })
        "$($gate -ge 0) $($gate -lt $build) $(@($lines | Where-Object { $_ -match '\$Is(Windows|Linux|MacOS)' }).Count)" | Should BeExactly 'True True 0' } }
Describe 'MuxAgents activation provider refusals' {
    It 'the entry and the launcher print a state-root refusal as the one JSON reply, with its exit code' {
        $pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
        $saved = @{ MUX_AGENTS_STATE = $env:MUX_AGENTS_STATE; LOCALAPPDATA = $env:LOCALAPPDATA }
        # Unix: a relative MUX_AGENTS_STATE refuses 1 'state' (B3). Windows: no LOCALAPPDATA throws inside the factory.
        if ($IsWindows) { $env:MUX_AGENTS_STATE = $null; $env:LOCALAPPDATA = $null; $want = '1 1 internal False 1' } else { $env:MUX_AGENTS_STATE = 'relative/state'; $want = '1 1 state False 1' }
        try {
            $answers = foreach ($pair in @(@('mux-agent.ps1', 'list'), @('mux-agent-launcher.ps1', 'x/spec.json'))) {
                $lines = @(& $pwsh -NoProfile -File (Join-Path $repoRoot "scripts/$($pair[0])") $pair[1]); $reply = $lines[0] | ConvertFrom-Json
                "$LASTEXITCODE $($reply.code) $($reply.check) $($reply.ok) $($lines.Count)" } }
        finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
        ($answers -join ';') | Should BeExactly "$want;$want" } }
Describe 'MuxAgents activation pane environment' {
    It 'passes the selected state root to a new Unix pane with -e, and nothing on Windows' {
        $answers = foreach ($platform in 'windows', 'linux', 'macos') { "[$(@(Get-MuxAgentPaneEnvironment -P ([pscustomobject]@{ Platform = $platform; StateRoot = '/s p/state' })) -join '|')]" }
        ($answers -join ';') | Should BeExactly '[];[-e|MUX_AGENTS_STATE=/s p/state];[-e|MUX_AGENTS_STATE=/s p/state]' } }
