# Shortcuts for PowerShell 7 and Windows PowerShell 5.1, dot-sourced from $PROFILE:
# `. "<plugin folder>/scripts/shell/mux-agents.ps1"`. cc runs `<entry> up -Client claude -- <args>`, gg the same with
# copilot, where <entry> is the plugin's scripts/mux-agent.ps1; all argument handling (quoting, globbing, the `--%`
# refusal, preferences, the exit code in $LASTEXITCODE) is the entry shim's. cc and gg are plain functions, so
# -Verbose, -Debug and every other flag reach the client. The entry is $env:MUX_AGENTS_HOME/scripts/mux-agent.ps1 when
# MUX_AGENTS_HOME is set, otherwise the one two folders above this file, as dot-sourced (symlinks unresolved); it is
# made absolute once, here. Dot-sourcing defines cc, gg and $__MuxAgentsEntry only. cc shadows the C compiler; type
# `claude` or `copilot` for the bare client.

# Computed in a child scope, so no temporary variable reaches the caller; a missing entry leaves everything as it was.
$__MuxAgentsEntry = & {
    param($self, $previous)
    $entry = if ($env:MUX_AGENTS_HOME) {
        Join-Path (Join-Path $env:MUX_AGENTS_HOME 'scripts') 'mux-agent.ps1'
    } else {
        Join-Path (Join-Path (Join-Path (Split-Path -Parent $self) '..') '..') 'scripts/mux-agent.ps1'
    }
    # IsPathRooted accepts `C:foo` (drive-relative) and `\foo` (root-relative), so neither decides absoluteness. On
    # Windows a drive-relative path is resolved against that drive's current location, and a root-relative one against
    # the current location's drive; then the path is made full against the current location.
    $base = (Get-Location -PSProvider FileSystem).ProviderPath
    if ([System.IO.Path]::DirectorySeparatorChar -eq '\') {
        if ($entry -match '^[A-Za-z]:(?![\\/])') {
            $drive = Get-PSDrive -Name $entry.Substring(0, 1) -PSProvider FileSystem -ErrorAction SilentlyContinue
            $dir = if ($drive) { [System.IO.Path]::Combine($drive.Root, $drive.CurrentLocation) } else { $entry.Substring(0, 2) + '\' }
            $entry = [System.IO.Path]::Combine($dir, $entry.Substring(2))
        } elseif ($entry -match '^[\\/](?![\\/])') {
            $entry = [System.IO.Path]::GetPathRoot($base).TrimEnd('\', '/') + $entry
        }
    }
    $entry = if ($PSVersionTable.PSVersion.Major -ge 6) {
        [System.IO.Path]::GetFullPath($entry, $base)
    } else {
        [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($base, $entry))
    }
    if (Test-Path -LiteralPath $entry -PathType Leaf) { return $entry }
    Write-Warning "mux-agents: no entry at $entry; dot-source scripts/shell/mux-agents.ps1 from the plugin folder or set MUX_AGENTS_HOME"
    $previous
} $PSCommandPath $(Get-Variable -Name __MuxAgentsEntry -ValueOnly -ErrorAction SilentlyContinue)
if (-not $__MuxAgentsEntry) {
    Remove-Variable -Name __MuxAgentsEntry -ErrorAction SilentlyContinue
    return
}

# `'--'` is quoted so PowerShell passes it on rather than reading it as the end of parameters.
function cc { & $__MuxAgentsEntry up -Client claude '--' @args }
function gg { & $__MuxAgentsEntry up -Client copilot '--' @args }
