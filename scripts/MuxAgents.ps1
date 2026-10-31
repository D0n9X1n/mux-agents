# mux-agent core: verb table, argument parser, dispatcher, result output, providers, the rmux seam,
# caller resolution, the rmux transport and the registry (state files, locks, panes and names).
# PowerShell 7 only. Process, rmux and clock side effects go through the providers object ($P) from
# New-MuxAgentProviders; registry files live under $P.StateRoot and locks are named mutexes.
$script:MuxAgentsRoot = $PSScriptRoot

$script:MuxAgentVerbs = @{}
# A Claude or Copilot session id: a GUID, any letter case. It becomes a path and an argv token.
$script:MuxAgentSessionIdPattern = '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z'
# The only characters an Entry may hold, since the header puts it inside double quotes that pwsh and bash expand.
$script:MuxAgentEntryPattern = '^[\p{L}\p{Nd} _\\/:.()-]+\z'
$script:MuxAgentPlatformMessage = 'mux-agents 0.1.0 runs on Windows, Linux and macOS only'

function Register-MuxAgentVerb {
    # Handler is a scriptblock or function name called with -P and -Request; it returns one dictionary or nothing.
    param([Parameter(Mandatory)][string]$Verb, [Parameter(Mandatory)]$Handler, [string[]]$Values = @(), [string[]]$Switches = @(), [int]$Min = 0, [int]$Max = 0, [Parameter(Mandatory)][string]$Usage)
    $script:MuxAgentVerbs[$Verb] = [pscustomobject]@{ Verb = $Verb; Handler = $Handler; Values = $Values; Switches = $Switches; Min = $Min; Max = $Max; Usage = $Usage }
}

function Exit-MuxAgentCommand {
    # Typed refusal: code 1 (cannot run) or 2 (usage or policy), a short check name and a fixed message.
    param([ValidateRange(1, 2)][int]$Code, [Parameter(Mandatory)][string]$Check, [string]$Message = $Check)
    $exception = [InvalidOperationException]::new($Message)
    $exception.Data['MuxAgentCode'] = $Code
    $exception.Data['MuxAgentCheck'] = $Check
    throw $exception
}

function ConvertFrom-MuxAgentArgs {
    # <verb>, then -Value <next token, whatever it looks like>, -Switch and positionals; -Text is a switch of every verb.
    param([string[]]$Arguments)
    $tokens = [string[]]@()
    if ($null -ne $Arguments) { $tokens = $Arguments }
    if ($tokens.Count -eq 0 -or -not $script:MuxAgentVerbs.ContainsKey([string]$tokens[0])) {
        Exit-MuxAgentCommand 2 'usage' ('usage: mux-agent <' + (($script:MuxAgentVerbs.Keys | Sort-Object) -join '|') + '> ...')
    }
    $definition = $script:MuxAgentVerbs[[string]$tokens[0]]
    $options = [ordered]@{}
    $positional = [Collections.Generic.List[string]]::new()
    $text = $false
    for ($index = 1; $index -lt $tokens.Count; $index++) {
        $token = [string]$tokens[$index]
        $value = @($definition.Values | Where-Object { "-$_" -eq $token })
        $switch = @($definition.Switches | Where-Object { "-$_" -eq $token })
        if ($value.Count) {
            if ($index + 1 -ge $tokens.Count -or $options.Contains($value[0])) { Exit-MuxAgentCommand 2 'usage' $definition.Usage }
            $index++
            $options[$value[0]] = [string]$tokens[$index]
        }
        elseif ($switch.Count) { $options[$switch[0]] = $true }
        elseif ($token -eq '-Text') { $text = $true }
        elseif ($token.StartsWith('-')) { Exit-MuxAgentCommand 2 'usage' $definition.Usage }
        else { $positional.Add($token) }
    }
    if ($positional.Count -lt $definition.Min -or $positional.Count -gt $definition.Max) { Exit-MuxAgentCommand 2 'usage' $definition.Usage }
    [pscustomobject]@{ Verb = $definition.Verb; Handler = $definition.Handler; Options = $options; Positional = [string[]]$positional.ToArray(); Text = $text }
}

function ConvertTo-MuxAgentTextRow {
    param($Row)
    $cells = if ($Row -is [Collections.IDictionary]) { foreach ($cell in $Row.Values) { [string]$cell } }
    elseif ($Row -is [Management.Automation.PSCustomObject]) { foreach ($property in $Row.PSObject.Properties) { [string]$property.Value } }
    else { [string]$Row }
    @($cells) -join '  '
}

function Format-MuxAgentResult {
    # One compressed ASCII JSON line, or text: array values as rows joined by two spaces, other values as key: value.
    param([Parameter(Mandatory)][Collections.IDictionary]$Data, [switch]$Text)
    if (-not $Text) { return (ConvertTo-Json -InputObject $Data -Depth 6 -Compress -EscapeHandling EscapeNonAscii) }
    $lines = foreach ($key in $Data.Keys) {
        if ($key -in @('ok', 'verb')) { continue }
        $value = $Data[$key]
        if ($value -is [Collections.IEnumerable] -and $value -isnot [string] -and $value -isnot [Collections.IDictionary]) {
            foreach ($row in $value) { ConvertTo-MuxAgentTextRow $row }
        }
        else { "${key}: $value" }
    }
    @($lines) -join "`n"
}

function Invoke-MuxAgentCommand {
    # Parse, run the verb and map the outcome to code 0, 1 or 2 without echoing exception text.
    param([Parameter(Mandatory)]$P, [string[]]$Arguments)
    # Stop reaches the verb's whole call tree, so a non-terminating error is also code 1 internal and never printed.
    $ErrorActionPreference = 'Stop'
    $verb = $null
    $text = $false
    try {
        $request = ConvertFrom-MuxAgentArgs -Arguments $Arguments
        $verb = $request.Verb
        $text = $request.Text
        # Every verb refuses a sandboxed caller before any rmux call.
        if (-not $P.Admitted) { & $P.Sandbox $P }
        $data = & $request.Handler -P $P -Request $request
        if ($null -ne $data -and $data -isnot [Collections.IDictionary]) { throw [InvalidOperationException]::new('verb result must be one dictionary') }
        $result = [ordered]@{ ok = $true; verb = $verb }
        if ($null -ne $data) { foreach ($key in $data.Keys) { $result[$key] = $data[$key] } }
        $code = 0
    }
    catch { $result = ConvertTo-MuxAgentFailure $_ $verb; $code = $result.code }
    [pscustomobject]@{ Code = $code; Text = (Format-MuxAgentResult -Data $result -Text:$text) }
}

function Get-MuxAgentHash {
    # SHA-256 of the UTF-8 text as lowercase hex.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Text))).ToLowerInvariant()
}

function ConvertTo-MuxAgentJsonText {
    param([Parameter(Mandatory)]$Value)
    ConvertTo-Json -InputObject $Value -Depth 100 -Compress
}

function Get-MuxAgentStateRoot {
    # MUX_AGENTS_STATE when set, else %LOCALAPPDATA%\mux-agents\state. Environment variables only: GetFolderPath
    # ignores a LOCALAPPDATA override.
    param([Parameter(Mandatory)][Collections.IDictionary]$Environment)
    if ($Environment['MUX_AGENTS_STATE']) { return [string]$Environment['MUX_AGENTS_STATE'] }
    if (-not $Environment['LOCALAPPDATA']) { throw [InvalidOperationException]::new('mux-agent: LOCALAPPDATA is not set') }
    [IO.Path]::Combine([string]$Environment['LOCALAPPDATA'], 'mux-agents', 'state')
}

function Get-MuxAgentPlatform {
    # windows, linux or macos for the given OS flags (default: this process), else $null.
    param([bool]$OnWindows = $IsWindows, [bool]$OnLinux = $IsLinux, [bool]$OnMacOS = $IsMacOS)
    if ($OnWindows) { 'windows' } elseif ($OnLinux) { 'linux' } elseif ($OnMacOS) { 'macos' }
}

function Get-MuxAgentEntryRefusal {
    # The entry's one-line reply on a platform mux-agents does not run on; $null on windows, linux and macos.
    param([AllowEmptyString()][string]$Platform)
    if ($Platform -cin 'windows', 'linux', 'macos') { return $null }
    ConvertTo-MuxAgentJsonText ([ordered]@{ ok = $false; verb = $null; code = 2; check = 'platform'; message = $script:MuxAgentPlatformMessage }) }

function Get-MuxAgentPlatformMembers {
    # The OS members of $P (the contract): Windows today's code; linux and macos the B2/B3 bodies; anything else refuses 2 'platform'.
    param([AllowEmptyString()][string]$Platform)
    if ($Platform -cin 'linux', 'macos') {
        # One kernel per process; the members read it from script scope (B2). Only Processes and Sandbox differ by OS.
        $script:MuxAgentUnixKernel = if ($Platform -ceq 'linux') { New-MuxAgentLinuxKernel } else { New-MuxAgentMacKernel }
        return [ordered]@{
            Platform = $Platform
            Pwsh = [IO.Path]::Combine($PSHOME, 'pwsh')
            Processes = if ($Platform -ceq 'linux') { { Get-MuxAgentLinuxProcessTable -Kernel $script:MuxAgentUnixKernel } } else { { Get-MuxAgentMacProcessTable -Kernel $script:MuxAgentUnixKernel } }
            Open = { param([int]$Id) Open-MuxAgentUnixProcess -Id $Id -Kernel $script:MuxAgentUnixKernel }
            Kill = { param($Opened) Stop-MuxAgentUnixProcess -Opened $Opened -Kernel $script:MuxAgentUnixKernel }
            Token = { $null }
            Sandbox = if ($Platform -ceq 'linux') { { param($P) Assert-MuxAgentLinuxSandbox -Environment $P.Env -Kernel $script:MuxAgentUnixKernel } } else { { param($P) Assert-MuxAgentMacSandbox -Environment $P.Env -Kernel $script:MuxAgentUnixKernel } }
            Endpoint = { param($Environment) Get-MuxAgentUnixEndpoint -Environment $Environment }
            Mux = { param($P) Resolve-MuxAgentUnixMux -P $P }
            ClientOf = { param($Row) Get-MuxAgentUnixClient -Row $Row -Kernel $script:MuxAgentUnixKernel }
            IsShell = { param($Row) Test-MuxAgentUnixShell -Row $Row }
            SameStart = { param($Opened, $Listed) Test-MuxAgentUnixSameStart -Opened $Opened -Listed $Listed }
            Argv = { param($Row) Get-MuxAgentUnixArgv -Row $Row }
            ClaudeDirectory = { param($P, $Caller) Get-MuxAgentUnixClaudeDirectory -P $P -Caller $Caller -Kernel $script:MuxAgentUnixKernel }
            StopReport = { param($Left) Get-MuxAgentUnixStopReport -Left $Left }
            StateRootOf = { param($Environment) Get-MuxAgentUnixStateRoot -Environment $Environment }
            # The factory sets $script:MuxAgentProviders after StateRoot, so Root is the selected state root (B3).
            PrivateDirectories = { param([string[]]$Paths) New-MuxAgentUnixPrivateDirectories -Root $script:MuxAgentProviders.StateRoot -Paths $Paths }
            # Global\MuxAgents-<uid>-<hash> (B3); the uid is read once per process, at the first lock.
            LockName = { param([string]$LockKey) if (-not $script:MuxAgentUnixUser) { $script:MuxAgentUnixUser = Get-MuxAgentUnixUserId }; Get-MuxAgentUnixLockName -LockKey $LockKey -User $script:MuxAgentUnixUser }
            ClientImage = { param([string]$Client) $Client }
            # StateRoot is canonical (realpath) on Unix, so no GetFullPath here.
            IsUnder = { param([string]$Root, [string]$Full) $base = $Root.TrimEnd('/') + '/'; $Full.Length -gt $base.Length -and $Full.StartsWith($base, [StringComparison]::Ordinal) }
        }
    }
    if ($Platform -cne 'windows') { Exit-MuxAgentCommand 2 'platform' $script:MuxAgentPlatformMessage }
    [ordered]@{
        Platform = 'windows'
        Pwsh = (Join-Path $PSHOME 'pwsh.exe')
        Processes = { Get-MuxAgentProcessTable }
        Open = {
            param([int]$Id)
            $process = Get-Process -Id $Id -ErrorAction SilentlyContinue
            if (-not $process) { return $null }
            # get_Handle opens and keeps the process handle, so the id cannot be reused while it is held. Call the
            # accessors: a PowerShell property read turns a failing getter into $null instead of throwing.
            $start = try { $null = $process.get_Handle(); $process.get_StartTime().ToFileTimeUtc() } catch { $null }
            [pscustomobject]@{ Id = $Id; Start = $start; Process = $process }
        }
        Kill = {
            param($Opened)
            # Process.Kill is asynchronous; report whether the opened process exited within 5 s.
            try {
                if (-not $Opened.Process.HasExited) { $Opened.Process.Kill() }
                $Opened.Process.WaitForExit(5000)
            }
            catch { $false }
        }
        Token = { Get-MuxAgentToken }
        Sandbox = { param($P) Assert-MuxAgentToken -Token (& $P.Token) }
        Endpoint = {
            param($Environment)
            $endpoint = if ($Environment['RMUX']) { $Environment['RMUX'] } else { $Environment['TMUX'] }
            if ($endpoint -match '^(\\\\\.\\pipe\\[^,\r\n]+),(\d+),\d+$') { [pscustomobject]@{ Socket = $Matches[1]; ServerPid = $Matches[2] } }
        }
        Mux = { param($P) Resolve-MuxAgentExecutable -P $P }
        ClientOf = {
            param($Row)
            $file = [IO.Path]::GetFileName($Row.Path)
            if ($file -in @('claude.exe', 'copilot.exe')) { [IO.Path]::GetFileNameWithoutExtension($file).ToLowerInvariant() }
        }
        IsShell = { param($Row) [IO.Path]::GetFileName($Row.Path) -in @('pwsh.exe', 'powershell.exe', 'cmd.exe', 'bash.exe', 'sh.exe') }
        # Open keeps 100 ns and the CIM row whole microseconds; an unreadable start is never the same.
        SameStart = { param($Opened, $Listed) $null -ne $Opened -and ($Opened - $Opened % 10) -eq $Listed }
        Argv = { param($Row) @(Split-MuxAgentCommandLine -CommandLine ([string]$Row.CommandLine)) }
        ClaudeDirectory = {
            param($P, $Caller)
            $session = Read-MuxAgentJson -Path ([IO.Path]::Combine($P.Home, '.claude', 'sessions', "$($Caller.Agent.Pid).json"))
            # procStart is a FILETIME; compare at whole microseconds like the process table.
            $start = [long]$session.procStart
            $same = ($start - $start % 10) -eq ($Caller.Agent.Start - $Caller.Agent.Start % 10)
            if ($same -and $session.sessionId -ceq $P.Env['CLAUDE_CODE_SESSION_ID']) { [string]$session.cwd }
        }
        # Left holds every tree row stop did not kill ({Pid, Reason open|start|kill, Row}); survivors counts the failed kills.
        StopReport = { param($Left) [ordered]@{ survivors = @($Left | Where-Object { $_.Reason -ceq 'kill' }).Count } }
        StateRootOf = { param($Environment) Get-MuxAgentStateRoot -Environment $Environment }
        PrivateDirectories = {
            param([string[]]$Paths)
            $sid = Get-MuxAgentUserSid
            foreach ($path in $Paths) { New-MuxAgentPrivateDirectory $sid $path }
        }
        LockName = { param([string]$LockKey) 'Local\MuxAgents-' + (Get-MuxAgentHash $LockKey).Substring(0, 32) }
        ClientImage = { param([string]$Client) "$Client.exe" }
        IsUnder = { param([string]$Root, [string]$Full) $Full.StartsWith([IO.Path]::GetFullPath($Root).TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) }
    }
}
# Locks use the Windows name until New-MuxAgentProviders installs the selected platform's LockName.
$script:MuxAgentLockName = (Get-MuxAgentPlatformMembers -Platform windows).LockName

