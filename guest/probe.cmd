@echo off
rem Runs d3dprobe (what Direct3D sees of the adapter); results go to the host.
cd /d %~dp0
d3dprobe.exe
