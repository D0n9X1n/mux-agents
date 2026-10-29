# rmux-agent pane launcher: pwsh -NoProfile -File rmux-agent-launcher.ps1 <spec.json>. Waits for the launch commit,
# runs the client once in this pane and records its exit code; the pane closes with the client.
param([Parameter(Mandatory)][string]$Spec)
. (Join-Path $PSScriptRoot 'RmuxAgents.ps1')

$P = New-RmuxAgentProviders -Entry (Join-Path $PSScriptRoot 'rmux-agent.ps1')
# Only a spec.json inside an agent folder of the state root, with no junction or link on the way, is read.
$root = [IO.Path]::GetFullPath($P.StateRoot).TrimEnd('\') + '\'
$full = [IO.Path]::GetFullPath($Spec)
if (-not $full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($full) -cne 'spec.json') {
    [Console]::Error.WriteLine('rmux-agent: the launch spec is outside the state folder')
    exit 2
}
try { Assert-RmuxAgentNoReparse $full }
catch {
    [Console]::Error.WriteLine('rmux-agent: the launch spec path has a junction or link')
    exit 2
}
$code = try { Invoke-RmuxAgentLauncher -P $P -Spec $full }
catch {
    # Any launcher failure still leaves an exit record, so the parent gets 'exited' rather than 'lost'.
    [Console]::Error.WriteLine("rmux-agent: the launcher failed: $($_.Exception.Message)")
    try { Write-RmuxAgentJson -Path (Join-Path ([IO.Path]::GetDirectoryName($full)) 'launcher.json') -Value ([ordered]@{ pid = $PID; exitCode = 1; exitedAt = [DateTime]::UtcNow.ToString('o') }) } catch { }
    1
}
# The exit notice for the parent; a stop that already posted one wins.
try {
    $folder = [IO.Path]::GetDirectoryName($full)
    $record = Read-RmuxAgentJson -Path (Join-Path $folder 'record.json')
    $server = Read-RmuxAgentJson -Path (Join-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetDirectoryName($folder))) 'server.json')
    if ($record -and $server) {
        $caller = [pscustomobject]@{ ServerKey = [IO.Path]::GetFileName([IO.Path]::GetDirectoryName([IO.Path]::GetDirectoryName($folder))); Session = $record.session }
        Use-RmuxAgentLock -LockKey "notice|$($caller.ServerKey)|$($record.id)" -LockAction {
            $null = Add-RmuxAgentNotice -P $P -Caller $caller -Record $record -Kind exited -ExitCode $code
        }
    }
}
catch { }
exit $code
