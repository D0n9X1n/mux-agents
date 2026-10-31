# mux-agent entry: one verb per call, one JSON object (or -Text) on stdout, exit 0 ok, 1 error, 2 refused or usage.
. (Join-Path $PSScriptRoot 'MuxAgents.ps1')

$refusal = Get-MuxAgentEntryRefusal -Platform (Get-MuxAgentPlatform)
if ($refusal) {
    [Console]::Out.WriteLine($refusal)
    exit 2
}
$built = Initialize-MuxAgentProviders -Entry $PSCommandPath
$result = if ($built.Failure) { [pscustomobject]@{ Code = $built.Failure.code; Text = (Format-MuxAgentResult -Data $built.Failure) } } else { Invoke-MuxAgentCommand -P $built.P -Arguments $args }
[Console]::Out.WriteLine($result.Text)
exit $result.Code
