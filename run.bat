@echo off
rem Builds and runs the app (release). Pass "ink" or "wintab" to override the pen setting.
cd /d "%~dp0"
cargo run --release -p mmc-app -- %*
if errorlevel 1 pause
