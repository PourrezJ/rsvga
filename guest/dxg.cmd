@echo off
rem Enables the graphics kernel's detailed event channels (run as
rem administrator; they take effect at the next boot) and reports them.
cd /d %~dp0
set out=%TEMP%\dxg.txt
wevtutil el > "%out%.all" 2>&1
findstr /i "dxg graphics display" "%out%.all" > "%out%" 2>&1
for %%c in (Microsoft-Windows-DxgKrnl-Operational Microsoft-Windows-DxgKrnl-Admin) do (
  wevtutil sl %%c /e:true >> "%out%" 2>&1
  wevtutil gl %%c >> "%out%" 2>&1
)
rem The detailed debug channel: read by events.cmd, which stops it first.
wevtutil sl Microsoft-Windows-DxgKrnl/Diagnostic /e:true /q:true >> "%out%" 2>&1
wevtutil gl Microsoft-Windows-DxgKrnl/Diagnostic >> "%out%" 2>&1
hostcat.exe "%out%"
