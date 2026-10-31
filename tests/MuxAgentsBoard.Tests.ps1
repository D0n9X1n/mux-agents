# Side-bar feed tests (C1). Untagged Describes are OS-neutral; -Tag 'windows' ones lose the tag once B makes the fakes OS-neutral.
$repoRoot = Split-Path -Parent $PSScriptRoot
$helpers = Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1'
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. $helpers

function Invoke-BoardVerb { param($Fake, [string[]]$Arguments) $r = Invoke-MuxAgentCommand -P $Fake -Arguments $Arguments; [pscustomobject]@{ Code = $r.Code; Reply = ($r.Text | ConvertFrom-Json) } }
function Get-BoardReply { param($Fake, [string[]]$Arguments) $r = Invoke-BoardVerb $Fake $Arguments; "$($r.Code) $($r.Reply.check) $($r.Reply.message)" }
function New-BoardFake {
    # Fake providers under TestDrive\Name (same Name, same state); Pane is the caller's pane, Tag its @agent-id.
    param([string]$Name, [string]$Caller = 'human', [string]$Tag = '', [string]$Pane = '%1')
    $fake = New-FakeMuxAgentProviders -Caller $Caller -Root (Join-Path $TestDrive $Name)
    foreach ($variable in 'TMUX_PANE', 'RMUX_PANE') { $fake.Env[$variable] = $Pane }
    Add-FakeRmuxRule -Fake $fake -Match '#\{window_id\}\|#\{@agent-id\}$' -StdOut "900|100|`$3|@1|$Tag`n"
    Add-FakeRmuxRule -Fake $fake -Match '#\{session_id\}\|#\{@agent-id\}$' -StdOut "`$3|`n"
    Add-FakeRmuxRule -Fake $fake -Match '#\{pane_id\}\|#\{@agent-id\}$' -StdOut "|`n"
    $fake }
function Get-BoardState { param($Fake) Get-MuxAgentStateDirectory -P $Fake -Caller (Resolve-MuxAgentCaller -P $Fake) }
function New-BoardAgent {
    # A committed agent created AgeSeconds before the fake clock; returns its folder.
    param($Fake, [string]$Id = 'aaaaaaaaaaaa', [string]$Name = 'worker', [string]$Pane = '%9', [string]$Session = '$3', [string]$Client = 'claude', [string]$TaskLine = 'build it', [int]$AgeSeconds = 240)
    $folder = [IO.Path]::Combine((Get-BoardState $Fake), 'agents', $Id)
    $null = [IO.Directory]::CreateDirectory($folder)
    Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = $Id; name = $Name; client = $Client; sessionId = '0f11fc2f-2032-48ef-a920-4af2a8e5af50'; context = 'brief'; reason = 'human'
            session = $Session; pane = $Pane; window = '@5'; parentPane = '%1'; parentAgent = $null; directory = $Fake.Cwd; taskLine = $TaskLine; createdAt = $Fake.Clock.AddSeconds(-$AgeSeconds).ToString('o') })
    Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = 'x' })
    $folder }
function Get-BoardHex { param([string]$Path) [Convert]::ToHexString([IO.File]::ReadAllBytes($Path)) }
function Get-BoardJsonHex { param([string]$Json) [Convert]::ToHexString([Text.Encoding]::UTF8.GetBytes($Json)) }
function Get-BoardLockName { param($Fake, [string]$Kind, [string]$Key) & $script:MuxAgentLockName "$Kind|$((Resolve-MuxAgentCaller -P $Fake).ServerKey)|$Key" }
function Test-BoardLockHeld {
    # Whether some thread holds the named mutex, asked from a new thread (a mutex is re-entrant for its owner, so the
    # owner cannot ask itself). Called from a spy inside the code under test, True means that code holds the lock now.
    param([Parameter(Mandatory)][string]$Name)
    $job = Start-ThreadJob -ArgumentList $Name -ScriptBlock {
        param($MutexName)
        $mutex = [Threading.Mutex]::new($false, $MutexName)
        try { $taken = try { $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $true }; if ($taken) { $mutex.ReleaseMutex() }; -not $taken }
        finally { $mutex.Dispose() } }
    try { [bool](Receive-Job $job -Wait -ErrorAction Stop) } finally { Remove-Job $job -Force } }

