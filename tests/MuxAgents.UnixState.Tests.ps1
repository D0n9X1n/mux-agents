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
Describe 'MuxAgents lock order' {
    It 'the entry and the launcher build providers before the first command that can take a lock, so that lock uses LockName' {
        # Commands that reach Use-MuxAgentLock, from the function bodies of the script libraries; Invoke-MuxAgentCommand
        # reaches every handler given to Register-MuxAgentVerb. An entry gate that takes no lock may run first (B4).
        $calls = @{}; $handlers = [Collections.Generic.List[string]]::new()
        foreach ($library in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') {
            $tree = [Management.Automation.Language.Parser]::ParseFile((Join-Path (Join-Path $repoRoot 'scripts') $library), [ref]$null, [ref]$null)
            foreach ($definition in $tree.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $true)) {
                $calls[$definition.Name] = @($definition.Body.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] }, $true) | ForEach-Object { "$($_.GetCommandName())" })
            }
            foreach ($register in $tree.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] -and "$($node.GetCommandName())" -ceq 'Register-MuxAgentVerb' }, $true)) {
                $elements = $register.CommandElements
                for ($index = 1; $index -lt $elements.Count - 1; $index++) { if ("$($elements[$index])" -ceq '-Handler') { $handlers.Add($elements[$index + 1].SafeGetValue()) } }
            }
        }
        $calls['Invoke-MuxAgentCommand'] = @($calls['Invoke-MuxAgentCommand']) + $handlers
        $locking = [Collections.Generic.HashSet[string]]::new([string[]]@('Use-MuxAgentLock'))
        do { $grew = $false; foreach ($name in $calls.Keys) { if (-not $locking.Contains($name) -and @($calls[$name] | Where-Object { $locking.Contains($_) }).Count) { $grew = $locking.Add($name) -or $grew } } } while ($grew)
        $answers = foreach ($name in 'mux-agent.ps1', 'mux-agent-launcher.ps1') {
            $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path (Join-Path $repoRoot 'scripts') $name), [ref]$null, [ref]$null)
            # Ordered by where each call ends: an inner call used as an argument ends, and runs, before its outer call.
            $commands = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] }, $true) | Sort-Object { $_.Extent.EndOffset })
            $providers = $commands | Where-Object { "$($_.GetCommandName())" -ceq 'New-MuxAgentProviders' } | Select-Object -First 1
            $lock = $commands | Where-Object { $locking.Contains("$($_.GetCommandName())") } | Select-Object -First 1
            $order = if (-not $providers -or -not $lock) { 'missing' } elseif ($providers.Extent.EndOffset -lt $lock.Extent.EndOffset) { 'before' } else { 'after' }
            "$name New-MuxAgentProviders $order $($lock.GetCommandName())"
        }
        "$($handlers.Count) $($locking.Contains('Invoke-MuxAgentCommand')) $($locking.Contains('New-MuxAgentProviders'))|$($answers -join '|')" | Should BeExactly '7 True False|mux-agent.ps1 New-MuxAgentProviders before Invoke-MuxAgentCommand|mux-agent-launcher.ps1 New-MuxAgentProviders before Add-MuxAgentNotice'
    }
}
function Set-UnixStateReplies {
    # Per tool, a queue of verbatim 'exitcode|stdout' answers ('throw': the call cannot start); the last one repeats.
    param([Parameter(Mandatory)][hashtable]$Replies)
    $global:unixStateCalls = [Collections.Generic.List[string]]::new(); $global:unixStateReplies = @{}
    foreach ($tool in $Replies.Keys) { $global:unixStateReplies[$tool] = [Collections.Generic.Queue[string]]::new([string[]]@($Replies[$tool])) }
}
function Get-UnixStateReply {
    param([string]$FilePath, [string[]]$Arguments)
    $global:unixStateCalls.Add("$FilePath $($Arguments -join ' ')")
    $queue = $global:unixStateReplies[$FilePath]; $reply = if ($queue.Count -gt 1) { $queue.Dequeue() } else { $queue.Peek() }
    if ($reply -ceq 'throw') { throw [ComponentModel.Win32Exception]::new('spawn failed') }
    $code, $text = $reply -split '\|', 2
    [pscustomobject]@{ ExitCode = [int]$code; StdOut = $text; StdErr = '' }
}
Describe 'MuxAgents unix state root' {
    Context 'resolver mocked' {
        Mock Resolve-MuxAgentUnixRealPath { param($Path) "real:$Path" }
        It 'takes MUX_AGENTS_STATE, else an absolute XDG_STATE_HOME, else HOME, never LOCALAPPDATA, and resolves the choice' {
            $answers = foreach ($environment in @(@{ MUX_AGENTS_STATE = '/s'; XDG_STATE_HOME = '/x'; HOME = '/h' }, @{ XDG_STATE_HOME = '/x/'; HOME = '/h' }, @{ XDG_STATE_HOME = 'rel'; HOME = '/h/' }, @{ HOME = '/h'; LOCALAPPDATA = '/l' }, @{ HOME = 'h' }, @{})) {
                Get-UnixStateRefusal { Get-MuxAgentUnixStateRoot -Environment $environment }
            }
            ($answers -join ';') | Should BeExactly 'ok real:/s;ok real:/x/mux-agents/state;ok real:/h/.local/state/mux-agents/state;ok real:/h/.local/state/mux-agents/state;1 state HOME is not an absolute path;1 state HOME is not an absolute path'
        }
        It 'refuses a relative MUX_AGENTS_STATE instead of falling back to XDG_STATE_HOME or HOME' {
            Get-UnixStateRefusal { Get-MuxAgentUnixStateRoot -Environment @{ MUX_AGENTS_STATE = 'relative'; XDG_STATE_HOME = '/x'; HOME = '/h' } } | Should BeExactly '1 state MUX_AGENTS_STATE is not an absolute path'
        }
    }
    Context 'realpath mocked' {
        Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments) Get-UnixStateReply -FilePath $FilePath -Arguments $Arguments }
        It 'resolves the longest existing prefix, keeps the rest and every space, and refuses a failed or malformed answer or path' {
            Set-UnixStateReplies @{ realpath = @("0|/canon`n", "0|/`n", "0|/r/state `n", '1|/canon', "0|canon`n", "0|/a`n/b`n", '0|', 'throw') }
            $missing = '/mux-b3-' + [guid]::NewGuid().ToString('N')
            try {
                $answers = foreach ($path in @(@(1..8 | ForEach-Object { "$missing/a/b" }) + @('rel/a', '/x/../y', '/x/./y'))) { Get-UnixStateRefusal { Resolve-MuxAgentUnixRealPath -Path $path } }
                ($answers -join ';') | Should BeExactly ((@("ok /canon$missing/a/b", "ok $missing/a/b", "ok /r/state $missing/a/b") + @(1..5 | ForEach-Object { '1 state the state folder path cannot be resolved' }) + @(1..3 | ForEach-Object { '1 state the state folder path is not absolute' })) -join ';')
                ($global:unixStateCalls -join ';') | Should BeExactly ((@(1..8 | ForEach-Object { 'realpath -- /' })) -join ';')
            }
            finally { Remove-Variable unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
        }
    }
    Context 'prefix probe throws' {
        Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments) Get-UnixStateReply -FilePath $FilePath -Arguments $Arguments }
        Mock Test-Path { throw [UnauthorizedAccessException]::new('denied') }
        It 'refuses when the existing-prefix probe throws, and runs no realpath' {
            Set-UnixStateReplies @{ realpath = "0|/canon`n" }
            try {
                "$(Get-UnixStateRefusal { Resolve-MuxAgentUnixRealPath -Path '/r/state' })|$($global:unixStateCalls.Count)" | Should BeExactly '1 state the state folder path cannot be resolved|0'
            }
            finally { Remove-Variable unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
        }
    }
}
Describe 'MuxAgents unix private decision' {
    It 'passes only an unlinked 0700 directory of the user with no extended ACL, and names the first failed check' {
        $base = @{ Path = '/r/k'; Kind = 'directory'; Identity = '66:12'; Mode = 448; Owner = '501'; Listing = 'drwx------'; Aces = @(); Real = '/r/k' }
        $cases = @(@{}, @{ Kind = 'link'; Owner = $null }, @{ Kind = 'file' }, @{ Kind = 'missing' }, @{ Real = '/p/k'; Owner = '0'; Mode = 493 }, @{ Owner = '0'; Mode = 493 }, @{ Owner = '5010' }, @{ Mode = 493; Listing = 'drwxr-xr-x+' }, @{ Mode = 2496 }, @{ Mode = 960 }, @{ Mode = 384 }, @{ Listing = 'drwx------+' }, @{ Aces = @(' 0: group:everyone inherited allow list') }, @{ Listing = 'drwx------.' }, @{ Listing = 'drwx------@' })
        $answers = foreach ($case in $cases) {
            $facts = $base.Clone(); foreach ($key in $case.Keys) { $facts[$key] = $case[$key] }
            Get-UnixStateRefusal { Assert-MuxAgentUnixPrivate -Facts ([pscustomobject]$facts) -User '501' }
        }
        ($answers -join ';') | Should BeExactly 'ok;1 state the state folder path has a link;1 state the state folder path is a file;1 state the state folder is missing;1 state the state folder path has a link;1 state the state folder has another owner;1 state the state folder has another owner;1 state the state folder mode is 755, not 700;1 state the state folder mode is 4700, not 700;1 state the state folder mode is 1700, not 700;1 state the state folder mode is 600, not 700;1 state the state folder has an extended ACL;1 state the state folder has an extended ACL;ok;ok'
    }
}
Describe 'MuxAgents unix path facts' {
    Context 'native reads mocked' {
        Mock Get-MuxAgentUnixEntry { param($Path) if ($global:unixStateEntry -ceq 'throw') { throw [IO.IOException]::new('lstat failed') }; $global:unixStateEntry }
        Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments) Get-UnixStateReply -FilePath $FilePath -Arguments $Arguments }
        It 'reads stat, ls and realpath, then stat again, with the flags of each OS, and the uid with id -u' {
            $global:unixStateEntry = 'directory'
            try {
                $answers = foreach ($platform in 'linux', 'macos') {
                    Set-UnixStateReplies @{ stat = "0|66:12 1700 501`n"; ls = "0|drwx-----T+ 2 u staff 64 Oct 1 12:00 /r/a+b`n 0: group:everyone inherited allow list`n"; realpath = "0|/r/a+b`n"; id = "0|501`n" }
                    $f = Get-MuxAgentUnixPathFacts -Path '/r/a+b' -Platform $platform
                    "$($f.Kind) $($f.Identity) $($f.Mode) $($f.Owner) $($f.Listing) [$(@($f.Aces) -join '|')] $($f.Real) $(Get-MuxAgentUnixUserId)|$($global:unixStateCalls -join ';')"
                }
                $same = 'directory 66:12 960 501 drwx-----T+ [ 0: group:everyone inherited allow list] /r/a+b 501'
                ($answers -join '#') | Should BeExactly "$same|stat -c %d:%i %a %u -- /r/a+b;ls -ld -- /r/a+b;realpath -- /r/a+b;stat -c %d:%i %a %u -- /r/a+b;id -u#$same|stat -f %d:%i %Mp%Lp %u -- /r/a+b;ls -lde -- /r/a+b;realpath -- /r/a+b;stat -f %d:%i %Mp%Lp %u -- /r/a+b;id -u"
            }
            finally { Remove-Variable unixStateEntry, unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
        }
        It 'reads nothing for a link, file or missing path, and refuses every failed, malformed, vanished or changed read' {
            $good = @{ stat = '0|66:12 700 501'; ls = '0|drwx------ 2 u g 64 x /r/k'; realpath = '0|/r/k'; id = '0|501' }
            try {
                $quiet = foreach ($kind in 'link', 'file', 'missing') { $global:unixStateEntry = $kind; Set-UnixStateReplies $good; $f = Get-MuxAgentUnixPathFacts -Path '/r/k' -Platform linux; "$($f.Kind)/$($f.Identity)/$($global:unixStateCalls.Count)" }
                $global:unixStateEntry = 'directory'
                $bad = @(@{ stat = '1|66:12 700 501' }, @{ stat = 'throw' }, @{ stat = '0|66:12 700' }, @{ stat = '0|66:12 700 501 x' }, @{ ls = '0|d' }, @{ ls = '0|' }, @{ ls = '0|total 0' }, @{ ls = '0|-rw------- 1 u g 0 x /r/k' }, @{ realpath = '0|r/k' }, @{ realpath = "0|/r/k`n/x" }, @{ stat = @('0|66:12 700 501', '1|') }, @{ stat = @('0|66:12 700 501', '0|66:13 700 501') }, @{ stat = @('0|66:12 700 501', '0|66:12 777 501') })
                $answers = @(foreach ($case in $bad) { $replies = $good.Clone(); foreach ($key in $case.Keys) { $replies[$key] = $case[$key] }; Set-UnixStateReplies $replies; Get-UnixStateRefusal { Get-MuxAgentUnixPathFacts -Path '/r/k' -Platform linux } })
                $global:unixStateEntry = 'throw'; $answers += Get-UnixStateRefusal { Get-MuxAgentUnixPathFacts -Path '/r/k' -Platform linux }
                foreach ($id in '0|5 01', '1|501', '0|501 ') { Set-UnixStateReplies @{ id = $id }; $answers += Get-UnixStateRefusal { Get-MuxAgentUnixUserId } }
                $checked = '1 state the state folder cannot be checked'; $changed = '1 state the state folder changed while it was checked'
                "$($quiet -join ',')|$($answers -join ';')" | Should BeExactly ('link//0,file//0,missing//0|' + ((@(1..11 | ForEach-Object { $checked }) + @($changed, $changed, $checked) + @(1..3 | ForEach-Object { '1 state the user id cannot be read' })) -join ';'))
            }
            finally { Remove-Variable unixStateEntry, unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
        }
        It 'reads the ls permission field position by position and refuses full-length fields no file system prints' {
            $global:unixStateEntry = 'directory'
            try {
                $fields = @('drwx------', 'd--x--x--t', 'drwsr-Sr-T.', 'drwx------@', 'drrrrrrrrr', 'dwwwwwwwww', 'dxxxxxxxxx', 'dtttttttttt', 'drwtrwxrwx', 'drwxrwxrws', 'dSwxrwxrwx', 'drwxrwxrwx-', 'drwx------x', 'drwx------++')
                $answers = foreach ($field in $fields) {
                    Set-UnixStateReplies @{ stat = '0|66:12 700 501'; ls = "0|$field 2 u g 64 x /r/k"; realpath = '0|/r/k' }
                    Get-UnixStateRefusal { (Get-MuxAgentUnixPathFacts -Path '/r/k' -Platform linux).Listing }
                }
                $checked = '1 state the state folder cannot be checked'
                ($answers -join ';') | Should BeExactly ((@('ok drwx------', 'ok d--x--x--t', 'ok drwsr-Sr-T.', 'ok drwx------@') + @(1..10 | ForEach-Object { $checked })) -join ';')
            }
            finally { Remove-Variable unixStateEntry, unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
        }
    }
}
