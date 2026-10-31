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
    It 'the entry and the launcher build providers before any call that can take a lock' {
        # A call can take a lock when it is in the static call closure of Use-MuxAgentLock over the function bodies of the
        # script libraries, where Invoke-MuxAgentCommand also reaches every registered verb handler. PowerShell command
        # names ignore case, so every name comparison here ignores case too. A gate that takes no lock may run first (B4).
        $ignoreCase = [StringComparer]::OrdinalIgnoreCase
        function Get-LockOrderNames($Ast) { @($Ast.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] }, $true) | ForEach-Object { "$($_.GetCommandName())" }) }
        $calls = [Collections.Generic.Dictionary[string, string[]]]::new($ignoreCase)
        foreach ($library in 'MuxAgents.ps1', 'MuxAgents.Unix.ps1') {
            $tree = [Management.Automation.Language.Parser]::ParseFile((Join-Path (Join-Path $repoRoot 'scripts') $library), [ref]$null, [ref]$null)
            foreach ($definition in $tree.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $true)) {
                $known = if ($calls.ContainsKey($definition.Name)) { $calls[$definition.Name] } else { @() }
                $calls[$definition.Name] = [string[]](@($known) + (Get-LockOrderNames $definition.Body))
            }
        }
        $handlers = @($script:MuxAgentVerbs.Values | ForEach-Object { if ($_.Handler -is [scriptblock]) { Get-LockOrderNames $_.Handler.Ast } else { "$($_.Handler)" } })
        $calls['Invoke-MuxAgentCommand'] = [string[]](@($calls['Invoke-MuxAgentCommand']) + $handlers)
        $locking = [Collections.Generic.HashSet[string]]::new([string[]]@('Use-MuxAgentLock'), $ignoreCase)
        do { $grew = $false; foreach ($name in @($calls.Keys)) { if (-not $locking.Contains($name) -and @($calls[$name] | Where-Object { $locking.Contains($_) }).Count) { $null = $locking.Add($name); $grew = $true } } } while ($grew)
        $answers = foreach ($name in 'mux-agent.ps1', 'mux-agent-launcher.ps1') {
            $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path (Join-Path $repoRoot 'scripts') $name), [ref]$null, [ref]$null)
            $all = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] }, $true))
            $build = @($all | Where-Object { $ignoreCase.Equals("$($_.GetCommandName())", 'Initialize-MuxAgentProviders') })
            $lock = @($all | Where-Object { $locking.Contains("$($_.GetCommandName())") })
            # Early: a lock call that starts before the build ends, so it runs first or inside the build's arguments.
            $early = if ($build.Count) { @($lock | Where-Object { $_.Extent.StartOffset -lt $build[0].Extent.EndOffset }) } else { $lock }
            "$name $($build.Count) $($lock.Count -gt 0) $($early.Count)"
        }
        "$($script:MuxAgentVerbs.Count) $($locking.Contains('Invoke-MuxAgentCommand')) $($locking.Contains('Initialize-MuxAgentProviders')) $($locking.Contains('New-MuxAgentProviders'))|$($answers -join '|')" | Should BeExactly '7 True False False|mux-agent.ps1 1 True 0|mux-agent-launcher.ps1 1 True 0'
    }
}
function Set-UnixStateReplies {
    # Per tool, a queue of verbatim 'exitcode|stdout' answers ('throw': the call cannot start); the last one repeats.
    param([Parameter(Mandatory)][hashtable]$Replies)
    $global:unixStateCalls = [Collections.Generic.List[string]]::new(); $global:unixStateReplies = @{}
    foreach ($tool in $Replies.Keys) { $global:unixStateReplies[$tool] = [Collections.Generic.Queue[string]]::new([string[]]@($Replies[$tool])) }
}
function Get-UnixStateReply {
    # Logged and answered by the tool's file name; the contexts mock Resolve-MuxAgentUnixToolPath to /usr/bin/<name>.
    param([string]$FilePath, [string[]]$Arguments)
    $tool = [IO.Path]::GetFileName($FilePath)
    $global:unixStateCalls.Add("$tool $($Arguments -join ' ')")
    $queue = $global:unixStateReplies[$tool]; $reply = if ($queue.Count -gt 1) { $queue.Dequeue() } else { $queue.Peek() }
    if ($reply -ceq 'throw') { throw [ComponentModel.Win32Exception]::new('spawn failed') }
    $code, $text = $reply -split '\|', 2
    [pscustomobject]@{ ExitCode = [int]$code; StdOut = $text; StdErr = '' }
}
Describe 'MuxAgents unix tool path' {
    It 'takes a tool only from a fully qualified PATH folder; empty, dot and relative entries are skipped even when the file is there' {
        $cwd = Join-Path $TestDrive 'cwd'; $good = Join-Path $TestDrive 'good'
        foreach ($folder in $cwd, (Join-Path $cwd 'rel'), $good) { $null = [IO.Directory]::CreateDirectory($folder); [IO.File]::WriteAllText((Join-Path $folder 'tool'), '') }
        $sep = [IO.Path]::PathSeparator; $before = [Environment]::CurrentDirectory
        try {
            [Environment]::CurrentDirectory = $cwd
            $found = Get-UnixStateRefusal { Resolve-MuxAgentUnixToolPath -Name 'tool' -SearchPath (@('', '.', 'rel', $good) -join $sep) -Message 'no tool' }
            $none = Get-UnixStateRefusal { Resolve-MuxAgentUnixToolPath -Name 'tool' -SearchPath (@('', '.', 'rel') -join $sep) -Message 'no tool' }
            "$found|$none" | Should BeExactly "ok $(Join-Path $good 'tool')|1 state no tool"
        }
        finally { [Environment]::CurrentDirectory = $before }
    }
    Context 'tool started by full path' {
        Mock Resolve-MuxAgentUnixToolPath { param($Name) "/usr/bin/$Name" }
        Mock Invoke-MuxAgentProcess { param($FilePath, $Arguments) $global:unixToolStarted = $FilePath; [pscustomobject]@{ ExitCode = 0; StdOut = "501`n"; StdErr = '' } }
        It 'starts the resolved full path, never the bare name' {
            try { "$(Invoke-MuxAgentUnixTool -FilePath 'id' -Arguments @('-u') -Message 'm')|$global:unixToolStarted" | Should BeExactly '501|/usr/bin/id' }
            finally { Remove-Variable unixToolStarted -Scope Global -ErrorAction SilentlyContinue }
        }
    }
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
        Mock Resolve-MuxAgentUnixToolPath { param($Name) "/usr/bin/$Name" }
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
        Mock Resolve-MuxAgentUnixToolPath { param($Name) "/usr/bin/$Name" }
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
    It 'passes a folder above the root only when every ACL entry is a deny; an allow, a name hiding deny, or an unreadable line refuses' {
        $stat = [pscustomobject]@{ Owner = '0'; Mode = 493 }
        $cases = @(@(), @(' 0: group:everyone deny delete'), @(' 0: group:everyone inherited deny delete,chown', ' 1: user:me deny write'), @(' 0: user:other allow add_subdirectory,delete_child'),
            @(' 0: group:everyone inherited allow list'), @(' 0: user:a deny b allow write'), @('garbage'), @(' 0: group:everyone deny delete', ''))
        $answers = foreach ($aces in $cases) { Get-UnixStateRefusal { Assert-MuxAgentUnixParent -Stat $stat -User '501' -Aces $aces } }
        $refused = '1 state a folder above the state folder has an ACL entry that is not a deny'
        ($answers -join ';') | Should BeExactly (@('ok', 'ok', 'ok', $refused, $refused, $refused, $refused, $refused) -join ';')
    }
}
Describe 'MuxAgents unix path facts' {
    Context 'native reads mocked' {
        Mock Get-MuxAgentUnixEntry { param($Path) if ($global:unixStateEntry -ceq 'throw') { throw [IO.IOException]::new('lstat failed') }; $global:unixStateEntry }
        Mock Resolve-MuxAgentUnixToolPath { param($Name) "/usr/bin/$Name" }
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
        It 'reads the ACL entries of a folder above the root with ls -lde on macOS only, and refuses a failed or malformed read' {
            try {
                Set-UnixStateReplies @{ ls = "0|drwxr-xr-x+ 5 root wheel 160 x /r`n 0: group:everyone deny delete`n" }
                $linux = @(Get-MuxAgentUnixParentAces -Path '/r' -Platform linux); $linuxCalls = $global:unixStateCalls.Count
                $mac = @(Get-MuxAgentUnixParentAces -Path '/r' -Platform macos); $macCalls = $global:unixStateCalls -join ';'
                $answers = foreach ($ls in '1|', '0|', '0|total 0', '0|-rw-r--r-- 1 u g 0 x /r') { Set-UnixStateReplies @{ ls = $ls }; Get-UnixStateRefusal { Get-MuxAgentUnixParentAces -Path '/r' -Platform macos } }
                "$($linux.Count) $linuxCalls|$macCalls|$($mac.Count)[$($mac -join '|')]|$($answers -join ';')" | Should BeExactly ('0 0|ls -lde -- /r|1[ 0: group:everyone deny delete]|' + ((1..4 | ForEach-Object { '1 state the state folder cannot be checked' }) -join ';'))
            }
            finally { Remove-Variable unixStateCalls, unixStateReplies -Scope Global -ErrorAction SilentlyContinue }
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
Describe 'MuxAgents unix private folders' {
    Context 'steps mocked' {
        Mock Get-MuxAgentUnixUserId { $global:unixStateCalls.Add('id'); '501' }
        Mock New-MuxAgentUnixDirectory { param($Path) $global:unixStateCalls.Add("new $Path"); $global:unixStateMissing.Remove($Path) }
        Mock Get-MuxAgentUnixPathFacts { param($Path) $global:unixStateCalls.Add("facts $Path"); [pscustomobject]@{ Path = $Path; Kind = 'directory'; Identity = '1:2'; Mode = $(if ($global:unixStateModes.ContainsKey($Path)) { $global:unixStateModes[$Path] } else { 448 }); Owner = $(if ($Path -like '*/agents') { '0' } else { '501' }); Listing = 'drwx------'; Aces = @(); Real = $Path } }
        # A folder above the root: '<owner> <mode>' from unixStateAbove, else root-owned 0755.
        Mock Get-MuxAgentUnixStat { param($Path) $global:unixStateCalls.Add("stat $Path"); $owner, $mode = $(if ($global:unixStateAbove.ContainsKey($Path)) { $global:unixStateAbove[$Path] } else { '0 493' }) -split ' '; [pscustomobject]@{ Identity = '1:1'; Mode = [int]$mode; Owner = $owner; Text = "1:1 $mode $owner" } }
        # The ACL entries of a folder above the root: from unixStateAces, else none.
        Mock Get-MuxAgentUnixParentAces { param($Path) if ($global:unixStateAces.ContainsKey($Path)) { $global:unixStateAces[$Path] } }
        $run = { param($Root, $Missing, $Modes, $Paths, $Above = @{}, $Aces = @{})
            $global:unixStateCalls = [Collections.Generic.List[string]]::new(); $global:unixStateMissing = [Collections.Generic.HashSet[string]]::new([string[]]$Missing); $global:unixStateModes = $Modes; $global:unixStateAbove = $Above; $global:unixStateAces = $Aces
            "$(Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $Root -Paths $Paths })|$($global:unixStateCalls -join ';')" }
        It 'walks each path top-down, creates what is missing, checks the root, all below it and all it created once each, and stops at the first refusal' {
            try {
                & $run '/r/s' @('/r', '/r/s', '/r/s/k', '/r/s/k/mail') @{} @('/r/s/k', '/r/s/k/mail') | Should BeExactly 'ok|id;stat /;new /r;facts /r;new /r/s;facts /r/s;new /r/s/k;facts /r/s/k;new /r/s/k/mail;facts /r/s/k/mail'
                & $run '/r/s' @('/r/s/k') @{ '/r/s' = 511 } @('/r/s/k') | Should BeExactly '1 state the state folder mode is 777, not 700|id;stat /;new /r;stat /r;new /r/s;facts /r/s'
                $global:unixStateMissing.Contains('/r/s/k') | Should Be $true
                & $run '/r/s' @() @{} @('/r/s/k/agents', '/r/s/k/mail') | Should BeExactly '1 state the state folder has another owner|id;stat /;new /r;stat /r;new /r/s;facts /r/s;new /r/s/k;facts /r/s/k;new /r/s/k/agents;facts /r/s/k/agents'
                $bad = foreach ($pair in @(@('/r/s', '/r/x'), @('/r/s', '/r/s2/k'), @('/r/s', '/r/s/../x'), @('/r/s', '/r/s//k'), @('/r/s', '/r/s/./k'), @('/r/s', '/r/s/k/'), @('r/s', 'r/s/k'))) { & $run $pair[0] @() @{} @($pair[1]) }
                ($bad | Select-Object -Unique) -join ';' | Should BeExactly '1 state the state folder path is outside the state root|'
            }
            finally { Remove-Variable unixStateCalls, unixStateMissing, unixStateModes, unixStateAbove -Scope Global -ErrorAction SilentlyContinue }
        }
        It 'checks every existing folder from / down to the root parent: owned by root or the user, not group- or other-writable unless sticky' {
            try {
                # '<owner> <mode>' of the existing /r (or /): 0777 another uid, 1777 root, 0755 another uid, 0777 root, 0775 and 0757 ours, 0755 ours, 1777 another uid, / of another uid.
                $cases = @(@{ '/r' = '502 511' }, @{ '/r' = '0 1023' }, @{ '/r' = '502 493' }, @{ '/r' = '0 511' }, @{ '/r' = '501 509' }, @{ '/r' = '501 495' }, @{ '/r' = '501 493' }, @{ '/r' = '502 1023' }, @{ '/' = '502 493' })
                $answers = foreach ($above in $cases) { & $run '/r/s' @('/r/s', '/r/s/k') @{} @('/r/s/k') $above }
                $owner = '1 state a folder above the state folder has another owner|id;stat /;new /r;stat /r'; $open = '1 state a folder above the state folder is writable by others|id;stat /;new /r;stat /r'
                $made = 'ok|id;stat /;new /r;stat /r;new /r/s;facts /r/s;new /r/s/k;facts /r/s/k'
                ($answers -join '#') | Should BeExactly (@($owner, $made, $owner, $open, $open, $open, $made, $owner, '1 state a folder above the state folder has another owner|id;stat /') -join '#')
            }
            finally { Remove-Variable unixStateCalls, unixStateMissing, unixStateModes, unixStateAbove, unixStateAces -Scope Global -ErrorAction SilentlyContinue }
        }
        It 'refuses a folder above the root with an ACL entry that is not a deny, before creating anything below it' {
            try {
                $cases = @(@{ '/r' = @(' 0: group:everyone deny delete') }, @{ '/r' = @(' 0: user:other allow add_subdirectory,delete_child') }, @{ '/' = @(' 0: group:everyone inherited allow list') })
                $answers = foreach ($aces in $cases) { & $run '/r/s' @('/r/s', '/r/s/k') @{} @('/r/s/k') @{} $aces }
                $refused = '1 state a folder above the state folder has an ACL entry that is not a deny'
                ($answers -join '#') | Should BeExactly (@('ok|id;stat /;new /r;stat /r;new /r/s;facts /r/s;new /r/s/k;facts /r/s/k', "$refused|id;stat /;new /r;stat /r", "$refused|id;stat /") -join '#')
            }
            finally { Remove-Variable unixStateCalls, unixStateMissing, unixStateModes, unixStateAbove, unixStateAces -Scope Global -ErrorAction SilentlyContinue }
        }
    }
    Context 'creation steps mocked' {
        Mock Get-MuxAgentUnixEntry { param($Path) $global:unixStateCalls.Add("entry $Path"); if ($global:unixStateEntry -ceq 'throw') { throw [UnauthorizedAccessException]::new('lstat failed') }; $global:unixStateEntry }
        Mock Invoke-MuxAgentUnixMkdir { param($Path, $Mode) $global:unixStateCalls.Add("mkdir $Path $([int]$Mode)"); if ($global:unixStateFail -ceq 'mkdir') { throw [IO.IOException]::new('mkdir failed') } }
        Mock Invoke-MuxAgentUnixChmod { param($Path, $Mode) $global:unixStateCalls.Add("chmod $Path $([int]$Mode)"); if ($global:unixStateFail -ceq 'chmod') { throw [UnauthorizedAccessException]::new('chmod failed') } }
        It 'creates a missing folder by mkdir 0700 then chmod 0700, leaves any existing entry, and refuses a failed entry read, mkdir or chmod' {
            try {
                $answers = foreach ($case in @('missing|', 'directory|', 'link|', 'file|', 'throw|', 'missing|mkdir', 'missing|chmod')) {
                    $global:unixStateEntry, $global:unixStateFail = $case -split '\|'; $global:unixStateCalls = [Collections.Generic.List[string]]::new()
                    "$(Get-UnixStateRefusal { New-MuxAgentUnixDirectory -Path '/r/s/k' })|$($global:unixStateCalls -join ',')"
                }
                $created = '1 state the state folder cannot be created|entry /r/s/k,mkdir /r/s/k 448'
                ($answers -join '#') | Should BeExactly (@('ok True|entry /r/s/k,mkdir /r/s/k 448,chmod /r/s/k 448', 'ok False|entry /r/s/k', 'ok False|entry /r/s/k', 'ok False|entry /r/s/k', '1 state the state folder cannot be checked|entry /r/s/k', $created, "$created,chmod /r/s/k 448") -join '#')
            }
            finally { Remove-Variable unixStateEntry, unixStateFail, unixStateCalls -Scope Global -ErrorAction SilentlyContinue }
        }
    }
    It 'makes folders only through mkdir(2) with the mode, never CreateDirectory, which succeeds on an existing folder or link' {
        $file = [Management.Automation.Language.Parser]::ParseFile((Join-Path (Join-Path $repoRoot 'scripts') 'MuxAgents.Unix.ps1'), [ref]$null, [ref]$null)
        $member = { param($Name) { param($n) $n -is [Management.Automation.Language.InvokeMemberExpressionAst] -and "$($n.Member)" -cmatch $Name }.GetNewClosure() }
        $create = @($file.FindAll((& $member '^CreateDirectory$'), $true))
        $all = @($file.FindAll((& $member '^Mkdir(Linux|Mac)$'), $true))
        $mkdir = @((Get-Command Invoke-MuxAgentUnixMkdir).ScriptBlock.Ast.FindAll((& $member '^Mkdir(Linux|Mac)$'), $true))
        "$($create.Count) $($all.Count) $(@($mkdir | ForEach-Object { "$($_.Member) $($_.Arguments[1])" }) -join ',')" | Should BeExactly '0 2 MkdirMac [int]$Mode,MkdirLinux [int]$Mode'
    }
}
function New-UnixStateNativeRoot {
    # A canonical, missing folder under TestDrive (on macOS TestDrive sits under /var, a link to /private/var).
    Resolve-MuxAgentUnixRealPath -Path (Join-Path $TestDrive ([guid]::NewGuid().ToString('N')))
}
function Get-UnixStateMode([string]$Path) { if ([IO.Directory]::Exists($Path)) { [Convert]::ToString([int][IO.File]::GetUnixFileMode($Path), 8) } else { 'missing' } }
function Add-UnixStateNativeTests {
    It 'runs id from PATH, not a file named id in the current directory, even with . first on PATH' {
        $cwd = Join-Path $TestDrive ([guid]::NewGuid().ToString('N')); $null = [IO.Directory]::CreateDirectory($cwd)
        $planted = Join-Path $cwd 'id'; [IO.File]::WriteAllText($planted, "#!/bin/sh`necho 99999`n"); [IO.File]::SetUnixFileMode($planted, [IO.UnixFileMode]448)
        $uid = "$(& /usr/bin/id -u)"; $before = [Environment]::CurrentDirectory; $path = $env:PATH
        try {
            [Environment]::CurrentDirectory = $cwd; $env:PATH = ".$([IO.Path]::PathSeparator)$path"
            "$(Get-MuxAgentUnixUserId)" | Should BeExactly $uid
        }
        finally { [Environment]::CurrentDirectory = $before; $env:PATH = $path }
    }
    It 'creates the root, the folder above it and every folder below it at exactly 0700 under umask 177' {
        $above = New-UnixStateNativeRoot; $root = "$above/state"
        $probe = Join-Path $TestDrive 'umask-probe.ps1'
        [IO.File]::WriteAllLines($probe, [string[]]@(
            'param([string]$Scripts, [string]$Root)'
            'foreach ($name in "MuxAgents.ps1", "MuxAgents.Unix.ps1") { . (Join-Path $Scripts $name) }'
            '$paths = @((Split-Path -Parent $Root), $Root, "$Root/k", "$Root/k/agents", "$Root/k/mail")'
            '$refusal = try { New-MuxAgentUnixPrivateDirectories -Root $Root -Paths @($paths[2], $paths[3], $paths[4]); "ok" } catch { $_.Exception.Message }'
            '$modes = foreach ($path in $paths) { if ([IO.Directory]::Exists($path)) { [Convert]::ToString([int][IO.File]::GetUnixFileMode($path), 8) } else { "missing" } }'
            '"$($modes -join " ") $refusal"'
        ))
        $out = & sh -c 'umask 177; exec "$0" "$@"' $pwsh -NoProfile -File $probe -Scripts (Join-Path $repoRoot 'scripts') -Root $root
        "$out" | Should BeExactly '700 700 700 700 700 ok'
    }
    It 'refuses a 0777 root before creating anything in it, and leaves 0755, 1700, file and file/k as they are' {
        $wide = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($wide); [IO.File]::SetUnixFileMode($wide, [IO.UnixFileMode]511)
        $wideAnswer = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $wide -Paths @("$wide/k") }
        $root = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($root); [IO.File]::SetUnixFileMode($root, [IO.UnixFileMode]448)
        $open = "$root/open"; $sticky = "$root/sticky"; $file = "$root/file"
        foreach ($pair in @(@($open, 493), @($sticky, 960))) { $null = [IO.Directory]::CreateDirectory($pair[0]); [IO.File]::SetUnixFileMode($pair[0], [IO.UnixFileMode]$pair[1]) }
        [IO.File]::WriteAllText($file, 'x')
        $answers = foreach ($path in $open, $sticky, $file, "$file/k") { Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($path) } }
        "$wideAnswer|$(Get-UnixStateMode "$wide/k")|$($answers -join '|')|$(Get-UnixStateMode $open) $(Get-UnixStateMode $sticky)" | Should BeExactly '1 state the state folder mode is 777, not 700|missing|1 state the state folder mode is 755, not 700|1 state the state folder mode is 1700, not 700|1 state the state folder path is a file|1 state the state folder path is a file|755 1700'
    }
    It 'refuses a root below a folder that is 0777 and not sticky, and creates one below a sticky 1777 folder' {
        $open = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($open); [IO.File]::SetUnixFileMode($open, [IO.UnixFileMode]511)
        $sticky = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($sticky); [IO.File]::SetUnixFileMode($sticky, [IO.UnixFileMode]1023)
        $answers = foreach ($parent in $open, $sticky) { Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root "$parent/state" -Paths @("$parent/state/k") } }
        "$($answers -join '|')|$(Get-UnixStateMode $open) $(Get-UnixStateMode "$open/state") $(Get-UnixStateMode $sticky) $(Get-UnixStateMode "$sticky/state/k")" | Should BeExactly '1 state a folder above the state folder is writable by others|ok|777 missing 1777 700'
    }
    It 'refuses a leaf link, a dangling link and an inner link with lstat semantics' {
        $root = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory("$root/real/k", [IO.UnixFileMode]448)
        foreach ($path in $root, "$root/real", "$root/real/k") { [IO.File]::SetUnixFileMode($path, [IO.UnixFileMode]448) }
        $null = [IO.Directory]::CreateSymbolicLink("$root/link", "$root/real")
        $null = [IO.Directory]::CreateSymbolicLink("$root/dangling", "$root/nowhere")
        $answers = foreach ($path in "$root/link", "$root/dangling", "$root/link/k") { Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($path) } }
        "$($answers -join '|')|$(Get-UnixStateMode "$root/nowhere")" | Should BeExactly '1 state the state folder path has a link|1 state the state folder path has a link|1 state the state folder path has a link|missing'
    }
    Context 'a link appears after the entry check' {   # a Pester 4 Mock lasts to the end of its Context
        Mock Get-MuxAgentUnixEntry { 'missing' }
        It 'refuses when a link appears after the entry check, and leaves the link target mode as it was' {
            $base = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($base); [IO.File]::SetUnixFileMode($base, [IO.UnixFileMode]448)
            $target = "$base/target"; $null = [IO.Directory]::CreateDirectory($target); [IO.File]::SetUnixFileMode($target, [IO.UnixFileMode]493)
            $null = [IO.Directory]::CreateSymbolicLink("$base/k", $target)
            $made = try { Invoke-MuxAgentUnixMkdir -Path "$base/k" -Mode ([IO.UnixFileMode]448); 'made' } catch { 'refused' }
            $answer = Get-UnixStateRefusal { New-MuxAgentUnixDirectory -Path "$base/k" }
            "$made|$answer|$(Get-UnixStateMode $target)" | Should BeExactly 'refused|1 state the state folder cannot be created|755'
        }
    }
    It 'refuses a folder of another owner' {
        if ((Get-MuxAgentUnixUserId) -ceq '0') { Set-ItResult -Skipped -Because 'the tests run as root, who owns /usr'; return }
        Get-UnixStateRefusal { Assert-MuxAgentUnixPrivate -Facts (Get-MuxAgentUnixPathFacts -Path '/usr') -User (Get-MuxAgentUnixUserId) } | Should BeExactly '1 state the state folder has another owner'
    }
    It 'canonicalizes a linked or trailing-space root and keeps the missing tail' {
        $base = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory("$base/real")
        $real = "$base/real"; $null = [IO.Directory]::CreateSymbolicLink("$base/link", $real)
        $spaced = "$base/state "; $null = [IO.Directory]::CreateDirectory($spaced, [IO.UnixFileMode]448); [IO.File]::SetUnixFileMode($spaced, [IO.UnixFileMode]448)
        $root = Get-MuxAgentUnixStateRoot -Environment @{ MUX_AGENTS_STATE = $spaced }
        $made = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @("$root/k") }
        "$(Get-MuxAgentUnixStateRoot -Environment @{ MUX_AGENTS_STATE = "$base/link/s" })|$(Get-MuxAgentUnixStateRoot -Environment @{ XDG_STATE_HOME = "$base/link"; HOME = '/nonexistent' })|$root|$made $(Get-UnixStateMode "$root/k")" | Should BeExactly "$real/s|$real/mux-agents/state|$spaced|ok 700"
    }
}
Describe 'MuxAgents unix state on Linux' -Tag 'linux' {
    Add-UnixStateNativeTests
    It 'refuses a 0700 folder that carries a POSIX ACL' {
        if (-not (Get-Command setfacl -CommandType Application -ErrorAction SilentlyContinue)) { Set-ItResult -Skipped -Because 'setfacl is not installed'; return }
        $root = New-UnixStateNativeRoot
        $before = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($root) }
        $exists = [IO.Directory]::Exists($root)
        $exists | Should Be $true
        $out = "$(& setfacl -m u:nobody:rwx -- $root 2>&1)"; $code = $LASTEXITCODE
        if ($code -ne 0 -and $out -match 'Operation not supported') { Set-ItResult -Skipped -Because 'this file system takes no POSIX ACL'; return }
        [IO.File]::SetUnixFileMode($root, [IO.UnixFileMode]448)
        $facts = Get-MuxAgentUnixPathFacts -Path $root
        "$before|$exists|$code|$([Convert]::ToString([int]$facts.Mode, 8)) $($facts.Listing)|$(Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($root) })" | Should BeExactly 'ok|True|0|700 drwx------+|1 state the state folder has an extended ACL'
    }
}
Describe 'MuxAgents unix state on macOS' -Tag 'macos' {
    Add-UnixStateNativeTests
    It 'refuses a 0700 folder with an ACE' {
        $root = New-UnixStateNativeRoot
        $before = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($root) }
        & chmod +a 'everyone allow read' $root; $code = $LASTEXITCODE
        $facts = Get-MuxAgentUnixPathFacts -Path $root
        "$before|$code $([Convert]::ToString([int]$facts.Mode, 8)) $(@($facts.Aces).Count)|$(Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @($root) })" | Should BeExactly 'ok|0 700 1|1 state the state folder has an extended ACL'
    }
    It 'refuses a root below a folder with an allow ACE, before creating anything below that folder' {
        $parent = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($parent)
        & chmod +a 'everyone allow add_subdirectory,delete_child' $parent; $code = $LASTEXITCODE
        $refusal = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root "$parent/state" -Paths @("$parent/state/k") }
        "$code|$refusal|$(Get-UnixStateMode "$parent/state")" | Should BeExactly '0|1 state a folder above the state folder has an ACL entry that is not a deny|missing'
    }
    It 'refuses a new 0700 root that inherits an ACE, and creates nothing below it' {
        $parent = New-UnixStateNativeRoot; $null = [IO.Directory]::CreateDirectory($parent)
        & chmod +a 'everyone deny chown,directory_inherit' $parent; $code = $LASTEXITCODE   # a deny passes the parent check; the root inherits it
        $root = "$parent/state"
        $refusal = Get-UnixStateRefusal { New-MuxAgentUnixPrivateDirectories -Root $root -Paths @("$root/k") }
        $facts = Get-MuxAgentUnixPathFacts -Path $root
        "$code|$refusal|$(Get-UnixStateMode $root) $(@($facts.Aces | Where-Object { $_ -match 'inherited' }).Count) $(Get-UnixStateMode "$root/k")" | Should BeExactly '0|1 state the state folder has an extended ACL|700 1 missing'
    }
}
