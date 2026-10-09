#Requires -Version 5.1
#Requires -RunAsAdministrator
[CmdletBinding()]
param([string]$InstallRoot = 'C:\P4Forge')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
if ($InstallRoot.StartsWith('\\') -or $InstallRoot -match '["\r\n]' -or -not (Test-Path -LiteralPath "$InstallRoot\state" -PathType Container)) { throw 'Invalid existing installation directory.' }
$state = Join-Path $InstallRoot 'state\self-patch'
if ((Test-Path -LiteralPath "$state\pending.json") -or (Test-Path -LiteralPath "$state\journal.json")) { throw 'Finish the pending self patch first.' }
$expected = $null
foreach ($line in [IO.File]::ReadAllLines((Join-Path $PSScriptRoot 'SHA256SUMS'))) {
    if ($line -match '^([0-9a-fA-F]{64})  p4forge-agent.exe$') { $expected = $Matches[1] }
}
if (-not $expected -or (Get-FileHash -Algorithm SHA256 -LiteralPath "$PSScriptRoot\p4forge-agent.exe").Hash -ine $expected) { throw 'Updater package checksum mismatch.' }
$existing = Get-ScheduledTask -TaskName 'P4Forge-Updater' -ErrorAction SilentlyContinue
if ($existing) {
    if (@($existing.Actions).Count -ne 1 -or $existing.Actions[0].Execute -ine "$InstallRoot\p4forge-updater.exe") { throw 'Updater belongs to a different installation.' }
    Stop-ScheduledTask -TaskName 'P4Forge-Updater'
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ((Get-ScheduledTask -TaskName 'P4Forge-Updater').State -eq 'Running') {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Updater did not stop.' }
        Start-Sleep -Milliseconds 250
    }
}
New-Item -ItemType Directory -Path $state -Force | Out-Null
& "$env:SystemRoot\System32\icacls.exe" $state /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot protect update state.' }
Copy-Item -LiteralPath "$PSScriptRoot\p4forge-agent.exe" -Destination "$InstallRoot\p4forge-updater.exe" -Force
$action = New-ScheduledTaskAction -Execute "$InstallRoot\p4forge-updater.exe" -Argument '--patch-worker' -WorkingDirectory $InstallRoot
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable
Register-ScheduledTask -TaskName 'P4Forge-Updater' -Action $action -Principal $principal -Settings $settings -Trigger (New-ScheduledTaskTrigger -AtStartup) -Description 'P4Forge native Agent self-patch and rollback worker' -Force | Out-Null
Start-ScheduledTask -TaskName 'P4Forge-Updater'
Write-Host 'Self-patch worker installed.'
