# mux-agent entry shim for PowerShell 7 and Windows PowerShell 5.1: runs the first usable binary of MUX_AGENTS_BIN,
# scripts/bin/<os>-<arch>/mux-agent[.exe] and target/release/mux-agent[.exe], passing every argument and the exit
# code through. With none usable it prints an entry refusal and exits 1. The binary needs no PowerShell; this file is
# an optional entry point beside scripts/mux-agent (sh) and scripts/mux-agent.cmd.

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
    if (-not $path -or -not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    if ($onWindows) { return $true }
    & test -x $path
    if ($LASTEXITCODE -eq 0) { return $true }
    & chmod +x $path 2>$null
    & test -x $path
    return $LASTEXITCODE -eq 0
}

# One argument quoted for a Windows command line, as CommandLineToArgvW and Rust's std parse it.
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

$candidates = @(
    $env:MUX_AGENTS_BIN,
    (Join-Path (Join-Path (Join-Path $plugin 'scripts/bin') "$os-$arch") $exe),
    (Join-Path (Join-Path $plugin 'target/release') $exe)
)
foreach ($bin in $candidates) {
    if (-not $bin) { continue }
    # A name with no separator is a file in the current folder, the one checked, never a namesake on PATH.
    if ($bin -notmatch '[\\/]') { $bin = Join-Path (Get-Location).ProviderPath $bin }
    if (-not (Test-Usable $bin)) { continue }
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $bin
    $info.UseShellExecute = $false
    $info.Arguments = ($args | ForEach-Object { ConvertTo-Argument ([string] $_) }) -join ' '
    $process = [System.Diagnostics.Process]::Start($info)
    $process.WaitForExit()
    exit $process.ExitCode
}

[Console]::Out.Write('{"ok":false,"verb":null,"code":1,"check":"entry","message":"no usable mux-agent binary for ' + "$os-$arch" + '"}' + "`n")
exit 1
