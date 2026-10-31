# Lock creation retry (#25). Untagged: every OS. The seams replace mutex creation and the pause; a C# fake stands in for
# WaitOne, since an exception thrown by a ScriptMethod reaches catch as MethodInvocationException.
$repoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.ps1')
if (-not ('MuxAgentTestMutex' -as [type])) { Add-Type -TypeDefinition @'
public sealed class MuxAgentTestMutex : System.IDisposable {
    public string Mode; public int Released; public int Disposed;
    public MuxAgentTestMutex(string mode) { Mode = mode; }
    public bool WaitOne(int timeout) {
        if (Mode == "abandoned") throw new System.Threading.AbandonedMutexException();
        if (Mode == "io") throw new System.IO.IOException("wait");
        return Mode != "timeout";
    }
    public void ReleaseMutex() { Released++; }
    public void Dispose() { Disposed++; }
}
'@ }
$real = { param($n) [Threading.Mutex]::new($false, $n) }
$createRefusal = '2 lock could not create the agent lock (a named mutex); set XDG_RUNTIME_DIR to a folder you own'
function Invoke-LockCase {
    # Use-MuxAgentLock with spies: the factory throws Throws in turn, then returns Make's mutex. 'outcome|creations|sleeps'.
    param([object[]]$Throws = @(), [scriptblock]$Make, [scriptblock]$NameOf, [scriptblock]$Action = { 'ran' })
    $log = [Collections.Generic.List[string]]::new(); $queue = [Collections.Generic.Queue[object]]::new([object[]]$Throws)
    $saved = $script:MuxAgentNewMutex, $script:MuxAgentLockSleep, $script:MuxAgentLockName
    $unique = 'MuxAgentsLockTest-' + [guid]::NewGuid().ToString('N')
    $script:MuxAgentLockName = if ($NameOf) { $NameOf } else { { param([string]$LockKey) $unique }.GetNewClosure() }
    $script:MuxAgentNewMutex = { param([string]$Name) $log.Add("create $Name"); if ($queue.Count) { throw $queue.Dequeue() }; & $Make $Name }.GetNewClosure()
    $script:MuxAgentLockSleep = { param([int]$Milliseconds) $log.Add("sleep $Milliseconds") }.GetNewClosure()
    try { $out = try { "ok $(Use-MuxAgentLock -LockKey 'k' -LockAction $Action)" } catch { if ($_.Exception.Data.Contains('MuxAgentCode')) { "$($_.Exception.Data['MuxAgentCode']) $($_.Exception.Data['MuxAgentCheck']) $($_.Exception.Message)" } else { "threw $($_.Exception.GetBaseException().GetType().Name)" } } }
    finally { $script:MuxAgentNewMutex, $script:MuxAgentLockSleep, $script:MuxAgentLockName = $saved }
    "$out|$(@($log | Where-Object { $_.StartsWith('create ') }).Count)|$(@($log | Where-Object { $_.StartsWith('sleep ') }) -join ',')" }
function Test-LockFree {
    # True when another runspace takes Name at once; it releases what it takes.
    param([string]$Name)
    $job = Start-ThreadJob -ArgumentList $Name -ScriptBlock { param($MutexName) $m = [Threading.Mutex]::new($false, $MutexName); try { $got = $m.WaitOne(0); if ($got) { $m.ReleaseMutex() }; $got } catch [Threading.AbandonedMutexException] { $m.ReleaseMutex(); 'abandoned' } finally { $m.Dispose() } }
    try { Receive-Job $job -Wait -ErrorAction Stop } finally { Remove-Job $job -Force } }
$types = { [IO.IOException]::new('x'); [UnauthorizedAccessException]::new('x'); [Threading.WaitHandleCannotBeOpenedException]::new('x') }
Describe 'MuxAgents lock creation retry' {
    It 'retries creation once, after a 100 ms pause, for IOException, UnauthorizedAccessException and WaitHandleCannotBeOpenedException, asking the name once' {
        $got = foreach ($e in & $types) { $asked = [Collections.Generic.List[string]]::new(); $n = 'MuxAgentsLockTest-' + [guid]::NewGuid().ToString('N')
            "$(Invoke-LockCase -Throws @($e) -Make $real -NameOf { param($LockKey) $asked.Add($LockKey); $n }.GetNewClosure()) asked=$($asked.Count)" }
        ($got -join ';') | Should BeExactly ((@(1..3 | ForEach-Object { 'ok ran|2|sleep 100 asked=1' })) -join ';') }
    It 'refuses 2 lock naming XDG_RUNTIME_DIR when creation fails twice' {
        $got = foreach ($e in & $types) { Invoke-LockCase -Throws @($e, $e) -Make $real -Action { throw 'must not run' } }
        ($got -join ';') | Should BeExactly ((@(1..3 | ForEach-Object { "$createRefusal|2|sleep 100" })) -join ';') }
    It 'does not retry a refusal from the name or another exception type' {
        $refuse = ${function:Exit-MuxAgentCommand}
        $named = Invoke-LockCase -Make $real -NameOf { param($LockKey) & $refuse 2 'lock' 'name refused' }.GetNewClosure() -Action { throw 'must not run' }
        $other = Invoke-LockCase -Throws @([ArgumentException]::new('x')) -Make $real -Action { throw 'must not run' }
        "$named;$other" | Should BeExactly '2 lock name refused|0|;threw ArgumentException|1|' }
    It 'does not retry a WaitOne timeout, an abandoned mutex, an IOException from WaitOne or a refusal inside the action' {
        $refuse = ${function:Exit-MuxAgentCommand}
        $got = foreach ($mode in 'timeout', 'abandoned', 'io', 'held') { $fake = [MuxAgentTestMutex]::new($mode)
            $action = if ($mode -ceq 'held') { { & $refuse 2 'name' 'agent name is taken' }.GetNewClosure() } else { { 'ran' } }
            "$(Invoke-LockCase -Make { param($n) $fake }.GetNewClosure() -Action $action) released=$($fake.Released) disposed=$($fake.Disposed)" }
        ($got -join ';') | Should BeExactly '2 lock timed out waiting for the agent lock|1| released=0 disposed=1;ok ran|1| released=1 disposed=1;threw IOException|1| released=0 disposed=1;2 name agent name is taken|1| released=1 disposed=1' }
    It 'releases a retried take: with a handle kept open, another runspace cannot take the name inside the action and takes the same mutex with timeout 0 after' {
        # Catches: a take that disposes without ReleaseMutex; without the kept handle the probe would create a new mutex.
        $global:lockProbeName = 'MuxAgentsLockTest-' + [guid]::NewGuid().ToString('N')
        $keeper = [Threading.Mutex]::new($false, $global:lockProbeName)
        try { $n = $global:lockProbeName
            $out = Invoke-LockCase -Throws @([IO.IOException]::new('x')) -Make $real -NameOf { param($LockKey) $n }.GetNewClosure() -Action { $global:lockInside = Test-LockFree $global:lockProbeName; 'ran' }
            "$out $global:lockInside $(Test-LockFree $n)" | Should BeExactly 'ok ran|2|sleep 100 False True' }
        finally { $keeper.Dispose(); Remove-Variable lockProbeName, lockInside -Scope Global -ErrorAction SilentlyContinue } } }
