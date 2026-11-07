@echo off
rem Direct3D 9 diagnosis: with the VMware debug drivers (guestinfo
rem svga.wddm.buildType=debug), their user-mode log goes to C:\vm3dum_log.
rem Runs d3dprobe, then sends that log to the host (hostcat).
cd /d %~dp0
if not exist C:\vm3dum_log mkdir C:\vm3dum_log
d3dprobe.exe
type C:\vm3dum_log\*.log > "%TEMP%\umdlog.txt" 2>nul
hostcat.exe "%TEMP%\umdlog.txt"
