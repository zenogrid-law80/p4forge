#Requires -Version 5.1
#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$AgentTokenFile,
    [Parameter(Mandatory)][string]$P4TicketsFile,
    [Parameter(Mandatory)][string]$P4Exe,
    [string]$Server = 'https://p4forge.zenogrid.co.kr:8444/',
    [string]$InstallRoot = 'C:\P4Forge',
    [switch]$Start
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [Environment]::Is64BitProcess -or -not [Environment]::Is64BitOperatingSystem) { throw 'Use 64-bit PowerShell on Windows x64.' }
if ([Environment]::OSVersion.Version.Build -lt 14393) { throw 'Windows 10 / Server 2016 or newer is required.' }
if (([uri]$Server).Scheme -ne 'https' -or ([uri]$Server).AbsolutePath -ne '/' -or ([uri]$Server).Query -or ([uri]$Server).Fragment -or ([uri]$Server).UserInfo) { throw 'Server must be an HTTPS origin.' }
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
if ($InstallRoot.StartsWith('\\') -or $InstallRoot -match '["\r\n]' -or (Test-Path -LiteralPath $InstallRoot)) { throw 'Use a new local installation directory.' }
$account = 'p4forge-build'
if (Get-LocalUser -Name $account -ErrorAction SilentlyContinue) { throw 'p4forge-build already exists. Inspect the existing installation; this installer never replaces an account.' }
if (Get-ScheduledTask -TaskName 'P4Forge-Agent' -ErrorAction SilentlyContinue) { throw 'P4Forge-Agent task already exists.' }
foreach ($file in @($AgentTokenFile, $P4TicketsFile, $P4Exe, "$PSScriptRoot\p4forge-agent.exe", "$PSScriptRoot\SHA256SUMS")) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Required file is missing: $file" }
}
foreach ($line in Get-Content -LiteralPath "$PSScriptRoot\SHA256SUMS") {
    if ($line -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9._-]+)$') { throw 'Invalid package checksum manifest.' }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $PSScriptRoot $Matches[2])).Hash.ToLowerInvariant() -ne $Matches[1]) { throw "Package checksum mismatch: $($Matches[2])" }
}
if (([IO.File]::ReadAllText((Resolve-Path -LiteralPath $AgentTokenFile))).Trim() -notmatch '^[A-Za-z0-9_-]{43}$') { throw 'Invalid agent token file.' }
$P4Exe = (Resolve-Path -LiteralPath $P4Exe).Path
& $P4Exe -V | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'P4 executable did not pass its version check.' }

function Protect-Directory([string]$Path, [switch]$BuildRead) {
    New-Item -ItemType Directory -Path $Path -Force | Out-Null
    & "$env:SystemRoot\System32\icacls.exe" $Path /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Cannot protect directory: $Path" }
    if ($BuildRead) {
        & "$env:SystemRoot\System32\icacls.exe" $Path /grant:r "${account}:(OI)(CI)RX" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Cannot set build read access: $Path" }
    }
}
$bytes = New-Object byte[] 48
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
$random.GetBytes($bytes); $random.Dispose()
$password = 'P4forge!' + [Convert]::ToBase64String($bytes)
$secure = ConvertTo-SecureString $password -AsPlainText -Force
New-LocalUser -Name $account -Password $secure -Description 'P4Forge dedicated build process account' -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
# The account is intentionally never added to Administrators.
$users = Get-LocalGroup -SID 'S-1-5-32-545'
$buildUser = Get-LocalUser -Name $account
if (-not (Get-LocalGroupMember -Group $users | Where-Object { $_.SID -eq $buildUser.SID })) {
    Add-LocalGroupMember -Group $users -Member $buildUser
}
Protect-Directory $InstallRoot -BuildRead
Protect-Directory "$InstallRoot\secrets"
Protect-Directory "$InstallRoot\state"
Protect-Directory "$InstallRoot\work" -BuildRead
[IO.File]::WriteAllText("$InstallRoot\secrets\build-account.json", (@{username=$account; password=$password} | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
$password = $null; $secure = $null
Copy-Item -LiteralPath "$PSScriptRoot\p4forge-agent.exe" -Destination "$InstallRoot\p4forge-agent.exe"
Copy-Item -LiteralPath $P4Exe -Destination "$InstallRoot\p4.exe"
# Read/write the bytes into new files so the protected destination ACL is inherited.
[IO.File]::WriteAllText("$InstallRoot\secrets\agent-token", ([IO.File]::ReadAllText((Resolve-Path -LiteralPath $AgentTokenFile))).Trim(), [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllBytes("$InstallRoot\secrets\p4tickets", [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $P4TicketsFile)))
Copy-Item -LiteralPath "$PSScriptRoot\Test-Installation.ps1" -Destination $InstallRoot
Copy-Item -LiteralPath "$PSScriptRoot\Perforce-Check.ps1" -Destination $InstallRoot
$arguments = "--server `"$Server`" --token-file `"$InstallRoot\secrets\agent-token`" --tickets-file `"$InstallRoot\secrets\p4tickets`" --build-account-file `"$InstallRoot\secrets\build-account.json`" --p4-bin `"$InstallRoot\p4.exe`" --work-dir `"$InstallRoot\work`" --state-dir `"$InstallRoot\state`" --log-file `"$InstallRoot\state\agent.log`""
$action = New-ScheduledTaskAction -Execute "$InstallRoot\p4forge-agent.exe" -Argument $arguments -WorkingDirectory $InstallRoot
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable
$trigger = New-ScheduledTaskTrigger -AtStartup
Register-ScheduledTask -TaskName 'P4Forge-Agent' -Action $action -Principal $principal -Settings $settings -Trigger $trigger -Description 'Self-hosted P4Forge Windows build agent' | Out-Null
& (Join-Path $PSScriptRoot 'Install-SelfPatch.ps1') -InstallRoot $InstallRoot
@{server=$Server; build_account=$account; installed_at=(Get-Date).ToUniversalTime().ToString('o'); runtime_validation='pending'} | ConvertTo-Json | Set-Content -LiteralPath "$InstallRoot\installation.json" -Encoding UTF8
if ($Start) { Start-ScheduledTask -TaskName 'P4Forge-Agent' }
Write-Host "Installed at $InstallRoot. Run Test-Installation.ps1, then Start-ScheduledTask -TaskName P4Forge-Agent."
Write-Host 'Register the agent as windows / x86_64 in P4Forge. Compiler and SDK paths must be installed for this build account or on the machine PATH.'
