# Side-bar tests (C2). Untagged: every OS. Fakes use the running platform (-Native): Windows rows and members on
# Windows, Unix basenames, TMUX socket, state folders and lock names on Linux and macOS.
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. (Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1')

function New-SidebarFake {
    # Fake providers under TestDrive\Name. Caller human, claude or copilot; Tag is the caller pane's @agent-id.
    param([string]$Name, [string]$Caller = 'human', [string]$Tag = '', [string]$Pane = '%1')
    $fake = New-FakeMuxAgentProviders -Caller $Caller -Root (Join-Path $TestDrive $Name) -Native
    foreach ($variable in 'TMUX_PANE', 'RMUX_PANE') { $fake.Env[$variable] = $Pane }
    Add-FakeRmuxRule -Fake $fake -Match '#\{window_id\}\|#\{@agent-id\}$' -StdOut "900|100|`$3|@1|$Tag`n"
    Add-FakeRmuxRule -Fake $fake -Match '#\{pane_id\}\|#\{@agent-id\}$' -StdOut "|`n"
    $fake }

function Add-SidebarMux {
    # Stateful list-panes / split-window / set-option / kill-pane: a table pane -> 'role|agent-id'. %1 (the caller) has
    # neither, %2 role 'other'. -Open adds the bar %7; -AgentBar adds %3, an agent pane that also has role sidebar.
    # SplitError fails every split with that stderr; -RefuseSize fails only a split holding -l, with rmux 0.10.0's
    # words. TagFail and KillFail are 'exit' (exit 1) or 'throw' (TimeoutException, as a timed-out call).
    param($Fake, [switch]$Open, [switch]$AgentBar, [switch]$RefuseSize, [string]$SplitError, [string]$TagFail, [string]$KillFail)
    $panes = [ordered]@{ '%1' = '|'; '%2' = 'other|' }
    if ($AgentBar) { $panes['%3'] = 'sidebar|cccccccccccc' }
    if ($Open) { $panes['%7'] = 'sidebar|' }
    $fail = { param($How) if ($How -ceq 'throw') { throw [TimeoutException]::new('slow') }; [pscustomobject]@{ ExitCode = 1; StdOut = ''; StdErr = "no server`n" } }
    Add-FakeRmuxRule -Fake $Fake -Match '^-N list-panes .*#\{@agent-role\}\|#\{@agent-id\}$' -Script { [pscustomobject]@{ ExitCode = 0; StdErr = ''; StdOut = (@($panes.Keys | ForEach-Object { "$_|@5|$($panes[$_])`n" }) -join '') } }.GetNewClosure()
    Add-FakeRmuxRule -Fake $Fake -Match '^-N split-window ' -Script {
        param($a)
        if ($SplitError) { return [pscustomobject]@{ ExitCode = 1; StdOut = ''; StdErr = "$SplitError`n" } }
        if ($RefuseSize -and $a -contains '-l') { return [pscustomobject]@{ ExitCode = 1; StdOut = ''; StdErr = "invalid split size '42': invalid digit found in string`n" } }
        $panes['%7'] = '|'; [pscustomobject]@{ ExitCode = 0; StdOut = "%7|@5`n"; StdErr = '' } }.GetNewClosure()
    Add-FakeRmuxRule -Fake $Fake -Match '^-N set-option ' -Script {
        param($a)
        if ($TagFail) { return (& $fail $TagFail) }
        $panes[$a[4]] = "$($a[6])|$((([string]$panes[$a[4]]) -split '\|', 2)[1])"; $null }.GetNewClosure()
    Add-FakeRmuxRule -Fake $Fake -Match '^-N kill-pane ' -Script { param($a) if ($KillFail) { return (& $fail $KillFail) }; $panes.Remove($a[3]); $null }.GetNewClosure()
    , $panes }

function Invoke-SidebarVerb { param($Fake, [string[]]$Arguments) $r = Invoke-MuxAgentCommand -P $Fake -Arguments $Arguments; [pscustomobject]@{ Code = $r.Code; Reply = ($r.Text | ConvertFrom-Json) } }
function Get-SidebarCalls { param($Fake) @($Fake.Calls | Where-Object { $_[1] -ne 'display-message' } | ForEach-Object { $_ -join ' ' }) -join "`n" }
function Get-SidebarCount { param($Fake, [string]$Verb) @($Fake.Calls | Where-Object { $_[1] -eq $Verb }).Count }
function Get-SidebarRoles { param($Panes) @($Panes.Keys | ForEach-Object { "$_=$($Panes[$_])" }) -join ',' }
$script:SidebarList = "-N list-panes -s -t `$3 -F #{pane_id}|#{window_id}|#{@agent-role}|#{@agent-id}"

function New-SidebarAgent {
    # A committed agent of session $3 created AgeSeconds before the fake clock; Kind/ExitCode add notice.json; Status adds status.json.
    param($Fake, [string]$Id = 'aaaaaaaaaaaa', [string]$Name = 'worker', [string]$Pane = '%9', [string]$TaskLine = 'build it', [int]$AgeSeconds = 240, [string]$Status, [string]$Kind, $ExitCode)
    $folder = [IO.Path]::Combine((Get-MuxAgentStateDirectory -P $Fake -Caller (Resolve-MuxAgentCaller -P $Fake)), 'agents', $Id)
    $null = [IO.Directory]::CreateDirectory($folder)
    Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = $Id; name = $Name; client = 'claude'; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'; context = 'brief'; reason = 'human'
            session = '$3'; pane = $Pane; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $Fake.Cwd; taskLine = $TaskLine; createdAt = $Fake.Clock.AddSeconds(-$AgeSeconds).ToString('o') })
    Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
    if ($Status) { Write-MuxAgentJson -Path (Join-Path $folder 'status.json') -Value ([ordered]@{ text = $Status; at = 'x' }) }
    if ($Kind) { Write-MuxAgentJson -Path (Join-Path $folder 'notice.json') -Value ([ordered]@{ kind = $Kind; exitCode = $ExitCode; at = $Fake.Clock.AddSeconds(-60).ToString('o') }) }
    $folder }

