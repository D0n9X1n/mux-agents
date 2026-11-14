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

# PowerShell before 7.3 builds a native command line without escaping quotes and drops empty arguments, so each
# argument is quoted here as CommandLineToArgvW and Rust's std parse it; 7.3 and later pass them exactly.
$legacy = $PSVersionTable.PSVersion -lt [version]'7.3'
if (-not $legacy) { $PSNativeCommandArgumentPassing = 'Standard' }
function ConvertTo-Argument([string] $a) {
    if ($a.Length -gt 0 -and $a -notmatch '[\s"]') { return $a }
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
# Always an array: splatting a single string would pass it one character at a time.
[string[]] $pass = if ($legacy) { @($args | ForEach-Object { ConvertTo-Argument ([string] $_) }) } else { @($args) }

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
    # program, splatted ones included; single-quoted literals are passed as written.
    $line = '& ' + ((@($bin) + $pass | ForEach-Object { "'" + ($_ -replace "'", "''") + "'" }) -join ' ')
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
