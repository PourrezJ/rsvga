@echo off
rem Watches the Direct3D 9 runtime talk to the display driver (d3d9trace).
cd /d %~dp0
d3d9trace.exe
