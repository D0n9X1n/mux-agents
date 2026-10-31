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
