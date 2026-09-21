@echo off
rem wartales-tips installer: patches the game's own hlboot.dat / sdlboot.dat in place, keeping *.orig backups.
setlocal
set "HERE=%~dp0"
pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%HERE%install.ps1" %*
if errorlevel 1 (
  powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%HERE%install.ps1" %*
)
pause