function Resolve-MuxAgentExecutable {
    # The rmux image the caller's server runs (by the server PID in RMUX, then TMUX), else the first rmux.exe on PATH.
    param([Parameter(Mandatory)]$P)
    $endpoint = if ($P.Env['RMUX']) { $P.Env['RMUX'] } else { $P.Env['TMUX'] }
    if ($endpoint -match ',(\d+),\d+$') {
        $serverPid = [int]$Matches[1]
        $server = @(& $P.Processes | Where-Object Pid -EQ $serverPid) | Select-Object -First 1
        if ($server.Path -and [IO.Path]::GetFileName($server.Path) -eq 'rmux.exe') { return $server.Path }
    }
    $onPath = Get-Command rmux.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    Exit-MuxAgentCommand 1 'endpoint' 'rmux.exe was not found'
}

function Get-MuxAgentPaneEnvironment {
    # The environment arguments for a new agent pane. On Unix a pane gets the tmux server's environment, not the caller's,
    # so a MUX_AGENTS_STATE or XDG_STATE_HOME set in the caller's shell would not reach the launcher, which would pick
    # another state root and refuse the spec; the selected root goes in with -e. Windows: none (unchanged).
    param([Parameter(Mandatory)]$P)
    if ($P.Platform -ceq 'windows') { return }
    '-e'; "MUX_AGENTS_STATE=$($P.StateRoot)"
}

function Find-MuxAgentApplication {
    # The first application named Name on PATH, or $null.
    param([Parameter(Mandatory)][string]$Name)
    $found = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { $found.Source } }

function Invoke-MuxAgentProcess {
    # One bounded native call: argv through ArgumentList, stdin closed, UTF-8 output, working directory $HOME. A
    # timeout kills the process tree and throws.
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Arguments, [int]$TimeoutMilliseconds = 15000)
    $info = [Diagnostics.ProcessStartInfo]::new($FilePath)
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $info.WorkingDirectory = $HOME
    $process = [Diagnostics.Process]::Start($info)
    try {
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            try { $process.Kill($true) } catch { }
            throw [TimeoutException]::new('mux-agent: native call timed out')
        }
        $process.WaitForExit()
        [pscustomobject]@{ ExitCode = $process.ExitCode; StdOut = $stdout.GetAwaiter().GetResult(); StdErr = $stderr.GetAwaiter().GetResult() }
    }
    finally { $process.Dispose() }
}

function Invoke-MuxAgentRmux {
    # The production Rmux member: <rmux> -S <caller socket> -N <rest>. A call without a leading -N throws before any
    # process starts, so nothing here can create a server.
    param([Parameter(Mandatory)]$P, [string[]]$Arguments)
    if (-not $Arguments -or $Arguments[0] -cne '-N') { throw [InvalidOperationException]::new('mux-agent: every rmux call must start with -N') }
    $parsed = & $P.Endpoint $P.Env
    if (-not $parsed) { Exit-MuxAgentCommand 1 'endpoint' 'not inside an rmux pane' }
    $socket = $parsed.Socket
    if (-not $P.RmuxPath) { $P.RmuxPath = & $P.Mux $P }
    try { Invoke-MuxAgentProcess -FilePath $P.RmuxPath -Arguments (@('-S', $socket) + $Arguments) -TimeoutMilliseconds 15000 }
    catch { Exit-MuxAgentCommand 1 'endpoint' 'the rmux endpoint did not answer' }
}

function New-MuxAgentProviders {
    # The production side effects; tests pass New-FakeMuxAgentProviders instead.
    param([Parameter(Mandatory)][string]$Entry)
    # Resolve against the PowerShell location, not the process directory.
    $Entry = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Entry)
    $environment = [hashtable]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($item in [Environment]::GetEnvironmentVariables().GetEnumerator()) { $environment[[string]$item.Key] = [string]$item.Value }
    $members = [ordered]@{ Env = $environment; Pid = $PID; Cwd = (Get-Location -PSProvider FileSystem).ProviderPath; Home = $HOME }
    # The only OS branch: the members of the running platform (any other platform refuses 2 'platform').
    $os = Get-MuxAgentPlatformMembers -Platform (Get-MuxAgentPlatform)
    foreach ($key in $os.Keys) { $members[$key] = $os[$key] }
    $members.StateRoot = & $os.StateRootOf $environment
    $members.Entry = $Entry
    $members.Launcher = [IO.Path]::Combine([IO.Path]::GetDirectoryName($Entry), 'mux-agent-launcher.ps1')
    $members.RmuxPath = $null
    $members.Rmux = { param([string[]]$Arguments) Invoke-MuxAgentRmux -P $script:MuxAgentProviders -Arguments $Arguments }
    $members.Now = { [DateTime]::UtcNow }
    $members.Sleep = { param([int]$Milliseconds) Start-Sleep -Milliseconds $Milliseconds }
    $members.Admitted = $false
    $script:MuxAgentLockName = $os.LockName
    $providers = [pscustomobject]$members
    # The Rmux member reads the endpoint from the latest providers and caches the resolved rmux image on them. A
    # closure would hide the script's functions from it.
    $script:MuxAgentProviders = $providers
    $providers
}

function ConvertTo-MuxAgentFailure {
    # A caught error as the failure reply: a typed refusal keeps its code, check and message; anything else is 1 internal.
    param([Parameter(Mandatory)][Management.Automation.ErrorRecord]$ErrorRecord, $Verb)
    $failure = [ordered]@{ ok = $false; verb = $Verb; code = 1; check = 'internal'; message = 'unexpected error (' + $ErrorRecord.Exception.GetBaseException().GetType().FullName + ')' }
    for ($e = $ErrorRecord.Exception; $e; $e = $e.InnerException) { if ($e.Data.Contains('MuxAgentCode')) { $failure.code = [int]$e.Data['MuxAgentCode']; $failure.check = [string]$e.Data['MuxAgentCheck']; $failure.message = $e.Message; break } }
    $failure }
function Initialize-MuxAgentProviders {
    # New-MuxAgentProviders for the entry and the launcher: a refusal while building comes back as Failure, never thrown.
    param([Parameter(Mandatory)][string]$Entry)
    $ErrorActionPreference = 'Stop'
    try { [pscustomobject]@{ P = (New-MuxAgentProviders -Entry $Entry); Failure = $null } } catch { [pscustomobject]@{ P = $null; Failure = (ConvertTo-MuxAgentFailure $_) } } }

function Invoke-MuxAgentNative {
    # The only rmux path: every call starts with -N (never start a server); a failure never echoes stderr.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)][string[]]$Arguments, [switch]$AllowFailure)
    $result = & $P.Rmux (@('-N') + $Arguments)
    if ($result.ExitCode -ne 0 -and -not $AllowFailure) { Exit-MuxAgentCommand 1 'rmux' 'rmux call failed' }
    $result
}

function Get-MuxAgentProcessTable {
    # One CIM read of every process. Start (FILETIME) only orders parents before children; identity starts come from Open.
    foreach ($process in Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, CreationDate, ExecutablePath, CommandLine) {
        [pscustomobject]@{ Pid = [int]$process.ProcessId; ParentPid = [int]$process.ParentProcessId; Start = $process.CreationDate.ToFileTimeUtc(); Path = $process.ExecutablePath; CommandLine = $process.CommandLine }
    }
}

function Resolve-MuxAgentCaller {
    # The caller's pane, server identity and nearest Claude or Copilot client. Refuses 1 'endpoint' for an unusable
    # endpoint or pane, 1 'caller' when the caller is not below its pane or its agent's start is unreadable or changed,
    # and 2 'caller' for a non-shell in between. Chain holds the walked rows, command lines included, and stays in memory.
    param([Parameter(Mandatory)]$P)
    $parsed = & $P.Endpoint $P.Env
    if (-not $parsed) { Exit-MuxAgentCommand 1 'endpoint' 'not inside an rmux pane' }
    $socket = $parsed.Socket
    $serverPid = $parsed.ServerPid
    $pane = $P.Env['TMUX_PANE']
    if ($pane -notmatch '^%\d+$') { Exit-MuxAgentCommand 1 'endpoint' 'not inside an rmux pane' }
    # rmux 0.10.0 answers a probe of a gone pane with exit 0 and empty fields, so the row itself is validated.
    # A typed refusal (Connect's) keeps its message; any other failure, a timeout included, is an unanswered probe.
    $probe = try { Invoke-MuxAgentNative -P $P -Arguments @('display-message', '-p', '-t', $pane, '#{pid}|#{pane_pid}|#{session_id}|#{window_id}|#{@agent-id}') -AllowFailure }
    catch { if ($_.Exception.Data.Contains('MuxAgentCode')) { throw }; $null }
    if ($probe.ExitCode -ne 0 -or -not (([string]$probe.StdOut).TrimEnd("`r`n") -match '^(\d+)\|(\d+)\|(\$\d+)\|(@\d+)\|(.*)$')) {
        Exit-MuxAgentCommand 1 'endpoint' 'rmux endpoint did not answer'
    }
    if ($Matches[1] -ne $serverPid) { Exit-MuxAgentCommand 1 'endpoint' 'rmux server changed' }
    $panePid = $Matches[2]
    $session = $Matches[3]
    $window = $Matches[4]
    $tag = $Matches[5]
    # A restarted server can reuse the socket and PID, so the key includes its exact start; no state is touched without it.
    $server = & $P.Open $serverPid
    if ($null -eq $server.Start) { Exit-MuxAgentCommand 1 'endpoint' 'rmux server start is unreadable' }
    $serverKey = (Get-MuxAgentHash "$socket|$serverPid|$($server.Start)").Substring(0, 16)

    $table = @{}
    foreach ($process in & $P.Processes) { $table[$process.Pid] = $process }
    # Walk up at most 64 parents to the pane; a parent that started after its child is a reused PID and ends the walk.
    $chain = [Collections.Generic.List[object]]::new()
    $row = $table[$P.Pid]
    while ($row) {
        $chain.Add($row)
        if ($row.Pid -eq $panePid -or $chain.Count -gt 64) { break }
        $parent = $table[$row.ParentPid]
        $row = if ($parent.Start -le $row.Start) { $parent }
    }
    if ($row.Pid -ne $panePid) { Exit-MuxAgentCommand 1 'caller' 'caller is not below its rmux pane' }
    # The nearest row that $P.ClientOf names above the caller is the agent; only shells may run in between
    # (for a human, up to and including the pane).
    $agentIndex = 0
    for ($index = 1; $index -lt $chain.Count; $index++) {
        if (& $P.ClientOf $chain[$index]) { $agentIndex = $index; break }
    }
    $last = if ($agentIndex) { $agentIndex - 1 } else { $chain.Count - 1 }
    for ($index = 1; $index -le $last; $index++) {
        if (-not (& $P.IsShell $chain[$index])) { Exit-MuxAgentCommand 2 'caller' 'caller runs under a non-shell process' }
    }
    $agent = $null
    if ($agentIndex) {
        $opened = & $P.Open $chain[$agentIndex].Pid
        if ($null -eq $opened.Start) { Exit-MuxAgentCommand 1 'caller' 'agent start is unreadable' }
        # A start that SameStart rejects is a PID reused after the snapshot.
        if (-not (& $P.SameStart $opened.Start $chain[$agentIndex].Start)) { Exit-MuxAgentCommand 1 'caller' 'agent process changed' }
        $agent = [pscustomobject]@{ Pid = $chain[$agentIndex].Pid; Start = $opened.Start; Path = $chain[$agentIndex].Path }
    }
    [pscustomobject]@{
        Kind = if ($agent) { 'agent' } else { 'human' }
        Client = if ($agent) { & $P.ClientOf $chain[$agentIndex] }
        Agent = $agent; Chain = $chain.ToArray(); Pane = $pane; Window = $window; Session = $session
        ServerPid = [int]$serverPid; ServerStart = $server.Start; Socket = $socket; ServerKey = $serverKey; AgentTag = $tag
    }
}

