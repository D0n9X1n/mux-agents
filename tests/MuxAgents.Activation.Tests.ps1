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