function New-SidebarWatchFake {
    # The bar's own pane %7 (human, untagged), live worker %9 with a status, ended 'done' (exited 0). Probes lists the
    # answers of the bar's own-pane probe in order (live, gone, fail, throw); the last one repeats.
    param([string]$Name, [string[]]$Probes)
    $fake = New-SidebarFake $Name -Pane '%7'
    $null = New-SidebarAgent $fake -Status 'testing the parser'
    $null = New-SidebarAgent $fake -Id 'bbbbbbbbbbbb' -Name 'done' -Pane '%8' -Kind 'exited' -ExitCode 0
    Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes .*#\{@agent-name\}$' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
    $queue = [Collections.Generic.Queue[string]]::new([string[]]$Probes)
    Add-FakeRmuxRule -Fake $fake -Match '^-N display-message -p -t %7 #\{pane_id\}\|#\{@agent-role\}$' -Script {
        $answer = if ($queue.Count -gt 1) { $queue.Dequeue() } else { $queue.Peek() }
        switch ($answer) {
            'live' { [pscustomobject]@{ ExitCode = 0; StdOut = "%7|sidebar`n"; StdErr = '' } }
            'gone' { [pscustomobject]@{ ExitCode = 0; StdOut = "|`n"; StdErr = '' } }
            'fail' { [pscustomobject]@{ ExitCode = 1; StdOut = ''; StdErr = 'no server' } }
            'throw' { throw [TimeoutException]::new('slow') } } }.GetNewClosure()
    $fake }

function Get-SidebarProbes { param($Fake) @($Fake.Calls | Where-Object { ($_ -join ' ') -like '*#{pane_id}|#{@agent-role}' }).Count }
function Stop-SidebarRunaway {
    # A Sleep that advances the fake clock and throws after Max sleeps, so a loop that never ends fails instead of hanging.
    param($Fake, [int]$Max = 5)
    $count = [ref]0
    $Fake.Sleep = { param([int]$Milliseconds) $count.Value++; if ($count.Value -gt $Max) { throw 'runaway' }; $Fake.Clock = $Fake.Clock.AddMilliseconds($Milliseconds) }.GetNewClosure() }

