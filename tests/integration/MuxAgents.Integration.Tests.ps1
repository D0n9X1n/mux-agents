# Real tmux, real entry and launcher, a compiled fake claude on PATH. Linux and macOS only (the integration CI job).
Describe 'MuxAgents integration tmux' -Tag 'integration' {
    . (Join-Path $PSScriptRoot 'MuxAgents.IntegrationHarness.ps1')
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    # Pester 4 runs each It inline here, while AfterAll runs outside this scope, where the harness functions and $server
    # do not exist; so the server stops in this finally, after the last It.
    $server = $null
    try {
    $server = Start-MuxAgentItServer -Root (Join-Path $TestDrive 'it') -Driver (Join-Path $PSScriptRoot 'Driver.ps1')
    $dir = $server.Dir; $sid = $server.SessionId; Wait-MuxAgentItFile "$dir/done" 300
    $results = @{}; if (Test-Path "$dir/results.jsonl") { foreach ($line in [IO.File]::ReadAllLines("$dir/results.jsonl")) { $r = $line | ConvertFrom-Json; $results[$r.step] = $r } }
    function Get-ItWorker([string]$Step, [string]$Ext) { [IO.File]::ReadAllText("$dir/$($results[$Step].reply.sessionId).$Ext") }
    function Get-ItActive([string]$Name) { @([IO.File]::ReadAllLines("$dir/$Name.txt") | Where-Object { $_.EndsWith('|1|1') } | ForEach-Object { ($_ -split '\|')[0] }) -join ',' }
    It 'runs unsandboxed by the production signals, with a built fake, a terminal at the root and a finished driver' {
        . "$repoRoot/scripts/MuxAgents.ps1"
        $signal = if ($IsLinux) { "NoNewPrivs=$(@([IO.File]::ReadAllLines('/proc/self/status') -cmatch '^NoNewPrivs:\s*0$').Count)" } else { Import-MuxAgentUnixNative; "sandbox_check=$([MuxAgentUnixNative]::SandboxCheck())" }
        "$($server.Built) $($server.Started) $(Test-Path "$dir/done") $([string]::IsNullOrEmpty($env:SANDBOX_RUNTIME)) $signal $([IO.File]::ReadAllText("$dir/root.tty").Trim())" | Should BeExactly "True True True True $(if ($IsLinux) { 'NoNewPrivs=1' } else { 'sandbox_check=0' }) yes" }
    It 'spawn makes a tagged window and a tagged split beside the caller' {
        $w1 = $results['spawn-window'].reply; $w2 = $results['spawn-split'].reply
        "$($results['spawn-window'].code) $($w1.context) $($results['spawn-split'].code) $($w2.context) $($w1.window -cne '@0')" | Should BeExactly '0 fork 0 fork True'
        (@([IO.File]::ReadAllLines("$dir/tags.txt") | Where-Object { ($_ -split '\|')[2] -cmatch '^[0-9a-f]{12}$' } | ForEach-Object { @(($_ -split '\|') | Select-Object -First 6) -join '|' } | Sort-Object) -join ';') | Should BeExactly "$($w1.pane)|$($w1.window)|$($w1.id)|w1|%0|fork:same-client;$($w2.pane)|@0|$($w2.id)|w2|%0|fork:same-client" }
    It 'each worker is the PATH claude, forked, in its own terminal' {
        foreach ($step in 'spawn-window', 'spawn-split') {
            $s = $results[$step].reply.sessionId
            "$((@((Get-ItWorker $step 'argv') -split "`n") | Select-Object -First 8) -join ' ')|$((Get-ItWorker $step 'tty').Trim())" | Should BeExactly "--dangerously-skip-permissions --effort high --resume $sid --fork-session --session-id $s|yes" } }
    It 'a state root set only in the caller reaches each new pane; the tmux server does not carry it' {
        $null = & tmux -S $server.Socket show-environment -g MUX_AGENTS_STATE 2>$null; $serverHas = $LASTEXITCODE -eq 0
        $want = "$(& realpath -- "$dir/state")"
        $got = foreach ($step in 'spawn-window', 'spawn-split') { "$(& realpath -- (Get-ItWorker $step 'state').Trim())" }
        "$serverHas $($got -join ',')" | Should BeExactly "False $want,$want" }
    It 'list, send and read <name> answer, and send reaches the agent mailbox' {
        "$((@($results['list'].reply.agents.name) | Sort-Object) -join ',') $($results['send'].code) $(@($results['read-pane'].reply.text -split "`n" | ForEach-Object Trim) -ccontains 'worker ready')" | Should BeExactly 'w1,w2 0 True'
        $box = Get-ChildItem "$dir/state" -Recurse -Filter "$($results['spawn-window'].reply.id).jsonl" | Select-Object -First 1
        (@([IO.File]::ReadAllLines($box.FullName) | ConvertFrom-Json | ForEach-Object body) -join ';') | Should BeExactly 'hello w1' }
    It 'read -Wait waits on an empty mailbox, and returns a worker reply soon after it is sent' {
        # Waiting: alive 3 s after start, more id calls (state checks, one set per pass) than a -Wait 0 pass makes, nothing unread.
        # Only then did the driver let the workers send; the reader returned soon after the first send, well before 30 s.
        $first = $results['read-wait-1']
        "$($first.wait.armed) $($first.wait.alive) $($first.wait.ms -ge 3000) $($first.wait.baseline -ge 1) $($first.wait.calls -gt $first.wait.baseline) $($first.wait.unread)" | Should BeExactly 'True True True True True 0'
        "$($first.code) $(@($first.reply.messages).Count -ge 1) $($first.sentMs -ge $first.readingMs) $($first.exitMs - $first.sentMs -lt 5000) $($first.exitMs -lt 30000)" | Should BeExactly '0 True True True True'
        $waits = @($results.Keys | Where-Object { $_ -like 'read-wait-*' } | Sort-Object)
        (@($waits | ForEach-Object { $results[$_].reply.messages } | ForEach-Object { "$($_.from):$(($_.text -split "`n")[1])" } | Sort-Object) -join ';') | Should BeExactly "w1:reply from $($results['spawn-window'].reply.sessionId);w2:reply from $($results['spawn-split'].reply.sessionId)" }
    It 'focus selects the window and the pane, also a split beside an active pane' {
        "$(Get-ItActive 'focus-0')|$($results['focus-w1'].code) $(Get-ItActive 'focus-1')|$($results['focus-w2'].code) $(Get-ItActive 'focus-2')" | Should BeExactly "%0|0 $($results['spawn-window'].reply.pane)|0 $($results['spawn-split'].reply.pane)" }
    It 'stop posts stopped to a reader already waiting, closes each pane and leaves no process' {
        # The reader was confirmed waiting (as for read -Wait) before stop ran, and returned the notice soon after stop.
        $waited = $results['read-stopped-wait']
        "$($waited.wait.armed) $($waited.wait.alive) $($waited.wait.calls -gt $waited.wait.baseline) $($waited.wait.unread)" | Should BeExactly 'True True True 0'
        "$($waited.code) $(@($waited.reply.messages | ForEach-Object { ($_.text -split "`n")[1] }) -join ';') $($waited.exitMs - $waited.stoppedMs -lt 5000) $($waited.exitMs -lt 60000)" | Should BeExactly '0 w1 was stopped. True True'
        "$($results['stop-w1'].code) $($results['stop-w1'].reply.survivors) [$(@($results['stop-w1'].reply.survivorPids) -join ',')] $($results['stop-w2'].code) $($results['stop-w2'].reply.survivors) [$(@($results['stop-w2'].reply.survivorPids) -join ',')]" | Should BeExactly '0 0 [] 0 0 []'
        (@($results['read-stopped'].reply.messages | ForEach-Object { ($_.text -split "`n")[1] }) -join ';') | Should BeExactly 'w2 was stopped.'
        # As written: no quit file yet and no kill bypass; only kill-pane's SIGHUP (and Linux pidfd) may end them.
        $pids = foreach ($step in 'spawn-window', 'spawn-split') { -split (Get-ItWorker $step 'pids') }
        for ($i = 0; $i -lt 20 -and @($pids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }).Count; $i++) { Start-Sleep -Milliseconds 500 }
        "$(@($pids).Count) $(@($pids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }).Count) $(Test-Path "$dir/quit")" | Should BeExactly '4 0 False' }
    It 'an agent pane cannot spawn; real tmux tells a gone pane from a dead socket' {
        (@(foreach ($step in 'spawn-window', 'spawn-split') { $r = Get-ItWorker $step 'spawn.json' | ConvertFrom-Json; "$($r.ok) $($r.code) $($r.check)" }) -join ';') | Should BeExactly 'False 2 tagged;False 2 tagged'
        . "$repoRoot/scripts/MuxAgents.ps1"
        $probe = foreach ($s in $server.Socket, "$($server.Socket).none") { Invoke-MuxAgentProcess -FilePath (Get-Command tmux).Source -Arguments @('-S', $s, '-N', 'display-message', '-p', '-t', $results['spawn-window'].reply.pane, '#{pane_id}|#{@agent-id}') }
        "$(Test-MuxAgentPaneGone $probe[0]) $(Test-MuxAgentPaneGone $probe[1])" | Should BeExactly 'True False' }
    }
    finally { if ($server) { Stop-MuxAgentItServer $server } }
}
