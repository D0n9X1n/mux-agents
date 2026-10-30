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
            # -Tag 'windows', -Tag "windows" or -Tag windows, any spacing, then the opening brace.
            $tagged = '\s-Tag\s+([''"]?)windows\1\s*\{\s*$'
            "$name $($describes.Count -gt 0) $(@($describes | Where-Object { $_ -notmatch $tagged }).Count)" | Should BeExactly "$name True 0"
            @($lines | Where-Object { $_ -match '^\. ' -and $_.Contains('\') }).Count | Should Be 0
        }
    }
}

Describe 'MuxAgents foundation docs' {
    It 'README Test section installs Pester 4.10.1 and powershell-yaml 0.4.12 and runs the runner' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'README.md'))
        $start = [array]::IndexOf($lines, '## Test')
        $section = if ($start -ge 0) { @($lines[($start + 1)..($lines.Count - 1)] | ForEach-Object -Begin { $open = $true } -Process { if ($_ -match '^## ') { $open = $false }; if ($open) { $_ } }) } else { @() }
        # Each command is a whole line; a version must not continue (4.10.1.1 is another version).
        $expected = '^Install-Module Pester -RequiredVersion 4\.10\.1(\s|$)', '^Install-Module powershell-yaml -RequiredVersion 0\.4\.12(\s|$)', '^pwsh -NoProfile -File tests/Invoke-MuxAgentTests\.ps1$'
        "$($start -ge 0) $(@($expected | Where-Object { $pattern = $_; @($section | Where-Object { $_.Trim() -cmatch $pattern }).Count -eq 0 }) -join ' ; ')" | Should BeExactly 'True '
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
    # get_Keys(), not .Keys: a YAML entry named keys would shadow the property and hide the real entries.
    function Get-Keys($Map) { if ($Map -is [Collections.IDictionary]) { @($Map.get_Keys() | ForEach-Object { "$_" }) } }
    It 'is named CI' {
        "$($ci.name)" | Should BeExactly 'CI'
    }
    It 'runs on every pull request and on pushes to main, unfiltered' {
        $on = $ci['on']
        $pr = if ($on -is [Collections.IDictionary] -and $on.Contains('pull_request')) { $on.pull_request }
        $push = if ($on -is [Collections.IDictionary] -and $on.Contains('push')) { $on.push }
        $prFilters = @(Get-Keys $pr | Where-Object { $_ -cin 'branches', 'branches-ignore', 'paths', 'paths-ignore', 'types' })
        "$($on -is [Collections.IDictionary] -and $on.Contains('pull_request')) $(-not $pr -or $pr -is [Collections.IDictionary]) $($prFilters -join ',')" | Should BeExactly 'True True '
        "$($push -is [Collections.IDictionary]) $(@(Get-Keys $push) -join ',') $(@($push.branches) -join ',')" | Should BeExactly 'True branches main'
    }
    It 'grants contents read at the top and nothing beyond read in any job' {
        $top = $ci.permissions
        "$($top -is [Collections.IDictionary]) $(@($top.get_Keys()) -join ',') $($top.contents)" | Should BeExactly 'True contents read'
        $wide = @(foreach ($name in @($jobs.get_Keys())) {
            $perm = $jobs[$name].permissions
            if ($perm -is [Collections.IDictionary]) { foreach ($key in @($perm.get_Keys())) { if ("$($perm[$key])" -cnotin 'read', 'none') { "$name.$key=$($perm[$key])" } } }
            elseif ($null -ne $perm) { "$name=$perm" }
        })
        $wide -join ' ; ' | Should BeExactly ''
    }
    It 'groups runs by pull request and cancels only pull request runs' {
        $concurrency = $ci.concurrency
        $concurrency -is [Collections.IDictionary] | Should Be $true
        "$($concurrency.group)".Contains('${{ github.event.pull_request.number || github.sha }}') | Should Be $true
        $concurrency.'cancel-in-progress' | Should BeExactly '${{ github.event_name == ''pull_request'' }}'
    }
    It 'has only the lint job on ubuntu and the test job on a three-OS matrix without fail-fast' {
        "$(@($jobs.get_Keys() | Sort-Object) -join ',') $($jobs.lint.'runs-on') $($jobs.test.'runs-on')" | Should BeExactly 'lint,test ubuntu-latest ${{ matrix.os }}'
        @($jobs.test.strategy.matrix.os | Sort-Object) -join ',' | Should BeExactly 'macos-latest,ubuntu-latest,windows-latest'
        $failFast = $jobs.test.strategy.'fail-fast'
        "$($failFast -is [bool]) $failFast" | Should BeExactly 'True False'
    }
    It 'skips no job or matrix entry and lets no job or step fail or time out silently' {
        $problems = @(foreach ($name in @($jobs.get_Keys())) {
            $job = $jobs[$name]
            if ($job.Contains('if')) { "$name if" }
            foreach ($key in @(Get-Keys $job.strategy.matrix | Where-Object { $_ -cin 'include', 'exclude' })) { "$name matrix $key" }
            foreach ($item in @(@($job) + @($job.steps))) {
                if ($item -is [Collections.IDictionary]) {
                    if ($item.Contains('continue-on-error')) { "$name $($item.name) continue-on-error" }
                    if ($item.Contains('timeout-minutes') -and "$($item.'timeout-minutes')" -eq '0') { "$name $($item.name) timeout-minutes 0" }
                }
            }
        })
        $problems -join ' ; ' | Should BeExactly ''
    }
    It 'gates no step but the TEMP and TMP step, and that one only on Windows' {
        # A condition without its ${{ }} wrapper; success() is the default, so it counts as none.
        $conditions = @(foreach ($step in $steps) {
            $c = if ($step.Contains('if')) { "$($step['if'])".Trim() } else { '' }
            if ($c -match '^\$\{\{\s*(.*?)\s*\}\}$') { $c = $Matches[1] }
            if ($c -and $c -cne 'success()') { [pscustomobject]@{ Step = $step; Condition = $c } }
        })
        $temp = @(Get-JobSteps 'test' | Where-Object { "$($_.run)".Contains('"TEMP=$env:RUNNER_TEMP"') })
        $other = @($conditions | Where-Object { -not [object]::ReferenceEquals($_.Step, $temp[0]) -or $_.Condition -cne "runner.os == 'Windows'" } | ForEach-Object { "$($_.Step.name): $($_.Condition)" })
        "$($temp.Count) $($other -join ' ; ')" | Should BeExactly '1 '
    }
    It 'pins every action to a full commit SHA with a version comment' {
        $uses = @($steps | Where-Object { $_.Contains('uses') } | ForEach-Object { "$($_.uses)" })
        "$($uses.Count -gt 0) $(@($uses | Where-Object { $_ -cnotmatch '^[\w.-]+/[\w.-]+@[0-9a-f]{40}$' }) -join ' ; ')" | Should BeExactly 'True '
        $lines = @([IO.File]::ReadAllLines($ciPath) | Where-Object { $_ -match '^\s*(-\s+)?uses:' })
        "$($lines.Count) $(@($lines | Where-Object { $_ -notmatch '@[0-9a-f]{40}\s+#\s*v?\d+(\.\d+)*\b' }) -join ' ; ')" | Should BeExactly "$($uses.Count) "
    }
    It 'checks out the repository in lint and test before any run step' {
        $late = @(foreach ($name in 'lint', 'test') {
            $jobSteps = @(Get-JobSteps $name)
            $checkout = -1; $firstRun = -1
            for ($i = 0; $i -lt $jobSteps.Count; $i++) {
                if ($checkout -lt 0 -and "$($jobSteps[$i].uses)" -cmatch '^actions/checkout@') { $checkout = $i }
                if ($firstRun -lt 0 -and $jobSteps[$i].Contains('run')) { $firstRun = $i }
            }
            if ($checkout -lt 0 -or ($firstRun -ge 0 -and $firstRun -lt $checkout)) { $name }
        })
        $late -join ',' | Should BeExactly ''
    }
    It 'lint installs actionlint 1.7.12 strictly against its SHA-256 and runs it' {
        $lint = @(Get-JobSteps 'lint')
        $expected = @(
            'set -euo pipefail'
            'curl -fsSLo actionlint.tar.gz https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz'
            'echo ''8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  actionlint.tar.gz'' | sha256sum -c -'
            'tar -xzf actionlint.tar.gz actionlint'
        ) -join "`n"
        $install = @($lint | Where-Object { (Get-RunLines $_) -join "`n" -ceq $expected })
        $run = @($lint | Where-Object { "$($_.run)" -ceq './actionlint -color' })
        "$($install.Count) $($run.Count)" | Should BeExactly '1 1'
    }
    It 'lint parses each manifest by name in pwsh' {
        $script = 'foreach ($file in ''.claude-plugin/plugin.json'', ''.claude-plugin/marketplace.json'') { $null = Get-Content -Raw $file | ConvertFrom-Json }'
        @(Get-JobSteps 'lint' | Where-Object { $_.shell -ceq 'pwsh' -and "$($_.run)".Trim() -ceq $script }).Count | Should Be 1
    }
    It 'runs every step written in PowerShell under pwsh' {
        # PowerShell-only syntax: $env:, $null, foreach (...), or a Verb-Noun cmdlet that is not part of a path.
        $powershell = '\$env:|\$null\b|\bforeach\s*\(|(?<![\w./\\-])[A-Z][a-z]+-[A-Z]\w*(?![\w.])'
        $missing = @($steps | Where-Object { "$($_.run)" -cmatch $powershell -and $_.shell -cne 'pwsh' } | ForEach-Object { "$($_.name)" })
        $temp = @(Get-JobSteps 'test' | Where-Object { "$($_.run)".Contains('"TEMP=$env:RUNNER_TEMP"') -and $_.shell -ceq 'pwsh' })
        "$($missing -join ' ; ') $($temp.Count)" | Should BeExactly ' 1'
    }
    It 'test points TEMP and TMP at RUNNER_TEMP, installs the pinned modules and runs the runner last' {
        $test = @(Get-JobSteps 'test')
        $tempLines = @($test | Where-Object { "$($_.run)".Contains('"TEMP=$env:RUNNER_TEMP"') } | ForEach-Object { Get-RunLines $_ })
        $pwshLines = @($test | Where-Object { $_.shell -ceq 'pwsh' } | ForEach-Object { Get-RunLines $_ })
        $tempEnv = $tempLines -ccontains '"TEMP=$env:RUNNER_TEMP" >> $env:GITHUB_ENV'
        $tmpEnv = $tempLines -ccontains '"TMP=$env:RUNNER_TEMP" >> $env:GITHUB_ENV'
        $pester = @($pwshLines | Where-Object { $_ -cmatch '^Install-Module Pester -RequiredVersion 4\.10\.1(\s|$)' }).Count -gt 0
        $yaml = @($pwshLines | Where-Object { $_ -cmatch '^Install-Module powershell-yaml -RequiredVersion 0\.4\.12(\s|$)' }).Count -gt 0
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
                if ($type -cnotin @($attributeKeys.get_Keys())) { $problems.Add("type $type"); continue }
                $attributes = $item.attributes
                if ($attributes -is [Collections.IDictionary]) {
                    foreach ($key in @($attributes.get_Keys())) { if ("$key" -cnotin $attributeKeys[$type]) { $problems.Add("$type attribute $key") } }
                } elseif ($null -ne $attributes) { $problems.Add("$type attributes") }
                # An id is optional; when present it is a non-empty string, unique within the form.
                if ($item.Contains('id')) {
                    if (-not ($item.id -is [string] -and $item.id)) { $problems.Add("$type id") }
                    elseif ($ids -ccontains $item.id) { $problems.Add("duplicate id $($item.id)") }
                    else { $ids.Add($item.id) }
                }
                if ($type -cne 'markdown') {
                    if (-not ($attributes.label -is [string] -and $attributes.label.Trim())) { $problems.Add("$type label") }
                }
                if ($type -cin 'dropdown', 'checkboxes' -and -not ($attributes.options -is [Collections.IList] -and @($attributes.options).Count -gt 0)) { $problems.Add("$type options") }
                if ($item.Contains('validations')) {
                    $validations = $item.validations
                    if ($validations -isnot [Collections.IDictionary]) { $problems.Add("$type validations") }
                    else { foreach ($key in @($validations.get_Keys())) { if ("$key" -cne 'required') { $problems.Add("$type validation $key") } } }
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
            '- [ ] `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the checked headRefOid; not forced, so it lands only on the current `main`'
        )
        @($checklist | Where-Object { $lines -cnotcontains $_ }) -join ' ; ' | Should BeExactly ''
        # A GitHub merge re-authors the commit, so neither document may offer one.
        "$(($contributing -join "`n").Contains('gh pr merge')) $([IO.File]::ReadAllText($template).Contains('gh pr merge'))" | Should BeExactly 'False False'
        # The push is never forced: no --force, no -f flag, no +refspec, bare or quoted.
        @(@($contributing) + @($lines) | Where-Object { $_.Contains('git push') -and $_ -cmatch '--force|(?<![\w-])-f(?![\w-])|(^|[\s"''`])\+\S' }) -join ' ; ' | Should BeExactly ''
        $text = [IO.File]::ReadAllText($template)
        @('## Summary', '## Type', '## Closes', '## Test plan', '## Merge checklist' | Where-Object { -not $text.Contains($_) }).Count | Should Be 0
    }
    # The tests above check what the spec requires; this one pins the files as reviewed, so any other change to them
    # fails until it is reviewed and the hash updated. ci.yml is hashed with its action pins blanked, so a Dependabot
    # SHA bump passes; the pin test checks the pins.
    It 'keeps ci.yml, the issue forms and the PR template as reviewed' {
        function Get-ReviewedHash([string]$Path, [switch]$Pins) {
            $text = [IO.File]::ReadAllText((Join-Path $repoRoot $Path)) -replace "`r`n", "`n"
            if ($Pins) { $text = $text -replace '@[0-9a-f]{40} # v\d+(\.\d+)*', '@<pin>' }
            [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($text))).ToLowerInvariant()
        }
        $actual = @(
            "ci.yml=$(Get-ReviewedHash '.github/workflows/ci.yml' -Pins)"
            "bug.yml=$(Get-ReviewedHash '.github/ISSUE_TEMPLATE/bug.yml')"
            "feature.yml=$(Get-ReviewedHash '.github/ISSUE_TEMPLATE/feature.yml')"
            "pull_request_template.md=$(Get-ReviewedHash '.github/pull_request_template.md')"
        )
        $actual -join ' ; ' | Should BeExactly (@(
                'ci.yml=9c6c72287ca005fb59d817f6037bc2739e654edcbcaff07780b65f8259e600ac'
                'bug.yml=017ee31a64ba465010af2ab3fa839339ec28b7170dd3ce9df66e632e2561898a'
                'feature.yml=c70bb8bff8d86a444995e16a182b20e4c635e1cd4002b47482dc571980f999c1'
                'pull_request_template.md=91780120628ac4776e768af3bac521f4500f9d88298fdbc20718abfa1279ec60'
            ) -join ' ; ')
    }
}

