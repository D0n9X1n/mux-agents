# Runs under the fake claude in the root pane (an untagged agent caller): two workers report a status, then the side
# bar opens, is asked for again, is read off the screen, and closes. One JSON line per step.
$ErrorActionPreference = 'Stop'; $d = $env:MUX_IT_DIR; $entry = $env:MUX_IT_ENTRY; $pwsh = $env:MUX_IT_PWSH; $sock = $env:MUX_IT_SOCKET
# tmux by its full path from PATH, never by a bare name.
. (Join-Path (Split-Path -Parent $entry) 'MuxAgents.ps1')
$tmux = Find-MuxAgentApplication -Name 'tmux'
function Invoke-ItStep([string]$Step, [string[]]$Arguments) {
    $text = & $pwsh -NoProfile -File $entry @Arguments; $reply = ($text -join "`n") | ConvertFrom-Json
    Add-Content -LiteralPath "$d/results.jsonl" -Value ([ordered]@{ step = $Step; code = $LASTEXITCODE; reply = $reply } | ConvertTo-Json -Compress -Depth 20); $reply }
try {
    [IO.File]::WriteAllText("$d/task.txt", 'say hi')
    $w1 = Invoke-ItStep 'spawn-w1' @('spawn', '-Client', 'claude', '-TaskFile', "$d/task.txt", '-Name', 'w1')
    $w2 = Invoke-ItStep 'spawn-w2' @('spawn', '-Client', 'claude', '-TaskFile', "$d/task.txt", '-Name', 'w2')
    # The workers send and report their status only after 'reading' exists (B4's handshake).
    [IO.File]::WriteAllText("$d/reading", '')
    foreach ($w in $w1, $w2) { for ($i = 0; $i -lt 240 -and -not (Test-Path -LiteralPath "$d/$($w.sessionId).done"); $i++) { Start-Sleep -Milliseconds 500 } }
    $bar = Invoke-ItStep 'sidebar' @('sidebar'); $null = Invoke-ItStep 'sidebar-again' @('sidebar'); $screen = @()
    for ($i = 0; $i -lt 40; $i++) { $screen = @(& $tmux -S $sock capture-pane -p -t $bar.pane); if (@($screen | Where-Object { $_ -match ' 2 running' }).Count -and @($screen | Where-Object { $_ -ceq '  checking the side bar' }).Count -ge 2) { break }; Start-Sleep -Milliseconds 500 }
    [IO.File]::WriteAllLines("$d/bar.txt", [string[]]@($screen))
    [IO.File]::WriteAllLines("$d/bar-panes.txt", [string[]](& $tmux -S $sock list-panes -a -F '#{pane_id}|#{@agent-role}|#{@agent-id}|#{pane_width}|#{pane_pid}'))
    $null = Invoke-ItStep 'list-bar' @('list'); $null = Invoke-ItStep 'sidebar-close' @('sidebar', '-Close')
}
finally { [IO.File]::WriteAllText("$d/done", '') }
