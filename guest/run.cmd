@echo off
rem Runs the rsvga Direct3D checks from this disk (scripts/build-guest-tools.sh).
rem Results reach the host through the VMware backdoor (run-windows.sh --diagnose).
cd /d %~dp0
d3dtest.exe 8