Describe 'MuxAgents foundation release gate' {
    $gate = [IO.Path]::Combine($repoRoot, 'scripts', 'release', 'Test-MuxAgentRelease.ps1')
    $sha = 'a' * 40
    $green = [pscustomobject]@{ conclusion = 'success'; head_sha = $sha; event = 'push' }
    $red = [pscustomobject]@{ conclusion = 'failure'; head_sha = $sha; event = 'push' }
    function Invoke-FoundationGate([bool]$OnMain = $true, [object[]]$Runs = @($green), [string]$Version = '0.1.0') {
        $line = & $gate -Tag 'v0.1.0' -Sha $sha -IsAncestorOfMain $OnMain -Runs $Runs -ManifestVersion $Version
        "$LASTEXITCODE $line"
    }
    It 'passes an on-main tag with a green CI push run and a matching version' { Invoke-FoundationGate | Should BeExactly "0 release ok: v0.1.0 at $sha" }
    It 'refuses a tag that is not on main' { Invoke-FoundationGate -OnMain $false | Should BeExactly '1 refused: v0.1.0 is not on main' }
    It 'refuses a tag with no CI push run' { Invoke-FoundationGate -Runs @() | Should BeExactly "1 refused: no CI push run for $sha" }
    It 'refuses a tag whose CI push runs all failed' { Invoke-FoundationGate -Runs @($red) | Should BeExactly "1 refused: no successful CI push run for $sha" }
    It 'refuses a plugin.json version that differs from the tag' { Invoke-FoundationGate -Version '0.2.0' | Should BeExactly '1 refused: plugin.json version 0.2.0 does not match v0.1.0' }
    It 'counts only push runs for this sha' {
        $prRun = [pscustomobject]@{ conclusion = 'success'; head_sha = $sha; event = 'pull_request' }
        $otherSha = [pscustomobject]@{ conclusion = 'success'; head_sha = ('b' * 40); event = 'push' }
        Invoke-FoundationGate -Runs @($prRun, $otherSha) | Should BeExactly "1 refused: no CI push run for $sha"
    }
    It 'names the first failure in the order main, CI run, version' {
        $first = foreach ($case in @(
                @{ OnMain = $false; Runs = @(); Version = '0.2.0' },
                @{ OnMain = $true; Runs = @(); Version = '0.2.0' },
                @{ OnMain = $true; Runs = @($red); Version = '0.2.0' }
            )) { Invoke-FoundationGate @case }
        $first -join ' ; ' | Should BeExactly "1 refused: v0.1.0 is not on main ; 1 refused: no CI push run for $sha ; 1 refused: no successful CI push run for $sha"
    }
    It 'passes when a successful CI push run sits beside a failed one' {
        (Invoke-FoundationGate -Runs @($red, $green)), (Invoke-FoundationGate -Runs @($green, $red)) -join ' ; ' | Should BeExactly "0 release ok: v0.1.0 at $sha ; 0 release ok: v0.1.0 at $sha"
    }
}

