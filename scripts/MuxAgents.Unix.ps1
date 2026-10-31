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
# The script node runs: the first argument after node that is not an option; -r/--require/--import/--loader take the next
# argument; '--' ends options. $null for -e/-p/--eval/--print (or a short-option cluster holding e or p) or for no script.
function Get-MuxAgentUnixNodeScript { param([string[]]$Argv)
    for ($index = 1; $index -lt $Argv.Count; $index++) {
        $item = $Argv[$index]
        if ($item -ceq '--') { return $(if ($index + 1 -lt $Argv.Count) { $Argv[$index + 1] }) }
        if ($item -cmatch '^(--(eval|print)(=|$)|-[A-Za-z]*[ep][A-Za-z]*$)') { return $null }
        if ($item -cin @('-r', '--require', '--import', '--loader')) { $index++; continue }
        if (-not $item.StartsWith('-')) { return $item } }
    $null }
# ClientOf on Unix, by the real executable path: claude under */claude/versions/* or named claude; copilot named copilot, or node
# whose script resolves to @github/copilot/npm-loader.js. A relative script resolves against that process's own cwd (the C5
# reader); an unreadable cwd or link is no match. Case-sensitive.
function Get-MuxAgentUnixClient { param($Row, [Parameter(Mandatory)]$Kernel)
    $path = "$($Row.Path)"; $file = [IO.Path]::GetFileName($path)
    if ($path -clike '*/claude/versions/*' -or $file -ceq 'claude') { return 'claude' }
    if ($file -ceq 'copilot') { return 'copilot' }
    if ($file -cne 'node' -or $null -eq $Row.Argv) { return }
    $script = Get-MuxAgentUnixNodeScript -Argv $Row.Argv
    if (-not $script) { return }
    $target = try {
        if (-not $script.StartsWith('/')) { $cwd = "$(& $Kernel.ReadCwd ([int]$Row.Pid))"; if (-not $cwd.StartsWith('/')) { throw 'cwd' }; $script = $cwd.TrimEnd('/') + '/' + $script }
        "$(& $Kernel.ResolveFile $script)" } catch { '' }
    if ($target.EndsWith('/@github/copilot/npm-loader.js', [StringComparison]::Ordinal)) { 'copilot' } }
