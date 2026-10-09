#Requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($root)) { throw 'Run this script from the extracted private enrollment ZIP.' }
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $PSCommandPath
    $child = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    exit $child.ExitCode
}
try {
    $config = Get-Content -LiteralPath (Join-Path $root 'enrollment.json') -Raw | ConvertFrom-Json
    if ($env:COMPUTERNAME -ine $config.computer) { throw "This package is for $($config.computer)." }
    if ([DateTimeOffset]::UtcNow -ge [DateTimeOffset]::Parse($config.expires_at)) { throw 'This ticket has expired. Download a refreshed package.' }
    $installed = 'C:\P4Forge'
    foreach ($part in @($installed, "$installed\secrets", "$installed\secrets\p4tickets")) {
        $item = Get-Item -LiteralPath $part -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Refusing a reparse point in the installed credential path.' }
    }
    if (Test-Path -LiteralPath "$installed\state\inflight.json") { throw 'An attempt is active. Wait for it to finish before renewing the ticket.' }
    . (Join-Path $root 'Perforce-Check.ps1')
    $ticket = Join-Path $root 'p4tickets'
    $check = Invoke-P4Check -Executable "$installed\p4.exe" -Tickets $ticket -Port $config.p4port -User $config.p4user -Charset $config.p4charset
    if ($check.exit_code -ne 0) { throw 'The new Perforce ticket did not pass validation.' }
    # Keep the installed file ACL (SYSTEM and Administrators only).
    [IO.File]::WriteAllBytes("$installed\secrets\p4tickets", [IO.File]::ReadAllBytes($ticket))
    $check = Invoke-P4Check -Executable "$installed\p4.exe" -Tickets "$installed\secrets\p4tickets" -Port $config.p4port -User $config.p4user -Charset $config.p4charset
    if ($check.exit_code -ne 0) { throw 'The installed ticket failed validation.' }
    Write-Host 'Perforce read ticket renewed. Agent token, services and build account are unchanged.'
    Write-Host 'Delete the downloaded ZIP and extracted credential files after validation.'
    'Perforce read ticket renewed successfully.' | Set-Content -LiteralPath (Join-Path $root 'ticket-renewal-result.txt') -Encoding UTF8
} catch {
    Write-Host 'Ticket renewal failed:' $_.Exception.Message -ForegroundColor Red
    exit 1
}
