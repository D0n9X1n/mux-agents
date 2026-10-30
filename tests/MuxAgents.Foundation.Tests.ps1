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

Describe 'MuxAgents foundation legacy suites' {
    It 'tags every legacy Describe windows and dot-sources without backslashes' {
        foreach ($name in 'MuxAgents.Tests.ps1', 'MuxAgentsVerbs.Tests.ps1') {
            $lines = [IO.File]::ReadAllLines((Join-Path $testsRoot $name))
            $describes = @($lines | Where-Object { $_ -match '^Describe ' })
            "$name $($describes.Count -gt 0) $(@($describes | Where-Object { $_ -notmatch " -Tag 'windows' \{$" }).Count)" | Should BeExactly "$name True 0"
            @($lines | Where-Object { $_ -match '^\. ' -and $_.Contains('\') }).Count | Should Be 0
        }
    }
}

Describe 'MuxAgents foundation docs' {
    It 'README runs the tests through the runner on Pester 4.10.1' {
        $text = [IO.File]::ReadAllText((Join-Path $repoRoot 'README.md'))
        "$($text.Contains('pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1')) $($text -match 'Pester 3\.4')" | Should Be 'True False'
    }
}

Describe 'MuxAgents foundation CI' {
    It 'ci.yml has the triggers, pins, lint and the three-OS test matrix' {
        $text = [IO.File]::ReadAllText([IO.Path]::Combine($repoRoot, '.github', 'workflows', 'ci.yml'))
        $expected = @('(?m)^  pull_request:', '(?m)^    branches: \[main\]', '(?m)^permissions:\r?\n  contents: read',
            'cancel-in-progress: \$\{\{ github\.event_name == ''pull_request'' \}\}',
            '(?m)^  group: ci-\$\{\{ github\.event_name \}\}-\$\{\{ github\.event\.pull_request\.number \|\| github\.sha \}\}$', 'actionlint_1\.7\.12_linux_amd64\.tar\.gz',
            '8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  actionlint\.tar\.gz', 'plugin\.json', 'marketplace\.json',
            'fail-fast: false', 'os: \[ubuntu-latest, macos-latest, windows-latest\]', 'Install-Module Pester -RequiredVersion 4\.10\.1',
            'TEMP=\$env:RUNNER_TEMP', 'TMP=\$env:RUNNER_TEMP', 'pwsh -NoProfile -File tests/Invoke-MuxAgentTests\.ps1')
        @($expected | Where-Object { $text -notmatch $_ }) -join ' ; ' | Should BeExactly ''
        $uses = @([regex]::Matches($text, '(?m)^\s*- uses: .*$') | ForEach-Object Value)
        "$($uses.Count -gt 0) $(@($uses | Where-Object { $_ -notmatch '@[0-9a-f]{40} # v\d' }) -join ' ; ')" | Should BeExactly 'True '
    }
}
