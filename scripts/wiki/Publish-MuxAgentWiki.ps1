# Mirrors the top-level *.md pages of Source into the wiki worktree WikiRepo (branch master) and commits them as D0n9X1n.
# Never pushes. Writes changed=true|false to $env:GITHUB_OUTPUT when set. Exit 0 done, 1 refused or git failed, 2 usage.
param([string]$Source, [string]$WikiRepo, [string]$SourceSha)
function Stop-WikiPublish([int]$Code, [string]$Reason) { [Console]::Error.WriteLine($Reason); exit $Code }
function Write-WikiOutput([string]$Line) { if ($env:GITHUB_OUTPUT) { [IO.File]::AppendAllText($env:GITHUB_OUTPUT, "$Line`n") } }

if (-not $Source -or -not $WikiRepo -or -not $SourceSha) { Stop-WikiPublish 2 'usage: Publish-MuxAgentWiki.ps1 -Source <dir> -WikiRepo <dir> -SourceSha <sha>' }
# Resolved against the PowerShell location: .NET file calls resolve relative paths against the process directory instead.
$Source = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Source)
$WikiRepo = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($WikiRepo)
if (-not (Test-Path -LiteralPath $Source -PathType Container)) { Stop-WikiPublish 1 "wiki source is not a directory: $Source" }
if (-not (Test-Path -LiteralPath (Join-Path $WikiRepo '.git'))) { Stop-WikiPublish 1 "wiki destination is not a git worktree: $WikiRepo" }
if ([IO.Path]::TrimEndingDirectorySeparator($Source) -ieq [IO.Path]::TrimEndingDirectorySeparator($WikiRepo)) { Stop-WikiPublish 1 "wiki source and destination are the same directory: $WikiRepo" }
$branch = "$(git -C $WikiRepo branch --show-current)"
if ($branch -cne 'master') { Stop-WikiPublish 1 "wiki destination must be on master, not $branch" }
$pages = @(Get-ChildItem -LiteralPath $Source -File -Filter '*.md' | Where-Object { $_.Extension -ceq '.md' })
if ($pages.Count -eq 0) { Stop-WikiPublish 1 "wiki source has no Markdown pages: $Source" }

# Every page is read before the destination changes, so a page that cannot be read leaves the wiki as it was.
# A link could point outside wiki/, at the clone's token-bearing .git/config among others, so links are refused.
$content = [ordered]@{}
foreach ($page in $pages) {
    if ($page.LinkTarget) { Stop-WikiPublish 1 "wiki page is a link, not a file: $($page.Name)" }
    try { $content[$page.Name] = [IO.File]::ReadAllBytes($page.FullName) } catch { Stop-WikiPublish 1 "cannot read wiki page: $($page.Name)" }
}
try {
    Get-ChildItem -LiteralPath $WikiRepo -File -Force -Filter '*.md' | Where-Object { $_.Extension -ceq '.md' } | Remove-Item -Force -ErrorAction Stop
    # Written, not copied: a copy keeps the source mtime, and git would take a same-size change in the same second for unchanged.
    foreach ($name in $content.Keys) { [IO.File]::WriteAllBytes((Join-Path $WikiRepo $name), $content[$name]) }
} catch { Stop-WikiPublish 1 "cannot write the wiki destination: $($_.Exception.Message)" }
git -C $WikiRepo add --all
if ($LASTEXITCODE -ne 0) { Stop-WikiPublish 1 'git add failed in the wiki destination' }
git -C $WikiRepo diff --cached --quiet
if ($LASTEXITCODE -eq 0) {
    Write-WikiOutput 'changed=false'
    "wiki already matches $SourceSha"
    exit 0
}
git -C $WikiRepo -c user.name=D0n9X1n -c user.email=D0n9x1n@outlook.com commit -q -m "Publish wiki from $($SourceSha.Substring(0, [Math]::Min(7, $SourceSha.Length)))"
if ($LASTEXITCODE -ne 0) { Stop-WikiPublish 1 'git commit failed in the wiki destination' }
Write-WikiOutput 'changed=true'
exit 0
