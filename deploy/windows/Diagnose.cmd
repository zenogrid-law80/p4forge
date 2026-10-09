@echo off
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0Test-Host.ps1" -ReportPath "%~dp0windows-host-report.json"
echo.
pause