Describe 'MuxAgents board text' {
    It 'keeps the first non-blank line with C0, DEL and C1 removed, tabs as spaces, trimmed' {
        # Catches: no C1 strip (U+009B, U+0085 kept), C1 stripped after the blank-line test, tabs kept, no trim.
        ConvertTo-MuxAgentDoingLine ([string][char]0x9B + "`r`n `t`n`ta`tb" + [char]27 + '[31m' + [char]0x9B + 'c' + [char]0x7F + [char]0x85 + [char]7 + "  `r`nsecond") | Should BeExactly 'a b[31mc' }
    It 'cuts to 80 characters and never leaves half a surrogate pair' {
        # Catches: another cut length, a cut inside a surrogate pair.
        $emoji = [char]::ConvertFromUtf32(0x1F600)
        ConvertTo-MuxAgentDoingLine ('x' * 100) | Should BeExactly ('x' * 80)
        ConvertTo-MuxAgentDoingLine (('y' * 79) + $emoji + 'z') | Should BeExactly ('y' * 79)
        ConvertTo-MuxAgentDoingLine (('y' * 78) + $emoji + 'z') | Should BeExactly (('y' * 78) + $emoji) }
    It 'answers empty for blank text' {
        # Catches: a whitespace or control-only line kept.
        ConvertTo-MuxAgentDoingLine (" `t`r`n" + [char]0x9B + "`n") | Should BeExactly '' } }

Describe 'MuxAgents board sources' {
    It 'takes status, then the last sent line, then the task line; a blank one falls through' {
        # Catches: swapped order; an empty last-sent line ('') taken instead of the task; stored text not cleaned again.
        $order = foreach ($case in @(@('s', 'm', 't'), @('', 'm', 't'), @(' ', '', 't'), @($null, $null, $null), @(("a`tb" + [char]0x9B + "`nc"), 'm', 't'))) {
            $doing = Select-MuxAgentDoing -Status $case[0] -Message $case[1] -Task $case[2]; "$($doing.doingFrom)=$($doing.doing)" }
        ($order -join ';') | Should BeExactly 'status=s;message=m;task=t;=;status=a b' }
    It 'gives age in whole seconds since createdAt, never negative, null when unreadable' {
        # Catches: Round for Floor, no clamp, no UTC conversion, a ConvertFrom-Json DateTime refused.
        $now = [DateTime]::new(2026, 10, 29, 12, 0, 0, [DateTimeKind]::Utc)
        $ages = foreach ($created in @('2026-10-29T11:55:59.4000000Z', '2026-10-29T12:00:05.0000000Z', '2026-10-29T13:59:00+02:00', (ConvertFrom-Json '{"t":"2026-10-29T11:59:00.0000000Z"}').t, 'x', $null)) {
            $age = Get-MuxAgentAge -CreatedAt $created -Now $now; if ($null -eq $age) { 'null' } else { "$($age.GetType().Name):$age" } }
        ($ages -join ' ') | Should BeExactly 'Int32:240 Int32:0 Int32:60 Int32:60 null null' } }

