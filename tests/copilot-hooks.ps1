# #84: runs every `powershell` command of the plugin root's hooks.json as Copilot CLI runs it on Windows (cwd = the
# plugin root) against a recording binary that prints to stdout and stderr and exits 2: each must pass the expected
# arguments through scripts\mux-agent.cmd, print nothing and exit 0. Run: pwsh -NoProfile -File tests\copilot-hooks.ps1
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("copilot-hooks-" + [Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
$argv = Join-Path $tmp 'argv.txt'
$rec = Join-Path $tmp 'rec.cmd'
Set-Content -Encoding ascii $rec "@echo off`r`n>>`"$argv`" echo %*`r`necho noise`r`necho noise 1>&2`r`nexit /b 2`r`n"
$env:MUX_AGENTS_BIN = $rec
$hooks = Get-Content -Raw (Join-Path $repo 'hooks.json') | ConvertFrom-Json
$fail = 0
foreach ($event in $hooks.hooks.PSObject.Properties) {
  foreach ($hook in $event.Value) {
    Push-Location $repo
    try {
      $out = & pwsh -NoProfile -Command $hook.powershell 2>&1
      $code = $LASTEXITCODE
    } finally { Pop-Location }
    if ($code -ne 0) { Write-Output "FAIL $($event.Name): exit $code"; $fail++ }
    if ($out) { Write-Output "FAIL $($event.Name): printed $out"; $fail++ }
  }
}
$got = (Get-Content $argv | ForEach-Object { $_.Trim() } | Sort-Object) -join '|'
$want = 'report blocked -Kind question|report idle|report working|report working|report working'
if ($got -ne $want) { Write-Output "FAIL arguments: $got"; $fail++ }
Remove-Item -Recurse -Force $tmp
Write-Output "$fail failed"
exit $fail
