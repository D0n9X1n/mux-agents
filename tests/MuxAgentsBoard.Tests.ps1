# Side-bar feed tests (C1). Untagged Describes are OS-neutral; -Tag 'windows' ones lose the tag once B makes the fakes OS-neutral.
$repoRoot = Split-Path -Parent $PSScriptRoot
$helpers = Join-Path $PSScriptRoot 'MuxAgents.TestHelpers.ps1'
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
. $helpers

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
