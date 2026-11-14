# Tests for scripts/mux-agent.ps1, the PowerShell entry shim. Run: pwsh -NoProfile -File tests/shim.ps1 (or
# powershell -NoProfile -File on Windows). tests/shim_ps.rs runs it under every PowerShell present. Each case copies the
# shim into a plugin root with a space in its name and plants fake binaries that print a label and each argument as
# [arg]. The last line counts the failures.

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$onWindows = $env:OS -eq 'Windows_NT'
$ps = (Get-Process -Id $PID).Path
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("mux shim ps " + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $tmp | Out-Null
$script:fail = 0

if ($onWindows) {
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $os = 'windows'; $exe = 'mux-agent.exe'
} else {
    $os = switch -Exact ((uname -s)) { 'Darwin' { 'macos' } 'Linux' { 'linux' } default { $_ } }
    $arch = (uname -m); $exe = 'mux-agent'
}
$arch = switch -Exact ($arch.ToUpperInvariant()) {
    'AMD64' { 'x86_64' } 'X86_64' { 'x86_64' } 'ARM64' { 'aarch64' } 'AARCH64' { 'aarch64' } default { $arch }
}

# The fake binary: on Windows a stub .exe (built once with the .NET Framework csc.exe, as tests/shim.cmd does) that
# prints the first line of label.txt beside it; elsewhere a sh script with the label inside.
$stub = Join-Path $tmp 'stub.exe'
if ($onWindows) {
    $cs = Join-Path $tmp 'stub.cs'
    Set-Content -LiteralPath $cs -Encoding ASCII -Value @'
using System;using System.IO;
class S{static int Main(string[] a){
Console.WriteLine(File.ReadAllLines(AppDomain.CurrentDomain.BaseDirectory+"label.txt")[0].Trim());
foreach(string x in a)Console.WriteLine("["+x+"]");
if(!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("SHIM_STDIN")))Console.Write(Console.In.ReadToEnd());
string f=Environment.GetEnvironmentVariable("SHIM_ARGS_FILE");
if(!string.IsNullOrEmpty(f))File.WriteAllLines(f,a,new System.Text.UTF8Encoding(false));
string c=Environment.GetEnvironmentVariable("SHIM_COUNT_FILE");
if(!string.IsNullOrEmpty(c))File.AppendAllText(c,"run\n");
string v=Environment.GetEnvironmentVariable("SHIM_ENV_FILE");
if(!string.IsNullOrEmpty(v))File.WriteAllText(v,(Environment.GetEnvironmentVariable("MSYS2_ARG_CONV_EXCL")??"unset")+"|"+(Environment.GetEnvironmentVariable("MSYS_NO_PATHCONV")??"unset"));
string e=Environment.GetEnvironmentVariable("SHIM_EXIT");
return string.IsNullOrEmpty(e)?0:int.Parse(e);}}
'@
    & "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe" /nologo "/out:$stub" $cs | Out-Null
    if (-not (Test-Path -LiteralPath $stub)) { 'FAIL no stub binary'; '1 failed'; exit 1 }
}

function Fake([string] $path, [string] $label) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    if ($onWindows) {
        Copy-Item -LiteralPath $stub -Destination $path
        Set-Content -LiteralPath (Join-Path (Split-Path -Parent $path) 'label.txt') -Value $label -Encoding ASCII
    } else {
        $body = "#!/bin/sh`necho $label`nfor a in `"`$@`"; do printf '[%s]\n' `"`$a`"; done`n[ -n `"`${SHIM_STDIN:-}`" ] && cat`n[ -n `"`${SHIM_ARGS_FILE:-}`" ] && printf '%s\n' `"`$@`" > `"`$SHIM_ARGS_FILE`"`n[ -n `"`${SHIM_COUNT_FILE:-}`" ] && echo run >> `"`$SHIM_COUNT_FILE`"`nexit `${SHIM_EXIT:-0}`n"
        [System.IO.File]::WriteAllText($path, $body)
        & chmod +x $path
    }
}

