@echo off
rem wartales-tips uninstaller: restores hlboot.dat / sdlboot.dat from the *.orig backups.
setlocal
set "HERE=%~dp0"
pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%HERE%install.ps1" -Uninstall
if errorlevel 1 (
  powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%HERE%install.ps1" -Uninstall
)
pause
