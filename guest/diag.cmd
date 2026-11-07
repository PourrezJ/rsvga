@echo off
rem Runs dxdiag and sends its text report to the host (hostcat).
rem dxdiag.exe by its full path: a script of that name would run itself.
cd /d %~dp0
start "" /wait "%SystemRoot%\System32\dxdiag.exe" /whql:off /t "%TEMP%\dxdiag.txt"
hostcat.exe "%TEMP%\dxdiag.txt"
