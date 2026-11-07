@echo off
rem Unigine Heaven 4.0, a free benchmark with a Direct3D 9 renderer, as a
rem real-world D3D9 test. The user downloads Unigine_Heaven-4.0.exe from
rem benchmark.unigine.com into target/unigine/download; copied to this disk,
rem it is installed into C:\Heaven the first time (silently; run elevated:
rem guest-run.sh --admin heaven). Then the files are sent to the host.
cd /d %~dp0
if not exist C:\Heaven\bin Unigine_Heaven-4.0.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR=C:\Heaven
dir /s /b C:\Heaven > "%TEMP%\heaven.txt"
hostcat.exe "%TEMP%\heaven.txt"