function Assert-MuxAgentNoReparse {
    # Refuses 1 'state' when Path or any existing ancestor is a junction or symbolic link.
    param([Parameter(Mandatory)][string]$Path)
    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        if (Test-Path -LiteralPath $current) {
            if ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { Exit-MuxAgentCommand 1 'state' 'the state folder path has a junction or link' }
        }
        $parent = [IO.Path]::GetDirectoryName($current.TrimEnd('\'))
        if ($parent -eq $current) { break }
        $current = $parent
    }
}

function Assert-MuxAgentPrivateAcl {
    # Refuses 1 'state' unless the owner and every allow rule are the user, SYSTEM or Administrators.
    param([Parameter(Mandatory)][string]$Sid, [Parameter(Mandatory)][string]$Path)
    Assert-MuxAgentNoReparse $Path
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $allowed = @($Sid, 'S-1-5-18', 'S-1-5-32-544')
    if ($allowed -notcontains $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value) { Exit-MuxAgentCommand 1 'state' 'the state folder has another owner' }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and $allowed -notcontains $rule.IdentityReference.Value) {
            Exit-MuxAgentCommand 1 'state' 'the state folder is shared with other users'
        }
    }
}

function New-MuxAgentPrivateDirectory {
    # Creates Path (and missing parents) private to the user, SYSTEM and Administrators; an existing folder is checked.
    param([Parameter(Mandatory)][string]$Sid, [Parameter(Mandatory)][string]$Path)
    Assert-MuxAgentNoReparse $Path
    if (Test-Path -LiteralPath $Path) {
        if (-not (Test-Path -LiteralPath $Path -PathType Container)) { Exit-MuxAgentCommand 1 'state' 'the state folder path is a file' }
        Assert-MuxAgentPrivateAcl $Sid $Path
        return
    }
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $parent)) { New-MuxAgentPrivateDirectory $Sid $parent }
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetOwner([Security.Principal.SecurityIdentifier]::new($Sid))
    foreach ($member in @($Sid, 'S-1-5-18', 'S-1-5-32-544')) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($member), 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    [IO.FileSystemAclExtensions]::Create([IO.DirectoryInfo]::new($Path), $acl)
    Assert-MuxAgentPrivateAcl $Sid $Path
}

function Get-MuxAgentUserSid {
    # The current user's SID; the token handle is released at once, not left to the finalizer.
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try { $identity.User.Value } finally { $identity.Dispose() }
}

function Get-MuxAgentStateDirectory {
    # <StateRoot>\<ServerKey> and its agents and mail folders, each private to the current user, SYSTEM and
    # Administrators. An existing folder with a wider ACL is refused, not repaired.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    $state = Join-Path $P.StateRoot $Caller.ServerKey
    $null = & $P.PrivateDirectories -Paths @($state, (Join-Path $state 'agents'), (Join-Path $state 'mail'))
    $state
}

function Write-MuxAgentJson {
    # The JSON goes to a tmp file in the same folder, which then replaces or becomes Path, so the file is never
    # partial and a failed Replace keeps the old file. During a Replace the name is briefly absent: every read and
    # write of a mutable state file happens under the same session lock (Use-MuxAgentLock); this helper does not
    # lock. Replace, unlike an overwriting move, works while a reader holds the old file.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    $temporary = $Path + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
    try {
        [IO.File]::WriteAllText($temporary, (ConvertTo-MuxAgentJsonText $Value))
        if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, [NullString]::Value) }
        else { [IO.File]::Move($temporary, $Path) }
    }
    finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}

function Read-MuxAgentJson {
    # The parsed file, or $null when the file or its folder is missing; a file over 1 MB is an error. A read of a
    # mutable state file happens under the same session lock as its writes (Use-MuxAgentLock); this helper does
    # not lock. The read shares delete access, so it never blocks a Replace.
    param([Parameter(Mandatory)][string]$Path)
    try { $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::Read -bor [IO.FileShare]::Delete)) }
    catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] { return $null }
    try {
        if ($stream.Length -gt 1MB) { throw [InvalidOperationException]::new('mux-agent: state file is larger than 1 MB') }
        $text = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8).ReadToEnd()
    }
    finally { $stream.Dispose() }
    ConvertFrom-Json -InputObject $text
}

function Use-MuxAgentLock {
    # Runs LockAction under the named mutex for LockKey and returns its output. An abandoned mutex counts as held;
    # a timeout refuses 2 'lock'. Callers take the session lock before the mailbox lock. The Lock prefix keeps these
    # names from shadowing the caller variables that LockAction reads.
    param([Parameter(Mandatory)][string]$LockKey, [Parameter(Mandatory)][scriptblock]$LockAction, [int]$LockTimeoutMilliseconds = 15000)
    $lockMutex = [Threading.Mutex]::new($false, (& $script:MuxAgentLockName $LockKey))
    $lockHeld = $false
    try {
        try { $lockHeld = $lockMutex.WaitOne($LockTimeoutMilliseconds) } catch [Threading.AbandonedMutexException] { $lockHeld = $true }
        if (-not $lockHeld) { Exit-MuxAgentCommand 2 'lock' 'timed out waiting for the agent lock' }
        & $LockAction
    }
    finally {
        if ($lockHeld) { $lockMutex.ReleaseMutex() }
        $lockMutex.Dispose()
    }
}

