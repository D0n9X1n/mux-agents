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
    if (-not [System.IO.Path]::IsPathRooted($entry)) {
        $entry = Join-Path (Get-Location -PSProvider FileSystem).ProviderPath $entry
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
