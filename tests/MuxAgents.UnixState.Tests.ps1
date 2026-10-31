# Unix state root, private folders and locks. Unit and probe Describes are untagged (native reads mocked); the native
# Describes are tagged linux or macos, and the runner skips them on the other OSes.
$repoRoot = Split-Path -Parent $PSScriptRoot
foreach ($name in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') { . (Join-Path (Join-Path $repoRoot 'scripts') $name) }
$pwsh = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
function Get-UnixStateRefusal {
    # 'ok', 'ok <output verbatim>', '<code> <check> <message>' for a refusal, or 'error <base exception type>'.
    param([Parameter(Mandatory)][scriptblock]$Action)
    try { $out = & $Action; if ($null -eq $out) { 'ok' } else { "ok $out" } }
    catch { if ($_.Exception.Data.Contains('MuxAgentCode')) { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" } else { 'error ' + $_.Exception.GetBaseException().GetType().Name } }
}
Describe 'MuxAgents unix lock name' {
    It 'is Global\MuxAgents-<uid>-<first 32 hex of the key hash>, one name per user' {
        $hash = (Get-MuxAgentHash 'k').Substring(0, 32)
        "$(Get-MuxAgentUnixLockName -LockKey 'k' -User '501')|$(Get-MuxAgentUnixLockName -LockKey 'k' -User '0')" | Should BeExactly "Global\MuxAgents-501-$hash|Global\MuxAgents-0-$hash"
    }
}
Describe 'MuxAgents lock probe' {
    It 'a lock held from another session blocks; a holder killed while we keep a handle leaves an abandoned lock we take' {
        $holder = Join-Path $TestDrive 'holder.ps1'
        [IO.File]::WriteAllLines($holder, [string[]]@(
            'param([string]$Name, [string]$Ready, [string]$Release)'
            '$mutex = [Threading.Mutex]::new($false, $Name)'
            '$null = $mutex.WaitOne()'
            '[IO.File]::WriteAllText($Ready, "held")'
            'while (-not [IO.File]::Exists($Release)) { Start-Sleep -Milliseconds 50 }'
            '$mutex.ReleaseMutex()'
        ))
        $started = [Collections.Generic.List[Diagnostics.Process]]::new()
        function Start-Holder([string]$Key, [string]$Tag) {
            # On Unix the holder runs in a new session (perl setsid, then exec: same pid), as a tmux pane does.
            $ready = Join-Path $TestDrive "$Tag.ready"
            $argv = @($pwsh, '-NoProfile', '-File', $holder, '-Name', (& $script:MuxAgentLockName $Key), '-Ready', $ready, '-Release', (Join-Path $TestDrive "$Tag.release"))
            if (-not $IsWindows) { $argv = @('perl', '-MPOSIX', '-e', 'POSIX::setsid() > 0 or die "setsid failed"; exec @ARGV or die "exec failed"', '--') + $argv }
            $info = [Diagnostics.ProcessStartInfo]::new($argv[0]); foreach ($item in $argv[1..($argv.Count - 1)]) { $info.ArgumentList.Add($item) }
            $process = [Diagnostics.Process]::Start($info); $started.Add($process)
            $deadline = [DateTime]::UtcNow.AddSeconds(30)
            while (-not [IO.File]::Exists($ready)) { if ($process.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw "holder $Tag never took the lock" }; Start-Sleep -Milliseconds 50 }
            $process
        }
        $saved = $script:MuxAgentLockName; $handle = $null
        try {
            if (-not $IsWindows) { $global:unixStateUid = "$(& id -u)"; $script:MuxAgentLockName = { param([string]$LockKey) Get-MuxAgentUnixLockName -LockKey $LockKey -User $global:unixStateUid } }
            $key = 'probe|' + [guid]::NewGuid().ToString('N')
            $first = Start-Holder $key 'first'
            $blocked = Get-UnixStateRefusal { Use-MuxAgentLock -LockKey $key -LockTimeoutMilliseconds 300 -LockAction { 'ran' } }
            [IO.File]::WriteAllText((Join-Path $TestDrive 'first.release'), 'go'); $null = $first.WaitForExit(30000)
            $released = Get-UnixStateRefusal { Use-MuxAgentLock -LockKey $key -LockTimeoutMilliseconds 5000 -LockAction { 'ran' } }
            # Our own handle keeps the mutex object alive across the kill, so the next wait sees it abandoned.
            $handle = [Threading.Mutex]::new($false, (& $script:MuxAgentLockName $key))
            $second = Start-Holder $key 'second'
            $second.Kill(); $null = $second.WaitForExit(30000)
            $abandoned = Get-UnixStateRefusal { Use-MuxAgentLock -LockKey $key -LockTimeoutMilliseconds 5000 -LockAction { 'ran' } }
            "$blocked|$released|$abandoned" | Should BeExactly '2 lock timed out waiting for the agent lock|ok ran|ok ran'
        }
        finally {
            $script:MuxAgentLockName = $saved
            if ($handle) { $handle.Dispose() }
            foreach ($process in $started) { if (-not $process.HasExited) { $process.Kill() }; $process.Dispose() }
            Remove-Variable unixStateUid -Scope Global -ErrorAction SilentlyContinue
        }
    }
}