Describe 'MuxAgents board mail reader' {
    It 'returns the complete lines after the offset and leaves the offset alone' {
        # Catches: the offset written by the reader, the torn last line returned, the offset ignored.
        $box = Join-Path $TestDrive 'r.jsonl'; $cursor = Join-Path $TestDrive 'r.offset'
        [IO.File]::WriteAllText($box, "{`"id`":`"1`"}`n{`"id`":`"2`"}`n{`"id`":`"3`""); [IO.File]::WriteAllText($cursor, '11')
        $unread = Get-MuxAgentUnreadText -Box $box -Cursor $cursor
        "$($unread.End)|$($unread.Text)|$([IO.File]::ReadAllText($cursor))" | Should BeExactly "22|{`"id`":`"2`"}`n|11" }
    It 'answers nothing for a missing mailbox or an offset at the end, and parses only whole JSON lines' {
        # Catches: a missing file thrown on, text past the end returned, a bad line parsed.
        $missing = Get-MuxAgentUnreadText -Box (Join-Path $TestDrive 'none.jsonl') -Cursor (Join-Path $TestDrive 'none.offset')
        $box = Join-Path $TestDrive 'e.jsonl'; $cursor = Join-Path $TestDrive 'e.offset'
        [IO.File]::WriteAllText($box, "{`"id`":`"1`"}`n"); [IO.File]::WriteAllText($cursor, '11')
        $atEnd = Get-MuxAgentUnreadText -Box $box -Cursor $cursor
        "$($missing.End)|$($missing.Text)|$($atEnd.End)|$($atEnd.Text)" | Should BeExactly '0||11|'
        (@(ConvertFrom-MuxAgentMailText "{`"id`":`"a`"}`n{bad`n`n{`"id`":`"b`"}`n") | ForEach-Object id) -join ',' | Should BeExactly 'a,b' } }

Describe 'MuxAgents board verb table' {
    It 'registers status with -Body and -BodyFile' {
        # Catches: status unregistered or registered without -BodyFile.
        $request = ConvertFrom-MuxAgentArgs -Arguments @('status', '-BodyFile', 'a b.md', '-Text')
        "$($request.Verb) $($request.Options['BodyFile']) $($request.Text)" | Should BeExactly 'status a b.md True' } }
