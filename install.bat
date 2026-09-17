@echo off
rem Installs the wartales-mp shims into the Wartales folder.
rem
rem Drop this whole folder's contents into the Wartales directory and run
rem install.bat, or run it from anywhere with the game directory as the first
rem argument:
rem
rem     install.bat "D:\Steam\steamapps\common\Wartales"
rem
rem The shims ship under distinct names (libhl.mp.dll, ssl.mp.hdll) precisely so
rem that copying them next to the game can never overwrite an original: only
rem this script puts them in place, and only after the original has been renamed
rem out of the way.
rem
rem No administrator rights, no hosts file, no certificate store, no Steam
rem launch options: the game loads the shims by name from its own folder.
rem
rem Running it twice is safe, and it refuses rather than saving a shim over an
rem original.

setlocal enableextensions

set "SRC=%~dp0"
if "%SRC:~-1%"=="\" set "SRC=%SRC:~0,-1%"

set "DST=%~1"
if "%DST%"=="" set "DST=%SRC%"
if "%DST:~-1%"=="\" set "DST=%DST:~0,-1%"

if not exist "%DST%\Wartales.exe" (
    echo ERROR: %DST% does not look like the Wartales folder ^(no Wartales.exe^).
    echo Usage: install.bat [path to Wartales folder]
    exit /b 1
)

for %%F in (libhl.mp.dll ssl.mp.hdll wartales-mp.exe) do (
    if not exist "%SRC%\%%F" (
        echo ERROR: %%F is missing next to install.bat. Run shim\build.ps1 first.
        exit /b 1
    )
)

call :backup libhl.mp.dll libhl.dll libhl_o.dll || exit /b 1
call :backup ssl.mp.hdll  ssl.hdll  ssl_o.hdll  || exit /b 1

copy /y "%SRC%\libhl.mp.dll" "%DST%\libhl.dll" >nul || goto :copyfail
copy /y "%SRC%\ssl.mp.hdll"  "%DST%\ssl.hdll"  >nul || goto :copyfail

rem When install.bat runs from inside the game folder the helper is already
rem where it belongs and copy would refuse to copy a file onto itself.
if /i not "%SRC%"=="%DST%" (
    copy /y "%SRC%\wartales-mp.exe" "%DST%\wartales-mp.exe" >nul || goto :copyfail
)

echo Installed. Launch the game from Steam as usual.
exit /b 0

:copyfail
echo ERROR: could not copy the files. Is the game running?
exit /b 1

rem backup <shim> <live name> <saved name>
rem
rem Moves the pristine original aside. The file already at <live name> is saved
rem only when it is provably not our shim, so a second run cannot bury the
rem original, and a Steam update that restored a fresh original is picked up as
rem the new original instead of being overwritten by a stale saved copy.
:backup
if not exist "%DST%\%~2" (
    if exist "%DST%\%~3" (
        echo %~2 missing, original already saved as %~3.
        exit /b 0
    )
    echo ERROR: neither %~2 nor %~3 found in %DST%.
    exit /b 1
)
fc /b "%SRC%\%~1" "%DST%\%~2" >nul 2>&1
if not errorlevel 1 (
    if exist "%DST%\%~3" (
        echo %~2 is already the shim, original kept as %~3.
        exit /b 0
    )
    echo ERROR: %~2 is the shim but %~3 is missing - the original is gone.
    echo Verify the game files in Steam, then run install.bat again.
    exit /b 1
)
move /y "%DST%\%~2" "%DST%\%~3" >nul
if errorlevel 1 (
    echo ERROR: could not rename %~2 to %~3. Is the game running?
    exit /b 1
)
echo Saved the original %~2 as %~3.
exit /b 0
