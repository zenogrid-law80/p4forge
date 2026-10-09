#Requires -Version 5.1
[CmdletBinding()]
param([string]$InstallRoot = 'C:\P4Forge')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($root)) { throw 'Run Update.ps1 from the extracted ZIP.' }
. (Join-Path $root 'Perforce-Check.ps1')
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    if ($InstallRoot -match '["\r\n]') { throw 'Invalid installation path.' }
    $arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -File "{0}" -InstallRoot "{1}"' -f $PSCommandPath, $InstallRoot
    $child = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    exit $child.ExitCode
}
$exitCode = 1
$stopped = $false
$wasRunning = $false
$backup = $null
$files = @('p4forge-agent.exe', 'Perforce-Check.ps1', 'Test-Installation.ps1')
try {
    if (-not (Test-Path -LiteralPath "$InstallRoot\installation.json" -PathType Leaf)) { throw 'P4Forge is not installed. Use Install-ThisMachine.cmd first.' }
    $task = Get-ScheduledTask -TaskName 'P4Forge-Agent'
    $actions = @($task.Actions)
    if ($actions.Count -ne 1 -or $actions[0].Execute -ine "$InstallRoot\p4forge-agent.exe") { throw 'The scheduled task uses a different installation directory.' }
    if (Test-Path -LiteralPath "$InstallRoot\state\inflight.json") { throw 'A build is active or awaiting recovery. Drain this Agent in P4Forge and wait for completion before updating.' }
    if ((Test-Path -LiteralPath "$InstallRoot\state\self-patch\pending.json") -or (Test-Path -LiteralPath "$InstallRoot\state\self-patch\journal.json")) { throw 'Finish the pending self patch before a manual update.' }
    $manifest = @{}
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $root 'SHA256SUMS'))) {
        if ($line -match '^([0-9a-fA-F]{64})  ([A-Za-z0-9._-]+)$') { $manifest[$Matches[2]] = $Matches[1] }
    }
    foreach ($name in ($files + @('Install-SelfPatch.ps1'))) {
        if (-not $manifest.ContainsKey($name)) { throw "Missing package checksum for $name." }
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $root $name)).Hash -ine $manifest[$name]) { throw "Package checksum mismatch: $name." }
    }
    # The server's drain control prevents new assignments during this operation.
    $wasRunning = $task.State -eq 'Running'
    Stop-ScheduledTask -TaskName 'P4Forge-Agent'
    $stopped = $true
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ((Get-ScheduledTask -TaskName 'P4Forge-Agent').State -eq 'Running') {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Agent did not stop within 15 seconds; no files were replaced.' }
        Start-Sleep -Milliseconds 250
    }
    # A claim racing the initial check must recover with the existing executable.
    if (Test-Path -LiteralPath "$InstallRoot\state\inflight.json") { throw 'A build started during shutdown. Restart the Agent to recover before updating.' }
    $backup = Join-Path $InstallRoot ('updates\' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $backup -Force | Out-Null
    foreach ($name in $files) { Copy-Item -LiteralPath (Join-Path $InstallRoot $name) -Destination (Join-Path $backup $name) }
    foreach ($name in $files) { Copy-Item -LiteralPath (Join-Path $root $name) -Destination (Join-Path $InstallRoot $name) -Force }
    & (Join-Path $root 'Install-SelfPatch.ps1') -InstallRoot $InstallRoot
    if ($wasRunning) {
        Start-ScheduledTask -TaskName 'P4Forge-Agent'
        Start-Sleep -Seconds 4
        if ((Get-ScheduledTask -TaskName 'P4Forge-Agent').State -ne 'Running') { throw 'Updated Agent did not remain running.' }
    }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath "$InstallRoot\p4forge-agent.exe").Hash
    [IO.File]::WriteAllText((Join-Path $root 'update-result.txt'), "Update completed. Agent SHA-256: $hash`r`nPrevious executable: $backup`r`nServer/build validation and drain release are still required.", [Text.UTF8Encoding]::new($false))
    Write-Host 'Update completed. Keep the Agent drained until server validation resumes.'
    $exitCode = 0
} catch {
    $message = Protect-P4Diagnostic $_.Exception.Message
    if ($stopped) {
        try {
            Stop-ScheduledTask -TaskName 'P4Forge-Agent'
            if ($backup) {
                foreach ($name in $files) {
                    $old = Join-Path $backup $name
                    if (Test-Path -LiteralPath $old -PathType Leaf) { Copy-Item -LiteralPath $old -Destination (Join-Path $InstallRoot $name) -Force }
                }
            }
            if ($wasRunning) { Start-ScheduledTask -TaskName 'P4Forge-Agent' }
        } catch { $message += ' Rollback/restart also failed: ' + (Protect-P4Diagnostic $_.Exception.Message) }
    }
    [IO.File]::WriteAllText((Join-Path $root 'update-result.txt'), "Update failed: $message", [Text.UTF8Encoding]::new($false))
    Write-Host "Update failed: $message" -ForegroundColor Red
} finally {
    try { & (Join-Path $root 'Test-Host.ps1') -ReportPath (Join-Path $root 'windows-host-report.json') }
    catch { Write-Host ('Report collection failed: ' + (Protect-P4Diagnostic $_.Exception.Message)) }
}
exit $exitCode