# A fresh plugin root per case, with the shim copied in and no MUX_AGENTS_BIN.
$script:n = 0
function Reset {
    $script:n++
    $script:case = Join-Path $tmp "c$($script:n)"
    $script:root = Join-Path $script:case 'plug in'
    New-Item -ItemType Directory -Force -Path (Join-Path $script:root 'scripts') | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'scripts/mux-agent.ps1') -Destination (Join-Path $script:root 'scripts')
    $script:tier1 = Join-Path (Join-Path $script:case 'env bin') $exe
    $script:tier2 = Join-Path (Join-Path (Join-Path $script:root 'scripts/bin') "$os-$arch") $exe
    $script:tier3 = Join-Path (Join-Path $script:root 'target/release') $exe
    Remove-Item Env:MUX_AGENTS_BIN -ErrorAction SilentlyContinue
    Remove-Item Env:SHIM_EXIT -ErrorAction SilentlyContinue
    Remove-Item Env:SHIM_STDIN -ErrorAction SilentlyContinue
    Remove-Item Env:SHIM_ARGS_FILE -ErrorAction SilentlyContinue
    Remove-Item Env:SHIM_COUNT_FILE -ErrorAction SilentlyContinue
}

function Quote([string] $a) {
    if ($a.Length -gt 0 -and $a -notmatch '[\s"]') { return $a }
    $out = New-Object System.Text.StringBuilder
    [void] $out.Append('"'); $slashes = 0
    foreach ($c in $a.ToCharArray()) {
        if ($c -eq '\') { $slashes++; continue }
        if ($c -eq '"') { [void] $out.Append('\', 2 * $slashes + 1) } elseif ($slashes) { [void] $out.Append('\', $slashes) }
        $slashes = 0; [void] $out.Append($c)
    }
    [void] $out.Append('\', 2 * $slashes).Append('"')
    $out.ToString()
}

# Runs the shim in a new PowerShell from `$cwd` and sets $script:out (LF-joined lines) and $script:rc.
function Run([string[]] $argv, [string] $cwd = $tmp) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $ps
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.WorkingDirectory = $cwd
    $shim = Join-Path (Join-Path $script:root 'scripts') 'mux-agent.ps1'
    $info.Arguments = (@('-NoProfile', '-NonInteractive', '-File', $shim) + $argv | ForEach-Object { Quote $_ }) -join ' '
    $p = [System.Diagnostics.Process]::Start($info)
    $text = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    $script:rc = $p.ExitCode
    $script:out = ($text -replace "`r", '').TrimEnd("`n")
}

# Runs `$command` in a new PowerShell started in $tmp and sets $script:out and $script:rc, as Run does.
function RunIn([string] $command) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $ps
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.WorkingDirectory = $tmp
    $encoded = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($command))
    $info.Arguments = "-NoProfile -NonInteractive -EncodedCommand $encoded"
    $p = [System.Diagnostics.Process]::Start($info)
    $text = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    $script:rc = $p.ExitCode
    $script:out = ($text -replace "`r", '').TrimEnd("`n")
}

function Lit([string] $s) { "'" + ($s -replace "'", "''") + "'" }

function Check([string] $name, [bool] $ok) {
    if ($ok) { "ok   $name" } else { "FAIL ${name}: rc=$($script:rc) out=$($script:out)"; $script:fail++ }
}
function Expect([string] $name, [string[]] $lines, [int] $code = 0) {
    Check $name (($script:rc -eq $code) -and ($script:out -eq ($lines -join "`n")))
}

Reset; Fake $tier1 'tier1'; $env:MUX_AGENTS_BIN = $tier1; Run @('list')
Expect 'tier1_env_wins' @('tier1', '[list]')

Reset; Fake $tier2 'tier2'; Run @('list', '-All')
Expect 'tier2_bin_os_arch' @('tier2', '[list]', '[-All]')

Reset; Fake $tier3 'tier3'; Run @('list')
Expect 'tier3_target_release' @('tier3', '[list]')

Reset; Fake $tier1 'tier1'; Fake $tier2 'tier2'; Fake $tier3 'tier3'
$env:MUX_AGENTS_BIN = $tier1; Run @(); $a = $out
Remove-Item Env:MUX_AGENTS_BIN; Run @(); $b = $out
Remove-Item -LiteralPath $tier2; Run @(); $c = $out
Check 'precedence_1_2_3' ("$a,$b,$c" -eq 'tier1,tier2,tier3')

# A directory is never a binary: the shim moves on.
Reset; New-Item -ItemType Directory -Force -Path $tier1 | Out-Null; $env:MUX_AGENTS_BIN = $tier1; Fake $tier3 'tier3'; Run @('d')
Expect 'directory_skipped' @('tier3', '[d]')

Reset; Fake $tier3 'tier3'
Run @('send', 'a b', '', "it's", '"q"', '$HOME', '*', 'x  y', 'trail\', 'a\"b', '--', '-Lines', 'a" b', ' "x" ', 'c?', '[ab]')
Expect 'spaces_in_root_and_args' @('tier3', '[send]', '[a b]', '[]', "[it's]", '["q"]', '[$HOME]', '[*]', '[x  y]', '[trail\]', '[a\"b]', '[--]', '[-Lines]', '[a" b]', '[ "x" ]', '[c?]', '[[ab]]')

Reset; Fake $tier3 'tier3'; $env:SHIM_EXIT = '7'; Run @('stop', 'z')
Expect 'exit_code_forwarded' @('tier3', '[stop]', '[z]') 7

# A MUX_AGENTS_BIN with no separator names a file in the current folder, never a namesake on PATH.
Reset; $cwd = Join-Path $case 'cwd'; Fake (Join-Path $cwd $exe) 'in-cwd'; $env:MUX_AGENTS_BIN = $exe; Run @('go') $cwd
Expect 'bare_bin_runs_cwd_file' @('in-cwd', '[go]')

Reset; $env:MUX_AGENTS_BIN = Join-Path $tmp 'missing'; Run @('list')
Expect 'none_usable_prints_entry_json_exit_1' @('{"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for ' + "$os-$arch" + '"}') 1

# Called with `&` in a session whose location is not the process's working folder: the output is captured, the
# exit code is $LASTEXITCODE, and a relative MUX_AGENTS_BIN is a file under the current location.
Reset; $loc = Join-Path $case 'loc'; Fake (Join-Path $loc $exe) 'in-loc'
$shimPath = Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1'
$rel = '.' + [System.IO.Path]::DirectorySeparatorChar + $exe
RunIn ("Set-Location -LiteralPath $(Lit $loc); `$env:MUX_AGENTS_BIN = $(Lit $rel); `$env:SHIM_EXIT = '5'; " +
    "`$r = & $(Lit $shimPath) go 'a b'; 'captured=' + (`$r -join '|'); 'code=' + `$LASTEXITCODE")
Expect 'in_process_capture_location_and_code' @('captured=in-loc|[go]|[a b]', 'code=5')

# No argument can run as code: each character PowerShell reads as a single quote stays inside its argument.
Reset; Fake $tier3 'tier3'
$quotes = @(0x27, 0x2018, 0x2019, 0x201A, 0x201B) | ForEach-Object { [string][char]$_ + '; Write-Output INJECTED; #' }
Run (@('q') + $quotes)
# The fake prints in the console's code page, which may not hold these quotes, so the check is that each argument
# arrived as one line ending in its own text and that nothing ran.
$lines = @($out -split "`n")
Check 'no_argument_runs_as_code' (($rc -eq 0) -and ($lines.Count -eq 7) -and ($lines[1] -eq '[q]') -and
    -not ($lines -contains 'INJECTED') -and @($lines[2..6] | Where-Object { $_ -notlike '*; Write-Output INJECTED; #]' }).Count -eq 0)

# `--%` cannot be passed on as written by any PowerShell call of a program, so the shim refuses it and runs nothing,
# never a changed command. Called in-process, since `pwsh -File` itself drops `--%` before a script sees it.
Reset; Fake $tier3 'tier3'
RunIn ("`$env:SHIM_PROBE = 'EXPANDED'; & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) " +
    "x '--%' 'a b' '%SHIM_PROBE%'; exit `$LASTEXITCODE")
Expect 'stop_parsing_token_is_refused' @('{"ok":false,"verb":null,"code":1,"check":"entry","message":"the PowerShell entry cannot pass the argument --%"}') 1

# Exact values, read from a UTF-8 file the fake writes, so the console's code page cannot hide a change: backticks,
# every Unicode single quote, `$`, wildcards and quotes arrive byte for byte.
Reset; Fake $tier3 'tier3'
$exact = @('a`b', '``', [string][char]0x2018 + 'x' + [char]0x2019, [string][char]0x201A + [char]0x201B, '$env:PATH', '*', 'a" b', '')
$argsFile = Join-Path $case 'args.txt'; $env:SHIM_ARGS_FILE = $argsFile
Run (@('e') + $exact)
$got = if (Test-Path -LiteralPath $argsFile) { [System.IO.File]::ReadAllText($argsFile, [System.Text.Encoding]::UTF8) -replace "`r", '' } else { '<none>' }
$want = ((@('e') + $exact) -join "`n") + "`n"
Check 'arguments_arrive_exactly' (($rc -eq 0) -and ($got -eq $want))

# A binary that started is never run again: an error thrown after it, downstream of its output, reaches the caller
# and no other candidate runs.
Reset; Fake $tier1 'tier1'; Fake $tier3 'tier3'
$countFile = Join-Path $case 'runs.txt'
RunIn ("`$env:MUX_AGENTS_BIN = $(Lit $tier1); `$env:SHIM_COUNT_FILE = $(Lit $countFile); " +
    "try { & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) d | ForEach-Object { throw 'downstream' } } " +
    "catch { 'caught=' + `$_.Exception.Message }")
$runs = if (Test-Path -LiteralPath $countFile) { @(Get-Content -LiteralPath $countFile).Count } else { 0 }
# PowerShell wraps the downstream error (`failed to run: downstream`); what matters is that it reaches the caller.
Check 'a_started_binary_is_never_run_again' (($out -match 'caught=.*downstream') -and ($out -notmatch 'no usable') -and ($runs -eq 1))

# A candidate that passes the check but cannot run fails the shim: a non-zero exit, no other candidate, never 0.
if ($onWindows) {
    Reset; New-Item -ItemType Directory -Force -Path (Split-Path -Parent $tier1) | Out-Null
    [System.IO.File]::WriteAllBytes($tier1, [byte[]](0x4D, 0x5A, 0x00, 0x01, 0x02)); $env:MUX_AGENTS_BIN = $tier1
    Fake $tier3 'tier3'
    RunIn ("cmd.exe /d /c exit 0; & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) x 2>`$null; " +
        "'code=' + `$LASTEXITCODE")
    Check 'a_binary_that_cannot_start_fails' (($out -notmatch 'tier3') -and ($out -notmatch 'code=0'))
    Run @('x')
    Check 'a_binary_that_cannot_start_fails_under_file' (($rc -ne 0) -and ($out -notmatch 'tier3'))
}

# A caller with its own local $LASTEXITCODE still gets the binary's exit code.
Reset; Fake $tier3 'tier3'
RunIn ("function f { `$local:LASTEXITCODE = 0; `$env:SHIM_EXIT = '7'; & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) s | Out-Null; " +
    "'code=' + `$global:LASTEXITCODE }; f")
Expect 'exit_code_reaches_a_scoped_caller' @('code=7')

# A caller with a non-zero local $LASTEXITCODE still runs the binary (on macOS and Linux the executable check runs
# `test -x`, whose exit code must not be read from the caller's scope).
Reset; Fake $tier3 'tier3'
RunIn ("function f { `$local:LASTEXITCODE = 7; `$r = & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) u; " +
    "'captured=' + (`$r -join '|') }; f")
Expect 'a_scoped_caller_never_decides_usability' @('captured=tier3|[u]')

# The shim reads no unqualified $LASTEXITCODE: every read is $global:LASTEXITCODE.
$unscoped = @(Select-String -LiteralPath (Join-Path $repo 'scripts/mux-agent.ps1') -Pattern '(?<![:\w])\$LASTEXITCODE' |
    Where-Object { $_.Line -notmatch '^\s*#' })
Check 'no_unscoped_exit_code_read' ($unscoped.Count -eq 0)

# A caller's AllScope preference variables are the same after the shim as before it.
Reset; Fake $tier3 'tier3'
RunIn ("function f { Set-Variable ErrorActionPreference Stop -Option AllScope -Scope Local; " +
    "Set-Variable PSNativeCommandUseErrorActionPreference `$true -Option AllScope -Scope Local; " +
    "Set-Variable PSNativeCommandArgumentPassing Legacy -Option AllScope -Scope Local; " +
    "& $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) p | Out-Null; " +
    "'after=' + `$ErrorActionPreference + '|' + `$PSNativeCommandUseErrorActionPreference + '|' + `$PSNativeCommandArgumentPassing }; f")
Expect 'caller_preferences_are_restored' @('after=Stop|True|Legacy')

# A caller's read-only preference variables never fail the shim or change its exit code.
Reset; Fake $tier3 'tier3'
RunIn ("function f { `$ErrorActionPreference = 'Stop'; " +
    "Set-Variable PSNativeCommandArgumentPassing Legacy -Option ReadOnly,AllScope -Scope Local -ErrorAction SilentlyContinue; " +
    "`$env:SHIM_EXIT = '7'; & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) r | Out-Null; 'code=' + `$global:LASTEXITCODE }; f")
Expect 'read_only_caller_preferences_never_fail_the_shim' @('code=7')

# A caller's read-only argument-passing mode never changes the arguments, whichever mode it holds.
foreach ($mode in 'Legacy', 'Windows', 'Standard') {
    Reset; Fake $tier3 'tier3'
    $argsFile = Join-Path $case 'args.txt'
    RunIn ("function f { Set-Variable PSNativeCommandArgumentPassing $mode -Option ReadOnly,AllScope -Scope Local -ErrorAction SilentlyContinue; " +
        "`$env:SHIM_ARGS_FILE = $(Lit $argsFile); " +
        "& $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) e $(Lit 'a b') $(Lit '') $(Lit 'a" b') $(Lit 'c\') | Out-Null }; f")
    $got = if (Test-Path -LiteralPath $argsFile) { [System.IO.File]::ReadAllText($argsFile, [System.Text.Encoding]::UTF8) -replace "`r", '' } else { '<none>' }
    Check "read_only_${mode}_mode_keeps_arguments_exact" ($got -eq ((@('e', 'a b', '', 'a" b', 'c\') -join "`n") + "`n"))
}

# Called with `&` and no arguments: the binary gets no arguments, not one empty one.
Reset; Fake $tier3 'tier3'
RunIn ("`$r = & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')); 'captured=' + (`$r -join '|')")
Expect 'in_process_no_arguments' @('captured=tier3')

# A binary that ran and failed is never retried or turned into the entry refusal, whatever the caller's error
# preferences: its exit code is the shim's.
Reset; Fake $tier2 'tier2'; Fake $tier3 'tier3'
RunIn ("`$ErrorActionPreference = 'Stop'; `$PSNativeCommandUseErrorActionPreference = `$true; `$env:SHIM_EXIT = '7'; " +
    "try { & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) e } catch { 'caught' }; 'code=' + `$LASTEXITCODE")
Check 'native_exit_is_not_a_start_failure' (($out -split "`n")[0] -eq 'tier2' -and $out -notmatch 'tier3' -and $out -notmatch 'no usable' -and $out -match 'code=7')

# Input piped to the shim reaches the binary's standard input.
Reset; Fake $tier3 'tier3'
RunIn ("`$env:SHIM_STDIN = '1'; `$r = 'payload' | & $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) i; " +
    "'captured=' + ((`$r | ForEach-Object { `$_.Trim() }) -join '|')")
Expect 'pipeline_input_reaches_the_binary' @('captured=tier3|[i]|payload')

# A bare MUX_AGENTS_BIN with no such file in the current location never runs a namesake on PATH.
Reset; $empty = Join-Path $case 'empty'; New-Item -ItemType Directory -Force -Path $empty | Out-Null
$onPath = Join-Path $case 'path'; Fake (Join-Path $onPath $exe) 'on-path'; Fake $tier3 'tier3'
$savedPath = $env:PATH; $env:PATH = $onPath + [System.IO.Path]::PathSeparator + $env:PATH
RunIn ("Set-Location -LiteralPath $(Lit $empty); `$env:MUX_AGENTS_BIN = $(Lit $exe); " +
    "& $(Lit (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')) p; exit `$LASTEXITCODE")
$env:PATH = $savedPath
Expect 'bare_bin_never_runs_path_namesake' @('tier3', '[p]')

if ($onWindows) {
    # An override that is a file but not a program never starts: the next candidate runs; with none, exit 1.
    Reset; New-Item -ItemType Directory -Force -Path (Split-Path -Parent $tier1) | Out-Null
    Set-Content -LiteralPath $tier1 -Value 'not a program' -Encoding ASCII; $env:MUX_AGENTS_BIN = $tier1
    Fake $tier3 'tier3'; Run @('u')
    Expect 'unstartable_override_falls_through' @('tier3', '[u]')
    Remove-Item -LiteralPath $tier3; Run @('u')
    Expect 'unstartable_override_alone_exits_1' @('{"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for ' + "$os-$arch" + '"}') 1
}

if (-not $onWindows) {
    Reset; Fake $tier2 'tier2'; & chmod -x $tier2; Fake $tier3 'tier3'; Run @('x')
    & test -x $tier2; $made = $LASTEXITCODE -eq 0
    Check 'nonexec_gets_chmod' (($rc -eq 0) -and ($out -eq "tier2`n[x]") -and $made)
}

# Shell shortcuts (scripts/shell/mux-agents.ps1): dot-sourced from a profile, it defines cc and gg, which run the
# plugin's entry shim as `up -Client claude|copilot -- <args>`. Each case runs in a new PowerShell against a plugin
# root holding the real shim, the shortcut file and the fake binary tier3, which prints a label and [arg] per argument.
# TODO(up): once `mux-agent up` exists, add the spec's integration case: cc and gg through the real shim to a built
# binary's `up`, which starts the client with the arguments as given.
function ShortcutReset {
    Reset
    $shell = Join-Path (Join-Path $script:root 'scripts') 'shell'
    New-Item -ItemType Directory -Force -Path $shell | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'scripts/shell/mux-agents.ps1') -Destination $shell
    $script:sc = Join-Path $shell 'mux-agents.ps1'
    $script:glob = Join-Path $script:case 'glob'
    New-Item -ItemType Directory -Force -Path $script:glob | Out-Null
    Set-Content -LiteralPath (Join-Path $script:glob 'match-me') -Value '' -Encoding ASCII
    Fake $script:tier3 'tier3'
}

# The fake's output for `up -Client <client> -- <rest>`.
function Up([string] $label, [string] $client, [string[]] $rest = @()) {
    $lines = @($label, '[up]', '[-Client]', "[$client]", '[--]')
    foreach ($r in $rest) { $lines += "[$r]" }
    $lines
}

# RunIn with <SC>, <GLOB> and the other <NAME> tokens replaced by literals of the values given.
function RunCase([string] $template, [hashtable] $values = @{}) {
    $values['SC'] = $script:sc; $values['GLOB'] = $script:glob
    foreach ($k in $values.Keys) { $template = $template.Replace("<$k>", (Lit $values[$k])) }
    RunIn $template
}

ShortcutReset
RunCase @'
Set-Location -LiteralPath <GLOB>; . <SC>; cc a 'b c' '' '*' '$x' 'q"t' "it's" -r /tmp/f --dir=/tmp/f; 'code=' + $LASTEXITCODE
'@
Expect 'shortcut_cc_args_arrive_exactly' ((Up 'tier3' 'claude' @('a', 'b c', '', '*', '$x', 'q"t', "it's", '-r', '/tmp/f', '--dir=/tmp/f')) + 'code=0')

ShortcutReset
RunCase ". <SC>; gg x; 'code=' + `$LASTEXITCODE"
Expect 'shortcut_gg_runs_copilot' ((Up 'tier3' 'copilot' @('x')) + 'code=0')

ShortcutReset
RunCase "`$env:SHIM_EXIT = '7'; . <SC>; cc z | Out-Null; 'code=' + `$LASTEXITCODE"
Expect 'shortcut_last_exit_code_is_the_programs' @('code=7')

# A mux-agent on PATH, shim or binary, is never used.
ShortcutReset
$onPath = Join-Path $case 'path'; Fake (Join-Path $onPath $exe) 'on-path'
Set-Content -LiteralPath (Join-Path $onPath 'mux-agent.ps1') -Value "'on-path'" -Encoding ASCII
RunCase "`$env:PATH = <P> + [System.IO.Path]::PathSeparator + `$env:PATH; . <SC>; cc p" @{ P = $onPath }
Expect 'shortcut_mux_agent_on_path_never_used' (Up 'tier3' 'claude' @('p'))

# MUX_AGENTS_HOME, absolute and relative to where the file is dot-sourced, names the plugin for a copied or symlinked
# file, and still does after a cd.
foreach ($f in 'copy', 'link') {
    foreach ($h in 'abs', 'rel') {
        ShortcutReset
        $home2 = Join-Path $case 'home'
        New-Item -ItemType Directory -Force -Path (Join-Path $home2 'scripts') | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo 'scripts/mux-agent.ps1') -Destination (Join-Path $home2 'scripts')
        Fake (Join-Path (Join-Path $home2 'target/release') $exe) 'home'
        $else = Join-Path $case 'else'; New-Item -ItemType Directory -Force -Path $else | Out-Null
        $file = Join-Path $else "$f.ps1"
        if ($f -eq 'copy') { Copy-Item -LiteralPath $sc -Destination $file } else {
            try { New-Item -ItemType SymbolicLink -Path $file -Value $sc -ErrorAction Stop | Out-Null } catch { }
        }
        $name = "shortcut_home_${h}_${f}_after_cd"
        if (-not (Test-Path -LiteralPath $file)) {
            if ($env:CI) { Check $name $false } else { "skip $name (no symlink: $($Error[0]))" }
            continue
        }
        $value = if ($h -eq 'abs') { $home2 } else { Join-Path "c$($script:n)" 'home' }
        RunCase "`$env:MUX_AGENTS_HOME = <H>; . <F>; Set-Location -LiteralPath <GLOB>; cc h" @{ H = $value; F = $file }
        Expect $name (Up 'home' 'claude' @('h'))
    }
}

ShortcutReset
RunCase "Set-Location -LiteralPath <S>; . ./shell/mux-agents.ps1; Set-Location -LiteralPath <GLOB>; cc h" @{ S = (Join-Path $root 'scripts') }
Expect 'shortcut_relative_source_after_cd' (Up 'tier3' 'claude' @('h'))

# Snap writes the function names, then the global variable names, to a file; the case runs it once first so that the
# automatic variables a first command creates are in both snapshots.
$snap = 'function Snap([string] $f) { Set-Content -LiteralPath $f -Value (@(Get-ChildItem function: | ForEach-Object Name | Sort-Object) + ''--'' + @(Get-Variable -Scope Global | ForEach-Object Name | Sort-Object)) }; '
function Grew([string[]] $names) {
    $before = @(Get-Content -LiteralPath (Join-Path $case 'before.txt'))
    $after = @(Get-Content -LiteralPath (Join-Path $case 'after.txt'))
    $added = @($after | Where-Object { $before -notcontains $_ } | Sort-Object)
    $removed = @($before | Where-Object { $after -notcontains $_ })
    ($removed.Count -eq 0) -and (($added -join ',') -eq ((@($names) | Sort-Object) -join ','))
}

ShortcutReset
Remove-Item -LiteralPath (Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1')
$warnFile = Join-Path $case 'warn.txt'
# The warning record's message is written, not the redirected text, which Windows PowerShell wraps at the console width.
RunCase ($snap + "Snap <W>; Snap <B>; . <SC> 3>&1 | ForEach-Object { Add-Content -LiteralPath <WF> -Value ([string] `$_.Message) }; Snap <A>") @{
    W = (Join-Path $case 'warm.txt'); B = (Join-Path $case 'before.txt'); A = (Join-Path $case 'after.txt'); WF = $warnFile
}
$warn = @(if (Test-Path -LiteralPath $warnFile) { Get-Content -LiteralPath $warnFile })
$at = if ($warn.Count -eq 1 -and $warn[0] -match '^mux-agents: no entry at (.+); dot-source scripts/shell/mux-agents\.ps1 from the plugin folder or set MUX_AGENTS_HOME$') { $Matches[1] }
$want = [System.IO.Path]::GetFullPath((Join-Path (Join-Path $root 'scripts') 'mux-agent.ps1'))
Check 'shortcut_missing_entry_warns_and_defines_nothing' (($rc -eq 0) -and ($out -eq '') -and $at -and
    ($at -eq $want) -and (Grew @()))

# An empty MUX_AGENTS_HOME counts as unset: the dot-sourced file's plugin runs.
ShortcutReset
RunCase "`$env:MUX_AGENTS_HOME = ''; . <SC>; gg x"
Expect 'shortcut_empty_home_uses_the_sourced_plugin' (Up 'tier3' 'copilot' @('x'))

# A drive-relative MUX_AGENTS_HOME (`C:home`) is resolved against that drive's current location when dot-sourced,
# so cc still runs the fake after `Set-Location` to the drive's root.
if ($onWindows) {
    ShortcutReset
    $home2 = Join-Path $case 'home'
    New-Item -ItemType Directory -Force -Path (Join-Path $home2 'scripts') | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'scripts/mux-agent.ps1') -Destination (Join-Path $home2 'scripts')
    Fake (Join-Path (Join-Path $home2 'target/release') $exe) 'home'
    $drive = $case.Substring(0, 2)
    RunCase "Set-Location -LiteralPath <C>; `$env:MUX_AGENTS_HOME = <H>; . <SC>; Set-Location -LiteralPath <R>; cc d" @{
        C = $case; H = "${drive}home"; R = "$drive\"
    }
    Expect 'shortcut_drive_relative_home_survives_a_cd' (Up 'home' 'claude' @('d'))
}

ShortcutReset
RunCase ($snap + "Snap <W>; Snap <B>; . <SC>; . <SC>; Snap <A>; cc t") @{
    W = (Join-Path $case 'warm.txt'); B = (Join-Path $case 'before.txt'); A = (Join-Path $case 'after.txt')
}
Check 'shortcut_sourcing_twice_adds_three_names_only' (($rc -eq 0) -and ($out -eq ((Up 'tier3' 'claude' @('t')) -join "`n")) -and
    (Grew @('cc', 'gg', '__MuxAgentsEntry')))

# -Verbose and -Debug reach the program, and the caller's preferences are as before.
ShortcutReset
RunCase (". <SC>; `$ErrorActionPreference = 'Stop'; `$VerbosePreference = 'SilentlyContinue'; `$DebugPreference = 'SilentlyContinue'; " +
    "`$WarningPreference = 'Continue'; `$env:SHIM_EXIT = '7'; cc -Verbose -Debug v; 'code=' + `$LASTEXITCODE; " +
    "'prefs=' + `$ErrorActionPreference + '|' + `$VerbosePreference + '|' + `$DebugPreference + '|' + `$WarningPreference")
$lines = @($out -split "`n")
Check 'shortcut_verbose_and_debug_reach_the_program' ((($lines[0..7]) -join "`n") -eq ((Up 'tier3' 'claude' @('-Verbose', '-Debug', 'v')) -join "`n") -and ($lines -contains 'code=7'))
Check 'shortcut_caller_preferences_unchanged' ($lines -contains 'prefs=Stop|SilentlyContinue|SilentlyContinue|Continue')

# `--%` ends parsing for the rest of its line, so the code check is on a line of its own.
ShortcutReset
RunCase ". <SC>`ncc --% x 'a b'`n'code=' + `$LASTEXITCODE"
Expect 'shortcut_stop_parsing_token_is_refused' @('{"ok":false,"verb":null,"code":1,"check":"entry","message":"the PowerShell entry cannot pass the argument --%"}', 'code=1')

Remove-Item Env:MUX_AGENTS_BIN -ErrorAction SilentlyContinue
Remove-Item Env:SHIM_EXIT -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $tmp -Recurse -Force
"$($script:fail) failed"
if ($script:fail -gt 0) { exit 1 }
exit 0
