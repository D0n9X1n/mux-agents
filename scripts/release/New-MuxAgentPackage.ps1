# Writes the plugin zip for Ref: manifests, skills, scripts, hooks (when Ref has them), README.md and LICENSE.
# scripts/release is export-ignored. Exit code is git archive's.
param([Parameter(Mandatory)][string]$Ref, [Parameter(Mandatory)][string]$OutFile)
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$OutFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutFile)
$paths = @('.claude-plugin', 'skills', 'scripts', 'README.md', 'LICENSE')
if (git -C $root ls-tree --name-only $Ref -- hooks) { $paths += 'hooks' }
git -C $root archive --format=zip "--output=$OutFile" $Ref -- @paths
exit $LASTEXITCODE