Describe 'MuxAgents sidebar native fake' {
    It '-Native gives the running platform''s members, clients and lock names; without it the fake stays the Windows one' {
        # Catches: -Native ignored off Windows (Windows ClientOf on '\' paths: claude and copilot callers resolve human),
        # the default changed to the running platform (the Providers suite's Windows checks would fail on Linux), a fake
        # whose locks or state folders are not the platform's, the switch missing (the helper is not an advanced function,
        # so an unknown -Native binds to $args silently and the Windows run would pass without it).
        $platform = Get-MuxAgentPlatform
        $members = Get-MuxAgentPlatformMembers -Platform $platform
        $kinds = foreach ($caller in 'human', 'claude', 'copilot') { $c = Resolve-MuxAgentCaller -P (New-FakeMuxAgentProviders -Caller $caller -Root (Join-Path $TestDrive "native $caller") -Native); "$($c.Kind)/$($c.Client)" }
        $fake = New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 'native state') -Native
        $state = Get-MuxAgentStateDirectory -P $fake -Caller (Resolve-MuxAgentCaller -P $fake)
        $switch = (Get-Command New-FakeMuxAgentProviders).Parameters.ContainsKey('Native')
        "$($kinds -join ',') $($fake.Platform -ceq $platform) $((& $script:MuxAgentLockName 'k') -ceq (& $members.LockName 'k')) $([IO.Directory]::Exists((Join-Path $state 'mail'))) $((New-FakeMuxAgentProviders -Root (Join-Path $TestDrive 'default')).Platform) $switch" |
            Should BeExactly 'human/,agent/claude,agent/copilot True True True windows True' } }

