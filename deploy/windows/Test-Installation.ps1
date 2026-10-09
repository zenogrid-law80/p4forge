#Requires -Version 5.1
#Requires -RunAsAdministrator
[CmdletBinding()]
param([string]$InstallRoot = 'C:\P4Forge')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Perforce-Check.ps1')
$config = Get-Content -LiteralPath "$InstallRoot\installation.json" -Raw | ConvertFrom-Json
$result = Invoke-WebRequest -UseBasicParsing -Uri ($config.server.TrimEnd('/') + '/readyz')
if ($result.StatusCode -ne 200) { throw 'Server readiness check failed.' }
& "$InstallRoot\p4forge-agent.exe" --version
$login = Invoke-P4Check -Executable "$InstallRoot\p4.exe" -Tickets "$InstallRoot\secrets\p4tickets"
Write-Host $login.stdout
if ($login.exit_code -ne 0) { throw "Perforce check failed: $($login.stdout) $($login.stderr)" }
Get-ScheduledTask -TaskName 'P4Forge-Agent' | Select-Object TaskName, State
Get-ScheduledTaskInfo -TaskName 'P4Forge-Agent' | Select-Object LastRunTime, LastTaskResult
if (Test-Path -LiteralPath "$InstallRoot\state\agent.log") { Get-Content -LiteralPath "$InstallRoot\state\agent.log" -Tail 20 }
Write-Host 'Next: run the included Windows smoke pipeline with CL 6. A connectivity check is not a successful build.'
