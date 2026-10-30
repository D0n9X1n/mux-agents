# Wiki tests: the publisher that mirrors wiki/ into the GitHub wiki repository. Untagged: they run on every OS.
$repoRoot = Split-Path -Parent $PSScriptRoot
$pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName

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
}
