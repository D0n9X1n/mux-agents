# Wiki tests: the publisher that mirrors wiki/ into the GitHub wiki repository, and its workflow. Untagged: they run on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
$pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
Import-Module powershell-yaml -RequiredVersion 0.4.12

Describe 'MuxAgents wiki publisher' {
    $publisher = [IO.Path]::Combine($repoRoot, 'scripts', 'wiki', 'Publish-MuxAgentWiki.ps1')
    $sha = '0123456789abcdef0123456789abcdef01234567'
    function Invoke-WikiGit([string]$Repo) { & git -C $Repo -c user.name=fixture -c user.email=fixture@example.com @args }
    function Write-WikiFiles([string]$Dir, [hashtable]$Files) {
        foreach ($name in $Files.Keys) {
            $path = Join-Path $Dir $name
            $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path)
            [IO.File]::WriteAllText($path, $Files[$name])
        }
    }
    function New-WikiRepo([string]$Name, [hashtable]$Files, [string]$Branch = 'master') {
        $repo = Join-Path $TestDrive $Name
        $null = New-Item -ItemType Directory -Force -Path $repo
        $null = & git -C $repo init -q -b $Branch
        Write-WikiFiles $repo $Files
        $null = Invoke-WikiGit $repo add --all
        $null = Invoke-WikiGit $repo commit -q -m seed
        $repo
    }
    function New-WikiSource([string]$Name, [hashtable]$Files) {
        $dir = Join-Path $TestDrive $Name
        $null = New-Item -ItemType Directory -Force -Path $dir
        Write-WikiFiles $dir $Files
        $dir
    }
    # Runs the publisher in its own pwsh; GITHUB_OUTPUT is set to $Output, or removed when $Output is empty.
    function Invoke-WikiPublisher([string[]]$Arguments, [string]$Output) {
        $saved = $env:GITHUB_OUTPUT
        try {
            if ($Output) { $env:GITHUB_OUTPUT = $Output } else { Remove-Item Env:GITHUB_OUTPUT -ErrorAction SilentlyContinue }
            $lines = @(& $pwsh -NoProfile -File $publisher @Arguments 2>&1 | ForEach-Object { "$_" })
            [pscustomobject]@{ Code = $LASTEXITCODE; Lines = $lines }
        } finally {
            if ($null -ne $saved) { $env:GITHUB_OUTPUT = $saved } else { Remove-Item Env:GITHUB_OUTPUT -ErrorAction SilentlyContinue }
        }
    }

    It 'exits 2 with a usage line when a parameter is missing' {
        $source = New-WikiSource 'usage-src' @{ 'Home.md' = 'home' }
        $repo = New-WikiRepo 'usage-wiki' @{ 'Home.md' = 'home' }
        $usage = 'usage: Publish-MuxAgentWiki.ps1 -Source <dir> -WikiRepo <dir> -SourceSha <sha>'
        $result = foreach ($arguments in @(@('-WikiRepo', $repo, '-SourceSha', $sha), @('-Source', $source, '-SourceSha', $sha), @('-Source', $source, '-WikiRepo', $repo))) {
            $run = Invoke-WikiPublisher $arguments
            "$($run.Code) $($run.Lines -join ' | ')"
        }
        $result -join ' ; ' | Should BeExactly (@(1..3 | ForEach-Object { "2 $usage" }) -join ' ; ')
    }
    It 'exits 1 when Source is not a directory' {
        $repo = New-WikiRepo 'nosrc-wiki' @{ 'Home.md' = 'home' }
        $missing = Join-Path $TestDrive 'no-such-dir'
        $run = Invoke-WikiPublisher @('-Source', $missing, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ')" | Should BeExactly "1 wiki source is not a directory: $missing"
    }
    It 'exits 1 when WikiRepo is not a git worktree' {
        $source = New-WikiSource 'nogit-src' @{ 'Home.md' = 'home' }
        $plain = New-WikiSource 'nogit-wiki' @{ 'Home.md' = 'home' }
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $plain, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ')" | Should BeExactly "1 wiki destination is not a git worktree: $plain"
    }
    It 'exits 1 when WikiRepo is not on master' {
        $source = New-WikiSource 'branch-src' @{ 'Home.md' = 'new' }
        $repo = New-WikiRepo 'branch-wiki' @{ 'Home.md' = 'old' } -Branch main
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ') $([IO.File]::ReadAllText((Join-Path $repo 'Home.md')))" | Should BeExactly '1 wiki destination must be on master, not main old'
    }
    It 'exits 1 when Source has no Markdown pages' {
        $source = New-WikiSource 'empty-src' @{ 'notes.txt' = 'x'; 'sub/Inner.md' = 'inner' }
        $repo = New-WikiRepo 'empty-wiki' @{ 'Home.md' = 'home' }
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ') $(Test-Path -LiteralPath (Join-Path $repo 'Home.md'))" | Should BeExactly "1 wiki source has no Markdown pages: $source True"
    }
    It 'mirrors an added, a changed and a deleted page, ignoring subfolders and non-Markdown files' {
        $source = New-WikiSource 'mirror-src' @{ 'Home.md' = 'home v2'; 'Added.md' = 'added'; 'notes.txt' = 'not a page'; 'sub/Inner.md' = 'inner' }
        $repo = New-WikiRepo 'mirror-wiki' @{ 'Home.md' = 'home v1'; 'Gone.md' = 'gone'; 'keep.txt' = 'kept' }
        $output = Join-Path $TestDrive 'mirror-output.txt'
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha) $output
        $run.Code | Should Be 0
        $tracked = @(Invoke-WikiGit $repo ls-files) -join ','
        $status = @(Invoke-WikiGit $repo status --porcelain) -join ','
        "$tracked [$status] $(Invoke-WikiGit $repo show HEAD:Home.md) $(Invoke-WikiGit $repo rev-list --count HEAD)" | Should BeExactly 'Added.md,Home.md,keep.txt [] home v2 2'
        Invoke-WikiGit $repo log -1 --format=%s | Should BeExactly 'Publish wiki from 0123456'
        [IO.File]::ReadAllText($output) | Should BeExactly "changed=true`n"
    }
    It 'authors and commits the mirror as D0n9X1n <D0n9x1n@outlook.com>' {
        $source = New-WikiSource 'author-src' @{ 'Home.md' = 'new' }
        $repo = New-WikiRepo 'author-wiki' @{ 'Home.md' = 'old' }
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $(Invoke-WikiGit $repo log -1 '--format=%an <%ae> / %cn <%ce>')" | Should BeExactly '0 D0n9X1n <D0n9x1n@outlook.com> / D0n9X1n <D0n9x1n@outlook.com>'
    }
    It 'makes no commit and writes changed=false when the wiki already matches' {
        $source = New-WikiSource 'same-src' @{ 'Home.md' = 'home'; 'Other.md' = 'other' }
        $repo = New-WikiRepo 'same-wiki' @{ 'Home.md' = 'home'; 'Other.md' = 'other' }
        $before = Invoke-WikiGit $repo rev-parse HEAD
        $output = Join-Path $TestDrive 'same-output.txt'
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha) $output
        "$($run.Code) $($run.Lines -join ' | ') $((Invoke-WikiGit $repo rev-parse HEAD) -ceq $before)" | Should BeExactly "0 wiki already matches $sha True"
        [IO.File]::ReadAllText($output) | Should BeExactly "changed=false`n"
    }
    It 'runs without GITHUB_OUTPUT and never pushes' {
        $source = New-WikiSource 'push-src' @{ 'Home.md' = 'new' }
        $repo = New-WikiRepo 'push-wiki' @{ 'Home.md' = 'old' }
        $remote = Join-Path $TestDrive 'push-remote.git'
        $null = & git init -q --bare -b master $remote
        $null = Invoke-WikiGit $repo remote add origin $remote
        $null = Invoke-WikiGit $repo push -q origin master
        $seeded = Invoke-WikiGit $remote rev-parse master
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $((Invoke-WikiGit $remote rev-parse master) -ceq $seeded) $((Invoke-WikiGit $repo rev-parse HEAD) -cne $seeded)" | Should BeExactly '0 True True'
    }
    It 'mirrors Chinese pages byte for byte' {
        $body = "# 百科`n`n侧边栏与代理：``mux-agent list``。`n"
        $source = New-WikiSource 'utf8-src' @{ 'Home-zh-CN.md' = $body }
        $repo = New-WikiRepo 'utf8-wiki' @{ 'Home-zh-CN.md' = 'old' }
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        $want = [BitConverter]::ToString([IO.File]::ReadAllBytes((Join-Path $source 'Home-zh-CN.md')))
        "$($run.Code) $([BitConverter]::ToString([IO.File]::ReadAllBytes((Join-Path $repo 'Home-zh-CN.md'))) -ceq $want) $(@(Invoke-WikiGit $repo status --porcelain).Count)" | Should BeExactly '0 True 0'
    }
    It 'keeps pages whose names differ only in case apart, where the file system does' {
        $source = Join-Path $TestDrive 'case-src'
        $repo = Join-Path $TestDrive 'case-wiki'
        $null = New-Item -ItemType Directory -Force -Path $source, $repo
        # Windows folders are case-insensitive unless flagged; Linux folders always are case-sensitive; macOS ones usually are not.
        if ($IsWindows) { foreach ($dir in $source, $repo) { $null = & fsutil.exe file setCaseSensitiveInfo $dir enable 2>&1 } }
        [IO.File]::WriteAllText((Join-Path $source 'Page.md'), 'UPPER')
        if (Test-Path -LiteralPath (Join-Path $source 'page.md')) { Set-ItResult -Skipped -Because 'this file system folds case'; return }
        [IO.File]::WriteAllText((Join-Path $source 'page.md'), 'lower')
        $null = & git -C $repo init -q -b master
        Write-WikiFiles $repo @{ 'Page.md' = 'old upper' }
        [IO.File]::WriteAllText((Join-Path $repo 'page.md'), 'old lower')
        $null = Invoke-WikiGit $repo add --all
        $null = Invoke-WikiGit $repo commit -q -m seed
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $(@(Invoke-WikiGit $repo ls-files) -join ',') $(Invoke-WikiGit $repo show HEAD:Page.md) $(Invoke-WikiGit $repo show HEAD:page.md)" | Should BeExactly '0 Page.md,page.md UPPER lower'
    }
    It 'exits 1 and leaves the wiki untouched when a page cannot be read' {
        $source = New-WikiSource 'locked-src' @{ 'Home.md' = 'new'; 'Other.md' = 'other' }
        $repo = New-WikiRepo 'locked-wiki' @{ 'Home.md' = 'old' }
        $before = Invoke-WikiGit $repo rev-parse HEAD
        $locked = Join-Path $source 'Home.md'
        $hold = $null
        try {
            if ($IsWindows) { $hold = [IO.File]::Open($locked, 'Open', 'ReadWrite', 'None') } else { [IO.File]::SetUnixFileMode($locked, [IO.UnixFileMode]::None) }
            $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        } finally {
            if ($hold) { $hold.Dispose() } else { [IO.File]::SetUnixFileMode($locked, [IO.UnixFileMode]'UserRead, UserWrite') }
        }
        "$($run.Code) $($run.Lines[-1]) $((Invoke-WikiGit $repo rev-parse HEAD) -ceq $before) $(@(Invoke-WikiGit $repo status --porcelain).Count)" | Should BeExactly "1 cannot read wiki page: Home.md True 0"
    }
    It 'exits 1 when Source and WikiRepo are the same directory' {
        $repo = New-WikiRepo 'self-wiki' @{ 'Home.md' = 'home' }
        $before = Invoke-WikiGit $repo rev-parse HEAD
        $run = Invoke-WikiPublisher @('-Source', $repo, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ') $((Invoke-WikiGit $repo rev-parse HEAD) -ceq $before) $(@(Invoke-WikiGit $repo status --porcelain).Count)" | Should BeExactly "1 wiki source and destination are the same directory: $repo True 0"
    }
    It 'exits 1 and commits nothing when a page is a symbolic link' {
        $source = New-WikiSource 'link-src' @{ 'Home.md' = 'home' }
        $secret = New-WikiSource 'link-secret' @{ 'config' = 'token=FAKE_TOKEN' }
        $null = New-Item -ItemType SymbolicLink -Path (Join-Path $source 'Leak.md') -Target (Join-Path $secret 'config')
        $repo = New-WikiRepo 'link-wiki' @{ 'Home.md' = 'old' }
        $before = Invoke-WikiGit $repo rev-parse HEAD
        $run = Invoke-WikiPublisher @('-Source', $source, '-WikiRepo', $repo, '-SourceSha', $sha)
        "$($run.Code) $($run.Lines -join ' | ') $((Invoke-WikiGit $repo rev-parse HEAD) -ceq $before) $(@(Invoke-WikiGit $repo status --porcelain).Count) $(Test-Path -LiteralPath (Join-Path $repo 'Leak.md'))" | Should BeExactly '1 wiki page is a link, not a file: Leak.md True 0 False'
    }
    It 'resolves relative paths against the PowerShell location, not the process directory' {
        $base = Join-Path $TestDrive 'rel'
        $null = New-Item -ItemType Directory -Force -Path $base
        $null = New-WikiSource 'rel/wiki' @{ 'Home.md' = 'new' }
        $repo = New-WikiRepo 'rel/wiki-repo' @{ 'Home.md' = 'old' }
        $decoy = New-WikiSource 'decoy' @{ 'wiki-repo/Home.md' = 'decoy' }
        $script = "Set-Location -LiteralPath '$base'; [Environment]::CurrentDirectory = '$decoy'; & '$publisher' -Source wiki -WikiRepo wiki-repo -SourceSha $sha; exit `$LASTEXITCODE"
        $null = & $pwsh -NoProfile -Command $script 2>&1
        "$LASTEXITCODE $(Invoke-WikiGit $repo show HEAD:Home.md) $(@(Invoke-WikiGit $repo status --porcelain).Count) $([IO.File]::ReadAllText([IO.Path]::Combine($decoy, 'wiki-repo', 'Home.md')))" | Should BeExactly '0 new 0 decoy'
    }
}

