# Unix (Linux, macOS) bodies of the provider members, dot-sourced after MuxAgents.ps1; dormant until B4 binds them. OS reads go through a kernel object.
function Import-MuxAgentUnixNative { if (-not ('MuxAgentUnixNative' -as [type])) { Add-Type -Path (Join-Path $PSScriptRoot 'MuxAgents.Native.Unix.cs') } }
# Endpoint on Unix: $TMUX only, "<socket>,<server pid>,<session>" with no CR or LF anywhere (\z: $ would let a final LF
# through); the pid stays text as on Windows.
function Get-MuxAgentUnixEndpoint { param($Environment)
    if ("$($Environment['TMUX'])" -match '^(/[^,\r\n]+),(\d+),(-?\d+)\z') { [pscustomobject]@{ Socket = $Matches[1]; ServerPid = $Matches[2] } } }
# IsShell on Unix: the shells allowed between the caller and its agent or pane, by real-path basename.
function Test-MuxAgentUnixShell { param($Row) [IO.Path]::GetFileName("$($Row.Path)") -cin @('sh', 'bash', 'zsh', 'fish', 'dash', 'ksh', 'tcsh', 'pwsh') }
# SameStart on Unix: kernel starts compare exactly (no rounding); an unreadable start is never the same.
function Test-MuxAgentUnixSameStart { param($Opened, $Listed) $null -ne $Opened -and $null -ne $Listed -and [long]$Opened -eq [long]$Listed }
# Argv on Unix: the row's kernel argv; throws when unreadable, so C3 refuses 'flags'.
function Get-MuxAgentUnixArgv { param($Row)
    if ($null -eq $Row.Argv) { throw [InvalidOperationException]::new('mux-agent: argv is unreadable') }
    [string[]]$Row.Argv }
# StopReport on Unix: every candidate tree row stop did not kill (Reason open, start or kill) is a survivor, by pid.
function Get-MuxAgentUnixStopReport { param($Left)
    $pids = [int[]]@($Left | Where-Object { $null -ne $_ } | ForEach-Object { [int]$_.Pid })
    [ordered]@{ survivors = $pids.Count; survivorPids = $pids } }
