# Integration harness: a private tmux server whose root pane runs the compiled fake claude (a full-bypass agent caller).
# Only the server inherits HOME, PATH and MUX_IT_*; this process gets its variables back. The server has no
# MUX_AGENTS_STATE: the root pane sets it from MUX_IT_STATE, as a user would in their own shell, so each new pane gets
# it only if spawn passes it.
function Set-MuxAgentItVariable {
    # Sets Name to Value, or removes it when Value is $null.
    param([Parameter(Mandatory)][string]$Name, [AllowNull()]$Value)
    if ($null -eq $Value) { [Environment]::SetEnvironmentVariable($Name, [NullString]::Value) } else { [Environment]::SetEnvironmentVariable($Name, [string]$Value) }
}
function Start-MuxAgentItServer {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Driver)
    $here = $PSScriptRoot; $repo = Split-Path -Parent (Split-Path -Parent $here); $sid = [guid]::NewGuid().ToString()
    foreach ($sub in 'bin', 'home/.claude/sessions', 'home/.claude/projects/it', 'work') { $null = [IO.Directory]::CreateDirectory((Join-Path $Root $sub)) }
    $null = [IO.Directory]::CreateDirectory((Join-Path $Root 'state'), [IO.UnixFileMode]'UserRead, UserWrite, UserExecute')
    [IO.File]::WriteAllText("$Root/home/.claude/projects/it/$sid.jsonl", '{"type":"permission-mode","permissionMode":"bypassPermissions"}' + "`n")
    & cc -o "$Root/bin/claude" "-DFAKE_SCRIPT=`"$here/fake-claude.sh`"" "$here/fake-claude.c"; $built = $LASTEXITCODE -eq 0
    # mux-agent reads the uid with id -u at every state check, one set per read -Wait pass. This id logs its caller's pid,
    # then runs the real id, so the driver can tell a reader that is still waiting from one that made a single pass.
    $id = (Get-Command id -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source
    [IO.File]::WriteAllText("$Root/bin/id", (@('#!/bin/sh', '[ -n "$MUX_IT_DIR" ] && echo "$PPID" >> "$MUX_IT_DIR/id.log"', "exec '$id' `"`$@`"") -join "`n") + "`n")
    [IO.File]::SetUnixFileMode("$Root/bin/id", [IO.UnixFileMode]'UserRead, UserWrite, UserExecute, GroupRead, GroupExecute, OtherRead, OtherExecute')
    $sock = "/tmp/mux-it-$PID-$([guid]::NewGuid().ToString('N').Substring(0, 6)).sock"
    $set = [ordered]@{ HOME = "$Root/home"; PATH = "$Root/bin:$env:PATH"; MUX_AGENTS_STATE = $null; TMUX = $null; TMUX_PANE = $null
        MUX_IT_DIR = $Root; MUX_IT_STATE = "$Root/state"; MUX_IT_PWSH = [IO.Path]::Combine($PSHOME, 'pwsh'); MUX_IT_ENTRY = "$repo/scripts/mux-agent.ps1"; MUX_IT_DRIVER = $Driver; MUX_IT_SOCKET = $sock }
    # Set-MuxAgentItVariable removes a variable for $null: PowerShell passes $null to a [string] parameter as '', and
    # .NET keeps an empty variable, so tmux would inherit MUX_AGENTS_STATE= and TMUX= instead of none.
    $saved = @{}; foreach ($k in $set.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); Set-MuxAgentItVariable $k $set[$k] }
    try { & tmux -S $sock -f /dev/null new-session -d -x 200 -y 50 -c "$Root/work" -e "CLAUDE_CODE_SESSION_ID=$sid" -e 'CLAUDE_CODE_ENTRYPOINT=cli' "$Root/bin/claude" --dangerously-skip-permissions; $started = $LASTEXITCODE -eq 0 }
    finally { foreach ($k in $saved.Keys) { Set-MuxAgentItVariable $k $saved[$k] } }
    [pscustomobject]@{ Dir = $Root; Socket = $sock; SessionId = $sid; Built = $built; Started = $started }
}
function Wait-MuxAgentItFile { param([string]$Path, [int]$Seconds = 240) for ($i = 0; $i -lt $Seconds * 2 -and -not (Test-Path -LiteralPath $Path); $i++) { Start-Sleep -Milliseconds 500 } }
function Stop-MuxAgentItServer {
    # The quit file only after every assertion ran, so the stop checks see what kill-pane alone left.
    param([Parameter(Mandatory)]$Server)
    [IO.File]::WriteAllText("$($Server.Dir)/quit", ''); & tmux -S $Server.Socket kill-server; Remove-Item -LiteralPath $Server.Socket -Force -ErrorAction SilentlyContinue
}
