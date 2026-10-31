# Unix (Linux, macOS) bodies of the provider members, dot-sourced after MuxAgents.ps1; dormant until B4 binds them. OS reads go through a kernel object.
function Import-MuxAgentUnixNative { if (-not ('MuxAgentUnixNative' -as [type])) { Add-Type -Path (Join-Path $PSScriptRoot 'MuxAgents.Native.Unix.cs') } }
