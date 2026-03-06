@echo off
set CONFIG=%USERPROFILE%\.config\FindAnything\client.toml
echo === find-anything: initial scan ===
echo This will index all configured directories. Please wait...
echo.
"%~dp0find-scan.exe" --config "%CONFIG%"
echo.
echo === Starting find-watch service ===
sc start FindAnythingWatcher
echo.
echo === Starting system tray icon ===
start "" "%~dp0find-tray.exe" --config "%CONFIG%"
echo.
echo Done. find-watch is now running in the background.
pause