Describe 'MuxAgents board status verb' -Tag 'windows' {
    It 'stores the cleaned first line, cut to 80, for a Claude and a Copilot agent' {
        # Catches: no C1 strip before storage (U+009B bytes C29B stored), no cut, a non-atomic or misplaced file, a Copilot caller refused.
        $results = foreach ($client in 'claude', 'copilot') {
            $fake = New-BoardFake "status $client" -Caller $client -Tag 'aaaaaaaaaaaa'
            $folder = New-BoardAgent $fake -Client $client -Pane '%1'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 's.md'), "`r`n  parse" + [char]27 + [char]0x9B + "`tthe config " + ('z' * 100) + "`r`nnext")
            $run = Invoke-BoardVerb $fake @('status', '-BodyFile', 's.md')
            $text = 'parse the config ' + ('z' * 63)
            $expected = Get-BoardJsonHex ('{"text":"' + $text + '","at":"2026-10-29T12:00:00.0000000Z"}')
            "$($run.Code) $($run.Reply.name) $($run.Reply.text -ceq $text) $((Get-BoardHex (Join-Path $folder 'status.json')) -ceq $expected)" }
        ($results -join ';') | Should BeExactly '0 worker True True;0 worker True True' }
    It 'refuses target unless an agent caller has its own record, and refuses -Body, two or no bodies, and blank text' {
        # Catches: Kind not checked (a human pane with a tag accepted), the record or its session not checked, the file read
        # before the caller check (missing.md would answer usage), -Body accepted from either client, blank text stored.
        $null = New-BoardAgent (New-BoardFake 'refuse h' -Tag 'aaaaaaaaaaaa')
        $null = New-BoardAgent (New-BoardFake 'refuse s' -Caller claude -Tag 'aaaaaaaaaaaa') -Session '$4'
        $replies = @((Get-BoardReply (New-BoardFake 'refuse h' -Tag 'aaaaaaaaaaaa') @('status', '-BodyFile', 'missing.md')),
            (Get-BoardReply (New-BoardFake 'refuse h' -Tag 'aaaaaaaaaaaa') @('status', '-Body', 'x')),
            (Get-BoardReply (New-BoardFake 'refuse u' -Caller claude) @('status', '-BodyFile', 'missing.md')),
            (Get-BoardReply (New-BoardFake 'refuse n' -Caller copilot -Tag 'bbbbbbbbbbbb') @('status', '-BodyFile', 'missing.md')),
            (Get-BoardReply (New-BoardFake 'refuse s' -Caller claude -Tag 'aaaaaaaaaaaa') @('status', '-BodyFile', 'missing.md')))
        foreach ($client in 'claude', 'copilot') {
            $fake = New-BoardFake "refuse $client" -Caller $client -Tag 'aaaaaaaaaaaa'; $null = New-BoardAgent $fake -Client $client -Pane '%1'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 'blank.md'), " `t`r`n" + [char]0x9B + [char]7 + "`n")
            $replies += foreach ($arguments in @(@('-Body', 'x'), @('-Body', 'x', '-BodyFile', 'blank.md'), @(), @('-BodyFile', 'blank.md'))) { Get-BoardReply $fake (@('status') + $arguments) } }
        $usage = '2 usage usage: mux-agent status (-Body <text> | -BodyFile <path>) [-Text]'
        $agent = "2 usage agents pass free text only in files: use -BodyFile;$usage;$usage;2 usage the status is empty"
        ($replies -join ';') | Should BeExactly ((@('2 target only an agent reports status') * 5 + $agent, $agent) -join ';')
        @(Get-ChildItem -Path (Join-Path $TestDrive 'refuse*') -Recurse -Filter 'status.json').Count | Should Be 0 }
    # Pester 4 keeps a Mock until its Describe or Context ends, so each spying test has its own Context.
    Context 'status writer spy' {
        It 'writes status.json with the atomic writer while the agent lock is held' {
            # Catches: the write outside the lock (an empty lock action, then the write), the lock under another key,
            # a direct WriteAllText (no writer call). The spy asks a second thread whether the agent mutex is owned.
            $fake = New-BoardFake 'status lock' -Caller claude -Tag 'aaaaaaaaaaaa'; $folder = New-BoardAgent $fake -Pane '%1'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 's.md'), 'step')
            $global:boardLock = Get-BoardLockName $fake 'agent' 'aaaaaaaaaaaa'
            $global:boardSeen = [Collections.Generic.List[string]]::new()
            $global:boardWrite = ${function:Write-MuxAgentJson}
            Mock Write-MuxAgentJson { $global:boardSeen.Add("$(Split-Path -Leaf $Path) held=$(Test-BoardLockHeld $global:boardLock)"); & $global:boardWrite -Path $Path -Value $Value } -ParameterFilter { $Path -like '*status.json' }
            try {
                $run = Invoke-BoardVerb $fake @('status', '-BodyFile', 's.md')
                "$($run.Code) $($global:boardSeen -join ';') $((Read-MuxAgentJson -Path (Join-Path $folder 'status.json')).text)" | Should BeExactly '0 status.json held=True step' }
            finally { Remove-Variable -Name boardLock, boardSeen, boardWrite -Scope Global } } } }

