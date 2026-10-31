# Runtime check tests (#25). Untagged Describes run on every OS; the linux and macos ones only there.
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')
function Get-RuntimeOutcome {
    param([Parameter(Mandatory)][scriptblock]$Action)
    try { $r = & $Action; if ($null -eq $r) { 'null' } else { "text $r" } } catch { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" } }
function Set-RuntimeOverride {
    param($Value)
    [Environment]::SetEnvironmentVariable('MUX_AGENTS_ALLOW_AFFECTED_DOTNET', $(if ($null -eq $Value) { [NullString]::Value } else { [string]$Value })) }
Describe 'MuxAgents runtime verdict' {
    It 'calls only GA or RC 2+ builds at or past the fixed patch fixed, and anything malformed affected' {
        $cases = @(@('8.0.31', 'affected'), @('8.0.32', 'fixed'), @('9.0.20', 'affected'), @('9.0.21', 'fixed'), @('10.0.12', 'affected'), @('10.0.13', 'fixed'),
            @('10.0.14-servicing.26522.126+0427b9a', 'fixed'), @('11.0.0-preview.7.1', 'affected'), @('11.0.0-rc.1.26425.128', 'affected'), @('11.0.0-rc.2.26504.105', 'fixed'),
            @('11.0.0-rc.10.1', 'fixed'), @('11.0.0', 'fixed'), @('11.0.1-servicing.1', 'fixed'), @('12.0.0', 'fixed'), @('7.0.20', 'affected'), @('10.1.0', 'affected'),
            @('10.0.13-weird.1', 'affected'), @('10.0.13x', 'affected'), @('99999999999.0.0', 'affected'), @("10.0.14`n", 'affected'), @('', 'affected'), @("$([char]0x0661)0.0.14", 'affected'), @('11.0.0-rc.0000000002.1', 'fixed'), @('11.0.0-rc.99999999999.1', 'fixed'), @('11.0.0-rc.01.1', 'affected'))
        $got = foreach ($c in $cases) { "$($c[0])=$(Get-MuxAgentRuntimeVerdict -Version $c[0])" }
        ($got -join ';') | Should BeExactly ((@($cases | ForEach-Object { "$($_[0])=$($_[1])" })) -join ';') } }
