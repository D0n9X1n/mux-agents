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
Describe 'MuxAgents runtime version' {
    It 'reads the informational version of the assembly holding [object], whose numeric part is [Environment]::Version' {
        $live = [Reflection.CustomAttributeExtensions]::GetCustomAttribute([object].Assembly, [Reflection.AssemblyInformationalVersionAttribute]).InformationalVersion
        $read = Get-MuxAgentRuntimeVersion
        "$($read -ceq $live) $(($read -split '[-+]', 2)[0] -ceq [Environment]::Version.ToString(3)) $($read -cne $PSVersionTable.PSVersion.ToString())" | Should BeExactly 'True True True' } }
Describe 'MuxAgents runtime advisory' {
    It 'returns $null on Windows or a fixed .NET, the advisory for an exact 1, and refuses 2 runtime otherwise' {
        $got = foreach ($c in @(@('windows', '10.0.12', 'unset'), @('linux', '10.0.13', 'unset'), @('linux', '10.0.13', '1'), @('macos', '10.0.13', '1'), @('linux', '10.0.12', '1'), @('macos', '10.0.12', '1'),
                @('linux', '10.0.12', 'true'), @('linux', '10.0.12', '0'), @('linux', '10.0.12', ''), @('macos', '10.0.12', 'unset'), @('macos', '10.0.13', 'unset'), @('linux', '10.0.12', "1$([char]0x200B)"))) {
            $vars = @{}; if ($c[2] -cne 'unset') { $vars['MUX_AGENTS_ALLOW_AFFECTED_DOTNET'] = $c[2] }
            Get-RuntimeOutcome { Get-MuxAgentRuntimeAdvisory -Platform $c[0] -Version $c[1] -Environment $vars } }
        $text = 'text .NET 10.0.12 is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1.'
        $refuse = { param($os) "2 runtime mux-agents refuses .NET 10.0.12 on ${os}: GHSA-2j93-m82w-73j2 (CVE-2026-96231). Install PowerShell 7.4.21, 7.5.12, 7.6.7 or later, or set MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1 to run anyway." }
        ($got -join "`n") | Should BeExactly ((@('null', 'null', 'null', 'null', $text, $text, (& $refuse linux), (& $refuse linux), (& $refuse linux), (& $refuse macos), 'null', (& $refuse linux))) -join "`n") } }
