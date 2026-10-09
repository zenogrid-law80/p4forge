#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Server = 'https://p4forge.zenogrid.co.kr:8444/',
    [string]$P4Host = 'p4forge.zenogrid.co.kr',
    [int]$P4Port = 1666,
    [string]$InstallRoot = 'C:\P4Forge',
    [string]$ReportPath = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Automatic script-path variables can be empty during parameter default binding.
# Resolve the default only after the script body has started.
if ([string]::IsNullOrWhiteSpace($ReportPath)) {
    $reportDirectory = $PSScriptRoot
    if ([string]::IsNullOrWhiteSpace($reportDirectory) -and -not [string]::IsNullOrWhiteSpace($PSCommandPath)) {
        $reportDirectory = Split-Path -Parent $PSCommandPath
    }
    if ([string]::IsNullOrWhiteSpace($reportDirectory)) { $reportDirectory = (Get-Location).Path }
    $ReportPath = Join-Path -Path $reportDirectory -ChildPath 'windows-host-report.json'
}
# Read-only host checks. Never read credential contents, task arguments or build logs.
$checks = [Collections.Generic.List[object]]::new()
function Record([string]$Name, [string]$Status, [object]$Detail) {
    $checks.Add([pscustomobject]@{ name = $Name; status = $Status; detail = $Detail })
    Write-Host ('[{0}] {1}: {2}' -f $Status, $Name, ($Detail | ConvertTo-Json -Compress -Depth 4))
}
function Check-Port([string]$Name, [string]$Address, [int]$Port) {
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $pending = $client.BeginConnect($Address, $Port, $null, $null)
        try {
            if (-not $pending.AsyncWaitHandle.WaitOne(5000)) { throw 'TCP connection timed out after 5 seconds.' }
            $client.EndConnect($pending)
        } finally { $pending.AsyncWaitHandle.Close() }
        Record $Name 'PASS' "${Address}:$Port reachable"
    } catch { Record $Name 'FAIL' $_.Exception.Message }
    finally { $client.Dispose() }
}
$uri = [uri]$Server
if (-not $uri.IsAbsoluteUri -or $uri.Scheme -ne 'https' -or $uri.UserInfo -or $uri.AbsolutePath -ne '/' -or $uri.Query -or $uri.Fragment) { throw 'Server must be an HTTPS origin.' }
if ($env:OS -ne 'Windows_NT') { throw 'Run this diagnostic on the Windows build machine.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Record 'host' 'INFO' @{ computer = $env:COMPUTERNAME; os_version = [Environment]::OSVersion.Version.ToString(); powershell = $PSVersionTable.PSVersion.ToString(); administrator = $admin }
$supported = [Environment]::Is64BitProcess -and [Environment]::Is64BitOperatingSystem -and [Environment]::OSVersion.Version.Build -ge 14393
Record 'windows_x64' $(if ($supported) { 'PASS' } else { 'FAIL' }) 'Requires 64-bit PowerShell and Windows 10 / Server 2016 or newer.'
try {
    Record 'dns' 'PASS' @([Net.Dns]::GetHostAddresses($uri.DnsSafeHost) | ForEach-Object { $_.IPAddressToString })
} catch { Record 'dns' 'FAIL' $_.Exception.Message }
Check-Port 'https_tcp' $uri.DnsSafeHost $uri.Port
Check-Port 'perforce_tcp' $P4Host $P4Port
$previousTls = [Net.ServicePointManager]::SecurityProtocol
try {
    [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
    $response = Invoke-WebRequest -UseBasicParsing -Uri ($Server.TrimEnd('/') + '/readyz') -TimeoutSec 8 -MaximumRedirection 0
    Record 'https_readyz' $(if ($response.StatusCode -eq 200) { 'PASS' } else { 'FAIL' }) ([int]$response.StatusCode)
} catch { Record 'https_readyz' 'FAIL' $_.Exception.Message }
finally { [Net.ServicePointManager]::SecurityProtocol = $previousTls }
Record 'execution_policy' 'INFO' @(Get-ExecutionPolicy -List | Select-Object Scope, ExecutionPolicy)
try {
    $task = Get-ScheduledTask -TaskName 'P4Forge-Agent' -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        Record 'agent_task' 'MISSING' 'P4Forge-Agent is not installed.'
    } else {
        $info = Get-ScheduledTaskInfo -TaskName 'P4Forge-Agent'
        Record 'agent_task' 'INFO' @{ state = [string]$task.State; principal = $task.Principal.UserId; last_result = $info.LastTaskResult; last_run = $info.LastRunTime.ToString('o') }
    }
} catch { Record 'agent_task' 'UNKNOWN' 'Cannot inspect scheduled task. Run as Administrator to inspect this item.' }
try {
    $account = Get-LocalUser -Name 'p4forge-build' -ErrorAction SilentlyContinue
    if ($null -eq $account) { Record 'build_account' 'MISSING' 'p4forge-build is not installed.' }
    else { Record 'build_account' 'INFO' @{ enabled = $account.Enabled; sid = $account.SID.Value } }
} catch { Record 'build_account' 'UNKNOWN' 'Cannot inspect local build account.' }
foreach ($name in @('p4forge-agent.exe', 'p4.exe', 'installation.json', 'state\agent.log', 'secrets\agent-token', 'secrets\p4tickets', 'secrets\build-account.json')) {
    try {
        $exists = Test-Path -LiteralPath (Join-Path $InstallRoot $name) -PathType Leaf
        Record "installed_file:$name" $(if ($exists) { 'PRESENT' } else { 'MISSING' }) 'Existence check only; contents are not collected.'
    } catch { Record "installed_file:$name" 'UNKNOWN' 'Access denied; Administrator may be required.' }
}
foreach ($name in @('p4.exe', 'cargo.exe', 'rustc.exe', 'cl.exe', 'cmake.exe', 'dotnet.exe')) {
    $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    Record "tool:$name" $(if ($null -eq $command) { 'MISSING' } else { 'PRESENT' }) 'Current user PATH only; SYSTEM/build account PATH can differ.'
}
$result = [ordered]@{ schema_version = 1; checked_at = (Get-Date).ToUniversalTime().ToString('o'); checks = @($checks.ToArray()); note = 'Read-only preflight; no Agent authentication or build execution was attempted.' }
$json = $result | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText([IO.Path]::GetFullPath($ReportPath), $json, [Text.UTF8Encoding]::new($false))
Write-Host "Report saved: $ReportPath"
Write-Host 'Send windows-host-report.json or the console results. It contains no credential contents.'
