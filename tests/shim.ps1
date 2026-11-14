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

Remove-Item Env:MUX_AGENTS_BIN -ErrorAction SilentlyContinue
Remove-Item Env:SHIM_EXIT -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $tmp -Recurse -Force
"$($script:fail) failed"
if ($script:fail -gt 0) { exit 1 }
exit 0
