@echo off
rem Removes the wartales-mp shims and puts the original files back.
rem
rem     uninstall.bat [path to Wartales folder]
rem
rem Safe to run twice and safe to run when nothing is installed. The saved
rem originals are only ever moved back, never deleted.

setlocal enableextensions

set "SRC=%~dp0"
if "%SRC:~-1%"=="\" set "SRC=%SRC:~0,-1%"

set "DST=%~1"
if "%DST%"=="" set "DST=%SRC%"
if "%DST:~-1%"=="\" set "DST=%DST:~0,-1%"

if not exist "%DST%\Wartales.exe" (
    echo ERROR: %DST% does not look like the Wartales folder ^(no Wartales.exe^).
    echo Usage: uninstall.bat [path to Wartales folder]
    exit /b 1
)

call :restore libhl.dll libhl_o.dll || exit /b 1
call :restore ssl.hdll  ssl_o.hdll  || exit /b 1

call :drop wartales-mp.exe
call :drop libhl.mp.dll
call :drop ssl.mp.hdll

echo Done.
exit /b 0

rem restore <live name> <saved name>: move the saved original back over the shim.
:restore
if not exist "%DST%\%~2" (
    echo %~2 not present, nothing to restore for %~1.
    exit /b 0
)
if exist "%DST%\%~1" (
    del /f /q "%DST%\%~1" >nul 2>&1
    if exist "%DST%\%~1" (
        echo ERROR: could not delete %~1. Is the game running?
        exit /b 1
    )
)
move /y "%DST%\%~2" "%DST%\%~1" >nul
if errorlevel 1 (
    echo ERROR: could not rename %~2 back to %~1.
    exit /b 1
)
echo Restored %~1.
exit /b 0

rem drop <name>: remove one of our own files, complaining but not failing.
:drop
if not exist "%DST%\%~1" exit /b 0
del /f /q "%DST%\%~1" >nul 2>&1
if exist "%DST%\%~1" (
    echo WARNING: could not delete %~1. Is it still running?
) else (
    echo Removed %~1.
)
exit /b 0
