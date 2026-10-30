# Release gate: exit 0 when Tag may be released, else exit 1 naming the first failed check. The workflow gathers
# the inputs with git and gh api; this script reads nothing and writes one line.
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$Sha,
    [Parameter(Mandatory)][bool]$IsAncestorOfMain,
    [AllowEmptyCollection()][object[]]$Runs = @(),
    [Parameter(Mandatory)][AllowEmptyString()][string]$ManifestVersion)
$pushRuns = @($Runs | Where-Object { $_.event -ceq 'push' -and $_.head_sha -ceq $Sha })
$reason = if (-not $IsAncestorOfMain) { "$Tag is not on main" }
elseif ($pushRuns.Count -eq 0) { "no CI push run for $Sha" }
elseif (@($pushRuns | Where-Object { $_.conclusion -ceq 'success' }).Count -eq 0) { "no successful CI push run for $Sha" }
elseif ("v$ManifestVersion" -cne $Tag) { "plugin.json version $ManifestVersion does not match $Tag" }
if ($reason) { "refused: $reason"; exit 1 }
"release ok: $Tag at $Sha"
exit 0
