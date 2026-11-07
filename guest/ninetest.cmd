@echo off
rem Runs the rsvga Direct3D 9 checks from this disk (scripts/build-guest-tools.sh).
rem Results reach the host through the VMware backdoor (run-windows.sh --diagnose).
cd /d %~dp0
d3d9test.exe 8
