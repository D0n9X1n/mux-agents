# mux-agent pane launcher: pwsh -NoProfile -File mux-agent-launcher.ps1 <spec.json>. Waits for the launch commit,
# runs the client once in this pane and records its exit code; the pane closes with the client.
param([Parameter(Mandatory)][string]$Spec)
. (Join-Path $PSScriptRoot 'MuxAgents.ps1')

$P = New-MuxAgentProviders -Entry (Join-Path $PSScriptRoot 'mux-agent.ps1')
# Only a spec.json inside an agent folder of the state root, with no junction or link on the way, is read.
$full = [IO.Path]::GetFullPath($Spec)
if (-not (Test-MuxAgentSpecPath -P $P -Full $full)) {
    [Console]::Error.WriteLine('mux-agent: the launch spec is outside the state folder')
    exit 2
}
try { Assert-MuxAgentNoReparse $full }
catch {
    [Console]::Error.WriteLine('mux-agent: the launch spec path has a junction or link')
    exit 2
}
$code = try { Invoke-MuxAgentLauncher -P $P -Spec $full }
catch {
    # Any launcher failure still leaves an exit record, so the parent gets 'exited' rather than 'lost'.
    [Console]::Error.WriteLine("mux-agent: the launcher failed: $($_.Exception.Message)")
    try { Write-MuxAgentJson -Path (Join-Path ([IO.Path]::GetDirectoryName($full)) 'launcher.json') -Value ([ordered]@{ pid = $PID; exitCode = 1; exitedAt = [DateTime]::UtcNow.ToString('o') }) } catch { }
    1
}
# The exit notice for the parent; a stop that already posted one wins.
try {
    $folder = [IO.Path]::GetDirectoryName($full)
    $record = Read-MuxAgentJson -Path (Join-Path $folder 'record.json')
    $server = Read-MuxAgentJson -Path (Join-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetDirectoryName($folder))) 'server.json')
    if ($record -and $server) {
        $caller = [pscustomobject]@{ ServerKey = [IO.Path]::GetFileName([IO.Path]::GetDirectoryName([IO.Path]::GetDirectoryName($folder))); Session = $record.session }
        Use-MuxAgentLock -LockKey "notice|$($caller.ServerKey)|$($record.id)" -LockAction {
            $null = Add-MuxAgentNotice -P $P -Caller $caller -Record $record -Kind exited -ExitCode $code
        }
    }
}
catch { }
exit $code
