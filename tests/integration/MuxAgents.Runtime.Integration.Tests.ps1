# The runtime the integration job runs on passes the #25 check, so the real entry and launcher in the other integration
# suites run unrefused. A failure prints the live version.
Describe 'MuxAgents integration runtime' -Tag 'integration' {
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    . ([IO.Path]::Combine($repoRoot, 'scripts', 'MuxAgents.ps1'))
    It 'the job runtime is a fixed .NET, and the real entry never answers check runtime' {
        $version = Get-MuxAgentRuntimeVersion
        $reply = @(& ([IO.Path]::Combine($PSHOME, 'pwsh')) -NoProfile -File ([IO.Path]::Combine($repoRoot, 'scripts', 'mux-agent.ps1')) list)[0] | ConvertFrom-Json
        "$version=$(Get-MuxAgentRuntimeVerdict -Version $version) [$(Get-MuxAgentRuntimeAdvisory -Platform (Get-MuxAgentPlatform) -Version $version -Environment @{})] $($reply.check -cne 'runtime')" | Should BeExactly "$version=fixed [] True" } }
