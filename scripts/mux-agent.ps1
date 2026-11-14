# mux-agent entry shim for PowerShell 7 and Windows PowerShell 5.1: runs the first usable binary of MUX_AGENTS_BIN,
# scripts/bin/<os>-<arch>/mux-agent[.exe] and target/release/mux-agent[.exe], passing every argument and the exit
# code through. With none usable it prints an entry refusal and exits 1. The binary needs no PowerShell; this file is
# an optional entry point beside scripts/mux-agent (sh) and scripts/mux-agent.cmd. It runs the binary with `&`, so
# the pipeline, redirection, standard input and the current location work as for any native command.

$plugin = Split-Path -Parent $PSScriptRoot
$onWindows = $env:OS -eq 'Windows_NT'

if ($onWindows) {
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $os = 'windows'
    $exe = 'mux-agent.exe'
} else {
    $os = switch -Exact ((uname -s)) { 'Darwin' { 'macos' } 'Linux' { 'linux' } default { $_ } }
    $arch = (uname -m)
    $exe = 'mux-agent'
}
$arch = switch -Exact ($arch.ToUpperInvariant()) {
    'AMD64' { 'x86_64' } 'X86_64' { 'x86_64' } 'ARM64' { 'aarch64' } 'AARCH64' { 'aarch64' } default { $arch }
}

# A regular file that is executable, or becomes so with chmod +x (not Windows).
function Test-Usable([string] $path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    if ($onWindows) { return $true }
    & test -x $path
    if ($LASTEXITCODE -eq 0) { return $true }
    & chmod +x $path 2>$null
    & test -x $path
    return $LASTEXITCODE -eq 0
}

# The caller's error preferences never turn the binary's own exit code into a failure of the shim.
$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false

# PowerShell before 7.3 builds a native command line itself: it drops an empty argument and leaves quotes and
# backslashes unescaped; 7.3 and later pass arguments exactly. So before 7.3 each argument is quoted here, as
# CommandLineToArgvW and Rust's std parse it.
$legacy = $PSVersionTable.PSVersion -lt [version]'7.3'
if (-not $legacy) { $PSNativeCommandArgumentPassing = 'Standard' }

# Windows PowerShell 5.1 and pwsh 7.0-7.2 on Windows add their own quotes around an argument when whitespace falls
# outside a pair of `"` they count, ignoring backslashes. So the whole argument is quoted here and an embedded `"`
# is written `""` (a literal quote inside quotes), which keeps every whitespace inside a counted pair and the
# argument as written; backslashes before a `"` are doubled.
function ConvertTo-WindowsLegacyArgument([string] $a) {
    $out = New-Object System.Text.StringBuilder
    [void] $out.Append('"')
    $slashes = 0
    foreach ($c in $a.ToCharArray()) {
        if ($c -eq '\') { $slashes++; continue }
        if ($c -eq '"') { [void] $out.Append('\', 2 * $slashes).Append('""') } else {
            if ($slashes) { [void] $out.Append('\', $slashes) }
            [void] $out.Append($c)
        }
        $slashes = 0
    }
    [void] $out.Append('\', 2 * $slashes).Append('"')
    return $out.ToString()
}

# pwsh 7.0-7.2 on macOS and Linux: .NET splits the command line again, so the argument is quoted with escaped quotes.
function ConvertTo-UnixLegacyArgument([string] $a) {
    if ($a.Length -gt 0 -and $a -notmatch '[\s"\\'']') { return $a }
    $out = New-Object System.Text.StringBuilder
    [void] $out.Append('"')
    $slashes = 0
    foreach ($c in $a.ToCharArray()) {
        if ($c -eq '\') { $slashes++; continue }
        if ($c -eq '"') { [void] $out.Append('\', 2 * $slashes + 1) } elseif ($slashes) { [void] $out.Append('\', $slashes) }
        $slashes = 0
        [void] $out.Append($c)
    }
    [void] $out.Append('\', 2 * $slashes).Append('"')
    return $out.ToString()
}

# Always an array, empty when there are no arguments.
$pass = @(
    if (-not $legacy) { $args }
    elseif ($onWindows) { $args | ForEach-Object { ConvertTo-WindowsLegacyArgument ([string] $_) } }
    else { $args | ForEach-Object { ConvertTo-UnixLegacyArgument ([string] $_) } }
)
# Input piped to the shim goes to the binary; otherwise the binary reads the shim's own standard input.
$piped = if ($MyInvocation.ExpectingInput) { @($input) } else { $null }

# PowerShell gives `--%` ("stop parsing") a meaning in every way it calls a program: quoted, in a variable or in
# parentheses it is dropped or makes PowerShell pass the rest of the line raw, so no PowerShell entry can pass it
# on as written. mux-agent never takes it; the shim refuses it rather than run a changed command.
if (@($args | Where-Object { [string] $_ -eq '--%' }).Count) {
    [Console]::Out.Write('{"ok":false,"verb":null,"code":1,"check":"entry","message":"the PowerShell entry cannot pass the argument --%"}' + "`n")
    exit 1
}

$here = (Get-Location).ProviderPath
$candidates = @(
    $env:MUX_AGENTS_BIN,
    (Join-Path (Join-Path (Join-Path $plugin 'scripts/bin') "$os-$arch") $exe),
    (Join-Path (Join-Path $plugin 'target/release') $exe)
)
foreach ($bin in $candidates) {
    if (-not $bin) { continue }
    # A relative path, a bare name included, is a file under the current location, never a namesake on PATH.
    if (-not [System.IO.Path]::IsPathRooted($bin)) { $bin = Join-Path $here $bin }
    if (-not (Test-Usable $bin)) { continue }
    # PowerShell on macOS and Linux expands `*`, `?` and `[...]` in every unquoted argument it passes to a native
    # program, splatted ones included; single-quoted literals are passed as written. PowerShell's own escaper doubles
    # every character it reads as a single quote (U+0027, U+2018, U+2019, U+201A, U+201B), so no argument can close
    # its literal.
    $line = '& ' + ((@($bin) + $pass | ForEach-Object {
                "'" + [System.Management.Automation.Language.CodeGeneration]::EscapeSingleQuotedStringContent([string] $_) + "'"
            }) -join ' ')
        if ($null -ne $piped) { $line = '$piped | ' + $line }
    try {
        Invoke-Expression $line
    } catch {
        # It never started (not a program this system runs): the next candidate is tried.
        continue
    }
    exit $LASTEXITCODE
}

[Console]::Out.Write('{"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for ' + "$os-$arch" + '"}' + "`n")
exit 1
