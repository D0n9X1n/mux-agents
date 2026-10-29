# rmux-agent entry: one verb per call, one JSON object (or -Text) on stdout, exit 0 ok, 1 error, 2 refused or usage.
. (Join-Path $PSScriptRoot 'RmuxAgents.ps1')

if (-not $IsWindows) {
    [Console]::Out.WriteLine('{"ok":false,"verb":null,"code":2,"check":"platform","message":"rmux-agents 0.1.0 runs on Windows with rmux only"}')
    exit 2
}
$result = Invoke-RmuxAgentCommand -P (New-RmuxAgentProviders -Entry $PSCommandPath) -Arguments $args
[Console]::Out.WriteLine($result.Text)
exit $result.Code
