@echo off
rem Double-click to install tailscale-force-connect (asks for administrator rights).
rem Extra arguments are passed through, e.g.  install.cmd -NoTest
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
echo.
pause