Describe 'MuxAgents wiki publish workflow' {
    $workflowPath = [IO.Path]::Combine($repoRoot, '.github', 'workflows', 'publish-wiki.yml')
    $workflow = if (Test-Path -LiteralPath $workflowPath) { ConvertFrom-Yaml -Ordered ([IO.File]::ReadAllText($workflowPath)) } else { [ordered]@{} }
    $jobs = if ($workflow.jobs -is [Collections.IDictionary]) { $workflow.jobs } else { [ordered]@{} }
    $publish = if ($jobs.Contains('publish')) { $jobs['publish'] } else { [ordered]@{} }
    $steps = @(if ($publish.steps -is [Collections.IList]) { $publish.steps })
    # The comma keeps a one-element list a list; returned bare, it would unroll into its string.
    function Get-WikiValue($Map, [string]$Key) { if ($Map -is [Collections.IDictionary] -and $Map.Contains($Key)) { , $Map[$Key] } }
    # get_Keys(), not .Keys: a YAML entry named keys would shadow the property and hide the real entries.
    function Format-WikiMap($Map) { if ($Map -is [Collections.IDictionary]) { @($Map.get_Keys() | Sort-Object | ForEach-Object { "$_=$($Map[$_])" }) -join ',' } else { "$Map" } }
    function Get-WikiStepKind($Step) {
        $run = "$(Get-WikiValue $Step 'run')".Trim()
        if ("$(Get-WikiValue $Step 'uses')".StartsWith('actions/checkout@', [StringComparison]::Ordinal)) { 'checkout' }
        elseif ($run.StartsWith('git clone ', [StringComparison]::Ordinal)) { 'clone' }
        elseif ("$(Get-WikiValue $Step 'id')" -ceq 'mirror') { 'mirror' }
        elseif ($run.Contains(' push ')) { 'push' }
        else { 'other' }
    }
    function Get-WikiStep([string]$Kind) { @($steps | Where-Object { (Get-WikiStepKind $_) -ceq $Kind }) }
    It 'is named Publish wiki and runs only on pushes to main and by hand' {
        $on = $workflow['on']
        $push = Get-WikiValue $on 'push'
        $branches = Get-WikiValue $push 'branches'
        "$($workflow.name) | $(@($on.get_Keys() | Sort-Object) -join ',') | $(@($push.get_Keys()) -join ',') $($branches -is [Collections.IList]) $(@($branches) -join ',')" | Should BeExactly 'Publish wiki | push,workflow_dispatch | branches True main'
    }
    It 'reads contents at the top and writes contents only in its one job, publish' {
        $grants = @("top:$(Format-WikiMap $workflow.permissions)") + @(foreach ($name in @($jobs.get_Keys())) { "${name}:$(Format-WikiMap $jobs[$name].permissions)" })
        $grants -join ' ; ' | Should BeExactly 'top:contents=read ; publish:contents=write'
    }
    It 'runs one publish at a time and never cancels one in progress' {
        $concurrency = $workflow.concurrency
        $cancel = Get-WikiValue $concurrency 'cancel-in-progress'
        "$($concurrency -is [Collections.IDictionary]) $($concurrency.group) $($cancel -is [bool]) $cancel" | Should BeExactly 'True publish-wiki True False'
    }
    It 'publishes only from refs/heads/main, on ubuntu-latest' {
        "$(Get-WikiValue $publish 'if') | $(Get-WikiValue $publish 'runs-on')" | Should BeExactly "github.ref == 'refs/heads/main' | ubuntu-latest"
    }
    It 'checks out, clones, mirrors, then pushes' {
        @($steps | ForEach-Object { Get-WikiStepKind $_ }) -join ',' | Should BeExactly 'checkout,clone,mirror,push'
    }
    It 'pins checkout to a commit SHA with a version comment and does not persist credentials' {
        $uses = @($steps | Where-Object { $_ -is [Collections.IDictionary] -and $_.Contains('uses') } | ForEach-Object { "$($_.uses)" })
        $uses -join ' ; ' | Should BeExactly 'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1'
        $lines = @(if (Test-Path -LiteralPath $workflowPath) { [IO.File]::ReadAllLines($workflowPath) | Where-Object { $_ -match '^\s*(-\s+)?uses:' } })
        "$($lines.Count) $(@($lines | Where-Object { $_ -notmatch '\s#\s*v\d' }) -join ' ; ')" | Should BeExactly "$($uses.Count) "
        $persist = @(Get-WikiStep 'checkout' | ForEach-Object { Get-WikiValue $_.with 'persist-credentials' })
        "$($persist.Count) $($persist[0] -is [bool]) $($persist[0])" | Should BeExactly '1 True False'
    }
    It 'clones the wiki repository into wiki-repo with the workflow token' {
        $clone = @(Get-WikiStep 'clone' | ForEach-Object { "$($_.run)".Trim() + " | $(Get-WikiValue $_.env 'GH_TOKEN') | $(Get-WikiValue $_ 'shell')" })
        $clone -join ' ; ' | Should BeExactly 'git clone "https://x-access-token:${GH_TOKEN}@github.com/${GITHUB_REPOSITORY}.wiki.git" wiki-repo | ${{ secrets.GITHUB_TOKEN }} | '
    }
    It 'runs the publisher in pwsh as step mirror' {
        $mirror = @(Get-WikiStep 'mirror' | ForEach-Object { "$(Get-WikiValue $_ 'shell') | " + "$($_.run)".Trim() })
        $mirror -join ' ; ' | Should BeExactly 'pwsh | ./scripts/wiki/Publish-MuxAgentWiki.ps1 -Source wiki -WikiRepo wiki-repo -SourceSha $env:GITHUB_SHA'
    }
    It 'pushes the mirror to the wiki master only when it changed' {
        $push = @(Get-WikiStep 'push' | ForEach-Object { "$(Get-WikiValue $_ 'if') | " + "$($_.run)".Trim() })
        $push -join ' ; ' | Should BeExactly "steps.mirror.outputs.changed == 'true' | git -C wiki-repo push origin HEAD:master"
    }
}

