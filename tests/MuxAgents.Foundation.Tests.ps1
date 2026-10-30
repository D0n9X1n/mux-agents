# Foundation tests: rename, notice routing, runner, CI, release. Untagged: they run on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
$testsRoot = Join-Path $repoRoot 'tests'
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
$pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
Import-Module powershell-yaml -RequiredVersion 0.4.12

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
    It 'runs the suites on Pester 4.10.1' {
        $out = Join-Path $TestDrive 'pester-version.txt'
        $suite = "Describe 'version' { It 'records' { [IO.File]::WriteAllText('$($out.Replace("'", "''"))', (Get-Module Pester).Version.ToString()) } }"
        Invoke-FoundationRunner 'version' $suite | Should Be 0
        [IO.File]::ReadAllText($out) | Should BeExactly '4.10.1'
    }
    It 'imports Pester 4.10.1 and refuses any other loaded version' {
        $lines = [IO.File]::ReadAllLines($runner)
        $import = [array]::IndexOf($lines, 'Import-Module Pester -RequiredVersion 4.10.1')
        "$($import -ge 0) $("$($lines[$import + 1])".StartsWith("if ((Get-Module Pester).Version -ne [version]'4.10.1') { throw ", [StringComparison]::Ordinal))" | Should BeExactly 'True True'
    }
    It 'exits 1 when any test fails' {
        Invoke-FoundationRunner 'failing' "Describe 'sample' { It 'fails' { 1 | Should Be 2 } }" | Should Be 1
    }
    It 'exits 1 when two tests fail' {
        Invoke-FoundationRunner 'failing-two' "Describe 'sample' { It 'fails' { 1 | Should Be 2 }; It 'fails too' { 1 | Should Be 3 } }" | Should Be 1
    }
    It 'skips suites tagged windows only off Windows' {
        $suite = "Describe 'win' -Tag 'windows' { It 'fails' { 1 | Should Be 2 } }`nDescribe 'all' { It 'passes' { 1 | Should Be 1 } }"
        Invoke-FoundationRunner 'tagged' $suite | Should Be $(if ($IsWindows) { 1 } else { 0 })
    }
    It 'with no -Path discovers every *.Tests.ps1 under tests' {
        # A child pwsh runs the runner with Invoke-Pester aliased to a recorder, so no suite runs and nothing recurses.
        # The recorder logs the -Script it was given, then the files Pester 4.10.1's own discovery resolves from it.
        $record = Join-Path $TestDrive 'discovered.txt'
        $probe = Join-Path $TestDrive 'probe.ps1'
        [IO.File]::WriteAllLines($probe, [string[]]@(
            'param([string]$Runner, [string]$Record)'
            '$global:discoveryRecord = $Record'
            'Import-Module Pester -RequiredVersion 4.10.1'
            'function global:Get-DiscoveredTestScripts($Script) {'
            '    $lines = @((Resolve-Path -LiteralPath $Script).ProviderPath) + @(& (Get-Module Pester) { param($p) ResolveTestScripts $p } $Script | ForEach-Object { $_.Path })'
            '    [IO.File]::WriteAllLines($global:discoveryRecord, [string[]]$lines)'
            '    [pscustomobject]@{ FailedCount = 0 }'
            '}'
            'Set-Alias -Name Invoke-Pester -Value Get-DiscoveredTestScripts -Scope Global'
            '& $Runner'
        ))
        $null = & $pwsh -NoProfile -File $probe -Runner $runner -Record $record
        $lines = if (Test-Path -LiteralPath $record) { @([IO.File]::ReadAllLines($record)) } else { @('no record') }
        $separators = [char[]]'\/'
        $lines[0].TrimEnd($separators) | Should BeExactly ([IO.Path]::GetFullPath($testsRoot).TrimEnd($separators))
        $expected = @(Get-ChildItem -LiteralPath $testsRoot -Recurse -File -Filter '*.Tests.ps1' | ForEach-Object { $_.FullName } | Sort-Object)
        $found = @($lines | Select-Object -Skip 1 | Sort-Object)
        "$($found -join ' ; ')" | Should BeExactly "$($expected -join ' ; ')"
        @('MuxAgents.Tests.ps1', 'MuxAgentsVerbs.Tests.ps1' | Where-Object { $_ -notin @($found | ForEach-Object { Split-Path -Leaf $_ }) }).Count | Should Be 0
    }
}

