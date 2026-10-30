# Foundation tests: rename, notice routing, runner, CI, release. Untagged: they run on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
$testsRoot = Join-Path $repoRoot 'tests'
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
$pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName

Describe 'MuxAgents foundation rename' {
    It 'entry with no verb answers usage on Windows and platform elsewhere, exit 2' {
        $reply = & $pwsh -NoProfile -File ([IO.Path]::Combine($repoRoot, 'scripts', 'mux-agent.ps1')) | ConvertFrom-Json
        # Windows names the command (singular); elsewhere the platform refusal names the product.
        $expected, $pattern = if ($IsWindows) { '2 usage True', '^usage: mux-agent <' } else { '2 platform True', '^mux-agents ' }
        "$LASTEXITCODE $($reply.check) $($reply.message -cmatch $pattern)" | Should BeExactly $expected
    }
    It 'state root reads MUX_AGENTS_STATE and ignores the old variable' {
        $old = 'R' + 'MUX_AGENTS_STATE'
        Get-MuxAgentStateRoot -Environment @{ $old = 'D:\old'; LOCALAPPDATA = 'C:\l' } | Should BeExactly ([IO.Path]::Combine('C:\l', 'mux-agents', 'state'))
        Get-MuxAgentStateRoot -Environment @{ $old = 'D:\old'; MUX_AGENTS_STATE = 'D:\s'; LOCALAPPDATA = 'C:\l' } | Should BeExactly 'D:\s'
    }
    It 'no old name in tracked files outside docs/specs' {
        $pattern = ('r' + 'mux-agent') + '|' + ('R' + 'muxAgent') + '|' + ('R' + 'MUX_AGENTS')
        $hits = @(& git -C $repoRoot grep -n -E $pattern -- . ':(exclude)docs/specs')
        "$LASTEXITCODE $($hits -join ' ; ')" | Should BeExactly '1 '
    }
}

Describe 'MuxAgents foundation notice' {
    It 'posts the exit notice to pane-<n> of parentPane even with a valid parentAgentId' {
        $state = [IO.Path]::Combine($TestDrive, 'root', 'key')
        $null = New-Item -ItemType Directory -Force -Path (Join-Path $state 'mail'), ([IO.Path]::Combine($state, 'agents', 'bbbbbbbbbbbb'))
        Mock Get-MuxAgentStateDirectory { [IO.Path]::Combine($TestDrive, 'root', 'key') }
        $p = [pscustomobject]@{ StateRoot = (Join-Path $TestDrive 'root'); Now = { [DateTime]::UtcNow } }
        $caller = [pscustomobject]@{ ServerKey = 'key'; Session = '$3' }
        $record = [pscustomobject]@{ id = 'bbbbbbbbbbbb'; name = 'worker'; pane = '%9'; parentPane = '%7'; parentAgentId = '0123456789ab' }
        Add-MuxAgentNotice -P $p -Caller $caller -Record $record -Kind exited -ExitCode 0 | Should Be $true
        $mail = Join-Path $state 'mail'
        "$(Test-Path (Join-Path $mail 'pane-7.jsonl')) $(Test-Path (Join-Path $mail '0123456789ab.jsonl'))" | Should Be 'True False'
        (Get-Content (Join-Path $mail 'pane-7.jsonl') | ConvertFrom-Json).body | Should BeExactly 'worker exited with code 0.'
    }
}

Describe 'MuxAgents foundation runner' {
    $runner = Join-Path $testsRoot 'Invoke-MuxAgentTests.ps1'
    function Invoke-FoundationRunner([string]$Name, [string]$Suite) {
        $sample = Join-Path $TestDrive $Name
        $null = New-Item -ItemType Directory -Force -Path $sample
        [IO.File]::WriteAllText((Join-Path $sample 'Sample.Tests.ps1'), $Suite)
        $null = & $pwsh -NoProfile -File $runner -Path $sample
        $LASTEXITCODE
    }
    It 'exits 1 when any test fails' {
        Invoke-FoundationRunner 'failing' "Describe 'sample' { It 'fails' { 1 | Should Be 2 } }" | Should Be 1
    }
    It 'skips suites tagged windows only off Windows' {
        $suite = "Describe 'win' -Tag 'windows' { It 'fails' { 1 | Should Be 2 } }`nDescribe 'all' { It 'passes' { 1 | Should Be 1 } }"
        Invoke-FoundationRunner 'tagged' $suite | Should Be $(if ($IsWindows) { 1 } else { 0 })
    }
}