Describe 'MuxAgents board writers' -Tag 'windows' {
    It 'spawn stores the cleaned task line cut to 80' {
        # Catches: no taskLine, no C1 strip (U+009B kept in record.json), no cut, the whole task stored.
        $fake = New-BoardFake 'spawn'
        $run = Invoke-BoardVerb $fake @('spawn', '-Client', 'claude', '-Task', ("`n fix" + [char]27 + '[1m' + [char]0x9B + "`tthe build " + ('q' * 90) + "`nthen test"))
        $text = [IO.File]::ReadAllText([IO.Path]::Combine((Get-BoardState $fake), 'agents', $run.Reply.id, 'record.json'))
        "$($run.Code) $((ConvertFrom-Json $text).taskLine) $($text.Contains([string][char]0x9B))" | Should BeExactly ('0 fix[1m the build ' + ('q' * 63) + ' False') }
    It 'an agent send stores its cleaned first line for Claude and Copilot; a blank send stores an empty line; a human send none' {
        # Catches: no C1 strip, no write for one client, a blank send skipped (old line kept), a write for a human sender.
        $sentHex = Get-BoardJsonHex '{"line":"done: all green","at":"2026-10-29T12:00:00.0000000Z"}'
        $blankHex = Get-BoardJsonHex '{"line":"","at":"2026-10-29T12:00:05.0000000Z"}'
        $results = foreach ($client in 'claude', 'copilot') {
            $fake = New-BoardFake "sent $client" -Caller $client -Tag 'aaaaaaaaaaaa'; $folder = New-BoardAgent $fake -Client $client -Pane '%1'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), "`r`ndone:" + [char]0x9B + "`tall green" + [char]7 + "`nmore")
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 'blank.md'), " `n")
            $first = Invoke-BoardVerb $fake @('send', 'parent', '-BodyFile', 'b.md'); $one = Get-BoardHex (Join-Path $folder 'last-sent.json')
            $fake.Clock = $fake.Clock.AddSeconds(5); $second = Invoke-BoardVerb $fake @('send', 'parent', '-BodyFile', 'blank.md')
            $two = Get-BoardHex (Join-Path $folder 'last-sent.json')
            "$($first.Code)$($second.Code) $($one -ceq $sentHex) $($two -ceq $blankHex)" }
        $human = New-BoardFake 'sent human'
        $humanCode = (Invoke-BoardVerb $human @('send', '%1', '-Body', 'hello')).Code
        ($results -join ';') + " $humanCode $(@(Get-ChildItem -LiteralPath $human.StateRoot -Recurse -Filter 'last-sent.json').Count)" | Should BeExactly '00 True True;00 True True 0 0' }
    It 'a failed last-sent write still delivers the message' {
        # Catches: the write error turned into a send failure.
        $fake = New-BoardFake 'sent blocked' -Caller claude -Tag 'aaaaaaaaaaaa'; $folder = New-BoardAgent $fake -Pane '%1'
        $null = [IO.Directory]::CreateDirectory((Join-Path $folder 'last-sent.json')); [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), 'ok')
        $run = Invoke-BoardVerb $fake @('send', 'parent', '-BodyFile', 'b.md')
        "$($run.Code) $((Get-Content -LiteralPath ([IO.Path]::Combine((Get-BoardState $fake), 'mail', 'pane-1.jsonl')) | ConvertFrom-Json).body)" | Should BeExactly '0 ok' }
    Context 'sender lock spy' {
        It 'appends and writes last-sent.json inside one hold of the sender''s agent lock' {
            # Forces the order instead of racing: spies number each Use-MuxAgentLock hold and record which holds are open
            # when the append and the last-sent write run. Catches: two separate holds (one per call), the append or the
            # write outside the sender lock, the sender lock under another key, a direct WriteAllText (no writer call).
            $fake = New-BoardFake 'sent lock' -Caller claude -Tag 'aaaaaaaaaaaa'; $folder = New-BoardAgent $fake -Pane '%1'
            [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), 'ok')
            $server = (Resolve-MuxAgentCaller -P $fake).ServerKey
            $global:boardHolds = [Collections.Generic.List[string]]::new(); $global:boardOpen = [Collections.Generic.List[string]]::new()
            $global:boardEvents = [Collections.Generic.List[string]]::new()
            $global:boardLock = ${function:Use-MuxAgentLock}; $global:boardMail = ${function:Add-MuxAgentMail}; $global:boardWrite = ${function:Write-MuxAgentJson}
            Mock Use-MuxAgentLock {
                $hold = "$($global:boardHolds.Count + 1)"; $global:boardHolds.Add("$hold=$LockKey"); $global:boardOpen.Add("$hold=$LockKey")
                try { & $global:boardLock -LockKey $LockKey -LockAction $LockAction } finally { $null = $global:boardOpen.Remove("$hold=$LockKey") } }
            Mock Add-MuxAgentMail { $global:boardEvents.Add("append in [$($global:boardOpen -join ',')]"); & $global:boardMail -P $P -Caller $Caller -Key $Key -From $From -FromPane $FromPane -Body $Body }
            Mock Write-MuxAgentJson { $global:boardEvents.Add("last-sent in [$($global:boardOpen -join ',')]"); & $global:boardWrite -Path $Path -Value $Value } -ParameterFilter { $Path -like '*last-sent.json' }
            try {
                $run = Invoke-BoardVerb $fake @('send', 'parent', '-BodyFile', 'b.md')
                "$($run.Code) $($global:boardHolds -join ';') | $($global:boardEvents -join ';') | $((Read-MuxAgentJson -Path (Join-Path $folder 'last-sent.json')).line)" |
                    Should BeExactly "0 1=agent|$server|aaaaaaaaaaaa;2=mail|$server|pane-1 | append in [1=agent|$server|aaaaaaaaaaaa];last-sent in [1=agent|$server|aaaaaaaaaaaa] | ok" }
            finally { Remove-Variable -Name boardHolds, boardOpen, boardEvents, boardLock, boardMail, boardWrite -Scope Global } } } }