Describe 'MuxAgents sidebar format' {
    $d = [char]0x00B7; $f = [char]0x25CF; $o = [char]0x25CB; $e = [char]27
    $controls = -join @(@(0..31) + 127 + @(128..159) | ForEach-Object { [char]$_ })
    It 'draws the count line, live rows with an indented doing line and ended rows with their end' {
        # Catches: count from agents.Count (6), no blank doing skipped, exit code 0 dropped (falsy), exit code on stopped, state not shown.
        $board = [ordered]@{ running = 2; agents = @(
                [ordered]@{ name = 'claude-1'; client = 'claude'; state = 'running'; doing = 'parse the config'; age = 240 },
                [ordered]@{ name = 'idle'; client = 'copilot'; state = 'running'; doing = $null; age = 5 },
                [ordered]@{ name = 'copilot-1'; client = 'copilot'; state = 'exited'; exitCode = 0; age = 900 },
                [ordered]@{ name = 'crashed'; client = 'claude'; state = 'exited'; exitCode = $null; age = 60 },
                [ordered]@{ name = 'halted'; client = 'claude'; state = 'stopped'; exitCode = 7; age = 60 },
                [ordered]@{ name = 'vanished'; client = 'claude'; state = 'lost'; age = 60 }) }
        Format-MuxAgentBoard $board | Should BeExactly "mux-agents $d 2 running`n$f claude-1  claude  4m`n  parse the config`n$f idle  copilot  5s`n$o copilot-1  exited 0`n$o crashed  exited`n$o halted  stopped`n$o vanished  lost" }
    It 'draws only the count line for an empty board' {
        # Catches: a header that needs an agent, a trailing LF, a null row drawn.
        Format-MuxAgentBoard ([ordered]@{ running = 0; agents = @() }) | Should BeExactly "mux-agents $d 0 running" }
    It 'shows age as whole seconds, minutes, hours or days, and ? when unknown' {
        # Catches: Round for Floor (3599 -> 60m), an off-by-one bucket edge, a null age shown as 0s.
        (@(foreach ($s in 0, 59, 60, 3599, 3600, 86399, 86400, $null) { Format-MuxAgentAge $s }) -join ',') | Should BeExactly '0s,59s,1m,59m,1h,23h,1d,?' }
    It 'removes every C0, DEL and C1 code point, one at a time, and keeps the code points next to them' {
        # Catches: a narrowed cleaner that keeps U+001F or U+0080 (or any one code point), a cleaner that eats U+0020, U+007E or U+00A0.
        # Ordinal: -cne and BeExactly compare by culture, which ignores most control characters ("a`u{1F}b" -ceq 'ab').
        $kept = foreach ($code in @(0..31) + 127 + @(128..159)) {
            $text = Format-MuxAgentBoard ([ordered]@{ running = 0; agents = @([ordered]@{ name = "a$([char]$code)b"; state = 'lost' }) })
            if (-not [string]::Equals($text, "mux-agents $d 0 running`n$o ab  lost", [StringComparison]::Ordinal)) { '{0:X4}' -f $code } }
        "[$(@($kept) -join ',')] $(Format-MuxAgentBoard ([ordered]@{ running = 0; agents = @([ordered]@{ name = "a b~$([char]0xA0)c"; state = 'lost' }) }))" | Should BeExactly "[] mux-agents $d 0 running`n$o a b~$([char]0xA0)c  lost" }
    It 'cleans every printed field, state included, so field controls and a field LF never add a row or move the cursor' {
        # Catches: no cleaning in the ended-state branch (state printed raw), state compared before cleaning (a running
        # row drawn as ended), client or exit code not cleaned, LF kept (5 lines become more).
        $board = [ordered]@{ running = 1; agents = @(
                [ordered]@{ name = "w${controls}1"; client = "cl${controls}aude"; state = "run${controls}ning"; doing = "do${controls}ing"; age = 240 },
                [ordered]@{ name = "x${controls}2"; state = "exi${controls}ted"; exitCode = "3$controls" },
                [ordered]@{ name = "y${controls}3"; state = "lo${controls}st" }) }
        $text = Format-MuxAgentBoard $board
        "$([regex]::IsMatch($text, '[\x00-\x09\x0B-\x1F\x7F\u0080-\u009F]')) $(@($text.Split("`n")).Count)" | Should BeExactly 'False 5'
        $text | Should BeExactly "mux-agents $d 1 running`n$f w1  claude  4m`n  doing`n$o x2  exited 3`n$o y3  lost" }
    It 'leaves the printable rest of an escape sequence as text' {
        # Catches: ESC kept (a live sequence), BEL or C1 kept. BeExactly alone ignores them (culture compare), hence the match.
        $board = [ordered]@{ running = 1; agents = @([ordered]@{ name = "a$e[2J" + [char]0x9B + 'b' + [char]7; client = 'claude'; state = 'running'; doing = "do$e]0;x" + [char]7 + 'ing'; age = 240 }) }
        $text = Format-MuxAgentBoard $board
        "$([regex]::IsMatch($text, '[\x00-\x09\x0B-\x1F\x7F\u0080-\u009F]'))" | Should BeExactly 'False'
        $text | Should BeExactly "mux-agents $d 1 running`n$f a[2Jb  claude  4m`n  do]0;xing" } }

Describe 'MuxAgents sidebar board' {
    It '-WithExitCode adds exitCode from notice.json to ended rows only; without it no row has the key' {
        # Catches: exitCode on live rows, the notice's code dropped, the key added without the switch.
        $fake = New-SidebarFake 'exit codes'; $null = New-SidebarAgent $fake
        $null = New-SidebarAgent $fake -Id 'bbbbbbbbbbbb' -Name 'done' -Pane '%8' -Kind 'exited' -ExitCode 3
        $null = New-SidebarAgent $fake -Id 'cccccccccccc' -Name 'halted' -Pane '%6' -Kind 'stopped'
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes .*#\{@agent-name\}$' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
        $caller = Resolve-MuxAgentCaller -P $fake
        $with = Get-MuxAgentBoard -P $fake -Caller $caller -ReaderKey 'pane-1' -All -WithExitCode
        $without = Get-MuxAgentBoard -P $fake -Caller $caller -ReaderKey 'pane-1' -All
        $show = { param($row) "$($row.name)=$(if ($row.Contains('exitCode')) { if ($null -eq $row.exitCode) { 'null' } else { $row.exitCode } } else { '-' })" }
        "$(@($with.agents | ForEach-Object { & $show $_ }) -join ',') | $(@($without.agents | ForEach-Object { & $show $_ }) -join ',')" | Should BeExactly 'worker=-,done=3,halted=null | worker=-,done=-,halted=-' }
    It 'draws a board read from hostile state files with no ESC, BEL or C1 byte' {
        # Catches: a name from record.json drawn uncleaned; Format trusting the stored task line or status.
        $e = [char]27; $d = [char]0x00B7; $f = [char]0x25CF; $o = [char]0x25CB
        $fake = New-SidebarFake 'hostile'
        $null = New-SidebarAgent $fake -Name ("w$e[31m" + [char]0x9B + 'one' + [char]7) -TaskLine ("fix$e[2J" + [char]0x85 + 'it' + [char]7)
        $null = New-SidebarAgent $fake -Id 'dddddddddddd' -Name 'two' -Pane '%6' -Status ("st$e]2;t" + [char]7 + 'atus' + [char]0x9C)
        $null = New-SidebarAgent $fake -Id 'bbbbbbbbbbbb' -Name ("gone$e[H" + [char]0x9F) -Pane '%8' -Kind 'exited' -ExitCode 2
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes .*#\{@agent-name\}$' -StdOut "%9|@5|300|aaaaaaaaaaaa|w`n%6|@5|301|dddddddddddd|two`n"
        $text = Format-MuxAgentBoard (Get-MuxAgentBoard -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -ReaderKey 'pane-1' -All -WithExitCode)
        "$([regex]::IsMatch($text, '[\x00-\x09\x0B-\x1F\x7F\u0080-\u009F]'))" | Should BeExactly 'False'
        $text | Should BeExactly "mux-agents $d 2 running`n$f w[31mone  claude  4m`n  fix[2Jit`n$f two  claude  4m`n  st]2;tatus`n$o gone[H  exited 2" } }

Describe 'MuxAgents sidebar watch loop' {
    $e = [char]27; $d = [char]0x00B7; $f = [char]0x25CF; $o = [char]0x25CB
    $board = "mux-agents $d 1 running`r`n$f worker  claude  4m`r`n  testing the parser`r`n$o done  exited 0`r`n"
    $frame = "$e[H$e[2J$board"
    $write = { param($Frames) { param($Frame) $Frames.Add($Frame); if ($Frames.Count -gt 5) { throw 'runaway' } }.GetNewClosure() }
    It 'asks its own pane each tick, draws the -All board after home and clear every 2 s, and ends when the pane is gone' {
        # Catches: no -All (no done row), no -WithExitCode, no home/clear, LF without CR, another interval, another pane or format probed, drawing before the probe, no sleep.
        $fake = New-SidebarWatchFake 'watch' -Probes 'live', 'live', 'gone'
        $frames = [Collections.Generic.List[string]]::new()
        $ended = Start-MuxAgentWatch -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Write (& $write $frames)
        "$ended $($frames.Count) $(($fake.Clock - [DateTime]::new(2026, 10, 29, 12, 0, 0, [DateTimeKind]::Utc)).TotalMilliseconds) $(Get-SidebarProbes $fake)" | Should BeExactly 'pane-closed 2 4000 3'
        # Ordinal: -ceq compares by culture, which ignores ESC, so a frame without its escapes would still be equal.
        "$([string]::Equals($frames[0], $frame, [StringComparison]::Ordinal)) $([string]::Equals($frames[1], $frame, [StringComparison]::Ordinal))" | Should BeExactly 'True True' }
    It 'a thrown or failed probe is unknown, not gone: the loop draws on until the pane answers gone' {
        # Catches: a thrown probe ending or crashing the loop, a failed probe (exit 1) counted as gone.
        $fake = New-SidebarWatchFake 'watch probe' -Probes 'throw', 'fail', 'live', 'gone'
        $frames = [Collections.Generic.List[string]]::new()
        $ended = Start-MuxAgentWatch -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Write (& $write $frames)
        "$ended $($frames.Count) $(Get-SidebarProbes $fake)" | Should BeExactly 'pane-closed 3 4' }
    It 'ends when its output fails, before sleeping' {
        # Catches: a write error swallowed (3 probes), the error thrown out of watch, a sleep before the return.
        $fake = New-SidebarWatchFake 'watch output' -Probes 'live', 'live', 'gone'
        $ended = Start-MuxAgentWatch -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Write { param($Frame) throw [IO.IOException]::new('EIO') }
        "$ended $(Get-SidebarProbes $fake) $($fake.Clock.ToString('o'))" | Should BeExactly 'output-closed 1 2026-10-29T12:00:00.0000000Z' }
    It 'the frame writer writes UTF-8 without BOM and throws once its stream is closed' {
        # Catches: a BOM, another encoding, a writer that swallows a closed stream.
        $stream = [IO.MemoryStream]::new(); $writer = New-MuxAgentFrameWriter -Stream $stream
        & $writer ('a' + [char]0x25CF + [char]0x00B7 + "`r`n")
        $hex = [Convert]::ToHexString($stream.ToArray()); $stream.Dispose()
        "$hex $(try { & $writer 'b'; 'wrote' } catch { 'threw' })" | Should BeExactly '61E2978FC2B70D0A threw' }
    Context 'board read failures' {
        It 'a failed board read still writes a frame: the last board and refresh failed; the board is read as the bar pane with -All -WithExitCode' {
            # Catches: a failed read skipping its frame (a dead terminal never found), the last board dropped, another
            # reader key, -All or -WithExitCode not passed, a board error ending or escaping the loop.
            $fake = New-SidebarWatchFake 'watch board' -Probes 'live', 'live', 'live', 'gone'
            $global:sidebarBoardCalls = [Collections.Generic.List[string]]::new()
            $global:sidebarBoard = ${function:Get-MuxAgentBoard}
            Mock Get-MuxAgentBoard {
                $global:sidebarBoardCalls.Add("$ReaderKey all=$([bool]$All) exit=$([bool]$WithExitCode)")
                if ($global:sidebarBoardCalls.Count -ne 2) { throw [TimeoutException]::new('lock') }
                & $global:sidebarBoard -P $P -Caller $Caller -ReaderKey $ReaderKey -All:$All -WithExitCode:$WithExitCode }
            try {
                $frames = [Collections.Generic.List[string]]::new()
                $ended = Start-MuxAgentWatch -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Write (& $write $frames)
                "$ended $($frames.Count) $(@($global:sidebarBoardCalls | Select-Object -Unique) -join ';')" | Should BeExactly 'pane-closed 3 pane-7 all=True exit=True'
                $frames[0] | Should BeExactly "$e[H$e[2Jmux-agents $d ? running`r`nrefresh failed`r`n"
                $frames[1] | Should BeExactly $frame
                $frames[2] | Should BeExactly "$e[H$e[2J$($board)refresh failed`r`n" }
            finally { Remove-Variable -Name sidebarBoardCalls, sidebarBoard -Scope Global } }
        It 'with the probe failing, the board read failing and the writer throwing IOException, it ends at the first tick' {
            # Catches: no frame written when the board read fails (the loop never meets the dead terminal: runaway after 5
            # sleeps), a failed probe taken as gone (pane-closed), the write error swallowed.
            $fake = New-SidebarWatchFake 'watch dead' -Probes 'fail'
            Stop-SidebarRunaway $fake
            Mock Get-MuxAgentBoard { throw [TimeoutException]::new('lock') }
            $ended = try { Start-MuxAgentWatch -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Write { param($Frame) throw [IO.IOException]::new('EIO') } } catch { "threw $($_.Exception.Message)" }
            "$ended $(Get-SidebarProbes $fake) $($fake.Clock.ToString('o'))" | Should BeExactly 'output-closed 1 2026-10-29T12:00:00.0000000Z' } } }