Describe 'MuxAgents foundation package' {
    It 'LICENSE is the full MIT text and release tooling, the wiki publisher and the wiki are export-ignored' {
        $mit = @(
            'MIT License'
            ''
            'Copyright (c) 2026 Dongxin Tang'
            ''
            'Permission is hereby granted, free of charge, to any person obtaining a copy'
            'of this software and associated documentation files (the "Software"), to deal'
            'in the Software without restriction, including without limitation the rights'
            'to use, copy, modify, merge, publish, distribute, sublicense, and/or sell'
            'copies of the Software, and to permit persons to whom the Software is'
            'furnished to do so, subject to the following conditions:'
            ''
            'The above copyright notice and this permission notice shall be included in all'
            'copies or substantial portions of the Software.'
            ''
            'THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR'
            'IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,'
            'FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE'
            'AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER'
            'LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,'
            'OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE'
            'SOFTWARE.'
        ) -join "`n"
        ([IO.File]::ReadAllText((Join-Path $repoRoot 'LICENSE')) -replace '\r\n', "`n").TrimEnd("`n") | Should BeExactly $mit
        $attributes = [IO.File]::ReadAllLines((Join-Path $repoRoot '.gitattributes'))
        @('scripts/release export-ignore', 'scripts/wiki export-ignore', 'wiki export-ignore' | Where-Object { $attributes -cnotcontains $_ }) -join ' ; ' | Should BeExactly ''
    }
    It 'zips only the plugin files of a commit' {
        $zip = Join-Path $TestDrive 'plugin.zip'
        & ([IO.Path]::Combine($repoRoot, 'scripts', 'release', 'New-MuxAgentPackage.ps1')) -Ref HEAD -OutFile $zip
        $LASTEXITCODE | Should Be 0
        $out = Join-Path $TestDrive 'plugin'
        Expand-Archive -LiteralPath $zip -DestinationPath $out
        $seen = foreach ($path in '.claude-plugin/plugin.json', '.claude-plugin/marketplace.json', 'skills/mux-agents/SKILL.md', 'scripts/mux-agent.ps1', 'scripts/MuxAgents.ps1', 'scripts/mux-agent-launcher.ps1', 'README.md', 'LICENSE', 'tests', '.github', 'scripts/release', 'scripts/wiki', 'wiki') { "$path=$(Test-Path -LiteralPath (Join-Path $out $path))" }
        $seen -join ' ' | Should BeExactly '.claude-plugin/plugin.json=True .claude-plugin/marketplace.json=True skills/mux-agents/SKILL.md=True scripts/mux-agent.ps1=True scripts/MuxAgents.ps1=True scripts/mux-agent-launcher.ps1=True README.md=True LICENSE=True tests=False .github=False scripts/release=False scripts/wiki=False wiki=False'
    }
    It 'includes hooks only when the commit has them' {
        $fixture = Join-Path $TestDrive 'pkg-repo'
        $null = New-Item -ItemType Directory -Force -Path ([IO.Path]::Combine($fixture, 'scripts', 'release')), (Join-Path $fixture '.claude-plugin'), (Join-Path $fixture 'skills'), (Join-Path $fixture 'hooks')
        Copy-Item ([IO.Path]::Combine($repoRoot, 'scripts', 'release', 'New-MuxAgentPackage.ps1')) ([IO.Path]::Combine($fixture, 'scripts', 'release'))
        foreach ($file in '.claude-plugin/plugin.json', 'skills/s.md', 'scripts/x.ps1', 'README.md', 'LICENSE') { [IO.File]::WriteAllText((Join-Path $fixture $file), 'x') }
        $null = & git -C $fixture init -q -b main
        $null = & git -C $fixture -c user.name=t -c user.email=t@t add -- .claude-plugin skills scripts README.md LICENSE
        $null = & git -C $fixture -c user.name=t -c user.email=t@t commit -q -m one
        [IO.File]::WriteAllText([IO.Path]::Combine($fixture, 'hooks', 'hooks.json'), '{}')
        $null = & git -C $fixture -c user.name=t -c user.email=t@t add -- hooks
        $null = & git -C $fixture -c user.name=t -c user.email=t@t commit -q -m two
        $packager = [IO.Path]::Combine($fixture, 'scripts', 'release', 'New-MuxAgentPackage.ps1')
        $result = foreach ($ref in 'HEAD~1', 'HEAD') {
            $zip = Join-Path $TestDrive "hooks-$($ref -replace '\W', '').zip"
            & $packager -Ref $ref -OutFile $zip
            $code = $LASTEXITCODE
            $out = Join-Path $TestDrive "hooks-$($ref -replace '\W', '')"
            Expand-Archive -LiteralPath $zip -DestinationPath $out
            "$ref=$code,$(Test-Path -LiteralPath ([IO.Path]::Combine($out, 'hooks', 'hooks.json')))"
        }
        $result -join ' ' | Should BeExactly 'HEAD~1=0,False HEAD=0,True'
    }
}

