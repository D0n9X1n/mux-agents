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
