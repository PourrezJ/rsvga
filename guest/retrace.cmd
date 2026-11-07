@echo off
rem d3d9trace, handing the runtime A8R8G8B8 for the display modes the kernel
rem lists in A8B8G8R8 (an experiment on how Direct3D 9 reads the mode list).
cd /d %~dp0
d3d9trace.exe rewrite
