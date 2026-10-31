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
# /proc/<pid>/stat: comm may hold spaces, ')' and newlines, so fields are read after the last ')'. Every field through starttime
# (field 22) and the one after it must be present, so a starttime cut short is refused. starttime is raw ticks.
function ConvertFrom-MuxAgentLinuxStat { param([AllowNull()][string]$Text)
    $close = "$Text".LastIndexOf(')'); $open = "$Text".IndexOf(' (')
    if ($open -lt 1 -or $close -lt $open) { return $null }
    $fields = $Text.Substring($close + 1).Trim().Split(' '); $id = 0; $parent = 0; $start = 0L
    if ($fields.Count -lt 21 -or -not [int]::TryParse($Text.Substring(0, $open), [ref]$id) -or -not [int]::TryParse($fields[1], [ref]$parent) -or -not [long]::TryParse($fields[19], [ref]$start)) { return $null }
    [pscustomobject]@{ Pid = $id; ParentPid = $parent; Start = $start } }
# /proc/<pid>/cmdline: NUL-terminated strings. The data must end in NUL; exactly that terminator is dropped, so empty trailing
# arguments survive. No final NUL: unreadable ($null).
function ConvertFrom-MuxAgentLinuxCmdline { param([AllowNull()][byte[]]$Bytes)
    if ($null -eq $Bytes -or $Bytes.Length -eq 0 -or $Bytes[-1] -ne 0) { return $null }
    [Text.Encoding]::UTF8.GetString($Bytes, 0, $Bytes.Length - 1).Split([char]0) }
# One process row. Argv is the kernel argv ($null when unreadable); CommandLine only joins it for display.
function New-MuxAgentUnixRow { param([int]$ProcessId, [int]$ParentId, [long]$Start, $Path, [AllowNull()][string[]]$Argv)
    [pscustomobject]@{ Pid = $ProcessId; ParentPid = $ParentId; Start = $Start; Path = $Path; CommandLine = $(if ($null -ne $Argv) { $Argv -join ' ' }); Argv = $Argv } }
# The kernel argv of one pid as string[] (empty strings kept), or $null when unreadable or empty (a kernel thread).
function Read-MuxAgentUnixArgv { param([Parameter(Mandatory)][scriptblock]$Reader, [int]$Id)
    $argv = $null; try { $argv = [string[]]@(& $Reader $Id | Where-Object { $null -ne $_ }) } catch { }
    if ($argv.Count) { , $argv } }
# Processes on Linux: /proc/[0-9]* stat, exe link, cmdline; a pid that exits mid-read is skipped.
function Get-MuxAgentLinuxProcessTable { param([Parameter(Mandatory)]$Kernel)
    foreach ($id in & $Kernel.ListIds) {
        $stat = try { ConvertFrom-MuxAgentLinuxStat -Text (& $Kernel.ReadStat $id) } catch { $null }
        if (-not $stat) { continue }
        $path = try { & $Kernel.ReadExe $id } catch { $null }
        New-MuxAgentUnixRow -ProcessId $stat.Pid -ParentId $stat.ParentPid -Start $stat.Start -Path $path -Argv (Read-MuxAgentUnixArgv -Reader $Kernel.ReadCmdline -Id $id) } }
