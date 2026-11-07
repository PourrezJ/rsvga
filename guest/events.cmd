@echo off
rem Sends recent errors to the host (hostcat): application crashes, the last
rem System events, the graphics kernel's own channels, the display devices.
cd /d %~dp0
set out=%TEMP%\events.txt
wevtutil qe Application /q:"*[System[Provider[@Name='Application Error']]]" /c:5 /rd:true /f:text > "%out%" 2>&1
wevtutil qe System /c:30 /rd:true /f:text >> "%out%" 2>&1
wevtutil qe Microsoft-Windows-DxgKrnl-Admin /c:20 /rd:true /f:text >> "%out%" 2>&1
wevtutil qe Microsoft-Windows-DxgKrnl-Operational /c:40 /rd:true /f:text >> "%out%" 2>&1
rem The debug channel (enabled by dxg.cmd) can only be read once stopped.
wevtutil sl Microsoft-Windows-DxgKrnl/Diagnostic /e:false /q:true >> "%out%" 2>&1
wevtutil qe Microsoft-Windows-DxgKrnl/Diagnostic /c:300 /f:text >> "%out%" 2>&1
pnputil /enum-devices /class Display >> "%out%" 2>&1
hostcat.exe "%out%"
