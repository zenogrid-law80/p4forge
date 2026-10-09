@echo off
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0Setup.ps1"
echo.
echo See installation-result.txt, perforce-install-check.json and windows-host-report.json in this folder.
pause
