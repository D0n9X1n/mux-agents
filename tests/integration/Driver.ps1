# Runs under the fake claude in the root pane, so every verb sees a real agent caller. One JSON line per step.
$ErrorActionPreference = 'Stop'; $d = $env:MUX_IT_DIR; $entry = $env:MUX_IT_ENTRY; $pwsh = $env:MUX_IT_PWSH
function Add-ItLine([string]$Step, $Code, $Reply, [long]$Ms, [Collections.IDictionary]$Extra = @{}) {
    $line = [ordered]@{ step = $Step; code = $Code; reply = $Reply; ms = $Ms }; foreach ($k in $Extra.Keys) { $line[$k] = $Extra[$k] }
    Add-Content -LiteralPath "$d/results.jsonl" -Value ($line | ConvertTo-Json -Compress -Depth 20) }
function Invoke-ItStep([string]$Step, [string[]]$Arguments) {
    $clock = [Diagnostics.Stopwatch]::StartNew(); $text = & $pwsh -NoProfile -File $entry @Arguments
    $reply = ($text -join "`n") | ConvertFrom-Json; Add-ItLine $Step $LASTEXITCODE $reply $clock.ElapsedMilliseconds; $reply }
function Save-ItPanes([string]$Name) { [IO.File]::WriteAllLines("$d/$Name.txt", [string[]](& tmux -S $env:MUX_IT_SOCKET list-panes -a -F '#{pane_id}|#{window_id}|#{@agent-id}|#{@agent-name}|#{@agent-parent}|#{@agent-context}|#{window_active}|#{pane_active}')) }
function Wait-ItDone([string]$Path) { for ($i = 0; $i -lt 240 -and -not (Test-Path -LiteralPath $Path); $i++) { Start-Sleep -Milliseconds 500 } }
function Start-ItReader([string[]]$Arguments) {
    # A mux-agent run in the background from this pane, its stdout collected.
    $info = [Diagnostics.ProcessStartInfo]::new($pwsh); foreach ($a in @('-NoProfile', '-File', $entry) + $Arguments) { $info.ArgumentList.Add($a) }
    $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true
    $process = [Diagnostics.Process]::Start($info)
    [pscustomobject]@{ Process = $process; Output = $process.StandardOutput.ReadToEndAsync() } }
function Get-ItReply($Reader) { $text = $Reader.Output.GetAwaiter().GetResult(); try { $text | ConvertFrom-Json } catch { $null } }
# The harness's id logs its caller's pid; mux-agent runs id -u at each state check, so each read pass adds calls.
function Get-ItIdCalls([int]$ProcessId) { if (Test-Path -LiteralPath "$d/id.log") { @([IO.File]::ReadAllLines("$d/id.log") | Where-Object { $_.Trim() -ceq "$ProcessId" }).Count } else { 0 } }
function Get-ItUnread {
    # Unread bytes in this pane's mailbox (pane-<n>): its length past the read offset; 0 before any mail.
    $box = Get-ChildItem -LiteralPath "$d/state" -Recurse -File -Filter "pane-$($env:TMUX_PANE.TrimStart('%')).jsonl" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $box) { return 0 }
    $cursor = [IO.Path]::ChangeExtension($box.FullName, '.offset')
    $offset = if (Test-Path -LiteralPath $cursor) { [long]([IO.File]::ReadAllText($cursor).Trim()) } else { 0L }
    $box.Length - $offset }
function Get-ItBaseline {
    # The id calls of exactly one read pass on this mailbox: read -Wait 0 makes one pass and returns.
    $probe = Start-ItReader @('read', '-Wait', '0'); $probe.Process.WaitForExit(); $null = Get-ItReply $probe
    Get-ItIdCalls $probe.Process.Id }
function Wait-ItReaderWaiting($Reader, $Clock, [int]$Baseline) {
    # Waiting: 3 s or more after its start the reader has made more id calls than one pass makes, is still running, and
    # nothing is unread. A reader that stops after one pass (a zero deadline), however slowly it started, never qualifies.
    $calls = 0
    while ($Clock.ElapsedMilliseconds -lt 60000 -and -not $Reader.Process.HasExited) {
        $calls = Get-ItIdCalls $Reader.Process.Id
        if ($Clock.ElapsedMilliseconds -ge 3000 -and $calls -gt $Baseline) { break }
        Start-Sleep -Milliseconds 100 }
    $ms = $Clock.ElapsedMilliseconds; $calls = Get-ItIdCalls $Reader.Process.Id; $alive = -not $Reader.Process.HasExited; $unread = Get-ItUnread
    [ordered]@{ armed = $alive -and $ms -ge 3000 -and $calls -gt $Baseline -and $unread -eq 0; alive = $alive; ms = $ms; baseline = $Baseline; calls = $calls; unread = $unread } }