Describe 'MuxAgents wiki pages' {
    $wikiRoot = Join-Path $repoRoot 'wiki'
    $pages = @(if (Test-Path -LiteralPath $wikiRoot) { Get-ChildItem -LiteralPath $wikiRoot -File -Filter '*.md' | Where-Object { $_.Extension -ceq '.md' } | ForEach-Object { $_.BaseName } | Sort-Object })
    $english = @($pages | Where-Object { -not $_.EndsWith('-zh-CN', [StringComparison]::Ordinal) })
    $chinese = @($pages | Where-Object { $_.EndsWith('-zh-CN', [StringComparison]::Ordinal) })
    function Get-WikiTwin([string]$Page) { if ($Page.EndsWith('-zh-CN', [StringComparison]::Ordinal)) { $Page.Substring(0, $Page.Length - 6) } else { "$Page-zh-CN" } }
    function Get-WikiLinks([string]$Page) { @([regex]::Matches([IO.File]::ReadAllText((Join-Path $wikiRoot "$Page.md")), '\[[^\]]*\]\(([^)\s]+)\)') | ForEach-Object { $_.Groups[1].Value }) }
    function Get-WikiDrift([string]$Page) {
        # '' when Page-zh-CN matches Page line for line: the same count and, per line, the same shape, the same code
        # spans and numbers, and Chinese prose; inside a ``` fence the lines are identical instead. Else
        # '<en lines> <zh lines> <drifting line numbers>'.
        $en = [IO.File]::ReadAllLines((Join-Path $wikiRoot "$Page.md"))
        $zh = [IO.File]::ReadAllLines((Join-Path $wikiRoot "$Page-zh-CN.md"))
        $fenced = $false
        $drift = @(for ($i = 0; $i -lt [Math]::Max($en.Count, $zh.Count); $i++) {
            $a = "$($en[$i])"; $b = "$($zh[$i])"
            if ($a.StartsWith('```') -or $fenced) {
                if ($a.StartsWith('```')) { $fenced = -not $fenced }
                if ($a -cne $b) { $i + 1 }
                continue
            }
            $codeA = @([regex]::Matches($a, '`[^`]+`|\d+(\.\d+)*') | ForEach-Object Value | Sort-Object -Unique) -join ' '
            $codeB = @([regex]::Matches($b, '`[^`]+`|\d+(\.\d+)*') | ForEach-Object Value | Sort-Object -Unique) -join ' '
            $shape = { param($l) if (-not $l.Trim()) { 'blank' } elseif ($l -match '^(#+|\||-|\d+\.) ?') { $Matches[1] } else { 'text' } }
            $prose = $b.Trim() -and $b -notmatch '^\[[^\]]+\]\([^)]+\)$' -and $b -notmatch '^\|[-|]+\|$'
            if ((& $shape $a) -cne (& $shape $b) -or $codeA -cne $codeB -or ($prose -and $b -notmatch '\p{IsCJKUnifiedIdeographs}')) { $i + 1 }
        })
        if ($en.Count -ne $zh.Count -or $drift.Count) { "$($en.Count) $($zh.Count) $($drift -join ',')" } else { '' }
    }
    It 'has Home, the three design pages and the v0.1.0 release notes in English and Chinese' {
        @('Home', 'Home-zh-CN', 'Design-Foundation', 'Design-Foundation-zh-CN', 'Design-Cross-Platform', 'Design-Cross-Platform-zh-CN', 'Design-Side-Bar', 'Design-Side-Bar-zh-CN', 'Release-v0.1.0', 'Release-v0.1.0-zh-CN' | Where-Object { $_ -cnotin $pages }) -join ',' | Should BeExactly ''
    }
    It 'pairs every English page with its -zh-CN page and back' {
        "$($pages.Count -gt 0) $(@($pages | Where-Object { (Get-WikiTwin $_) -cnotin $pages }) -join ',')" | Should BeExactly 'True '
    }
    It 'links every page to its twin within its first five lines' {
        $missing = @(foreach ($page in $pages) {
            $head = @([IO.File]::ReadAllLines((Join-Path $wikiRoot "$page.md")) | Select-Object -First 5) -join "`n"
            # The release notes become a release body on github.com, so they link their twin by absolute URL.
            if (-not ($head.Contains("($(Get-WikiTwin $page))") -or $head.Contains("(https://github.com/D0n9X1n/mux-agents/wiki/$(Get-WikiTwin $page))"))) { $page }
        })
        "$($english.Count -gt 0) $($chinese.Count -gt 0) $($missing -join ',')" | Should BeExactly 'True True '
    }
    It 'points every page link inside wiki/ at an existing page' {
        $links = @(foreach ($page in $pages) { foreach ($link in Get-WikiLinks $page) { if ($link -notmatch '^([a-z][a-z0-9+.-]*:|#)') { [pscustomobject]@{ Page = $page; Target = ($link -split '#', 2)[0] } } } })
        $broken = @($links | Where-Object { $_.Target -cnotin $pages } | ForEach-Object { "$($_.Page)->$($_.Target)" })
        "$($links.Count -gt 0) $($broken -join ' ; ')" | Should BeExactly 'True '
    }
    It 'Home links the design pages and the release notes in its own language' {
        $want = @{ 'Home' = @('Design-Foundation', 'Design-Cross-Platform', 'Design-Side-Bar', 'Release-v0.1.0'); 'Home-zh-CN' = @('Design-Foundation-zh-CN', 'Design-Cross-Platform-zh-CN', 'Design-Side-Bar-zh-CN', 'Release-v0.1.0-zh-CN') }
        (@(foreach ($page in 'Home', 'Home-zh-CN') { $links = Get-WikiLinks $page; @($want[$page] | Where-Object { $links -cnotcontains $_ }) -join ',' }) -join ';') | Should BeExactly ';'
    }
    It 'keeps wiki/ flat: no folders, since the mirror publishes top-level pages only' {
        @(if (Test-Path -LiteralPath $wikiRoot) { Get-ChildItem -LiteralPath $wikiRoot -Directory -Force | ForEach-Object { $_.Name } }) -join ',' | Should BeExactly ''
        @(& git -C $repoRoot ls-files -- wiki | Where-Object { $_ -match '^wiki/.+/' }) -join ',' | Should BeExactly ''
    }
    It 'Home states the platforms and the side bar of v0.1.0, in both languages' {
        $status = foreach ($page in 'Home', 'Home-zh-CN') { @([IO.File]::ReadAllLines((Join-Path $wikiRoot "$page.md")) | Where-Object { $_ -match '^v0\.1\.0 ' }) -join ' / ' }
        $status -join ' ; ' | Should BeExactly 'v0.1.0 runs on Windows with rmux 0.10.0 and on Linux and macOS with tmux 3.2 or later. It shows a side bar (#4): a multiplexer split for every client and a pane in Claude Code. ; v0.1.0 在 Windows 上配合 rmux 0.10.0 运行，在 Linux 和 macOS 上配合 tmux 3.2 或更高版本运行。它提供侧边栏（#4）：每个客户端都可用的多路复用器分屏，以及 Claude Code 中的窗格。'
    }
    It 'Home, the release notes and the README state the .NET named-mutex advisory as published, in both languages' {
        # Catches: a lock limitation that understates GHSA-2j93-m82w-73j2 (file overwrite, loosened permissions, privilege gain).
        $en = 'On Linux and macOS, .NET older than 8.0.32, 9.0.21 or 10.0.13 has an elevation-of-privilege flaw in its named mutexes ([GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)), which mux-agents uses for its `lock` check: another local user can erase or replace file contents, loosen folder permissions or gain higher privileges, and can make mux-agent commands time out with check `lock`. Check with `[Environment]::Version` in pwsh and use a PowerShell built on a fixed .NET.'
        $zh = '在 Linux 和 macOS 上，早于 8.0.32、9.0.21 或 10.0.13 的 .NET 在命名互斥体中存在权限提升漏洞（[GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)），mux-agents 的 `lock` 检查就使用命名互斥体：其他本地用户可以清除或替换文件内容、放宽文件夹权限或获得更高权限，还可以让 mux-agent 命令以 `lock` 检查超时。请在 pwsh 中用 `[Environment]::Version` 检查，并使用基于已修复 .NET 的 PowerShell。'
        $want = [ordered]@{ 'wiki/Home.md' = $en; 'wiki/Home-zh-CN.md' = $zh; 'wiki/Release-v0.1.0.md' = $en; 'wiki/Release-v0.1.0-zh-CN.md' = $zh; 'README.md' = $en }
        (@(foreach ($path in $want.Keys) { [IO.File]::ReadAllLines((Join-Path $repoRoot $path)) -ccontains $want[$path] }) -join ' ') | Should BeExactly 'True True True True True'
        (@(foreach ($path in $want.Keys) { if ([IO.File]::ReadAllText((Join-Path $repoRoot $path)).Contains('Nothing leaks')) { $path } }) -join ',') | Should BeExactly ''
    }
    It 'Design-Foundation keeps its sections, and its Chinese twin matches it line for line' {
        $en = [IO.File]::ReadAllLines((Join-Path $wikiRoot 'Design-Foundation.md'))
        @($en | Where-Object { $_ -match '^## ' }) -join ',' | Should BeExactly '## Purpose,## Decisions,## Changes,## Component map,## Behavior and its tests,## Must-pass,## Non-goals,## Prior art'
        Get-WikiDrift 'Design-Foundation' | Should BeExactly ''
    }
    It 'Design-Cross-Platform keeps its sections and states the Unix rules as built; its Chinese twin matches it line for line' {
        # Catches: a lost section, a stale rule (the old Local\ lock name, kill on a start re-read, chmod-only folders), drift.
        @([IO.File]::ReadAllLines((Join-Path $wikiRoot 'Design-Cross-Platform.md')) | Where-Object { $_ -match '^## ' }) -join ',' | Should BeExactly '## Purpose,## Decisions,## Unix provider members,## Changed behavior,## Component map,## Tests,## Must-pass,## Non-goals,## Prior art'
        $found = foreach ($page in 'Design-Cross-Platform', 'Design-Cross-Platform-zh-CN') {
            $text = [IO.File]::ReadAllText((Join-Path $wikiRoot "$page.md"))
            "$page missing=$(@('`Global\MuxAgents-<uid>-<hash>`', '`mkdir(2)`', '`tmux -e`', '`realpath`', 'ESRCH' | Where-Object { -not $text.Contains($_) }) -join ',')"
        }
        $found -join ' ; ' | Should BeExactly 'Design-Cross-Platform missing= ; Design-Cross-Platform-zh-CN missing='
        Get-WikiDrift 'Design-Cross-Platform' | Should BeExactly ''
    }
    It 'Release-v0.1.0 names the release zip, all four install commands and only absolute links; its Chinese twin matches it line for line' {
        # Catches: a zip name the release workflow does not build, an install line that drifts from the README, a
        # wiki-relative link that breaks once the page is the GitHub release body, drift.
        $install = 'claude plugin marketplace add D0n9X1n/mux-agents', 'claude plugin install mux-agents@mux-agents', 'copilot plugin marketplace add D0n9X1n/mux-agents', 'copilot plugin install mux-agents@mux-agents'
        $found = foreach ($page in 'Release-v0.1.0', 'Release-v0.1.0-zh-CN') {
            $lines = [IO.File]::ReadAllLines((Join-Path $wikiRoot "$page.md"))
            $text = $lines -join "`n"
            "$page zip=$($text.Contains('`mux-agents-v0.1.0.zip`')) install=$(@($install | Where-Object { $lines -cnotcontains $_ }).Count) relative=$(@(Get-WikiLinks $page | Where-Object { $_ -notmatch '^https://' }) -join ',')"
        }
        $found -join ' ; ' | Should BeExactly 'Release-v0.1.0 zip=True install=0 relative= ; Release-v0.1.0-zh-CN zip=True install=0 relative='
        (@(foreach ($page in 'Release-v0.1.0', 'Release-v0.1.0-zh-CN') { [IO.File]::ReadAllLines((Join-Path $wikiRoot "$page.md"))[2] }) -join ' ; ') | Should BeExactly '[简体中文](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.1.0-zh-CN) ; [English](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.1.0)'
        Get-WikiDrift 'Release-v0.1.0' | Should BeExactly ''
    }
    It 'Design-Side-Bar states the pane and watch as built, keeps its sections, and its Chinese twin matches it line for line' {
        # Catches: a blanked or stale line (the pane's types, the -Text reply, the test files, watch's three ends and its
        # terminal rule), a lost section and drift between the languages.
        $en = [IO.File]::ReadAllLines((Join-Path $wikiRoot 'Design-Side-Bar.md'))
        @($en | Where-Object { $_ -match '^## ' }) -join ',' | Should BeExactly '## Purpose,## Decisions (PM''s calls under the release goal),## Verbs,## Claude Code pane,## Skill,## Component map,## Tests,## Must-pass,## Non-goals,## Prior art'
        $found = foreach ($page in 'Design-Side-Bar', 'Design-Side-Bar-zh-CN') {
            $text = [IO.File]::ReadAllText((Join-Path $wikiRoot "$page.md")) -replace "`r`n", "`n"
            $missing = @('`types/index.d.ts`', '`-Text`', '`pane-closed`', '`output-closed`', '`probe-failed`', '`usage`', '`1 sidebar`', '`/mux-agents-pane`', '`tests/claude/register.test.tsx`', '`tests/claude/board.test.ts`', '`claude plugin test .`' | Where-Object { -not $text.Contains($_) })
            "$page blank-run=$($text.Contains("`n`n`n")) missing=$($missing -join ',')"
        }
        $found -join ' ; ' | Should BeExactly 'Design-Side-Bar blank-run=False missing= ; Design-Side-Bar-zh-CN blank-run=False missing='
        Get-WikiDrift 'Design-Side-Bar' | Should BeExactly ''
    }
    It 'tracks no path under docs/' {
        @(& git -C $repoRoot ls-files -- docs) -join ',' | Should BeExactly ''
    }
}

Describe 'MuxAgents wiki contributor docs' {
    It 'CONTRIBUTING has a Documentation section naming wiki/, both page names and the Publish wiki workflow' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'CONTRIBUTING.md'))
        $start = [array]::IndexOf($lines, '## Documentation')
        $section = if ($start -ge 0) { @($lines[($start + 1)..($lines.Count - 1)] | ForEach-Object -Begin { $open = $true } -Process { if ($_ -match '^## ') { $open = $false }; if ($open) { $_ } }) -join "`n" } else { '' }
        "$($start -ge 0) $(@('`wiki/`', '`Page.md`', '`Page-zh-CN.md`', 'Publish wiki', 'main' | Where-Object { -not $section.Contains($_) }) -join ' ; ')" | Should BeExactly 'True '
    }
    It 'README Layout table has a wiki/ row and a scripts/wiki/ row' {
        $lines = [IO.File]::ReadAllLines((Join-Path $repoRoot 'README.md'))
        @('| `wiki/` |', '| `scripts/wiki/` |' | ForEach-Object { $prefix = $_; @($lines | Where-Object { $_.StartsWith($prefix, [StringComparison]::Ordinal) }).Count }) -join ',' | Should BeExactly '1,1'
    }
}
