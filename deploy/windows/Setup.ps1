#Requires -Version 5.1
[CmdletBinding()]
param([string]$P4Exe = '')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($root)) { throw 'Run Setup.ps1 from the extracted installation ZIP.' }
. (Join-Path $root 'Perforce-Check.ps1')
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ([string]::IsNullOrWhiteSpace($P4Exe)) {
    $p4 = Get-Command p4.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $p4) { $P4Exe = $p4.Source }
}
if (-not $admin) {
    $arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $PSCommandPath
    if ($P4Exe) {
        if ($P4Exe -match '["\r\n]') { throw 'Invalid P4 executable path.' }
        $arguments += ' -P4Exe "{0}"' -f $P4Exe
    }
    $child = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    exit $child.ExitCode
}
$exitCode = 1
$statusPath = Join-Path $root 'installation-result.txt'
try {
    $config = Get-Content -LiteralPath (Join-Path $root 'enrollment.json') -Raw | ConvertFrom-Json
    if ($env:COMPUTERNAME -ine $config.computer) { throw "This installer is for $($config.computer)." }
    if ([DateTimeOffset]::UtcNow -ge [DateTimeOffset]::Parse($config.expires_at)) { throw 'Installation credentials have expired. Request a refreshed installation ZIP.' }
    if (-not $P4Exe -or -not (Test-Path -LiteralPath $P4Exe -PathType Leaf)) { throw 'p4.exe was not found. Run Setup.ps1 -P4Exe C:\path\to\p4.exe.' }
    $token = Join-Path $root 'agent-token'
    $tickets = Join-Path $root 'p4tickets'
    foreach ($path in @($token, $tickets)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'Use the administrator-only installation ZIP, which includes enrollment credentials.' }
    }
    # Verify both connections before creating the build account or scheduled task.
    $previousTls = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
        $ready = Invoke-WebRequest -UseBasicParsing -Uri ($config.server.TrimEnd('/') + '/readyz') -TimeoutSec 10
        if ($ready.StatusCode -ne 200) { throw 'P4Forge readiness check failed.' }
    } finally { [Net.ServicePointManager]::SecurityProtocol = $previousTls }
    $version = Invoke-P4Check -Executable $P4Exe -Tickets $tickets -Operation version
    $charset = 'utf8'
    if ($config.PSObject.Properties.Name -contains 'p4charset') { $charset = $config.p4charset }
    $login = Invoke-P4Check -Executable $P4Exe -Tickets $tickets -Port $config.p4port -User $config.p4user -Charset $charset
    $ticketEntries = @([IO.File]::ReadAllLines($tickets) | ForEach-Object {
        if ($_ -match '^([^=]+)=([^:]+):[0-9a-fA-F]{32}$') { [pscustomobject]@{server=$Matches[1]; user=$Matches[2]} }
    })
    $p4Report = [ordered]@{
        checked_at = (Get-Date).ToUniversalTime().ToString('o')
        executable = $P4Exe
        charset = $charset
        ignored_environment_names = @(Get-ChildItem Env:P4* | Select-Object -ExpandProperty Name)
        ticket_entries = $ticketEntries
        version = $version
        login = $login
    }
    [IO.File]::WriteAllText((Join-Path $root 'perforce-install-check.json'), ($p4Report | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    if ($login.exit_code -ne 0) { throw "Perforce ticket check failed (exit $($login.exit_code)): $($login.stdout) $($login.stderr)" }
    & (Join-Path $root 'Install.ps1') -AgentTokenFile $token -P4TicketsFile $tickets -P4Exe $P4Exe -Server $config.server -Start
    Start-Sleep -Seconds 4
    $task = Get-ScheduledTask -TaskName 'P4Forge-Agent'
    if ($task.State -ne 'Running') {
        $taskInfo = Get-ScheduledTaskInfo -TaskName 'P4Forge-Agent'
        throw "Installation completed, but the task is $($task.State) (result $($taskInfo.LastTaskResult)). Inspect C:\P4Forge\state\agent.log."
    }
    'Installation completed; P4Forge-Agent is running. Server connection and build validation are still required.' | Set-Content -LiteralPath $statusPath -Encoding UTF8
    Write-Host 'Installation completed. P4Forge-Agent is running.'
    Write-Host 'Delete the downloaded installation ZIP and its extracted credential files after server validation.'
    $exitCode = 0
} catch {
    $message = Protect-P4Diagnostic $_.Exception.Message
    [IO.File]::WriteAllText($statusPath, "Installation failed: $message", [Text.UTF8Encoding]::new($false))
    Write-Host "Installation failed: $message" -ForegroundColor Red
} finally {
    try {
        & (Join-Path $root 'Test-Host.ps1') -ReportPath (Join-Path $root 'windows-host-report.json')
    } catch { Write-Host "Report collection failed: $($_.Exception.Message)" }
}
exit $exitCode