Describe 'MuxAgents foundation release workflow' {
    $releasePath = [IO.Path]::Combine($repoRoot, '.github', 'workflows', 'release.yml')
    $release = if (Test-Path -LiteralPath $releasePath) { ConvertFrom-Yaml -Ordered ([IO.File]::ReadAllText($releasePath)) } else { [ordered]@{} }
    $jobs = if ($release.jobs -is [Collections.IDictionary]) { $release.jobs } else { [ordered]@{} }
    $jobNames = @($jobs.get_Keys())
    function Get-ReleaseSteps([string]$Name) { if ($jobs.Contains($Name)) { @($jobs[$Name].steps) } }
    function Get-ReleaseRunLines($Step) { @("$($Step.run)" -split '\r?\n' | ForEach-Object { $_.Trim() } | Where-Object { $_ }) }
    # get_Keys(), not .Keys: a YAML entry named keys would shadow the property and hide the real entries.
    function Format-ReleaseMap($Map) { if ($Map -is [Collections.IDictionary]) { @($Map.get_Keys() | Sort-Object | ForEach-Object { "$_=$($Map[$_])" }) -join ',' } else { "$Map" } }
    $steps = @(foreach ($name in $jobNames) { Get-ReleaseSteps $name })
    It 'runs only on pushes of v<major>.<minor>.<patch> tags' {
        $on = $release['on']
        $push = if ($on -is [Collections.IDictionary] -and $on.Contains('push')) { $on.push }
        # Assigned, not taken from an if expression, which would unroll a one-element list into its string.
        $tags = $null
        if ($push -is [Collections.IDictionary]) { $tags = $push.tags }
        "$($on -is [Collections.IDictionary]) $(@($on.get_Keys()) -join ',') $($push -is [Collections.IDictionary]) $(@($push.get_Keys()) -join ',') $($tags -is [Collections.IList]) $(@($tags).Count) $(@($tags) -join ',')" | Should BeExactly 'True push True tags True 1 v[0-9]+.[0-9]+.[0-9]+'
    }
    It 'reads contents at the top, also reads actions in validate and writes contents only in publish' {
        @($jobNames | Sort-Object) -join ',' | Should BeExactly 'package,publish,validate'
        $grants = @("top:$(Format-ReleaseMap $release.permissions)") + @(foreach ($name in 'validate', 'package', 'publish') { "${name}:$(Format-ReleaseMap $jobs[$name].permissions)" })
        $grants -join ' ; ' | Should BeExactly 'top:contents=read ; validate:actions=read,contents=read ; package:contents=read ; publish:contents=write'
    }
    It 'packages after validate and publishes after both, every job on ubuntu-latest' {
        $package = $jobs['package']; $publish = $jobs['publish']
        "$($package.needs -is [string]) $($package.needs) $($publish.needs -is [Collections.IList]) $(@($publish.needs) -join ',')" | Should BeExactly 'True validate True validate,package'
        "$($jobNames.Count) $(@($jobNames | Where-Object { "$($jobs[$_].'runs-on')" -cne 'ubuntu-latest' }) -join ',')" | Should BeExactly '3 '
    }
    It 'gates no job and no step with if' {
        $gated = @(foreach ($name in $jobNames) {
            if ($jobs[$name] -is [Collections.IDictionary] -and $jobs[$name].Contains('if')) { $name }
            foreach ($step in @(Get-ReleaseSteps $name)) { if ($step -is [Collections.IDictionary] -and $step.Contains('if')) { "$name step $($step.name)$($step.uses)" } }
        })
        "$($steps.Count -gt 0) $($gated -join ' ; ')" | Should BeExactly 'True '
    }
    It 'pins every action to a commit SHA and checks out without persisted credentials, validate with full history' {
        $uses = @($steps | Where-Object { $_.Contains('uses') } | ForEach-Object { "$($_.uses)" })
        "$($uses.Count -gt 0) $(@($uses | Where-Object { $_ -cnotmatch '^[\w.-]+/[\w.-]+@[0-9a-f]{40}$' }) -join ' ; ')" | Should BeExactly 'True '
        $lines = @(if (Test-Path -LiteralPath $releasePath) { [IO.File]::ReadAllLines($releasePath) | Where-Object { $_ -match '^\s*(-\s+)?uses:' } })
        "$($lines.Count) $(@($lines | Where-Object { $_ -notmatch '\s#\s*v\d' }) -join ' ; ')" | Should BeExactly "$($uses.Count) "
        $checkouts = @(foreach ($name in $jobNames) { foreach ($step in @(Get-ReleaseSteps $name)) { if ("$($step.uses)".StartsWith('actions/checkout@', [StringComparison]::Ordinal)) { [pscustomobject]@{ Job = $name; Step = $step } } } })
        $kept = @($checkouts | Where-Object { $persist = $_.Step.with.'persist-credentials'; -not ($persist -is [bool] -and -not $persist) } | ForEach-Object { $_.Job })
        "$($checkouts.Count -gt 0) $($kept -join ',')" | Should BeExactly 'True '
        $depth = @($checkouts | Where-Object { $_.Job -ceq 'validate' } | ForEach-Object { $_.Step.with.'fetch-depth' })
        "$($depth.Count) $($depth[0] -is [int]) $($depth[0])" | Should BeExactly '1 True 0'
    }
    It 'validate resolves the tag, asks main, reads CI push runs and the manifest version, and exits with the gate' {
        $gate = @(
            '$sha = git rev-parse "$($env:TAG)^{commit}"'
            'if ($sha -cne $env:GITHUB_SHA) { "refused: $($env:TAG) does not point at $env:GITHUB_SHA"; exit 1 }'
            'git merge-base --is-ancestor $sha origin/main'
            '$onMain = $LASTEXITCODE -eq 0'
            '$runs = @(gh api "repos/$env:GITHUB_REPOSITORY/actions/workflows/ci.yml/runs?head_sha=$sha&event=push&per_page=100" --jq ''.workflow_runs[] | {conclusion, head_sha, event}'' | ForEach-Object { $_ | ConvertFrom-Json })'
            '$version = (Get-Content -Raw .claude-plugin/plugin.json | ConvertFrom-Json).version'
            '& ./scripts/release/Test-MuxAgentRelease.ps1 -Tag $env:TAG -Sha $sha -IsAncestorOfMain $onMain -Runs $runs -ManifestVersion $version'
            'exit $LASTEXITCODE'
        ) -join "`n"
        $gates = @(Get-ReleaseSteps 'validate' | Where-Object { $_.Contains('run') } | ForEach-Object { if ((Get-ReleaseRunLines $_) -join "`n" -ceq $gate) { "gate $($_.env.GH_TOKEN) $($_.env.TAG)" } else { 'other' } })
        $gates -join ' ; ' | Should BeExactly 'gate ${{ github.token }} ${{ github.ref_name }}'
    }
    It 'starts every job with a checkout, runs every script in pwsh and lets no job or step continue on error' {
        $shape = foreach ($name in 'validate', 'package', 'publish') {
            $kinds = @(Get-ReleaseSteps $name | ForEach-Object {
                if ("$($_.uses)".StartsWith('actions/checkout@', [StringComparison]::Ordinal)) { 'checkout' }
                elseif ($_.Contains('run')) { "run:$($_.shell)" }
                else { 'action' }
            })
            "${name}=$($kinds -join ',')"
        }
        $shape -join ' ; ' | Should BeExactly 'validate=checkout,run:pwsh ; package=checkout,run:pwsh,action ; publish=checkout,action,run:pwsh'
        $loose = @(foreach ($name in $jobNames) {
            if ($jobs[$name] -is [Collections.IDictionary] -and $jobs[$name].Contains('continue-on-error')) { $name }
            foreach ($step in @(Get-ReleaseSteps $name)) { if ($step -is [Collections.IDictionary] -and $step.Contains('continue-on-error')) { "$name step $($step.name)$($step.uses)" } }
        })
        "$($steps.Count -gt 0) $($loose -join ' ; ')" | Should BeExactly 'True '
    }
    It 'package builds the zip of the tagged commit, then uploads it and fails when it is missing' {
        $build = './scripts/release/New-MuxAgentPackage.ps1 -Ref $env:GITHUB_SHA -OutFile "mux-agents-$env:GITHUB_REF_NAME.zip"'
        $sequence = @(Get-ReleaseSteps 'package' | ForEach-Object {
            if ("$($_.run)".TrimEnd() -ceq $build) { 'build' }
            elseif ("$($_.uses)" -cmatch '^actions/upload-artifact@') { "upload $($_.with.name) $($_.with.path) $($_.with.'if-no-files-found')" }
        })
        $sequence -join ' ; ' | Should BeExactly 'build ; upload plugin-zip mux-agents-${{ github.ref_name }}.zip error'
    }
    It 'publish downloads the zip, then creates the release with generated notes plus the wiki release page when present' {
        $create = @(
            # The tag must still name the commit validate approved and package archived, read at publish time.
            '$line = @(git ls-remote origin "refs/tags/$($env:TAG)" "refs/tags/$($env:TAG)^{}")[-1]'
            'if (-not $line -or ($line -split ''\s+'')[0] -cne $env:GITHUB_SHA) { "refused: $($env:TAG) no longer points at $env:GITHUB_SHA"; exit 1 }'
            '$options = @(''release'', ''create'', $env:TAG, "mux-agents-$($env:TAG).zip", ''--verify-tag'', ''--title'', $env:TAG, ''--generate-notes'')'
            '$notes = "wiki/Release-$($env:TAG).md"'
            'if (Test-Path -LiteralPath $notes) { $options += @(''--notes-file'', $notes) }'
            'gh @options'
        ) -join "`n"
        $sequence = @(Get-ReleaseSteps 'publish' | ForEach-Object {
            if ("$($_.uses)" -cmatch '^actions/download-artifact@') { "download $(Format-ReleaseMap $_.with)" }
            elseif ((Get-ReleaseRunLines $_) -join "`n" -ceq $create) { "create $($_.env.GH_TOKEN) $($_.env.GH_REPO) $($_.env.TAG)" }
        })
        # The whole with map: a path key would move the zip away from the workspace root, where gh release create reads it.
        $sequence -join ' ; ' | Should BeExactly 'download name=plugin-zip ; create ${{ github.token }} ${{ github.repository }} ${{ github.ref_name }}'
    }
}
