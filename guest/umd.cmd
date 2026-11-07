@echo off
rem Asks the Direct3D 9 user-mode driver what the runtime would get (umdcaps).
cd /d %~dp0
umdcaps.exe
