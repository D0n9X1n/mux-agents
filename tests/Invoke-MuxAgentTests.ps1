# Runs the Pester 4.10.1 suites under Path. Each OS skips the other OSes' tags; integration suites run only, and alone, with -Integration.
param([string]$Path = $PSScriptRoot, [switch]$Integration)
$ErrorActionPreference = 'Stop'
# Pester 4's TestDrive lives under $env:TEMP, which Linux and macOS leave unset.
if (-not $env:TEMP) { $env:TEMP = [IO.Path]::GetTempPath().TrimEnd([IO.Path]::DirectorySeparatorChar) }
Import-Module Pester -RequiredVersion 4.10.1
if ((Get-Module Pester).Version -ne [version]'4.10.1') { throw "Pester 4.10.1 is required; loaded $((Get-Module Pester).Version -join ', ')." }
$options = @{ Script = $Path; PassThru = $true }
$platform = if ($IsWindows) { 'windows' } elseif ($IsLinux) { 'linux' } else { 'macos' }
$exclude = @('windows', 'linux', 'macos' | Where-Object { $_ -ne $platform })
if ($Integration) { $options.Tag = 'integration' } else { $exclude += 'integration' }
$options.ExcludeTag = $exclude
$result = Invoke-Pester @options
# An integration run must show a pass and no skipped, pending or inconclusive test: such a run is a misconfigured job, not a green one.
if ($result.FailedCount -gt 0 -or ($Integration -and ($result.PassedCount -lt 1 -or $result.SkippedCount -gt 0 -or $result.PendingCount -gt 0 -or $result.InconclusiveCount -gt 0))) { exit 1 }
exit 0
