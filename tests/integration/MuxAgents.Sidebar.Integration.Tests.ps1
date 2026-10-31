# The side bar on real tmux (C2): B4's harness with SidebarDriver.ps1. Linux and macOS only (the integration CI job).
Describe 'MuxAgents integration sidebar' -Tag 'integration' {
    . (Join-Path $PSScriptRoot 'MuxAgents.IntegrationHarness.ps1')
    # Pester 4 runs AfterAll outside this scope, where the harness functions and $server do not exist; so the server
    # stops in this finally, after the It.
    $server = $null
    try {
    $server = Start-MuxAgentItServer -Root (Join-Path $TestDrive 'bar') -Driver (Join-Path $PSScriptRoot 'SidebarDriver.ps1')
    $dir = $server.Dir; Wait-MuxAgentItFile "$dir/done" 300
    $results = @{}; if (Test-Path "$dir/results.jsonl") { foreach ($line in [IO.File]::ReadAllLines("$dir/results.jsonl")) { $r = $line | ConvertFrom-Json; $results[$r.step] = $r } }
    It 'sidebar opens one 42-column bar beside the caller showing the count and each status, outside list; -Close ends it and its watch' {
        # Catches: no bar or a second bar; -l 42 lost; the bar given an @agent-id (list-bar would name it); the count or a
        # status line missing from the screen (also: watch reading another state root); -Close not killing it; watch
        # outliving its pane.
        $open = $results['sidebar']; $again = $results['sidebar-again']; $close = $results['sidebar-close']
        "$($server.Built) $($server.Started) $($open.code) $($open.reply.opened) $($again.code) $($again.reply.opened) $($again.reply.pane -ceq $open.reply.pane) $($close.code) $($close.reply.closed) $($close.reply.pane -ceq $open.reply.pane)" | Should BeExactly 'True True 0 True 0 False True 0 True True'
        $bar = @([IO.File]::ReadAllLines("$dir/bar-panes.txt") | Where-Object { $_.StartsWith("$($open.reply.pane)|", [StringComparison]::Ordinal) })
        "$($bar.Count) $((($bar[0] -split '\|')[1..3]) -join '|')" | Should BeExactly '1 sidebar||42'
        ((@($results['list-bar'].reply.agents.name) | Sort-Object) -join ',') | Should BeExactly 'w1,w2'
        $screen = @([IO.File]::ReadAllLines("$dir/bar.txt") | ForEach-Object TrimEnd)
        "$(@($screen | Where-Object { $_ -cmatch '^mux-agents . 2 running$' }).Count) $(@($screen | Where-Object { $_ -ceq '  checking the side bar' }).Count) $(@($screen | Where-Object { $_ -cmatch '^. w[12]  claude  \d+[sm]$' }).Count)" | Should BeExactly '1 2 2'
        $barPid = [int](($bar[0] -split '\|')[4])
        for ($i = 0; $i -lt 20 -and (Get-Process -Id $barPid -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 500 }
        "$($barPid -gt 0) $([bool](Get-Process -Id $barPid -ErrorAction SilentlyContinue))" | Should BeExactly 'True False' }
    }
    finally { if ($server) { Stop-MuxAgentItServer $server } }
}