function Get-MuxAgentPanes {
    # The caller session's panes tagged with a well-formed agent id and, unless -All, only agents with commit.json.
    # It only reads: the state folder is never created here.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [switch]$All)
    $listed = Invoke-MuxAgentNative -P $P -Arguments @('list-panes', '-s', '-t', $Caller.Session, '-F', '#{pane_id}|#{window_id}|#{pane_pid}|#{@agent-id}|#{@agent-name}')
    foreach ($line in ([string]$listed.StdOut -split "`r?`n")) {
        $pane, $window, $panePid, $id, $name = $line -split '\|', 5
        if ($id -cnotmatch '^[0-9a-f]{12}$') { continue }
        if (-not $All -and -not [IO.File]::Exists([IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents', $id, 'commit.json'))) { continue }
        [pscustomobject]@{ Pane = $pane; Window = $window; Pid = $panePid; Id = $id; Name = $name }
    }
}

function Resolve-MuxAgentName {
    # The requested name after the pattern, reserved and taken checks, or <client>-<n> with the smallest free n when
    # none is requested. Taken names compare without case.
    param($Requested, [Parameter(Mandatory)][string]$Client, [string[]]$Taken)
    if ($null -eq $Requested) {
        $number = 1
        while ($Taken -contains "$Client-$number") { $number++ }
        return "$Client-$number"
    }
    # Only a string is a name; an array, even of one valid name, is refused rather than flattened.
    $name = $Requested
    if ($name -isnot [string] -or $name -cnotmatch '^[a-z][a-z0-9-]{0,23}\z') { Exit-MuxAgentCommand 2 'name' 'agent names are 1-24 lowercase letters, digits or hyphens, starting with a letter' }
    if ($name -in @('parent', 'root', 'all')) { Exit-MuxAgentCommand 2 'name' 'agent name is reserved' }
    if ($Taken -contains $name) { Exit-MuxAgentCommand 2 'name' 'agent name is taken' }
    $name
}

function Get-MuxAgentContextPlan {
    # Context fork (the new agent starts from the caller's session, ParentSession) or brief, and the Reason. Only a
    # same-client agent with a well-formed session id forks; a Claude fork also needs the interactive entrypoint cli,
    # since a fork of a -p or SDK session fails interactively with API 400.
    param([Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][ValidateSet('claude', 'copilot')][string]$Client, [Parameter(Mandatory)][Collections.IDictionary]$Env, [switch]$NoFork)
    $parent = [string]$Env[$(if ($Client -eq 'claude') { 'CLAUDE_CODE_SESSION_ID' } else { 'COPILOT_AGENT_SESSION_ID' })]
    $reason = if ($NoFork) { 'no-fork' }
    elseif ($Caller.Kind -eq 'human') { 'human' }
    elseif ($Caller.Client -ne $Client) { 'cross-client' }
    elseif ($parent -notmatch $script:MuxAgentSessionIdPattern) { 'no-session-id' }
    elseif ($Client -eq 'claude' -and $Env['CLAUDE_CODE_ENTRYPOINT'] -cne 'cli') { 'not-interactive' }
    if ($reason) { return [pscustomobject]@{ Context = 'brief'; Reason = $reason; ParentSession = $null } }
    [pscustomobject]@{ Context = 'fork'; Reason = 'same-client'; ParentSession = $parent }
}

function Test-MuxAgentJsonLine {
    param([string]$Line)
    try { [Text.Json.JsonDocument]::Parse($Line).Dispose(); $true } catch { $false }
}

function Copy-MuxAgentCopilotSession {
    # Copies ~/.copilot/session-state/<SourceId> to <TargetId> and returns whether the copy is usable. Both ids must be
    # GUIDs. Top-level *.jsonl lose a final unterminated segment that does not parse (a parsable one gets its LF); *.yaml
    # are never cut. In both, SourceId becomes TargetId and workspace.yaml loses its mc_ lines. Subfolders are copied as
    # they are. The copy is built and verified in a uniquely named sibling temp folder and then moved into place; the
    # move fails when anything holds the target, and a failure deletes only the temp folder.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$HomeDirectory, [Parameter(Mandatory)][AllowEmptyString()][string]$SourceId, [Parameter(Mandatory)][AllowEmptyString()][string]$TargetId)
    if ($SourceId -notmatch $script:MuxAgentSessionIdPattern -or $TargetId -notmatch $script:MuxAgentSessionIdPattern) { return $false }
    $root = [IO.Path]::Combine($HomeDirectory, '.copilot', 'session-state')
    $source = Join-Path $root $SourceId
    $target = Join-Path $root $TargetId
    # Without a source, creating the temp folder would create session-state itself.
    if (-not [IO.Directory]::Exists($source)) { return $false }
    $temp = Join-Path $root ('.' + $TargetId + '.' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        $null = [IO.Directory]::CreateDirectory($temp)
        foreach ($folder in [IO.Directory]::EnumerateDirectories($source, '*', [IO.SearchOption]::AllDirectories)) { $null = [IO.Directory]::CreateDirectory($temp + $folder.Substring($source.Length)) }
        foreach ($file in [IO.Directory]::EnumerateFiles($source, '*', [IO.SearchOption]::AllDirectories)) {
            # Copilot may still be appending, so read with write sharing; File.Copy refuses an open writer.
            $reader = [IO.File]::Open($file, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
            try {
                $copy = [IO.File]::Create($temp + $file.Substring($source.Length))
                try { $reader.CopyTo($copy) } finally { $copy.Dispose() }
            }
            finally { $reader.Dispose() }
        }
        $written = @{}
        foreach ($file in [IO.Directory]::GetFiles($temp)) {
            $extension = [IO.Path]::GetExtension($file)
            if ($extension -notin @('.jsonl', '.yaml')) { continue }
            $jsonl = $extension -eq '.jsonl'
            $text = [IO.File]::ReadAllText($file)
            if ($jsonl) {
                $end = $text.LastIndexOf([char]10) + 1
                $text = if (Test-MuxAgentJsonLine $text.Substring($end)) { $text + "`n" } else { $text.Substring(0, $end) }
            }
            $text = $text.Replace($SourceId, $TargetId)
            if ([IO.Path]::GetFileName($file) -eq 'workspace.yaml') { $text = @($text.Split("`n") | Where-Object { -not $_.StartsWith('mc_', [StringComparison]::Ordinal) }) -join "`n" }
            [IO.File]::WriteAllText($file, $text)
            if ($jsonl) {
                foreach ($line in @($text.Split("`n") | Select-Object -SkipLast 1)) { if (-not (Test-MuxAgentJsonLine $line)) { throw [InvalidOperationException]::new('copied JSONL line does not parse') } }
            }
            if ($text.Contains($SourceId)) { throw [InvalidOperationException]::new('copied file keeps the source id') }
            $written[[IO.Path]::GetFileName($file)] = $text
        }
        if (-not (@(([string]$written['workspace.yaml']).Split("`n")) -ccontains "id: $TargetId")) { throw [InvalidOperationException]::new('copied workspace.yaml lacks the target id') }
        if (-not ([string]$written['events.jsonl']).Contains($TargetId)) { throw [InvalidOperationException]::new('copied events.jsonl lacks the target id') }
        [IO.Directory]::Move($temp, $target)
        $true
    }
    catch {
        if ([IO.Directory]::Exists($temp)) { Remove-Item -LiteralPath $temp -Recurse }
        $false
    }
}

function Get-MuxAgentClientArgs {
    # The client arguments after its executable. A Claude fork resumes and forks the parent into SessionId; a Copilot
    # fork resumes SessionId, the copy Copy-MuxAgentCopilotSession made. A brief starts SessionId fresh.
    param([Parameter(Mandatory)][ValidateSet('claude', 'copilot')][string]$Client, [Parameter(Mandatory)]$Plan, [Parameter(Mandatory)][string]$SessionId, [Parameter(Mandatory)][string]$Prompt)
    $session = switch ($Plan.Context) {
        'fork' { if ($Client -eq 'claude') { '--resume', $Plan.ParentSession, '--fork-session', '--session-id', $SessionId } else { '--resume', $SessionId } }
        'brief' { '--session-id', $SessionId }
        default { throw [ArgumentException]::new('unknown context plan') }
    }
    if ($Client -eq 'claude') { $session + $Prompt } else { $session + '-i' + $Prompt }
}

function Find-MuxAgentClaudeTranscript {
    # ~/.claude/projects/<folder>/<SessionId>.jsonl when exactly one project folder has it, else $null. A malformed
    # SessionId gives $null before any path is built.
    param([Parameter(Mandatory)][string]$HomeDirectory, [Parameter(Mandatory)][string]$SessionId)
    if ($SessionId -notmatch $script:MuxAgentSessionIdPattern) { return $null }
    $projects = [IO.Path]::Combine($HomeDirectory, '.claude', 'projects')
    if (-not [IO.Directory]::Exists($projects)) { return $null }
    $found = @(foreach ($folder in [IO.Directory]::GetDirectories($projects)) {
            $transcript = Join-Path $folder "$SessionId.jsonl"
            if ([IO.File]::Exists($transcript)) { $transcript }
        })
    if ($found.Count -eq 1) { $found[0] } else { $null }
}

function Get-MuxAgentParentTranscript {
    # The calling agent's own transcript: Claude's project JSONL or Copilot's events.jsonl. A human, a malformed
    # session id or a missing file gives $null.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    if ($Caller.Kind -ne 'agent') { return $null }
    $session = [string]$P.Env[$(if ($Caller.Client -eq 'claude') { 'CLAUDE_CODE_SESSION_ID' } else { 'COPILOT_AGENT_SESSION_ID' })]
    if ($session -notmatch $script:MuxAgentSessionIdPattern) { return $null }
    if ($Caller.Client -eq 'claude') { return (Find-MuxAgentClaudeTranscript -HomeDirectory $P.Home -SessionId $session) }
    $events = [IO.Path]::Combine($P.Home, '.copilot', 'session-state', $session, 'events.jsonl')
    if ([IO.File]::Exists($events)) { $events } else { $null }
}

function New-MuxAgentHeader {
    # The five first-turn lines: identity, parent, context, reply and listen. Paths use forward slashes. Entry goes
    # inside double quotes that pwsh and bash expand, so an Entry outside MuxAgentEntryPattern refuses 1 'entry'.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)][string]$Client, [Parameter(Mandatory)]$Plan, [string]$BriefPath, [string]$TranscriptPath)
    if ($P.Entry -notmatch $script:MuxAgentEntryPattern) { Exit-MuxAgentCommand 1 'entry' 'mux-agent entry path may hold only letters, digits, spaces and _ \ / : . ( ) -' }
    $parent = if ($Caller.Kind -eq 'agent') { "$($Caller.Client) agent" } else { 'root' }
    $context = switch ($Plan.Context) {
        'fork' { "forked from the parent session ($($Plan.Reason))" }
        'brief' {
            $brief = if ($BriefPath) { $BriefPath.Replace('\', '/') } else { 'none' }
            $transcript = if ($TranscriptPath) { $TranscriptPath.Replace('\', '/') } else { 'none' }
            "brief-only ($($Plan.Reason)); brief: $brief; parent transcript: $transcript"
        }
        default { throw [ArgumentException]::new('unknown context plan') }
    }
    $command = 'pwsh -NoProfile -File "' + $P.Entry.Replace('\', '/') + '"'
    "[mux-agent] You are $Name ($Client, id $Id), started in this RMUX session."
    "Your parent is $parent ($($Caller.Pane)); report to it with send parent; free text only in files."
    "Context: $context"
    "Reply: $command send parent -BodyFile <file>"
    "Listen: $command read -Wait 600"
}

function Join-MuxAgentPrompt {
    # The first prompt: the header lines, a blank line and the task exactly as given.
    param([Parameter(Mandatory)][string[]]$Header, [Parameter(Mandatory)][string]$Task)
    ($Header -join "`n") + "`n`n" + $Task
}

function Read-MuxAgentText {
    # A task or brief file as strict UTF-8 without its BOM. A missing, unreadable, oversized or non-UTF-8 file
    # refuses 2 'usage'.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][long]$MaxBytes)
    try { $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite) }
    catch { Exit-MuxAgentCommand 2 'usage' 'a task or brief file cannot be read' }
    try {
        if ($stream.Length -gt $MaxBytes) { Exit-MuxAgentCommand 2 'usage' "a task or brief file is larger than $MaxBytes bytes" }
        $text = try { [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false, $true), $false).ReadToEnd() }
        catch [Text.DecoderFallbackException] { Exit-MuxAgentCommand 2 'usage' 'a task or brief file is not UTF-8' }
    }
    finally { $stream.Dispose() }
    if ($text.StartsWith([char]0xFEFF)) { $text = $text.Substring(1) }
    $text
}

# The launcher's flags before the client arguments.
$script:MuxAgentClientFlags = @{ claude = @('--dangerously-skip-permissions', '--effort', 'high'); copilot = @('--no-sandbox', '--allow-all') }

function Get-MuxAgentCommandLineLength {
    # The length of Arguments as ProcessStartInfo.ArgumentList joins them: one space apart; an empty argument or one
    # with whitespace or '"' is quoted, each '"' becomes \" and a backslash run before a '"' or the closing quote doubles.
    param([Parameter(Mandatory)][AllowEmptyString()][AllowEmptyCollection()][string[]]$Arguments)
    $length = [Math]::Max(0, $Arguments.Count - 1)
    foreach ($argument in $Arguments) {
        $quoted = $argument.Length -eq 0
        $escapes = 0
        $run = 0
        foreach ($character in $argument.ToCharArray()) {
            if ($character -eq '\') { $run++; continue }
            if ($character -eq '"') { $quoted = $true; $escapes += $run + 1 }
            elseif ([char]::IsWhiteSpace($character)) { $quoted = $true }
            $run = 0
        }
        $length += $argument.Length + $(if ($quoted) { $escapes + $run + 2 } else { 0 })
    }
    $length
}

function Invoke-MuxAgentSpawn {
    # Checks the request and caller, then under the spawn lock: cap, name, context, files, pane, tag, record.json and
    # last commit.json; only a committed agent is live. A failure after the launch call writes abort.json, closes any
    # pane it named and refuses 1 'launch'. A Copilot copy is deleted on any failure.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $options = $Request.Options
    $client = $options['Client']
    if ($client -cnotin @('claude', 'copilot') -or $options.Contains('Task') -eq $options.Contains('TaskFile') -or ($options.Contains('Split') -and $options['Split'] -cnotin @('h', 'v'))) {
        Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['spawn'].Usage
    }
    $caller = Resolve-MuxAgentCaller -P $P
    if ($caller.AgentTag) { Exit-MuxAgentCommand 2 'tagged' 'an agent pane cannot spawn agents' }
    if ($caller.Kind -eq 'agent' -and $options.Contains('Task')) { Exit-MuxAgentCommand 2 'usage' 'agents pass free text only in files: use -TaskFile' }
    # Relative files are the caller's; the task travels on the 32767-character command line, so it stays at 16 KB.
    $task = if ($options.Contains('Task')) { $options['Task'] } else { Read-MuxAgentText -Path ([IO.Path]::Combine($P.Cwd, $options['TaskFile'])) -MaxBytes 16KB }
    if ($task.Length -eq 0) { Exit-MuxAgentCommand 2 'usage' 'the task is empty' }
    $brief = if ($options.Contains('BriefFile')) { Read-MuxAgentText -Path ([IO.Path]::Combine($P.Cwd, $options['BriefFile'])) -MaxBytes 1MB }
    # An agent caller passes the flag, policy and directory checks (part 3); its children start where it started.
    $directory = if ($caller.Kind -eq 'agent' -and -not $P.Admitted) { Assert-MuxAgentSpawnAllowed -P $P -Caller $caller } else { $P.Cwd }
    Use-MuxAgentLock -LockKey "spawn|$($caller.ServerKey)|$($caller.Session)" -LockAction {
        $state = Get-MuxAgentStateDirectory -P $P -Caller $caller
        Write-MuxAgentJson -Path (Join-Path $state 'server.json') -Value ([ordered]@{ socket = $caller.Socket; serverPid = $caller.ServerPid; serverStart = $caller.ServerStart })
        # Every tagged pane counts, committed or not, so a launch in progress holds its place.
        $live = @(Get-MuxAgentPanes -P $P -Caller $caller -All)
        if ($live.Count -ge 6) { Exit-MuxAgentCommand 2 'cap' 'the session already has 6 live agents' }
        $name = Resolve-MuxAgentName -Requested $options['Name'] -Client $client -Taken @($live | ForEach-Object Name)
        $plan = Get-MuxAgentContextPlan -Caller $caller -Client $client -Env $P.Env -NoFork:([bool]$options['NoFork'])
        $sessionId = [guid]::NewGuid().ToString()
        $copied = $null
        if ($plan.Context -eq 'fork' -and $client -eq 'copilot') {
            if (Copy-MuxAgentCopilotSession -HomeDirectory $P.Home -SourceId $plan.ParentSession -TargetId $sessionId) { $copied = [IO.Path]::Combine($P.Home, '.copilot', 'session-state', $sessionId) }
            else { $plan = [pscustomobject]@{ Context = 'brief'; Reason = 'copy-verification-failed'; ParentSession = $null } }
        }
        if ($plan.Context -eq 'brief' -and $caller.Kind -eq 'agent' -and $null -eq $brief) { Exit-MuxAgentCommand 2 'brief' 'this agent starts brief-only, so an agent caller passes -BriefFile' }
        $id = [guid]::NewGuid().ToString('N').Substring(0, 12)
        $folder = [IO.Path]::Combine($state, 'agents', $id)
        $briefPath = if ($null -ne $brief) { Join-Path $folder 'brief.md' }
        $launched = $false
        $pane = $null
        try {
            $header = New-MuxAgentHeader -P $P -Caller $caller -Name $name -Id $id -Client $client -Plan $plan -BriefPath $briefPath -TranscriptPath (Get-MuxAgentParentTranscript -P $P -Caller $caller)
            $prompt = Join-MuxAgentPrompt -Header $header -Task $task
            $arguments = @(Get-MuxAgentClientArgs -Client $client -Plan $plan -SessionId $sessionId -Prompt $prompt)
            # The client command line holds 32767 characters after quoting; 1024 of them are kept for the executable path.
            if (1024 + (Get-MuxAgentCommandLineLength -Arguments ([string[]]($script:MuxAgentClientFlags[$client] + $arguments))) -gt 32767) {
                Exit-MuxAgentCommand 2 'usage' 'task is too long for the client command line'
            }
            # The agent folder is made private by the platform member, as the state, agents and mail folders are.
            $null = & $P.PrivateDirectories -Paths @($folder)
            if ($briefPath) { [IO.File]::WriteAllText($briefPath, $brief) }
            $spec = Join-Path $folder 'spec.json'
            Write-MuxAgentJson -Path $spec -Value ([ordered]@{ schema = 1; stateDirectory = $state; id = $id
                    client = $client; parentPane = $caller.Pane; args = $arguments; prompt = $prompt })
            $target = if ($options.Contains('Split')) { @('split-window', '-d', "-$($options['Split'])", '-P', '-F', '#{pane_id}|#{window_id}', '-t', $caller.Pane) }
            else { @('new-window', '-d', '-P', '-F', '#{pane_id}|#{window_id}', '-t', "$($caller.Session):", '-n', $name) }
            # From here a pane may exist, so any failure is a launch failure.
            $launched = $true
            $created = Invoke-MuxAgentNative -P $P -Arguments ($target + @(Get-MuxAgentPaneEnvironment -P $P) + @('-c', $directory, $P.Pwsh, '-NoProfile', '-File', $P.Launcher, $spec))
            if (-not (([string]$created.StdOut).TrimEnd("`r`n") -match '^(%\d+)\|(@\d+)$')) { Exit-MuxAgentCommand 1 'launch' 'rmux did not name the new pane' }
            $pane = $Matches[1]
            $window = $Matches[2]
            $tags = [ordered]@{ '@agent-id' = $id; '@agent-name' = $name; '@agent-parent' = $caller.Pane; '@agent-context' = "$($plan.Context):$($plan.Reason)" }
            $null = Invoke-MuxAgentNative -P $P -Arguments @(@(foreach ($key in $tags.Keys) { ';', 'set-option', '-p', '-t', $pane, $key, $tags[$key] }) | Select-Object -Skip 1)
            $parentSession = [string]$P.Env[$(if ($caller.Client -eq 'claude') { 'CLAUDE_CODE_SESSION_ID' } else { 'COPILOT_AGENT_SESSION_ID' })]
            $parentAgent = if ($caller.Kind -eq 'agent') {
                [ordered]@{ client = $caller.Client; pid = $caller.Agent.Pid; start = $caller.Agent.Start; sessionId = $(if ($parentSession -match $script:MuxAgentSessionIdPattern) { $parentSession }) }
            }
            Write-MuxAgentJson -Path (Join-Path $folder 'record.json') -Value ([ordered]@{ schema = 1; id = $id; name = $name; client = $client; sessionId = $sessionId
                    context = $plan.Context; reason = $plan.Reason; session = $caller.Session; pane = $pane; window = $window; parentPane = $caller.Pane
                    parentAgent = $parentAgent; directory = $directory; taskLine = (ConvertTo-MuxAgentDoingLine $task); createdAt = (& $P.Now).ToString('o') })
            Write-MuxAgentJson -Path (Join-Path $folder 'commit.json') -Value ([ordered]@{ committedAt = (& $P.Now).ToString('o') })
        }
        catch {
            if ($launched) {
                # Each cleanup step runs on its own: a failing one skips none of the others and never hides the refusal.
                try { Write-MuxAgentJson -Path (Join-Path $folder 'abort.json') -Value ([ordered]@{ abortedAt = (& $P.Now).ToString('o'); pane = $pane }) } catch { }
                if ($pane) { try { $null = Invoke-MuxAgentNative -P $P -Arguments @('kill-pane', '-t', $pane) -AllowFailure } catch { } }
                if ($copied) { try { if ([IO.Directory]::Exists($copied)) { Remove-Item -LiteralPath $copied -Recurse } } catch { } }
                Exit-MuxAgentCommand 1 'launch' 'the agent did not start; its pane was closed'
            }
            try { if ([IO.Directory]::Exists($folder)) { Remove-Item -LiteralPath $folder -Recurse } }
            finally { if ($copied -and [IO.Directory]::Exists($copied)) { Remove-Item -LiteralPath $copied -Recurse } }
            throw
        }
        [ordered]@{ name = $name; id = $id; pane = $pane; window = $window; client = $client; sessionId = $sessionId; context = $plan.Context; reason = $plan.Reason }
    }
}


# Part 3: no widening. The native helpers read the caller's own token and split command lines; they keep nothing.
$script:MuxAgentNativeSource = @'
using System;
using System.Runtime.InteropServices;
public static class MuxAgentNative {
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool GetTokenInformation(IntPtr token, int infoClass, IntPtr info, int length, out int returned);
    [DllImport("advapi32.dll", SetLastError = true)] static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll", SetLastError = true)] static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
    [DllImport("shell32.dll", SetLastError = true)] static extern IntPtr CommandLineToArgvW([MarshalAs(UnmanagedType.LPWStr)] string commandLine, out int count);
    static int ReadDword(IntPtr token, int infoClass) {
        IntPtr buffer = Marshal.AllocHGlobal(4);
        try { int returned; if (!GetTokenInformation(token, infoClass, buffer, 4, out returned)) throw new InvalidOperationException("token"); return Marshal.ReadInt32(buffer); }
        finally { Marshal.FreeHGlobal(buffer); }
    }
    // [isAppContainer, isRestricted, integrity RID]
    public static int[] Token() {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), 8, out token)) throw new InvalidOperationException("token");
        try {
            int appContainer = ReadDword(token, 29);
            int restricted = ReadDword(token, 40);
            int length;
            GetTokenInformation(token, 25, IntPtr.Zero, 0, out length);
            IntPtr buffer = Marshal.AllocHGlobal(length);
            try {
                if (!GetTokenInformation(token, 25, buffer, length, out length)) throw new InvalidOperationException("token");
                IntPtr sid = Marshal.ReadIntPtr(buffer);
                int count = Marshal.ReadByte(GetSidSubAuthorityCount(sid));
                int rid = Marshal.ReadInt32(GetSidSubAuthority(sid, (uint)(count - 1)));
                return new int[] { appContainer, restricted, rid };
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }
        finally { CloseHandle(token); }
    }
    public static string[] Argv(string commandLine) {
        int count;
        IntPtr list = CommandLineToArgvW(commandLine, out count);
        if (list == IntPtr.Zero) throw new InvalidOperationException("argv");
        try {
            string[] result = new string[count];
            for (int index = 0; index < count; index++) result[index] = Marshal.PtrToStringUni(Marshal.ReadIntPtr(list, index * IntPtr.Size));
            return result;
        }
        finally { LocalFree(list); }
    }
}
'@

function Import-MuxAgentNative {
    if (-not ('MuxAgentNative' -as [type])) { Add-Type -TypeDefinition $script:MuxAgentNativeSource }
}

function Get-MuxAgentToken {
    # The caller's own token: AppContainer, restricted and the integrity level SID. $null when unreadable.
    try {
        Import-MuxAgentNative
        $values = [MuxAgentNative]::Token()
        [pscustomobject]@{ IsAppContainer = [bool]$values[0]; IsRestricted = [bool]$values[1]; IntegrityLevel = "S-1-16-$($values[2])" }
    }
    catch { $null }
}

function Assert-MuxAgentToken {
    # C1: refuses 2 'sandbox' for an unreadable, AppContainer or restricted token or one below Medium integrity.
    param($Token)
    $level = if ($Token.IntegrityLevel -match '^S-1-16-(\d+)$') { [int]$Matches[1] } else { -1 }
    if ($null -eq $Token -or $Token.IsAppContainer -or $Token.IsRestricted -or $level -lt 8192) {
        Exit-MuxAgentCommand 2 'sandbox' 'the caller runs in a sandbox or below Medium integrity'
    }
}

# C3: the flags an agent's client command lines may carry, named by their text before '='.
$script:MuxAgentAllowedFlags = @{
    claude  = @('--model', '--add-dir', '--session-id', '--continue', '-r', '--resume', '-n', '--name', '-c', '--dangerously-skip-permissions', '--permission-mode', '--effort', '--fork-session')
    copilot = @('--model', '--add-dir', '--session-id', '--continue', '-r', '--resume', '-n', '--name', '--allow-all', '--yolo', '--no-sandbox', '--allow-all-tools', '--allow-all-paths', '--allow-all-urls', '--reasoning-effort', '--autopilot', '-i', '--interactive', '-p', '--prompt')
}

function Split-MuxAgentCommandLine {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$CommandLine)
    Import-MuxAgentNative
    [MuxAgentNative]::Argv($CommandLine)
}

function Find-MuxAgentLastLine {
    # The last line of a file that contains Needle and matches Pattern, or $null. Shares the file with its writer.
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string]$Needle, [Parameter(Mandatory)][string]$Pattern)
    $stream = [IO.File]::Open($FilePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
    try {
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8)
        $last = $null
        while ($null -ne ($line = $reader.ReadLine())) { if ($line.Contains($Needle) -and $line -match $Pattern) { $last = $line } }
        $last
    }
    finally { $stream.Dispose() }
}

function Test-MuxAgentPolicy {
    # C2: whether the caller's live policy is full bypass. Claude: the latest transcript permission-mode and the pane's
    # mode line; Copilot: the latest permissions and mode events (no permissions event: an allow-all flag at launch).
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [string[]]$AgentArgv = @())
    if ($Caller.Client -eq 'claude') {
        $session = [string]$P.Env['CLAUDE_CODE_SESSION_ID']
        if ($session -notmatch $script:MuxAgentSessionIdPattern) { return $false }
        $transcript = Find-MuxAgentClaudeTranscript -HomeDirectory $P.Home -SessionId $session
        if (-not $transcript) { return $false }
        $line = Find-MuxAgentLastLine -FilePath $transcript -Needle '"permission-mode"' -Pattern '^\{"type":"permission-mode","permissionMode":"([A-Za-z]+)"'
        if (-not $line -or $line -cnotmatch '^\{"type":"permission-mode","permissionMode":"bypassPermissions"') { return $false }
        # The mode line catches a Shift+Tab the transcript has not recorded yet. A pane no client has sized (under 3
        # rows) shows no mode line, so the transcript decides alone.
        $size = Invoke-MuxAgentNative -P $P -Arguments @('display-message', '-p', '-t', $Caller.Pane, '#{pane_height}') -AllowFailure
        $height = 0
        if ($size.ExitCode -ne 0 -or -not [int]::TryParse(([string]$size.StdOut).Trim(), [ref]$height)) { return $false }
        if ($height -lt 3) { return $true }
        $screen = Invoke-MuxAgentNative -P $P -Arguments @('capture-pane', '-p', '-t', $Caller.Pane) -AllowFailure
        if ($screen.ExitCode -ne 0) { return $false }
        $rows = @(([string]$screen.StdOut -split "`r?`n") | Where-Object { $_.Trim() } | Select-Object -Last 4)
        return [bool](@($rows | Where-Object { $_ -match 'bypass permissions on' }).Count)
    }
    $session = [string]$P.Env['COPILOT_AGENT_SESSION_ID']
    if ($session -notmatch $script:MuxAgentSessionIdPattern) { return $false }
    $events = [IO.Path]::Combine($P.Home, '.copilot', 'session-state', $session, 'events.jsonl')
    if (-not [IO.File]::Exists($events)) { return $false }
    $permissions = Find-MuxAgentLastLine -FilePath $events -Needle 'session.permissions_changed' -Pattern '"type":"session\.permissions_changed"'
    $allowAll = if ($permissions) { $permissions -match '"allowAllPermissions":true' }
    else { [bool](@($AgentArgv | Where-Object { ($_ -split '=', 2)[0] -in @('--allow-all', '--yolo') }).Count) }
    if (-not $allowAll) { return $false }
    $mode = Find-MuxAgentLastLine -FilePath $events -Needle 'session.mode_changed' -Pattern '"type":"session\.mode_changed"'
    -not ($mode -and $mode -match '"plan"')
}

function Get-MuxAgentLaunchDirectory {
    # C5: the agent's launch directory. Claude: ~/.claude/sessions/<pid>.json with the agent's start and session id;
    # Copilot: cwd in the session's workspace.yaml. Anything missing or different refuses 2 'directory'.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    $directory = $null
    try {
        if ($Caller.Client -eq 'claude') { $directory = & $P.ClaudeDirectory $P $Caller }
        else {
            $session = [string]$P.Env['COPILOT_AGENT_SESSION_ID']
            if ($session -match $script:MuxAgentSessionIdPattern) {
                $yaml = [IO.Path]::Combine($P.Home, '.copilot', 'session-state', $session, 'workspace.yaml')
                $line = @([IO.File]::ReadAllLines($yaml) | Where-Object { $_.StartsWith('cwd: ', [StringComparison]::Ordinal) }) | Select-Object -First 1
                if ($line) { $directory = $line.Substring(5).Trim().Trim('"', "'") }
            }
        }
    }
    catch { $directory = $null }
    if (-not $directory -or -not [IO.Directory]::Exists($directory)) { Exit-MuxAgentCommand 2 'directory' "the caller's launch directory cannot be confirmed" }
    $directory
}

function Assert-MuxAgentSpawnAllowed {
    # C3, C2 and C5 for an agent caller; returns the launch directory. Refusals name only the check (and for C3 the flag).
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    $agentIndex = [Array]::FindIndex([object[]]$Caller.Chain, [Predicate[object]] { param($row) $row.Pid -eq $Caller.Agent.Pid })
    $agentArgv = @()
    for ($index = $agentIndex; $index -ge 0 -and $index -lt $Caller.Chain.Count; $index++) {
        $row = $Caller.Chain[$index]
        if (-not (& $P.ClientOf $row)) { continue }
        $argv = try { @(& $P.Argv $row) } catch { Exit-MuxAgentCommand 2 'flags' 'a client command line cannot be read' }
        if ($index -eq $agentIndex) { $agentArgv = $argv }
        foreach ($argument in @($argv | Select-Object -Skip 1)) {
            if (-not $argument.StartsWith('-')) { continue }
            $flag = ($argument -split '=', 2)[0]
            if ($flag -cnotin $script:MuxAgentAllowedFlags[$Caller.Client]) { Exit-MuxAgentCommand 2 'flags' "the flag $flag is not allowed for a spawning agent" }
        }
    }
    if (-not (Test-MuxAgentPolicy -P $P -Caller $Caller -AgentArgv $agentArgv)) { Exit-MuxAgentCommand 2 'policy' "the caller's live policy is not full bypass" }
    Get-MuxAgentLaunchDirectory -P $P -Caller $Caller
}

# Part 2: registry reads, mailboxes, exit notices and the other verbs.

function Get-MuxAgentRecord {
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Id)
    if ($Id -cnotmatch '^[0-9a-f]{12}$') { return $null }
    Read-MuxAgentJson -Path ([IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents', $Id, 'record.json'))
}

function Get-MuxAgentMailboxKey {
    # An agent's mailbox is its id; any other pane's is pane-<n>.
    param([Parameter(Mandatory)][string]$Pane, [AllowEmptyString()][string]$Tag)
    if ($Tag -cmatch '^[0-9a-f]{12}$') { return $Tag }
    if ($Pane -notmatch '^%(\d+)$') { Exit-MuxAgentCommand 2 'target' 'not a pane' }
    "pane-$($Matches[1])"
}

function ConvertTo-MuxAgentMessageText {
    # LF line ends; ESC and the other C0 controls except LF and TAB removed.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    [regex]::Replace($Text.Replace("`r`n", "`n"), '[\x00-\x08\x0B-\x1F\x7F]', '')
}

function ConvertTo-MuxAgentDoingLine {
    # One side-bar line, cleaned before storage: C0 (but LF), DEL and C1 removed, then the first non-blank line, tabs as
    # spaces, trimmed, at most 80 characters without splitting a surrogate pair; '' when every line is blank.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    $clean = [regex]::Replace((ConvertTo-MuxAgentMessageText $Text), '[\u0080-\u009F]', '')
    foreach ($line in $clean.Split("`n")) {
        $line = $line.Replace("`t", ' ').Trim()
        if (-not $line) { continue }
        if ($line.Length -gt 80) { $line = $line.Substring(0, $(if ([char]::IsHighSurrogate($line[79])) { 79 } else { 80 })) }
        return $line
    }
    ''
}

function Select-MuxAgentDoing {
    # What an agent is doing: its last status, else the first line of its last sent message, else of its task. Each is
    # cleaned again; a blank one falls through.
    param($Status, $Message, $Task)
    foreach ($source in @(@('status', $Status), @('message', $Message), @('task', $Task))) {
        if ($null -eq $source[1]) { continue }
        $line = ConvertTo-MuxAgentDoingLine ([string]$source[1])
        if ($line) { return [ordered]@{ doing = $line; doingFrom = $source[0] } }
    }
    [ordered]@{ doing = $null; doingFrom = $null }
}

function ConvertTo-MuxAgentUtc {
    # A state-file time (ISO 8601 text, or the DateTime ConvertFrom-Json makes of it) as UTC; $null when unreadable.
    param($Value)
    $time = [DateTime]::MinValue
    if ($Value -is [DateTime]) { $time = $Value }
    elseif ($Value -isnot [string] -or -not [DateTime]::TryParse($Value, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind, [ref]$time)) { return $null }
    if ($time.Kind -eq [DateTimeKind]::Utc) { $time } else { $time.ToUniversalTime() }
}

function Get-MuxAgentAge {
    # Whole seconds from CreatedAt to Now, at least 0; $null when CreatedAt is unreadable.
    param($CreatedAt, [Parameter(Mandatory)][DateTime]$Now)
    $created = ConvertTo-MuxAgentUtc $CreatedAt
    if ($null -eq $created) { return $null }
    [int][Math]::Max(0, [Math]::Floor(($Now - $created).TotalSeconds))
}

function Add-MuxAgentMail {
    # Appends one message to Key's mailbox under the mailbox lock and returns its id. A last line without LF, left by an
    # interrupted append, gets its LF first, so it never joins the new message; read skips it if it does not parse. With
    # MessageId, a mailbox with a parsable line holding that id, the last line included, gets no second copy.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$From, [Parameter(Mandatory)][string]$FromPane, [Parameter(Mandatory)][AllowEmptyString()][string]$Body, [string]$MessageId)
    $state = Get-MuxAgentStateDirectory -P $P -Caller $Caller
    $messageId = if ($MessageId) { $MessageId } else { [guid]::NewGuid().ToString('N').Substring(0, 8) }
    $line = (ConvertTo-MuxAgentJsonText ([ordered]@{ id = $messageId; from = $From; pane = $FromPane; at = (& $P.Now).ToString('o'); body = $Body })) + "`n"
    Use-MuxAgentLock -LockKey "mail|$($Caller.ServerKey)|$Key" -LockAction {
        $box = [IO.Path]::Combine($state, 'mail', "$Key.jsonl")
        # Assigned, not returned from an if: PowerShell would unroll the bytes, and an empty array would become $null.
        [byte[]]$existing = [byte[]]::new(0)
        if (Test-Path -LiteralPath $box) { $existing = [IO.File]::ReadAllBytes($box) }
        $torn = $existing.Length -gt ([Array]::LastIndexOf($existing, [byte]10) + 1)
        if ($torn) { $line = "`n" + $line }
        if ($MessageId) {
            foreach ($stored in [Text.Encoding]::UTF8.GetString($existing).Split("`n", [StringSplitOptions]::RemoveEmptyEntries)) {
                # Already delivered: only the missing LF is written, so read can return it.
                if ((Test-MuxAgentJsonLine $stored) -and (ConvertFrom-Json -InputObject $stored).id -ceq $MessageId) { $line = $(if ($torn) { "`n" } else { '' }); break }
            }
        }
        if (-not $line) { return }
        $stream = [IO.File]::Open($box, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        try { $bytes = [Text.Encoding]::UTF8.GetBytes($line); $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
    }
    $messageId
}

function Get-MuxAgentUnreadText {
    # The mailbox's complete lines after its read offset, and the byte offset after them (the old offset when none).
    # Reads only: the caller holds the mailbox lock and moves the offset if it consumes the lines. With -Tolerant (list),
    # an offset that is not a number, lies outside the mailbox or falls inside a line counts from the start instead, so
    # a damaged offset file never fails list or hides unread mail.
    param([Parameter(Mandatory)][string]$Box, [Parameter(Mandatory)][string]$Cursor, [switch]$Tolerant)
    $offset = 0L
    if (Test-Path -LiteralPath $Cursor) {
        $raw = [IO.File]::ReadAllText($Cursor).Trim()
        if ($Tolerant) { if (-not [long]::TryParse($raw, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$offset)) { $offset = 0L } }
        else { $offset = [long]$raw }
    }
    if (-not (Test-Path -LiteralPath $Box)) { return [pscustomobject]@{ Text = ''; End = $offset } }
    $bytes = [IO.File]::ReadAllBytes($Box)
    if ($Tolerant -and ($offset -gt $bytes.Length -or ($offset -gt 0 -and $bytes[$offset - 1] -ne 10))) { $offset = 0L }
    $end = [Array]::LastIndexOf($bytes, [byte]10) + 1
    if ($end -le $offset) { return [pscustomobject]@{ Text = ''; End = $offset } }
    [pscustomobject]@{ Text = [Text.Encoding]::UTF8.GetString($bytes, [int]$offset, [int]($end - $offset)); End = [long]$end }
}

function ConvertFrom-MuxAgentMailText {
    # The parsed messages of mailbox text; a sealed torn line does not parse and is skipped, so one bad line never
    # blocks the mailbox.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    $Text.Split("`n", [StringSplitOptions]::RemoveEmptyEntries) | Where-Object { Test-MuxAgentJsonLine $_ } | ForEach-Object { ConvertFrom-Json -InputObject $_ }
}

function Read-MuxAgentMail {
    # Every unread message of Key's mailbox in arrival order; the read offset moves past them under the mailbox lock.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Key)
    $state = Get-MuxAgentStateDirectory -P $P -Caller $Caller
    $box = [IO.Path]::Combine($state, 'mail', "$Key.jsonl")
    $cursor = [IO.Path]::Combine($state, 'mail', "$Key.offset")
    $found = Use-MuxAgentLock -LockKey "mail|$($Caller.ServerKey)|$Key" -LockAction {
        $unread = Get-MuxAgentUnreadText -Box $box -Cursor $cursor
        if (-not $unread.Text) { return , @() }
        [IO.File]::WriteAllText($cursor, [string]$unread.End)
        , @(ConvertFrom-MuxAgentMailText $unread.Text)
    }
    foreach ($message in @($found)) {
        [ordered]@{ id = $message.id; from = $message.from; pane = $message.pane; at = $message.at
            text = "[mux-agent from $($message.from) ($($message.pane)) #$($message.id)]`n$($message.body)" }
    }
}

function Add-MuxAgentNotice {
    # Posts the agent's one exit notice (exited, stopped or lost) to its parent pane's mailbox, pane-<n>; false when already posted.
    # The caller holds the notice lock for the agent. The message goes first and notice.json last, so a failed post is
    # retried by the next list, read or spawn; the fixed message id keeps a retry after a failed marker write single.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)]$Record, [Parameter(Mandatory)][ValidateSet('exited', 'stopped', 'lost')][string]$Kind, $ExitCode)
    $folder = [IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents', $Record.id)
    $marker = Join-Path $folder 'notice.json'
    if (Test-Path -LiteralPath $marker) { return $false }
    $body = switch ($Kind) {
        'exited' { "$($Record.name) exited with code $ExitCode." }
        'stopped' { "$($Record.name) was stopped." }
        'lost' { "$($Record.name) is gone: its pane closed without an exit record." }
    }
    $null = Add-MuxAgentMail -P $P -Caller $Caller -Key (Get-MuxAgentMailboxKey -Pane $Record.parentPane -Tag '') -From $Record.name -FromPane $Record.pane -Body $body -MessageId "exit-$($Record.id)"
    Write-MuxAgentJson -Path $marker -Value ([ordered]@{ kind = $Kind; exitCode = $ExitCode; at = (& $P.Now).ToString('o') })
    $true
}

function Test-MuxAgentPaneGone {
    # A gone pane answers display-message with exit 0 and empty fields (rmux 0.10.0, tmux 3.2+). A failed, timed-out
    # or other answer is no proof, so the agent waits for the next sweep.
    param($Probe)
    $null -ne $Probe -and $Probe.ExitCode -eq 0 -and ([string]$Probe.StdOut).TrimEnd("`r`n") -ceq '|' }

function Update-MuxAgentNotices {
    # Posts 'lost' for each committed agent of the caller's session whose pane is gone and that has no notice yet.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    $agents = [IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents')
    if (-not [IO.Directory]::Exists($agents)) { return }
    $live = @(Get-MuxAgentPanes -P $P -Caller $Caller -All | ForEach-Object Id)
    foreach ($folder in [IO.Directory]::GetDirectories($agents)) {
        $id = [IO.Path]::GetFileName($folder)
        if ($id -in $live -or -not [IO.File]::Exists((Join-Path $folder 'commit.json')) -or [IO.File]::Exists((Join-Path $folder 'notice.json'))) { continue }
        $record = Get-MuxAgentRecord -P $P -Caller $Caller -Id $id
        if (-not $record -or $record.session -cne $Caller.Session) { continue }
        Use-MuxAgentLock -LockKey "notice|$($Caller.ServerKey)|$id" -LockAction {
            # The snapshot may predate a spawn that committed since: ask the agent's own pane before calling it gone.
            # Only a clean answer with an empty pane id means gone; a live pane or a failed probe waits for the next try.
            $probe = Invoke-MuxAgentNative -P $P -Arguments @('display-message', '-p', '-t', $record.pane, '#{pane_id}|#{@agent-id}') -AllowFailure
            if (-not (Test-MuxAgentPaneGone $probe)) { return }
            $exit = Read-MuxAgentJson -Path (Join-Path $folder 'launcher.json')
            if ($exit -and $null -ne $exit.exitCode) { $null = Add-MuxAgentNotice -P $P -Caller $Caller -Record $record -Kind exited -ExitCode $exit.exitCode }
            else { $null = Add-MuxAgentNotice -P $P -Caller $Caller -Record $record -Kind lost }
        }
    }
}

function Find-MuxAgentPane {
    # The live committed agent named Name in the caller's session; refuses 2 'target' otherwise.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Name)
    if ($Name -cnotmatch '^[a-z][a-z0-9-]{0,23}\z') { Exit-MuxAgentCommand 2 'target' 'no live agent has that name' }
    $pane = @(Get-MuxAgentPanes -P $P -Caller $Caller | Where-Object Name -CEQ $Name) | Select-Object -First 1
    if (-not $pane) { Exit-MuxAgentCommand 2 'target' 'no live agent has that name' }
    $pane
}

function Get-MuxAgentUnreadCounts {
    # Unread messages in Key's mailbox per '<from>|<pane>', read under the mailbox lock; never moves the offset or creates a folder.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$Key)
    $mail = [IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'mail')
    $text = Use-MuxAgentLock -LockKey "mail|$($Caller.ServerKey)|$Key" -LockAction { (Get-MuxAgentUnreadText -Box (Join-Path $mail "$Key.jsonl") -Cursor (Join-Path $mail "$Key.offset") -Tolerant).Text }
    $counts = @{}
    foreach ($message in @(ConvertFrom-MuxAgentMailText ([string]$text))) { $from = "$($message.from)|$($message.pane)"; $counts[$from] = 1 + [int]$counts[$from] }
    $counts
}

function New-MuxAgentBoardRow {
    # One list row. status.json and last-sent.json are read under the agent lock that status and send write with; an
    # unreadable one is skipped. Doing never comes from the screen.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)]$Record, [string]$Pane, [string]$Window, [string]$State, [hashtable]$Unread)
    $folder = [IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents', $Record.id)
    $files = @{}
    Use-MuxAgentLock -LockKey "agent|$($Caller.ServerKey)|$($Record.id)" -LockAction {
        foreach ($name in 'status', 'last-sent') { $files[$name] = try { Read-MuxAgentJson -Path (Join-Path $folder "$name.json") } catch { $null } }
    }
    $doing = Select-MuxAgentDoing -Status $files['status'].text -Message $files['last-sent'].line -Task $Record.taskLine
    [ordered]@{ name = $Record.name; client = $Record.client; pane = $Pane; window = $Window; context = $Record.context; parent = $Record.parentPane; sessionId = $Record.sessionId
        directory = $Record.directory; createdAt = $Record.createdAt; state = $State; doing = $doing.doing; doingFrom = $doing.doingFrom
        age = (Get-MuxAgentAge -CreatedAt $Record.createdAt -Now (& $P.Now)); unread = [int]$Unread["$($Record.name)|$Pane"] }
}

function Get-MuxAgentBoard {
    # The side-bar feed of Caller's session: the running count, live agents in pane order and, with -All, the session's
    # ended agents, newest notice first, at most 10. Unread is ReaderKey's.
    # Locks one at a time: notice locks (Update-MuxAgentNotices), the reader's mail lock, then each row's agent lock.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller, [Parameter(Mandatory)][string]$ReaderKey, [switch]$All)
    Update-MuxAgentNotices -P $P -Caller $Caller
    $unread = Get-MuxAgentUnreadCounts -P $P -Caller $Caller -Key $ReaderKey
    $panes = @(Get-MuxAgentPanes -P $P -Caller $Caller)
    $live = @(foreach ($pane in $panes) {
            $record = Get-MuxAgentRecord -P $P -Caller $Caller -Id $pane.Id
            if (-not $record -or $record.session -cne $Caller.Session) { continue }
            New-MuxAgentBoardRow -P $P -Caller $Caller -Record $record -Pane $pane.Pane -Window $pane.Window -State 'running' -Unread $unread
        })
    $ended = @()
    if ($All) {
        $agents = [IO.Path]::Combine($P.StateRoot, $Caller.ServerKey, 'agents')
        $liveIds = @($panes | ForEach-Object Id)
        $found = @(if ([IO.Directory]::Exists($agents)) {
                foreach ($folder in [IO.Directory]::GetDirectories($agents)) {
                    $id = [IO.Path]::GetFileName($folder)
                    if ($id -in $liveIds) { continue }
                    # notice.json is written once (Add-MuxAgentNotice refuses a second), so it is read without the lock.
                    $notice = Read-MuxAgentJson -Path (Join-Path $folder 'notice.json')
                    if (-not $notice -or $notice.kind -cnotin @('exited', 'stopped', 'lost')) { continue }
                    $record = Get-MuxAgentRecord -P $P -Caller $Caller -Id $id
                    if (-not $record -or $record.session -cne $Caller.Session -or $record.id -cne $id) { continue }
                    $at = ConvertTo-MuxAgentUtc $notice.at
                    [pscustomobject]@{ At = $(if ($null -eq $at) { [DateTime]::MinValue } else { $at }); Id = $id; Kind = [string]$notice.kind; Record = $record }
                }
            })
        $ended = @($found | Sort-Object -Property @{ Expression = 'At'; Descending = $true }, @{ Expression = 'Id' } | Select-Object -First 10 | ForEach-Object {
                New-MuxAgentBoardRow -P $P -Caller $Caller -Record $_.Record -Pane $_.Record.pane -Window $_.Record.window -State $_.Kind -Unread $unread
            })
    }
    [ordered]@{ running = $live.Count; agents = @($live) + $ended }
}

function Invoke-MuxAgentList {
    # The board as the caller's pane sees it: unread from its own mailbox (an agent's id, else pane-<n>).
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $caller = Resolve-MuxAgentCaller -P $P
    Get-MuxAgentBoard -P $P -Caller $caller -ReaderKey (Get-MuxAgentMailboxKey -Pane $caller.Pane -Tag $caller.AgentTag) -All:([bool]$Request.Options['All'])
}

function Invoke-MuxAgentFocus {
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $caller = Resolve-MuxAgentCaller -P $P
    $pane = Find-MuxAgentPane -P $P -Caller $caller -Name $Request.Positional[0]
    $null = Invoke-MuxAgentNative -P $P -Arguments @('select-window', '-t', $pane.Window)
    $null = Invoke-MuxAgentNative -P $P -Arguments @('select-pane', '-t', $pane.Pane)
    [ordered]@{ name = $pane.Name; pane = $pane.Pane; window = $pane.Window }
}

function Get-MuxAgentSelf {
    # The caller's name and mailbox key: an agent pane is its record name and id; any other pane is root.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Caller)
    if ($Caller.AgentTag) {
        $record = Get-MuxAgentRecord -P $P -Caller $Caller -Id $Caller.AgentTag
        if (-not $record) { Exit-MuxAgentCommand 2 'parent' 'this agent pane has no record' }
        return [pscustomobject]@{ Name = $record.name; Key = $record.id; Record = $record }
    }
    [pscustomobject]@{ Name = 'root'; Key = (Get-MuxAgentMailboxKey -Pane $Caller.Pane -Tag ''); Record = $null }
}

function Invoke-MuxAgentSend {
    # Appends a message to a live agent's, the parent's or a session pane's mailbox. Never types into a pane.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $options = $Request.Options
    if ($options.Contains('Body') -eq $options.Contains('BodyFile')) { Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['send'].Usage }
    $caller = Resolve-MuxAgentCaller -P $P
    if ($caller.Kind -eq 'agent' -and $options.Contains('Body')) { Exit-MuxAgentCommand 2 'usage' 'agents pass free text only in files: use -BodyFile' }
    $body = if ($options.Contains('Body')) { $options['Body'] } else { Read-MuxAgentText -Path ([IO.Path]::Combine($P.Cwd, $options['BodyFile'])) -MaxBytes 64KB }
    $body = ConvertTo-MuxAgentMessageText $body
    if ([Text.Encoding]::UTF8.GetByteCount($body) -gt 64KB) { Exit-MuxAgentCommand 2 'usage' 'the message is larger than 64 KB' }
    $self = Get-MuxAgentSelf -P $P -Caller $caller
    $target = $Request.Positional[0]
    if ($target -ceq 'parent') {
        if (-not $self.Record) { Exit-MuxAgentCommand 2 'parent' 'only an agent has a parent' }
        $pane = $self.Record.parentPane
    }
    elseif ($target -match '^%\d+$') { $pane = $target }
    else { $pane = (Find-MuxAgentPane -P $P -Caller $caller -Name $target).Pane }
    $probe = Invoke-MuxAgentNative -P $P -Arguments @('display-message', '-p', '-t', $pane, '#{session_id}|#{@agent-id}') -AllowFailure
    if ($probe.ExitCode -ne 0 -or -not (([string]$probe.StdOut).TrimEnd("`r`n") -match '^(\$\d+)\|(.*)$') -or $Matches[1] -cne $caller.Session) {
        Exit-MuxAgentCommand 2 'target' 'the recipient is not a live pane of this session'
    }
    $key = Get-MuxAgentMailboxKey -Pane $pane -Tag $Matches[2]
    $deliver = {
        $sentId = Add-MuxAgentMail -P $P -Caller $caller -Key $key -From $self.Name -FromPane $caller.Pane -Body $body
        if ($self.Record) {
            # Best effort, inside the sender lock, so last-sent.json names the last appended message; '' for a blank body.
            $folder = [IO.Path]::Combine($P.StateRoot, $caller.ServerKey, 'agents', $self.Record.id)
            try { Write-MuxAgentJson -Path (Join-Path $folder 'last-sent.json') -Value ([ordered]@{ line = (ConvertTo-MuxAgentDoingLine $body); at = (& $P.Now).ToString('o') }) } catch { }
        }
        $sentId
    }
    # Lock order: the sender's agent lock, then (inside Add-MuxAgentMail) the recipient's mail lock.
    $messageId = if ($self.Record) { Use-MuxAgentLock -LockKey "agent|$($caller.ServerKey)|$($self.Record.id)" -LockAction $deliver } else { & $deliver }
    [ordered]@{ to = $target; pane = $pane; id = $messageId }
}

function Invoke-MuxAgentStatus {
    # An agent's one-line status in status.json in its folder, written under its agent lock. Only an agent pane with
    # its own record in this session reports status, and only from a file.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $options = $Request.Options
    if ($options.Contains('Body') -eq $options.Contains('BodyFile')) { Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['status'].Usage }
    $caller = Resolve-MuxAgentCaller -P $P
    $record = if ($caller.Kind -eq 'agent' -and $caller.AgentTag) { Get-MuxAgentRecord -P $P -Caller $caller -Id $caller.AgentTag }
    if (-not $record -or $record.id -cne $caller.AgentTag -or $record.session -cne $caller.Session) { Exit-MuxAgentCommand 2 'target' 'only an agent reports status' }
    if ($options.Contains('Body')) { Exit-MuxAgentCommand 2 'usage' 'agents pass free text only in files: use -BodyFile' }
    $text = ConvertTo-MuxAgentDoingLine (Read-MuxAgentText -Path ([IO.Path]::Combine($P.Cwd, $options['BodyFile'])) -MaxBytes 64KB)
    if (-not $text) { Exit-MuxAgentCommand 2 'usage' 'the status is empty' }
    $folder = [IO.Path]::Combine((Get-MuxAgentStateDirectory -P $P -Caller $caller), 'agents', $record.id)
    Use-MuxAgentLock -LockKey "agent|$($caller.ServerKey)|$($record.id)" -LockAction {
        Write-MuxAgentJson -Path (Join-Path $folder 'status.json') -Value ([ordered]@{ text = $text; at = (& $P.Now).ToString('o') })
    }
    [ordered]@{ name = $record.name; text = $text }
}

function Invoke-MuxAgentRead {
    # read [-Wait <s>]: the caller's unread messages, waiting up to 600 s for one. read <name> [-Lines n]: the pane tail.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $options = $Request.Options
    $wait = 0
    $lines = 60
    if ($options.Contains('Wait') -and (-not [int]::TryParse($options['Wait'], [ref]$wait) -or $wait -lt 0 -or $wait -gt 600)) { Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['read'].Usage }
    if ($options.Contains('Lines') -and (-not [int]::TryParse($options['Lines'], [ref]$lines) -or $lines -lt 1 -or $lines -gt 2000)) { Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['read'].Usage }
    if (($Request.Positional.Count -and $options.Contains('Wait')) -or (-not $Request.Positional.Count -and $options.Contains('Lines'))) { Exit-MuxAgentCommand 2 'usage' $script:MuxAgentVerbs['read'].Usage }
    $caller = Resolve-MuxAgentCaller -P $P
    if ($Request.Positional.Count) {
        $pane = Find-MuxAgentPane -P $P -Caller $caller -Name $Request.Positional[0]
        $captured = Invoke-MuxAgentNative -P $P -Arguments @('capture-pane', '-p', '-J', '-t', $pane.Pane, '-S', "-$lines")
        $tail = @(([string]$captured.StdOut).TrimEnd("`r`n") -split "`r?`n" | Select-Object -Last $lines)
        return [ordered]@{ name = $pane.Name; pane = $pane.Pane; text = ($tail -join "`n") }
    }
    $self = Get-MuxAgentSelf -P $P -Caller $caller
    $deadline = (& $P.Now).AddSeconds($wait)
    while ($true) {
        Update-MuxAgentNotices -P $P -Caller $caller
        $messages = @(Read-MuxAgentMail -P $P -Caller $caller -Key $self.Key)
        if ($messages.Count -or (& $P.Now) -ge $deadline) { break }
        & $P.Sleep 500
    }
    [ordered]@{ messages = $messages }
}

function Invoke-MuxAgentStop {
    # Closes a live agent's pane, then stops any survivor of the pane's process tree (same PID and start only). Its
    # parent or a human may stop it. Posts the 'stopped' notice and returns the resume command.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)]$Request)
    $caller = Resolve-MuxAgentCaller -P $P
    $pane = Find-MuxAgentPane -P $P -Caller $caller -Name $Request.Positional[0]
    $record = Get-MuxAgentRecord -P $P -Caller $caller -Id $pane.Id
    if (-not $record) { Exit-MuxAgentCommand 2 'target' 'no live agent has that name' }
    if ($caller.Kind -eq 'agent' -and $record.parentPane -cne $caller.Pane) { Exit-MuxAgentCommand 2 'parent' 'only the parent or a human can stop this agent' }
    $left = Use-MuxAgentLock -LockKey "notice|$($caller.ServerKey)|$($record.id)" -LockAction {
        $table = @{}
        foreach ($process in & $P.Processes) { $table[$process.Pid] = $process }
        $tree = [Collections.Generic.List[object]]::new()
        $queue = [Collections.Generic.Queue[int]]::new()
        $queue.Enqueue([int]$pane.Pid)
        while ($queue.Count -and $tree.Count -lt 256) {
            $current = $queue.Dequeue()
            if ($table.ContainsKey($current)) { $tree.Add($table[$current]) }
            foreach ($row in $table.Values) { if ($row.ParentPid -eq $current -and $row.Pid -ne $current -and $row.Start -ge $table[$current].Start) { $queue.Enqueue($row.Pid) } }
        }
        # Every tree row that is not killed goes to StopReport with its reason: not opened, another start, or a failed kill.
        $left = [Collections.Generic.List[object]]::new()
        $opened = @(foreach ($row in $tree) {
                $handle = & $P.Open $row.Pid
                if (-not $handle) { $left.Add([pscustomobject]@{ Pid = [int]$row.Pid; Reason = 'open'; Row = $row }) }
                elseif (-not (& $P.SameStart $handle.Start $row.Start)) { $left.Add([pscustomobject]@{ Pid = [int]$row.Pid; Reason = 'start'; Row = $row }) }
                else { [pscustomobject]@{ Handle = $handle; Row = $row } }
            })
        $null = Invoke-MuxAgentNative -P $P -Arguments @('kill-pane', '-t', $pane.Pane) -AllowFailure
        foreach ($item in $opened) { if (-not (& $P.Kill $item.Handle)) { $left.Add([pscustomobject]@{ Pid = [int]$item.Row.Pid; Reason = 'kill'; Row = $item.Row }) } }
        $null = Add-MuxAgentNotice -P $P -Caller $caller -Record $record -Kind stopped
        , $left.ToArray()
    }
    $resume = if ($record.client -eq 'claude') { "claude --resume $($record.sessionId)" } else { "copilot --resume $($record.sessionId)" }
    $result = [ordered]@{ name = $record.name; pane = $pane.Pane }
    $report = & $P.StopReport $left
    foreach ($key in $report.Keys) { $result[$key] = $report[$key] }
    $result.directory = $record.directory
    $result.resume = $resume
    $result
}

# Side bar (C2): the board text, the watch loop and the sidebar pane.

function ConvertTo-MuxAgentBoardText {
    # A board field made safe to draw: C0 (LF included), DEL and C1 removed, so no field can move the cursor or add a line.
    param($Value)
    [regex]::Replace([string]$Value, '[\x00-\x1F\x7F\u0080-\u009F]', '')
}

function Format-MuxAgentAge {
    # Whole seconds as 59s, 59m, 23h or 1d; '?' when unknown.
    param($Seconds)
    if ($null -eq $Seconds) { return '?' }
    $s = [long]$Seconds
    if ($s -lt 60) { "${s}s" } elseif ($s -lt 3600) { "$([long][Math]::Floor($s / 60))m" } elseif ($s -lt 86400) { "$([long][Math]::Floor($s / 3600))h" } else { "$([long][Math]::Floor($s / 86400))d" }
}

function Format-MuxAgentBoard {
    # The side-bar text of a board (Get-MuxAgentBoard): the count line, then per live agent its line and an indented
    # doing line, per ended agent one line. Every printed field, state included, is cleaned first; lines are joined with LF.
    param([Parameter(Mandatory)]$Board)
    $lines = [Collections.Generic.List[string]]::new()
    $lines.Add("mux-agents $([char]0x00B7) $([int]$Board.running) running")
    foreach ($row in @($Board.agents)) {
        if ($null -eq $row) { continue }
        $name = ConvertTo-MuxAgentBoardText $row.name
        $state = ConvertTo-MuxAgentBoardText $row.state
        if ($state -ceq 'running') {
            $lines.Add("$([char]0x25CF) $name  $(ConvertTo-MuxAgentBoardText $row.client)  $(Format-MuxAgentAge $row.age)")
            $doing = (ConvertTo-MuxAgentBoardText $row.doing).Trim()
            if ($doing) { $lines.Add("  $doing") }
            continue
        }
        $ended = if ($state -ceq 'exited' -and $null -ne $row.exitCode) { "exited $(ConvertTo-MuxAgentBoardText $row.exitCode)" } else { $state }
        $lines.Add("$([char]0x25CB) $name  $ended")
    }
    $lines -join "`n"
}

# The pane launcher.

$script:MuxAgentScrub = @('CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_MESSAGING_SOCKET', 'CLAUDE_CODE_MESSAGING_TOKEN', 'CLAUDE_CODE_CHILD_SESSION',
    'CLAUDE_CODE_SESSION_ATTENDED', 'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_EXECPATH', 'CLAUDE_PID', 'CLAUDE_EFFORT', 'COPILOT_AGENT_SESSION_ID', 'COPILOT_CLI')

function Clear-MuxAgentIdentity {
    # Removes the parent's client identity variables from this process only. [NullString]::Value, not $null: PowerShell
    # passes $null to a [string] parameter as '', and .NET on Unix keeps an empty variable instead of removing it.
    foreach ($name in $script:MuxAgentScrub) { [Environment]::SetEnvironmentVariable($name, [NullString]::Value) }
}

function Wait-MuxAgentCommit {
    # True once commit.json exists; false at once for abort.json, or after TimeoutSeconds with neither.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)][string]$AgentDirectory, [int]$TimeoutSeconds = 30)
    $deadline = (& $P.Now).AddSeconds($TimeoutSeconds)
    while ($true) {
        if ([IO.File]::Exists((Join-Path $AgentDirectory 'abort.json'))) { return $false }
        if ([IO.File]::Exists((Join-Path $AgentDirectory 'commit.json'))) { return $true }
        if ((& $P.Now) -ge $deadline) { return $false }
        & $P.Sleep 250
    }
}

function Test-MuxAgentSpecPath {
    # True for a spec.json (exact case) that IsUnder places inside the state root.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)][string]$Full)
    (& $P.IsUnder $P.StateRoot $Full) -and [IO.Path]::GetFileName($Full) -ceq 'spec.json'
}

function Invoke-MuxAgentLauncher {
    # Waits for the launch commit, then runs the client once in this console and records its exit code. No retry.
    param([Parameter(Mandatory)]$P, [Parameter(Mandatory)][string]$Spec, [scriptblock]$Client)
    $folder = [IO.Path]::GetDirectoryName($Spec)
    # Not $spec: PowerShell names ignore case, and the [string] parameter would turn the parsed spec back into text.
    $launch = Read-MuxAgentJson -Path $Spec
    $exitPath = Join-Path $folder 'launcher.json'
    $opened = & $P.Open $P.Pid
    $record = [ordered]@{ pid = $P.Pid; start = $opened.Start; startedAt = (& $P.Now).ToString('o') }
    Write-MuxAgentJson -Path $exitPath -Value $record
    if (-not (Wait-MuxAgentCommit -P $P -AgentDirectory $folder)) {
        $record['aborted'] = $true
        $record['exitCode'] = 1
        Write-MuxAgentJson -Path $exitPath -Value $record
        return 1
    }
    Clear-MuxAgentIdentity
    $code = 1
    if ($launch.client -cnotin @('claude', 'copilot')) { [Console]::Error.WriteLine('mux-agent: the launch spec names no known client') }
    else {
        $arguments = [string[]](@($script:MuxAgentClientFlags[$launch.client]) + @($launch.args))
        if (-not $Client) {
            $image = & $P.ClientImage $launch.client
            $executable = (Get-Command $image -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source
            $length = if ($executable) { $executable.Length + 3 + (Get-MuxAgentCommandLineLength -Arguments $arguments) }
            if (-not $executable) { [Console]::Error.WriteLine("mux-agent: $image is not on PATH") }
            elseif ($length -gt 32767) { [Console]::Error.WriteLine("mux-agent: the client command line is too long ($length characters)") }
            else {
                # Started without redirection so the client owns the pane's console: '& exe' inside this function
                # would pipe its stdout, and Claude without a terminal answers once in print mode and exits.
                $Client = {
                    param([string[]]$ClientArguments)
                    $info = [Diagnostics.ProcessStartInfo]::new($executable)
                    foreach ($argument in $ClientArguments) { $info.ArgumentList.Add($argument) }
                    $info.UseShellExecute = $false
                    $process = [Diagnostics.Process]::Start($info)
                    try { $process.WaitForExit(); $global:LASTEXITCODE = $process.ExitCode } finally { $process.Dispose() }
                }.GetNewClosure()
            }
        }
    }
    if ($Client) {
        $global:LASTEXITCODE = $null
        $code = try { $null = & $Client $arguments; if ($null -eq $global:LASTEXITCODE) { 1 } else { [int]$global:LASTEXITCODE } } catch { 1 }
    }
    $record['exitCode'] = $code
    $record['exitedAt'] = (& $P.Now).ToString('o')
    Write-MuxAgentJson -Path $exitPath -Value $record
    return $code
}

Register-MuxAgentVerb -Verb 'spawn' -Handler 'Invoke-MuxAgentSpawn' -Values @('Client', 'Task', 'TaskFile', 'BriefFile', 'Name', 'Split') -Switches @('NoFork') -Usage 'usage: mux-agent spawn -Client <claude|copilot> [-Task <text>] [-TaskFile <path>] [-BriefFile <path>] [-Name <name>] [-Split <value>] [-NoFork] [-Text]'
Register-MuxAgentVerb -Verb 'list' -Handler 'Invoke-MuxAgentList' -Switches @('All') -Usage 'usage: mux-agent list [-All] [-Text]'
Register-MuxAgentVerb -Verb 'focus' -Handler 'Invoke-MuxAgentFocus' -Min 1 -Max 1 -Usage 'usage: mux-agent focus <name> [-Text]'
Register-MuxAgentVerb -Verb 'send' -Handler 'Invoke-MuxAgentSend' -Values @('Body', 'BodyFile') -Min 1 -Max 1 -Usage 'usage: mux-agent send <name|parent|%pane> (-Body <text> | -BodyFile <path>) [-Text]'
Register-MuxAgentVerb -Verb 'read' -Handler 'Invoke-MuxAgentRead' -Values @('Wait', 'Lines') -Min 0 -Max 1 -Usage 'usage: mux-agent read [-Wait <seconds>] | read <name> [-Lines <n>] [-Text]'
Register-MuxAgentVerb -Verb 'stop' -Handler 'Invoke-MuxAgentStop' -Min 1 -Max 1 -Usage 'usage: mux-agent stop <name> [-Text]'
Register-MuxAgentVerb -Verb 'status' -Handler 'Invoke-MuxAgentStatus' -Values @('Body', 'BodyFile') -Usage 'usage: mux-agent status (-Body <text> | -BodyFile <path>) [-Text]'

# Unix member bodies (B2, B3, B4); they load everywhere and run only in the linux and macos member sets.
. (Join-Path $PSScriptRoot 'MuxAgents.Unix.ps1')