Describe 'MuxAgents board list' -Tag 'windows' {
    # Pester 4 keeps a Mock until its Describe or Context ends: the no-op writer below lives only in this Context, so the
    # later tests in this Describe write state for real.
    Context 'list without state writes' {
        It 'lists exact old and new fields in a fixed key order, with no capture-pane, no state write and the offset unmoved' {
            # Catches: a field from the wrong source (window from the record, not the pane), exitCode or any other key added, unread keyed by
            # name only, an age off by one, a capture-pane (screen scraping) or a state write while listing, read losing a message.
            $fake = New-BoardFake 'list'; $null = New-BoardAgent $fake
            $null = New-BoardAgent $fake -Id 'bbbbbbbbbbbb' -Name 'idle' -Pane '%8' -Client 'copilot' -TaskLine '' -AgeSeconds 5
            Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%9|@6|300|aaaaaaaaaaaa|worker`n%8|@7|301|bbbbbbbbbbbb|idle`n"
            $caller = Resolve-MuxAgentCaller -P $fake
            foreach ($from in @(@('worker', '%9'), @('worker', '%9'), @('idle', '%8'), @('worker', '%4'))) { $null = Add-MuxAgentMail -P $fake -Caller $caller -Key 'pane-1' -From $from[0] -FromPane $from[1] -Body 'x' }
            Mock Write-MuxAgentJson { }
            $fake.Calls.Clear()
            $run = Invoke-BoardVerb $fake @('list'); $null = Get-MuxAgentBoard -P $fake -Caller $caller -ReaderKey 'pane-1' -All
            $rows = @($run.Reply.agents | ForEach-Object { (@($_.PSObject.Properties | ForEach-Object { "$($_.Name)=$(if ($_.Name -eq 'createdAt') { ([DateTime]$_.Value).ToUniversalTime().ToString('o') } else { $_.Value })" }) -join ',') })
            $tail = "context=brief,parent=%1,sessionId=0f11fc2f-2032-48ef-a920-4af2a8e5af50,directory=$($fake.Cwd)"
            "$($run.Code) $($run.Reply.running) $(@($run.Reply.PSObject.Properties.Name) -join ',') $($rows -join ';')" | Should BeExactly ("0 2 ok,verb,running,agents name=worker,client=claude,pane=%9,window=@6,$tail,createdAt=2026-10-29T11:56:00.0000000Z,state=running,doing=build it,doingFrom=task,age=240,unread=2;" +
                "name=idle,client=copilot,pane=%8,window=@7,$tail,createdAt=2026-10-29T11:59:55.0000000Z,state=running,doing=,doingFrom=,age=5,unread=1")
            Assert-MockCalled Write-MuxAgentJson -Exactly 0
            "$(@($fake.Calls | Where-Object { $_[1] -eq 'capture-pane' }).Count) $(@((Invoke-BoardVerb $fake @('read')).Reply.messages).Count)" | Should BeExactly '0 4' } }
    It 'doing follows the newest of two sends and two statuses; a corrupt status.json is skipped; a blank send falls to the task' {
        # Catches: a write that appends instead of replacing, the first status or message kept, a corrupt file failing list, the old message
        # used after a blank send, a temp file left, a no-op writer mock leaking out of the Context above (no status or last-sent file).
        $fake = New-BoardFake 'doing' -Caller claude -Tag 'aaaaaaaaaaaa'; $folder = New-BoardAgent $fake -Pane '%1'
        Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%1|@1|100|aaaaaaaaaaaa|worker`n"
        $seen = foreach ($step in @(@('send', 'sent one'), @('send', 'sent two'), @('status', 'status one'), @('status', 'status two'), @('bad', '{bad'), @('send', ' '))) {
            $fake.Clock = $fake.Clock.AddSeconds(10)
            if ($step[0] -eq 'bad') { [IO.File]::WriteAllText((Join-Path $folder 'status.json'), $step[1]) }
            else { [IO.File]::WriteAllText((Join-Path $fake.Cwd 'b.md'), $step[1]); $null = Invoke-BoardVerb $fake (@(if ($step[0] -eq 'send') { 'send', 'parent' } else { 'status' }) + @('-BodyFile', 'b.md')) }
            $row = (Invoke-BoardVerb $fake @('list')).Reply.agents[0]; "$($row.doingFrom)=$($row.doing)" }
        $lastSent = [IO.File]::ReadAllText((Join-Path $folder 'last-sent.json'))
        "$($seen -join ';')|$((Get-ChildItem -LiteralPath $folder -Name | Sort-Object) -join ',')|$lastSent" |
            Should BeExactly 'message=sent one;message=sent two;status=status one;status=status two;message=sent two;task=build it|commit.json,last-sent.json,record.json,status.json|{"line":"","at":"2026-10-29T12:01:00.0000000Z"}' }
    It 'counts only messages after the cursor and leaves the cursor bytes unchanged' {
        # Catches: consumed messages counted, list moving or rewriting the cursor.
        $fake = New-BoardFake 'cursor'; $null = New-BoardAgent $fake; Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
        $caller = Resolve-MuxAgentCaller -P $fake
        foreach ($n in 1..3) { $null = Add-MuxAgentMail -P $fake -Caller $caller -Key 'pane-1' -From 'worker' -FromPane '%9' -Body "m$n" }
        $consumed = @((Invoke-BoardVerb $fake @('read')).Reply.messages).Count
        $null = Add-MuxAgentMail -P $fake -Caller $caller -Key 'pane-1' -From 'worker' -FromPane '%9' -Body 'm4'
        $cursor = [IO.Path]::Combine((Get-BoardState $fake), 'mail', 'pane-1.offset'); $before = Get-BoardHex $cursor
        $counts = @(1..2 | ForEach-Object { (Invoke-BoardVerb $fake @('list')).Reply.agents[0].unread })
        "$consumed $($counts -join ',') $((Get-BoardHex $cursor) -ceq $before)" | Should BeExactly '3 1,1 True' }
    It 'each reader counts its own mailbox: two human panes and a tagged agent, no cursor written' {
        # Catches: a fixed reader key, an agent read as pane-<n> instead of its id, a cursor written by list.
        $fake = New-BoardFake 'boxes'; $null = New-BoardAgent $fake; $caller = Resolve-MuxAgentCaller -P $fake
        foreach ($seed in @(@('pane-1', 1), @('pane-2', 2), @('aaaaaaaaaaaa', 3))) { foreach ($n in 1..$seed[1]) { $null = Add-MuxAgentMail -P $fake -Caller $caller -Key $seed[0] -From 'worker' -FromPane '%9' -Body 'x' } }
        $counts = foreach ($reader in @(@('human', '', '%1'), @('human', '', '%2'), @('claude', 'aaaaaaaaaaaa', '%9'))) {
            $view = New-BoardFake 'boxes' -Caller $reader[0] -Tag $reader[1] -Pane $reader[2]
            Add-FakeRmuxRule -Fake $view -Match '^-N list-panes ' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"; (Invoke-BoardVerb $view @('list')).Reply.agents[0].unread }
        "$($counts -join ',') $(@(Get-ChildItem -LiteralPath ([IO.Path]::Combine((Get-BoardState $fake), 'mail')) -Filter '*.offset').Count)" | Should BeExactly '1,2,3 0' }
    It 'counts with no mailbox without creating state' {
        # Catches: a state folder created by counting.
        $empty = New-BoardFake 'no state'
        "$((Get-MuxAgentUnreadCounts -P $empty -Caller (Resolve-MuxAgentCaller -P $empty) -Key 'pane-1').Count) $(Test-Path -LiteralPath $empty.StateRoot)" | Should BeExactly '0 False' }
    Context 'row reader spy' {
        It 'reads status.json and last-sent.json while the row''s agent lock is held' {
            # Catches: the reads outside the lock (acquire and release, then read), the lock under another key. The spy asks a
            # second thread whether the agent mutex is owned while each file is read.
            $fake = New-BoardFake 'row lock'; $folder = New-BoardAgent $fake; Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
            Write-MuxAgentJson -Path (Join-Path $folder 'status.json') -Value ([ordered]@{ text = 'testing'; at = 'x' })
            Write-MuxAgentJson -Path (Join-Path $folder 'last-sent.json') -Value ([ordered]@{ line = 'sent'; at = 'x' })
            $global:boardLock = Get-BoardLockName $fake 'agent' 'aaaaaaaaaaaa'
            $global:boardSeen = [Collections.Generic.List[string]]::new()
            $global:boardRead = ${function:Read-MuxAgentJson}
            Mock Read-MuxAgentJson { $global:boardSeen.Add("$(Split-Path -Leaf $Path) held=$(Test-BoardLockHeld $global:boardLock)"); & $global:boardRead -Path $Path } -ParameterFilter { $Path -like '*status.json' -or $Path -like '*last-sent.json' }
            try {
                $row = (Invoke-BoardVerb $fake @('list')).Reply.agents[0]
                "$($global:boardSeen -join ';') $($row.doingFrom)=$($row.doing)" | Should BeExactly 'status.json held=True;last-sent.json held=True status=testing' }
            finally { Remove-Variable -Name boardLock, boardSeen, boardRead -Scope Global } } }
    Context 'unread reader spy' {
        It 'counts unread mail while the reader''s mailbox lock is held' {
            # Catches: the unread reader called without Use-MuxAgentLock, the lock under another key. The spy asks a second
            # thread whether the mailbox mutex is owned while the reader runs.
            $fake = New-BoardFake 'unread lock'; $null = New-BoardAgent $fake; Add-FakeRmuxRule -Fake $fake -Match '^-N list-panes ' -StdOut "%9|@5|300|aaaaaaaaaaaa|worker`n"
            $null = Add-MuxAgentMail -P $fake -Caller (Resolve-MuxAgentCaller -P $fake) -Key 'pane-1' -From 'worker' -FromPane '%9' -Body 'x'
            $global:boardLock = Get-BoardLockName $fake 'mail' 'pane-1'
            $global:boardSeen = [Collections.Generic.List[string]]::new()
            $global:boardUnread = ${function:Get-MuxAgentUnreadText}
            Mock Get-MuxAgentUnreadText { $global:boardSeen.Add("$(Split-Path -Leaf $Box) held=$(Test-BoardLockHeld $global:boardLock)"); & $global:boardUnread -Box $Box -Cursor $Cursor }
            try {
                $row = (Invoke-BoardVerb $fake @('list')).Reply.agents[0]
                "$($global:boardSeen -join ';') $($row.unread)" | Should BeExactly 'pane-1.jsonl held=True 1' }
            finally { Remove-Variable -Name boardLock, boardSeen, boardUnread -Scope Global } } } }
