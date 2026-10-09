#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$source = Join-Path (Split-Path -Parent $PSScriptRoot) 'deploy/windows/Test-Host.ps1'
$text = [IO.File]::ReadAllText($source)
$marker = $text.IndexOf('# Read-only host checks.')
if ($marker -lt 0) { throw 'Cannot locate diagnostic bootstrap.' }
# Exercise the real parameter binding/path bootstrap without Windows-only host APIs.
$bootstrap = $text.Substring(0, $marker) + "`nWrite-Output ([IO.Path]::GetFullPath(`$ReportPath))"
$temp = Join-Path ([IO.Path]::GetTempPath()) ('p4forge-host-test-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $temp)
try {
    $script = Join-Path $temp 'Test-Host.ps1'
    [IO.File]::WriteAllText($script, $bootstrap)
    $expected = Join-Path $temp 'windows-host-report.json'
    if ((& $script) -ne $expected) { throw 'Default report must be beside the script.' }
    $custom = Join-Path $temp 'custom report.json'
    if ((& $script -ReportPath $custom) -ne $custom) { throw 'Explicit report path was not preserved.' }
    if ((& $script -ReportPath '') -ne $expected) { throw 'Empty report path must use the default.' }
    # Scriptblock invocation has no PSScriptRoot/PSCommandPath, reproducing the
    # empty automatic variables that caused the original parameter-binding failure.
    $inline = [scriptblock]::Create($bootstrap)
    $fallback = Join-Path (Get-Location).Path 'windows-host-report.json'
    if ((& $inline) -ne $fallback) { throw 'Missing script context must use the current directory.' }
    if ((& $inline -ReportPath $custom) -ne $custom) { throw 'Explicit inline report path was not preserved.' }
    Write-Output 'PASS: default, explicit, empty and missing-script-context report paths.'
} finally { Remove-Item -LiteralPath $temp -Recurse -Force }