Describe 'MuxAgents runtime initialize' {
    # Pester 4 keeps a Mock to the end of its Context, so each mock has its own.
    Context 'affected, no override' {
        Mock New-MuxAgentProviders { throw 'must not build' }
        Mock Get-MuxAgentPlatformMembers { throw 'must not ask' }
        It 'refuses 2 runtime on linux and macos before any provider, mutex or state folder' {
            $saved = $env:MUX_AGENTS_ALLOW_AFFECTED_DOTNET
            try {
                Set-RuntimeOverride $null
                $got = foreach ($os in 'linux', 'macos') { $b = Initialize-MuxAgentProviders -Entry 'x/mux-agent.ps1' -Platform $os -RuntimeVersion '10.0.12'; "$($null -eq $b.P) $($b.Failure.code) $($b.Failure.check) $($b.Failure.Contains('advisory'))" }
                ($got -join ';') | Should BeExactly 'True 2 runtime False;True 2 runtime False'
                Assert-MockCalled New-MuxAgentProviders -Times 0 -Exactly -Scope It
                Assert-MockCalled Get-MuxAgentPlatformMembers -Times 0 -Exactly -Scope It }
            finally { Set-RuntimeOverride $saved } } }
    Context 'override, then a state refusal' {
        Mock New-MuxAgentProviders { Exit-MuxAgentCommand 2 'state' 'stub state refusal' }
        It 'keeps advisory on a later build failure' {
            $saved = $env:MUX_AGENTS_ALLOW_AFFECTED_DOTNET
            try { Set-RuntimeOverride '1'; $b = Initialize-MuxAgentProviders -Entry 'x/mux-agent.ps1' -Platform linux -RuntimeVersion '10.0.12' }
            finally { Set-RuntimeOverride $saved }
            Format-MuxAgentResult -Data $b.Failure | Should BeExactly '{"ok":false,"verb":null,"code":2,"check":"state","message":"stub state refusal","advisory":".NET 10.0.12 is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1."}' } }
    Context 'built providers' {
        Mock New-MuxAgentProviders { [pscustomobject]@{ StateRoot = 's' } }
        It 'sets P.Advisory to the advisory text, or $null on windows and a fixed .NET' {
            $saved = $env:MUX_AGENTS_ALLOW_AFFECTED_DOTNET
            try {
                Set-RuntimeOverride '1'
                $got = foreach ($c in @(@('linux', '10.0.12'), @('windows', '10.0.12'), @('macos', '10.0.14'))) { $b = Initialize-MuxAgentProviders -Entry 'x/mux-agent.ps1' -Platform $c[0] -RuntimeVersion $c[1]; "$($null -eq $b.Failure) [$($b.P.Advisory)]" } }
            finally { Set-RuntimeOverride $saved }
            ($got -join ';') | Should BeExactly 'True [.NET 10.0.12 is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1.];True [];True []'
            Assert-MockCalled New-MuxAgentProviders -ParameterFilter { $Entry -ceq 'x/mux-agent.ps1' } -Times 3 -Exactly -Scope It } }
    It 'defaults Platform and RuntimeVersion to the live readers; the entry and launcher pass only -Entry; only Initialize calls New-MuxAgentProviders' {
        $params = (Get-Command Initialize-MuxAgentProviders -CommandType Function).ScriptBlock.Ast.Body.ParamBlock.Parameters
        (@($params | ForEach-Object { "$($_.Name.VariablePath.UserPath)=$($_.DefaultValue.Extent.Text)" }) -join ' ') | Should BeExactly 'Entry= Platform=(Get-MuxAgentPlatform) RuntimeVersion=(Get-MuxAgentRuntimeVersion)'
        $found = foreach ($file in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1', 'mux-agent.ps1', 'mux-agent-launcher.ps1') {
            $ast = [Management.Automation.Language.Parser]::ParseFile([IO.Path]::Combine($repoRoot, 'scripts', $file), [ref]$null, [ref]$null)
            foreach ($c in $ast.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] }, $true)) {
                $name = "$($c.GetCommandName())"
                if ($name -ceq 'Initialize-MuxAgentProviders') { "$file init $(@($c.CommandElements | Where-Object { $_ -is [Management.Automation.Language.CommandParameterAst] } | ForEach-Object ParameterName) -join ',')" }
                elseif ($name -ceq 'New-MuxAgentProviders') { $f = $c.Parent; while ($f -and $f -isnot [Management.Automation.Language.FunctionDefinitionAst]) { $f = $f.Parent }; "$file new in $($f.Name)" } } }
        ($found -join ';') | Should BeExactly 'MuxAgents.ps1 new in Initialize-MuxAgentProviders;mux-agent.ps1 init Entry;mux-agent-launcher.ps1 init Entry' }
    It 'never asks for platform members while the library loads or while Initialize refuses' {
        # Catches: the load-time Get-MuxAgentPlatformMembers call for the Windows lock name, which ran before the check.
        $file = Join-Path $TestDrive 'load-probe.ps1'
        [IO.File]::WriteAllLines($file, [string[]]@('$global:hits = 0', '$null = Set-PSBreakpoint -Command Get-MuxAgentPlatformMembers -Action { $global:hits++ }',
            ". '$([IO.Path]::Combine($repoRoot, 'scripts', 'MuxAgents.ps1').Replace("'", "''"))'", '$loaded = $global:hits',
            '$b = Initialize-MuxAgentProviders -Entry ''x/mux-agent.ps1'' -Platform linux -RuntimeVersion ''10.0.12''', '"$loaded $global:hits $($b.Failure.check)"'))
        $saved = $env:MUX_AGENTS_ALLOW_AFFECTED_DOTNET
        try { Set-RuntimeOverride $null; $got = & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -File $file } finally { Set-RuntimeOverride $saved }
        "$got" | Should BeExactly '0 0 runtime' } }
function Get-RuntimeOverrideBuild {
    # The real factory on this Unix OS with the override and an affected version; returns 'built platform advisory'.
    $saved = @{ O = $env:MUX_AGENTS_ALLOW_AFFECTED_DOTNET; S = $env:MUX_AGENTS_STATE }; $lock = $script:MuxAgentLockName; $p = $script:MuxAgentProviders
    $state = Join-Path $TestDrive 'override-state'; $null = [IO.Directory]::CreateDirectory($state)
    try { Set-RuntimeOverride '1'; $env:MUX_AGENTS_STATE = $state
        $b = Initialize-MuxAgentProviders -Entry (Join-Path $repoRoot 'scripts/mux-agent.ps1') -RuntimeVersion '10.0.12'
        "$($null -eq $b.Failure) $($b.P.Platform) $($b.P.Advisory)" }
    finally { Set-RuntimeOverride $saved.O; [Environment]::SetEnvironmentVariable('MUX_AGENTS_STATE', $(if ($null -eq $saved.S) { [NullString]::Value } else { $saved.S })); $script:MuxAgentLockName = $lock; $script:MuxAgentProviders = $p } }
$overrideWant = ' .NET 10.0.12 is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1.'
Describe 'MuxAgents runtime override on linux' -Tag 'linux' {
    It 'builds real providers on an affected version with the override and sets P.Advisory' { Get-RuntimeOverrideBuild | Should BeExactly "True linux$overrideWant" } }
Describe 'MuxAgents runtime override on macos' -Tag 'macos' {
    It 'builds real providers on an affected version with the override and sets P.Advisory' { Get-RuntimeOverrideBuild | Should BeExactly "True macos$overrideWant" } }
