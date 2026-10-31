# Real-kernel tests of the Unix bodies. Each Describe is tagged linux or macos; the runner (T1) skips the other OSes' tags.
$repoRoot = Split-Path -Parent $PSScriptRoot
foreach ($name in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') { . (Join-Path (Join-Path $repoRoot 'scripts') $name) }
function Start-NativeChild([string]$Directory) { $info = [Diagnostics.ProcessStartInfo]::new('/bin/sleep'); $info.ArgumentList.Add('60'); $info.WorkingDirectory = $Directory; [Diagnostics.Process]::Start($info) }
function Get-NativeRealPath([string]$Path) { (& perl -MCwd -e 'print Cwd::abs_path($ARGV[0])' $Path) }
# ExpectedStart reads the start independently of the kernel under test (Linux: /proc stat field 22; macOS: Process.StartTime in epoch microseconds).
function Test-NativeRow([scriptblock]$Table, $Kernel, [scriptblock]$ExpectedStart) {
    $child = Start-NativeChild $TestDrive
    try {
        $row = @(& $Table $Kernel | Where-Object { $_.Pid -eq $child.Id })[0]
        "$($row.ParentPid -eq $PID) $($row.Path -ceq (Get-NativeRealPath '/bin/sleep')) $($row.Argv.Count) $($row.Argv[1]) $($row.Start -eq (& $ExpectedStart $child.Id)) $((& $Kernel.ReadCwd $child.Id) -ceq (Get-NativeRealPath $TestDrive))" }
    finally { $child.Kill(); $child.WaitForExit() } }
function Test-NativeLoaderLink($Kernel) {
    $loader = Join-Path $TestDrive 'lib/node_modules/@github/copilot/npm-loader.js'; $null = New-Item -ItemType File -Force -Path $loader
    $bin = Join-Path $TestDrive 'bin'; $null = New-Item -ItemType Directory -Force -Path $bin; $null = [IO.File]::CreateSymbolicLink((Join-Path $bin 'copilot'), '../lib/node_modules/@github/copilot/npm-loader.js')
    "$(Get-MuxAgentUnixClient -Row ([pscustomobject]@{ Path = '/usr/bin/node'; Argv = [string[]]@('node', (Join-Path $bin 'copilot')) }) -Kernel $Kernel)" }
# IsGone on a live child, the same child once killed and reaped (WaitForExit reaps it), and pid 1, which a non-root runner may not
# signal (EPERM: the probe fails, so not gone). The raw probe of pid 1 is shown, so a run as root (errno 0) cannot pass unnoticed.
function Test-NativeGone($Kernel, [scriptblock]$Probe) {
    Import-MuxAgentUnixNative; $child = Start-NativeChild $TestDrive
    try { $live = & $Kernel.IsGone $child.Id } finally { $child.Kill(); $child.WaitForExit() }
    "$live $(& $Kernel.IsGone $child.Id) $(& $Probe $child.Id) $(& $Kernel.IsGone 1) $(& $Probe 1)" }
# C5 with ~/.claude/sessions searchable by no one: File.Exists is false there, but the read is denied, not missing, so C5 refuses.
# The first two fields show the denial is real (a root runner would read the file and fail this test, not pass it).
function Test-NativeDeniedSession {
    $root = Join-Path $TestDrive 'denied-home'; $sessions = [IO.Path]::Combine($root, '.claude', 'sessions'); $file = Join-Path $sessions '150.json'
    $null = [IO.Directory]::CreateDirectory($sessions); [IO.File]::WriteAllText($file, '{"cwd":"/w"}')
    $kernel = [pscustomobject]@{ ReadStart = { param([int]$Id) 1500 }; ReadCwd = { param([int]$Id) '/w' } }
    $caller = [pscustomobject]@{ Agent = [pscustomobject]@{ Pid = 150; Start = 1500 } }
    & chmod 000 $sessions
    try {
        $denied = try { $null = [IO.File]::ReadAllText($file); 'read' } catch [UnauthorizedAccessException] { 'denied' }
        "$([IO.File]::Exists($file))|$denied|$(Get-MuxAgentUnixClaudeDirectory -P ([pscustomobject]@{ Home = $root }) -Caller $caller -Kernel $kernel)" }
    finally { & chmod 700 $sessions } }
Describe 'MuxAgents unix native linux' -Tag 'linux' {
    It 'lists a real child with its parent, exe, argv, start and cwd' { Test-NativeRow { param($k) Get-MuxAgentLinuxProcessTable -Kernel $k } (New-MuxAgentLinuxKernel) { param($Id) $t = [IO.File]::ReadAllText("/proc/$Id/stat"); [long]$t.Substring($t.LastIndexOf(')') + 2).Split(' ')[19] } | Should BeExactly 'True True 2 60 True True' }
    It 'kills a real child through its pidfd' {
        $kernel = New-MuxAgentLinuxKernel; $child = Start-NativeChild $TestDrive; $opened = Open-MuxAgentUnixProcess -Id $child.Id -Kernel $kernel
        "$($opened.Fd -ge 0) $(Stop-MuxAgentUnixProcess -Opened $opened -Kernel $kernel) $($child.WaitForExit(5000)) $($child.ExitCode)" | Should BeExactly 'True True True 137' }
    It 'IsGone: a live child is not gone, the reaped child is (ESRCH), and pid 1 that the probe may not signal (EPERM) is not' {
        Test-NativeGone (New-MuxAgentLinuxKernel) { param([int]$Id) [MuxAgentUnixNative]::ProbeLinux($Id) } | Should BeExactly 'False True 3 False 1' }
    It 'refuses C5 when the sessions folder cannot be searched, though File.Exists says no file' { Test-NativeDeniedSession | Should BeExactly 'False|denied|' }
    It 'refuses sandbox under setpriv --no-new-privs and passes without it' {
        $probe = Join-Path $TestDrive 'probe.ps1'
        [IO.File]::WriteAllText($probe, "foreach (`$n in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') { . (Join-Path '$repoRoot/scripts' `$n) }; try { Assert-MuxAgentLinuxSandbox -Environment @{} -Kernel (New-MuxAgentLinuxKernel); 'none' } catch { `$_.Exception.Message }")
        "$(& setpriv --no-new-privs (Join-Path $PSHOME 'pwsh') -NoProfile -File $probe)|$(& (Join-Path $PSHOME 'pwsh') -NoProfile -File $probe)" | Should BeExactly 'NoNewPrivs is set|none' }
    It 'reads a real cmdline whose last argument is empty' {
        $child = [Diagnostics.Process]::Start('/usr/bin/perl', [string[]]@('-e', 'sleep 60', ''))
        try { $argv = @(& (New-MuxAgentLinuxKernel).ReadCmdline $child.Id); "$($argv.Count)|$($argv[1])|[$($argv[3])]" | Should BeExactly '4|-e|[]' }
        finally { $child.Kill(); $child.WaitForExit() } }
    It 'resolves a real loader link to copilot' { Test-NativeLoaderLink (New-MuxAgentLinuxKernel) | Should BeExactly 'copilot' } }
Describe 'MuxAgents unix native macos' -Tag 'macos' {
    It 'lists a real child with its parent, path, argv, start and cwd' { Test-NativeRow { param($k) Get-MuxAgentMacProcessTable -Kernel $k } (New-MuxAgentMacKernel) { param($Id) $r = 0L; [Math]::DivRem([Diagnostics.Process]::GetProcessById($Id).StartTime.ToUniversalTime().Ticks - [DateTime]::UnixEpoch.Ticks, 10L, [ref]$r) } | Should BeExactly 'True True 2 60 True True' }
    It 'never signals a real child and reports it' {
        $kernel = New-MuxAgentMacKernel; $child = Start-NativeChild $TestDrive
        try { $opened = Open-MuxAgentUnixProcess -Id $child.Id -Kernel $kernel; "$($opened.Fd) $(Stop-MuxAgentUnixProcess -Opened $opened -Kernel $kernel) $($child.HasExited)" | Should BeExactly '-1 False False' }
        finally { $child.Kill(); $child.WaitForExit() } }
    It 'IsGone: a live child is not gone, the reaped child is (ESRCH), and pid 1 that the probe may not signal (EPERM) is not' {
        Test-NativeGone (New-MuxAgentMacKernel) { param([int]$Id) [MuxAgentUnixNative]::ProbeMac($Id) } | Should BeExactly 'False True 3 False 1' }
    It 'refuses C5 when the sessions folder cannot be searched, though File.Exists says no file' { Test-NativeDeniedSession | Should BeExactly 'False|denied|' }
    It 'sandbox_check reports no sandbox for the runner' { "$(& (New-MuxAgentMacKernel).SandboxCheck)|$(try { Assert-MuxAgentMacSandbox -Environment @{} -Kernel (New-MuxAgentMacKernel); 'none' } catch { $_.Exception.Message })" | Should BeExactly '0|none' }
    It 'resolves a real loader link to copilot' { Test-NativeLoaderLink (New-MuxAgentMacKernel) | Should BeExactly 'copilot' } }