Describe 'MuxAgents foundation legacy suites' {
    It 'tags every legacy Describe windows and dot-sources without backslashes' {
        foreach ($name in 'MuxAgents.Tests.ps1', 'MuxAgentsVerbs.Tests.ps1') {
            $lines = [IO.File]::ReadAllLines((Join-Path $testsRoot $name))
            $describes = @($lines | Where-Object { $_ -match '^\s*Describe\b' })
            "$name $($describes.Count -gt 0) $(@($describes | Where-Object { $_ -notmatch " -Tag 'windows' \{$" }).Count)" | Should BeExactly "$name True 0"
            @($lines | Where-Object { $_ -match '^\. ' -and $_.Contains('\') }).Count | Should Be 0
        }
    }
}

Describe 'MuxAgents foundation docs' {
    It 'README Test section installs Pester 4.10.1 and powershell-yaml 0.4.12 and runs the runner' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'README.md'))
        $start = [array]::IndexOf($lines, '## Test')
        $section = if ($start -ge 0) { @($lines[($start + 1)..($lines.Count - 1)] | ForEach-Object -Begin { $open = $true } -Process { if ($_ -match '^## ') { $open = $false }; if ($open) { $_ } }) -join "`n" } else { '' }
        $expected = 'Install-Module Pester -RequiredVersion 4.10.1', 'Install-Module powershell-yaml -RequiredVersion 0.4.12', 'pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1'
        "$($start -ge 0) $(@($expected | Where-Object { -not $section.Contains($_) }) -join ' ; ')" | Should BeExactly 'True '
    }
    It 'README names no Pester version but 4.10.1' {
        $versions = @([regex]::Matches([IO.File]::ReadAllText((Join-Path $repoRoot 'README.md')), 'Pester\b[^\d\r\n]*(\d+(?:\.\d+)*)') | ForEach-Object { $_.Groups[1].Value })
        "$($versions.Count -gt 0) $(@($versions | Where-Object { $_ -cne '4.10.1' }) -join ',')" | Should BeExactly 'True '
    }
}

Describe 'MuxAgents foundation CI' {
    $ciPath = [IO.Path]::Combine($repoRoot, '.github', 'workflows', 'ci.yml')
    $ci = ConvertFrom-Yaml -Ordered ([IO.File]::ReadAllText($ciPath))
    $jobs = $ci.jobs
    $steps = @(foreach ($job in @($jobs.Values)) { @($job.steps) })
    function Get-JobSteps([string]$Name) { if ($jobs -is [Collections.IDictionary] -and $jobs.Contains($Name)) { @($jobs[$Name].steps) } }
    function Get-RunLines($Step) { @("$($Step.run)" -split '\r?\n' | ForEach-Object { $_.Trim() } | Where-Object { $_ }) }
    It 'runs on pull requests and on pushes to main' {
        $on = $ci['on']
        "$($on -is [Collections.IDictionary]) $($on.Contains('pull_request')) $($on.Contains('push')) $(@($on.push.branches) -join ',')" | Should BeExactly 'True True True main'
    }
    It 'grants contents read at the top and nothing beyond read in any job' {
        $top = $ci.permissions
        "$($top -is [Collections.IDictionary]) $(@($top.Keys) -join ',') $($top.contents)" | Should BeExactly 'True contents read'
        $wide = @(foreach ($name in @($jobs.Keys)) {
            $perm = $jobs[$name].permissions
            if ($perm -is [Collections.IDictionary]) { foreach ($key in @($perm.Keys)) { if ("$($perm[$key])" -cnotin 'read', 'none') { "$name.$key=$($perm[$key])" } } }
            elseif ($null -ne $perm) { "$name=$perm" }
        })
        $wide -join ' ; ' | Should BeExactly ''
    }
    It 'groups runs by event and pull request and cancels only pull request runs' {
        $concurrency = $ci.concurrency
        $concurrency -is [Collections.IDictionary] | Should Be $true
        $concurrency.group | Should BeExactly 'ci-${{ github.event_name }}-${{ github.event.pull_request.number || github.sha }}'
        $concurrency.'cancel-in-progress' | Should BeExactly '${{ github.event_name == ''pull_request'' }}'
    }
    It 'has only the lint job on ubuntu and the test job on a three-OS matrix without fail-fast' {
        "$(@($jobs.Keys | Sort-Object) -join ',') $($jobs.lint.'runs-on') $($jobs.test.'runs-on')" | Should BeExactly 'lint,test ubuntu-latest ${{ matrix.os }}'
        @($jobs.test.strategy.matrix.os) -join ',' | Should BeExactly 'ubuntu-latest,macos-latest,windows-latest'
        $failFast = $jobs.test.strategy.'fail-fast'
        "$($failFast -is [bool]) $failFast" | Should BeExactly 'True False'
    }
    It 'gates only the TEMP and TMP step, on Windows' {
        $gated = @($steps | Where-Object { $_.Contains('if') })
        $temp = @(Get-JobSteps 'test' | Where-Object { "$($_.run)".Contains('"TEMP=$env:RUNNER_TEMP"') })
        $condition = if ($gated.Count -gt 0) { $gated[0]['if'] }
        "$($gated.Count) $($temp.Count) $($gated.Count -gt 0 -and [object]::ReferenceEquals($gated[0], $temp[0])) $condition" | Should BeExactly "1 1 True runner.os == 'Windows'"
    }
    It 'pins every action to a full commit SHA with a version comment' {
        $uses = @($steps | Where-Object { $_.Contains('uses') } | ForEach-Object { "$($_.uses)" })
        "$($uses.Count -gt 0) $(@($uses | Where-Object { $_ -cnotmatch '^[\w.-]+/[\w.-]+@[0-9a-f]{40}$' }) -join ' ; ')" | Should BeExactly 'True '
        $lines = @([IO.File]::ReadAllLines($ciPath) | Where-Object { $_ -match '^\s*(-\s+)?uses:' })
        "$($lines.Count) $(@($lines | Where-Object { $_ -notmatch '\s#\s*v\d' }) -join ' ; ')" | Should BeExactly "$($uses.Count) "
    }
    It 'lint checks the actionlint 1.7.12 download, runs it and parses both manifests' {
        $lint = @(Get-JobSteps 'lint')
        $url = 'https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz'
        $check = 'echo ''8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  actionlint.tar.gz'' | sha256sum -c -'
        $install = @($lint | Where-Object { $runLines = Get-RunLines $_; @($runLines | Where-Object { $_.StartsWith('curl ') -and $_.Contains($url) }).Count -gt 0 -and $runLines -ccontains $check })
        $run = @($lint | Where-Object { "$($_.run)" -ceq './actionlint -color' })
        $manifest = @($lint | Where-Object { $text = "$($_.run)"; $_.shell -ceq 'pwsh' -and $text.Contains('.claude-plugin/plugin.json') -and $text.Contains('.claude-plugin/marketplace.json') -and $text -cmatch '\|\s*ConvertFrom-Json\b' })
        "$($install.Count -gt 0) $($run.Count -gt 0) $($manifest.Count -gt 0)" | Should BeExactly 'True True True'
    }
    It 'test points TEMP and TMP at RUNNER_TEMP, installs the pinned modules and runs the runner last' {
        $test = @(Get-JobSteps 'test')
        $tempLines = @($test | Where-Object { "$($_.run)".Contains('"TEMP=$env:RUNNER_TEMP"') } | ForEach-Object { Get-RunLines $_ })
        $pwshLines = @($test | Where-Object { $_.shell -ceq 'pwsh' } | ForEach-Object { Get-RunLines $_ })
        $tempEnv = $tempLines -ccontains '"TEMP=$env:RUNNER_TEMP" >> $env:GITHUB_ENV'
        $tmpEnv = $tempLines -ccontains '"TMP=$env:RUNNER_TEMP" >> $env:GITHUB_ENV'
        $pester = @($pwshLines | Where-Object { $_ -cmatch '^Install-Module Pester -RequiredVersion 4\.10\.1\b' }).Count -gt 0
        $yaml = @($pwshLines | Where-Object { $_ -cmatch '^Install-Module powershell-yaml -RequiredVersion 0\.4\.12\b' }).Count -gt 0
        "$tempEnv $tmpEnv $pester $yaml" | Should BeExactly 'True True True True'
        $last = if ($test.Count -gt 0) { $test[-1].run }
        "$last" | Should BeExactly 'pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1'
    }
}

Describe 'MuxAgents foundation hygiene' {
    It 'issue forms are valid GitHub forms that ask version, OS, multiplexer and client' {
        # GitHub's attribute keys per body type; validations take only required.
        $attributeKeys = @{
            markdown = @('value'); textarea = @('label', 'description', 'placeholder', 'value', 'render'); input = @('label', 'description', 'placeholder', 'value')
            dropdown = @('label', 'description', 'options', 'multiple', 'default'); checkboxes = @('label', 'description', 'options')
        }
        foreach ($name in 'bug.yml', 'feature.yml') {
            $form = ConvertFrom-Yaml -Ordered ([IO.File]::ReadAllText([IO.Path]::Combine($repoRoot, '.github', 'ISSUE_TEMPLATE', $name)))
            $problems = [Collections.Generic.List[string]]::new()
            foreach ($key in 'name', 'description') { if (-not ($form.$key -is [string] -and $form.$key.Trim())) { $problems.Add($key) } }
            $body = @($form.body)
            if (-not ($form.body -is [Collections.IList] -and $body.Count -gt 0)) { $problems.Add('body') }
            $ids = [Collections.Generic.List[string]]::new()
            foreach ($item in $body) {
                if ($item -isnot [Collections.IDictionary]) { $problems.Add("item $item"); continue }
                $type = "$($item.type)"
                if ($type -cnotin @($attributeKeys.Keys)) { $problems.Add("type $type"); continue }
                $attributes = $item.attributes
                if ($attributes -is [Collections.IDictionary]) {
                    foreach ($key in @($attributes.Keys)) { if ("$key" -cnotin $attributeKeys[$type]) { $problems.Add("$type attribute $key") } }
                } elseif ($null -ne $attributes) { $problems.Add("$type attributes") }
                if ($type -cne 'markdown') {
                    if ($item.id -is [string] -and $item.id) { $ids.Add($item.id) } else { $problems.Add("$type id") }
                    if (-not ($attributes.label -is [string] -and $attributes.label.Trim())) { $problems.Add("$type label") }
                }
                if ($item.Contains('validations')) {
                    $validations = $item.validations
                    if ($validations -isnot [Collections.IDictionary]) { $problems.Add("$type validations") }
                    else { foreach ($key in @($validations.Keys)) { if ("$key" -cne 'required') { $problems.Add("$type validation $key") } } }
                }
            }
            foreach ($id in 'version', 'os', 'multiplexer', 'client') { if ($id -cnotin $ids) { $problems.Add("missing id $id") } }
            "$name $($problems -join ' ; ')" | Should BeExactly "$name "
        }
    }
    It 'dependabot version 2 updates github-actions at / monthly' {
        $bot = ConvertFrom-Yaml -Ordered ([IO.File]::ReadAllText([IO.Path]::Combine($repoRoot, '.github', 'dependabot.yml')))
        $actions = @(@($bot.updates) | Where-Object { $_ -is [Collections.IDictionary] -and $_.'package-ecosystem' -ceq 'github-actions' -and $_.directory -ceq '/' -and $_.schedule.interval -ceq 'monthly' })
        "$($bot.version -is [int]) $($bot.version) $($actions.Count -gt 0)" | Should BeExactly 'True 2 True'
    }
    It 'CONTRIBUTING and the PR template carry the merge procedure sentences exactly' {
        $contributing = [IO.File]::ReadAllLines((Join-Path $repoRoot 'CONTRIBUTING.md'))
        $expected = @(
            '1. `gh pr view <n> --json headRefOid,statusCheckRollup`'
            '2. Every check on that `headRefOid` is `SUCCESS`, and `lint`, `test (ubuntu-latest)`, `test (macos-latest)` and `test (windows-latest)` are all present.'
            '3. `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the `headRefOid` from step 1. The push is not forced, so it lands only when `<sha>` builds on the current `main`; GitHub then marks the pull request merged.'
            'Commits are authored and committed as `D0n9X1n <D0n9x1n@outlook.com>`. A pull request lands by fast-forwarding `main` to its tested head, never through a GitHub merge, which would re-author it.'
        )
        @($expected | Where-Object { $contributing -cnotcontains $_ }) -join ' ; ' | Should BeExactly ''
        $template = [IO.Path]::Combine($repoRoot, '.github', 'pull_request_template.md')
        $lines = [IO.File]::ReadAllLines($template)
        $checklist = @(
            '- [ ] `gh pr view <n> --json headRefOid,statusCheckRollup`: every check on headRefOid is SUCCESS; `lint`, `test (ubuntu-latest)`, `test (macos-latest)` and `test (windows-latest)` are present'
            '- [ ] `git push origin <sha>:main`, with `<sha>` the checked headRefOid; not forced, so it lands only on the current `main`'
        )
        @($checklist | Where-Object { $lines -cnotcontains $_ }) -join ' ; ' | Should BeExactly ''
        # A GitHub merge re-authors the commit, so neither document may offer one.
        "$(($contributing -join "`n").Contains('gh pr merge')) $([IO.File]::ReadAllText($template).Contains('gh pr merge'))" | Should BeExactly 'False False'
        $text = [IO.File]::ReadAllText($template)
        @('## Summary', '## Type', '## Closes', '## Test plan', '## Merge checklist' | Where-Object { -not $text.Contains($_) }).Count | Should Be 0
    }
}