try {
    [IO.File]::WriteAllText("$d/task.txt", 'say hi'); [IO.File]::WriteAllText("$d/to-w1.txt", 'hello w1')
    $w1 = Invoke-ItStep 'spawn-window' @('spawn', '-Client', 'claude', '-TaskFile', "$d/task.txt", '-Name', 'w1')
    $w2 = Invoke-ItStep 'spawn-split' @('spawn', '-Client', 'claude', '-TaskFile', "$d/task.txt", '-Name', 'w2', '-Split', 'h')
    Save-ItPanes 'tags'; $null = Invoke-ItStep 'list' @('list'); $null = Invoke-ItStep 'send' @('send', 'w1', '-BodyFile', "$d/to-w1.txt")
    # Handshake: read -Wait starts on an empty mailbox; the workers send only after 'reading' exists, which is written
    # only once the reader is seen waiting.
    $baseline = Get-ItBaseline
    $clock = [Diagnostics.Stopwatch]::StartNew(); $reader = Start-ItReader @('read', '-Wait', '30')
    $wait = Wait-ItReaderWaiting $reader $clock $baseline
    [IO.File]::WriteAllText("$d/reading", ''); $readingMs = $clock.ElapsedMilliseconds
    $sentFiles = @($w1, $w2 | ForEach-Object { "$d/$($_.sessionId).sent" }); $sentMs = -1; $exitMs = -1
    while ($clock.ElapsedMilliseconds -lt 60000 -and ($exitMs -lt 0 -or $sentMs -lt 0)) {
        if ($sentMs -lt 0 -and @($sentFiles | Where-Object { Test-Path -LiteralPath $_ }).Count) { $sentMs = $clock.ElapsedMilliseconds }
        if ($exitMs -lt 0 -and $reader.Process.HasExited) { $exitMs = $clock.ElapsedMilliseconds }
        Start-Sleep -Milliseconds 50 }
    $null = $reader.Process.WaitForExit(60000); $first = Get-ItReply $reader
    Add-ItLine 'read-wait-1' $reader.Process.ExitCode $first $exitMs @{ wait = $wait; readingMs = $readingMs; sentMs = $sentMs; exitMs = $exitMs }
    $got = @($first.messages).Count
    for ($round = 2; $round -le 5 -and $got -lt 2; $round++) { $got += @((Invoke-ItStep "read-wait-$round" @('read', '-Wait', '30')).messages).Count }
    foreach ($w in $w1, $w2) { Wait-ItDone "$d/$($w.sessionId).done" }
    $null = Invoke-ItStep 'read-pane' @('read', 'w1', '-Lines', '20')
    Save-ItPanes 'focus-0'; $null = Invoke-ItStep 'focus-w1' @('focus', 'w1'); Save-ItPanes 'focus-1'; $null = Invoke-ItStep 'focus-w2' @('focus', 'w2'); Save-ItPanes 'focus-2'
    # The stopped notice: a reader seen waiting on an empty mailbox, as above, before stop runs.
    $baseline = Get-ItBaseline
    $clock = [Diagnostics.Stopwatch]::StartNew(); $reader = Start-ItReader @('read', '-Wait', '60')
    $wait = Wait-ItReaderWaiting $reader $clock $baseline
    $null = Invoke-ItStep 'stop-w1' @('stop', 'w1'); $stoppedMs = $clock.ElapsedMilliseconds
    while ($clock.ElapsedMilliseconds -lt 90000 -and -not $reader.Process.HasExited) { Start-Sleep -Milliseconds 50 }
    $exitMs = $clock.ElapsedMilliseconds; $null = $reader.Process.WaitForExit(90000)
    Add-ItLine 'read-stopped-wait' $reader.Process.ExitCode (Get-ItReply $reader) $exitMs @{ wait = $wait; stoppedMs = $stoppedMs; exitMs = $exitMs }
    $null = Invoke-ItStep 'stop-w2' @('stop', 'w2'); $null = Invoke-ItStep 'read-stopped' @('read', '-Wait', '30')
}
finally { [IO.File]::WriteAllText("$d/done", '') }
