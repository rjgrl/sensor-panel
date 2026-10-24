@echo off
setlocal
cd /d "%~dp0"
echo ========================================
echo  DS916 Tray - Build Script (Rust)
echo ========================================
echo.

:: 1. Toolchain check
echo [1/5] Checking Rust toolchain...
where cargo >nul 2>&1
if errorlevel 1 (
  echo ERROR: cargo was not found.
  echo   Install Rust from https://rustup.rs ^(default options^), plus the
  echo   "Desktop development with C++" workload from Visual Studio Build Tools,
  echo   then open a NEW terminal and run build.bat again.
  pause
  exit /b 1
)
cargo --version
echo.

set "INSTALL_DIR=%APPDATA%\DS916Tray"

:: 2. Close any running instance so the exe isn't locked when we overwrite it
echo [2/5] Closing any running DS916Tray instance...
:: Preserve the boot task, but stop its Session 0 worker before replacing the exe.
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0enable-boot-start.ps1" -Action Stop
if errorlevel 1 (
  echo ERROR: Could not stop the DS916 boot task. Try an administrator terminal.
  pause
  exit /b 1
)
tasklist /FI "IMAGENAME eq DS916Tray.exe" 2>nul | find /I "DS916Tray.exe" >nul
if not errorlevel 1 (
  taskkill /IM DS916Tray.exe /F >nul 2>&1
  timeout /t 1 /nobreak >nul
  echo   Closed running instance.
) else (
  echo   Not currently running.
)
echo.

:: 3. Build
echo [3/5] Building DS916Tray.exe (release)...
cargo build --release
if errorlevel 1 (echo ERROR: cargo build failed && pause && exit /b 1)
echo Done.
echo.

:: 4. Install into AppData -- exe, theme builder, config, themes and discovered
:: sensors all live together in one place
echo [4/5] Installing to %%APPDATA%%\DS916Tray...
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"
copy /Y "target\release\DS916Tray.exe" "%INSTALL_DIR%\DS916Tray.exe" >nul
if errorlevel 1 (echo ERROR: could not copy DS916Tray.exe && pause && exit /b 1)
copy /Y theme_builder.html "%INSTALL_DIR%\theme_builder.html" >nul
copy /Y enable-boot-start.ps1 "%INSTALL_DIR%\enable-boot-start.ps1" >nul
if errorlevel 1 (echo ERROR: could not copy the boot setup script && pause && exit /b 1)
echo Done.
echo.

:: 5. Desktop and Start Menu shortcuts (only created once)
echo [5/5] Checking shortcuts...
set "DESKTOP_LNK=%USERPROFILE%\Desktop\DS916 Tray.lnk"
if exist "%DESKTOP_LNK%" (
  echo   Shortcuts already exist, skipping.
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -Command ^
    "$ws = New-Object -ComObject WScript.Shell;" ^
    "$desktop = $ws.CreateShortcut([Environment]::GetFolderPath('Desktop') + '\DS916 Tray.lnk');" ^
    "$desktop.TargetPath = '%INSTALL_DIR%\DS916Tray.exe';" ^
    "$desktop.WorkingDirectory = '%INSTALL_DIR%';" ^
    "$desktop.IconLocation = '%INSTALL_DIR%\DS916Tray.exe';" ^
    "$desktop.Save();" ^
    "$startMenuDir = [Environment]::GetFolderPath('StartMenu') + '\Programs';" ^
    "$startMenu = $ws.CreateShortcut($startMenuDir + '\DS916 Tray.lnk');" ^
    "$startMenu.TargetPath = '%INSTALL_DIR%\DS916Tray.exe';" ^
    "$startMenu.WorkingDirectory = '%INSTALL_DIR%';" ^
    "$startMenu.IconLocation = '%INSTALL_DIR%\DS916Tray.exe';" ^
    "$startMenu.Save();"
  if errorlevel 1 (
    echo   WARNING: Shortcut creation failed - you can still run DS916Tray.exe directly from:
    echo     %INSTALL_DIR%
  ) else (
    echo   Created.
  )
)
echo.

echo ========================================
echo  Build complete!
echo ========================================
echo.
echo  Installed to: %INSTALL_DIR%
echo    - DS916Tray.exe
echo    - theme_builder.html
echo.
echo  The app registers itself to start with Windows the first time it runs
echo  (Settings - "Start display automatically with Windows").
echo  To also display the saved theme before sign-in, run this once in an
echo  administrator PowerShell using your normal account:
echo    powershell -ExecutionPolicy Bypass -File "%INSTALL_DIR%\enable-boot-start.ps1"
echo.

set /p RELAUNCH="Launch DS916 Tray now? (Y/N): "
if /I "%RELAUNCH%"=="Y" (
  start "" "%INSTALL_DIR%\DS916Tray.exe"
  echo Launched.
) else (
  echo You can launch it later from the Desktop or Start Menu shortcut.
)
echo.
pause
