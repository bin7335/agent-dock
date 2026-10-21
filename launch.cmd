@echo off
cd /d "%~dp0"
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
npx tauri dev
