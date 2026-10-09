@echo off
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0Update.ps1"
echo.
echo See update-result.txt and windows-host-report.json in this folder.
pause
