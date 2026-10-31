# Runs the Pester 4.10.1 suites under Path; suites tagged for another OS (windows, linux, macos) are skipped. Exit 0 all passed, else 1.
param([string]$Path = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
# Pester 4's TestDrive lives under $env:TEMP, which Linux and macOS leave unset.
if (-not $env:TEMP) { $env:TEMP = [IO.Path]::GetTempPath().TrimEnd([IO.Path]::DirectorySeparatorChar) }
Import-Module Pester -RequiredVersion 4.10.1
if ((Get-Module Pester).Version -ne [version]'4.10.1') { throw "Pester 4.10.1 is required; loaded $((Get-Module Pester).Version -join ', ')." }
$options = @{ Script = $Path; PassThru = $true }
$platform = if ($IsWindows) { 'windows' } elseif ($IsLinux) { 'linux' } else { 'macos' }
$options.ExcludeTag = @('windows', 'linux', 'macos' | Where-Object { $_ -ne $platform })
$result = Invoke-Pester @options
if ($result.FailedCount -gt 0) { exit 1 }
exit 0
