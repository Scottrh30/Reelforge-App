@echo off
rem Downloads the FFmpeg 8.1 shared build (BtbN, GPL) and copies its DLLs next to reelforge.exe.
setlocal
cd /d "%~dp0"
set URL=https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip
echo Downloading FFmpeg 8.1 (about 90 MB)...
powershell -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; $z=Join-Path $env:TEMP 'reelforge-ffmpeg.zip'; $d=Join-Path $env:TEMP 'reelforge-ffmpeg'; Invoke-WebRequest -Uri '%URL%' -OutFile $z -UseBasicParsing; if (Test-Path $d) { Remove-Item -Recurse -Force $d }; Expand-Archive -Path $z -DestinationPath $d -Force; Get-ChildItem -Path $d -Recurse -Filter *.dll | Where-Object { $_.Directory.Name -eq 'bin' } | Copy-Item -Destination '%~dp0' -Force; Remove-Item -Force $z; Remove-Item -Recurse -Force $d"
if errorlevel 1 (
  echo.
  echo The download failed. Check your internet connection and try again, or download
  echo %URL%
  echo yourself and copy the .dll files from its bin folder next to reelforge.exe.
  pause
  exit /b 1
)
echo.
echo Done: FFmpeg's DLLs are next to reelforge.exe. You can start Reelforge now.
pause
