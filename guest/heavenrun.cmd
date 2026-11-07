@echo off
rem Runs Unigine Heaven (installed by heaven.cmd) with its Direct3D 9
rem renderer, windowed, for two minutes, then sends its log to the host.
set tools=%~dp0
cd /d C:\Heaven\bin
start "" Heaven.exe -project_name Heaven -data_path ../ -engine_config ../data/heaven_4.0.cfg -system_script heaven/unigine.cpp -sound_app null -video_app direct3d9 -video_multisample 0 -video_fullscreen 0 -video_mode -1 -video_width 1280 -video_height 720 -extern_define RELEASE,LANGUAGE_EN,QUALITY_MEDIUM,TESSELLATION_DISABLED
timeout /t 120 /nobreak > nul
taskkill /im Heaven.exe > nul 2>&1
timeout /t 3 /nobreak > nul
set log=%USERPROFILE%\Heaven\log.html
if not exist "%log%" (
  dir /s /b "%USERPROFILE%\*log*.html" > "%TEMP%\heavenlog.txt" 2>&1
  set log=%TEMP%\heavenlog.txt
)
"%tools%hostcat.exe" "%log%"
