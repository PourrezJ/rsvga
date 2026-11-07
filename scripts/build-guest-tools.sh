#!/usr/bin/env bash
# Builds the Windows test programs of guest/ with mingw-w64 into
# target/guest-tools/, a folder run-windows.sh can give the VM as a disk
# (--guest-tools).
#
#   sudo apt install gcc-mingw-w64-x86-64
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/target/guest-tools
mkdir -p "$out"

cc() { x86_64-w64-mingw32-gcc -O2 -Wall -Wextra "$@"; }
cc -o "$out/d3dprobe.exe" "$root/guest/d3dprobe/d3dprobe.c" -ld3d11 -ldxgi -ld3d9 -ldxguid -luuid \
  -lpsapi -lshlwapi
cc -o "$out/d3dtest.exe" "$root/guest/d3dtest/d3dtest.c" -ld3d11 -ldxgi -ldxguid -luuid
cc -o "$out/d3d9test.exe" "$root/guest/d3d9test/d3d9test.c" -ld3d9 -lm
cc -o "$out/hostcat.exe" "$root/guest/hostcat/hostcat.c"
cc -o "$out/wininfo.exe" "$root/guest/wininfo/wininfo.c"
cc -o "$out/umdcaps.exe" "$root/guest/umdcaps/umdcaps.c"
cc -o "$out/d3d9trace.exe" "$root/guest/d3d9trace/d3d9trace.c" -ld3d9
cc -o "$out/modes.exe" "$root/guest/modes/modes.c"
cc -o "$out/videoprobe.exe" "$root/guest/videoprobe/videoprobe.c" -ld3d11 -ldxguid -luuid
# Unigine Heaven's installer, if the user downloaded it (guest/heaven.cmd).
for f in "$root"/target/unigine/download/*.exe; do
  [[ -f $f && ! -f $out/$(basename "$f") ]] && cp "$f" "$out/"
done
# CRLF line ends for cmd.exe.
for script in run.cmd diag.cmd info.cmd events.cmd probe.cmd nine.cmd umd.cmd dxg.cmd trace.cmd retrace.cmd ninetest.cmd heaven.cmd heavenrun.cmd modes.cmd reboot.cmd videoprobe.cmd; do
  sed 's/$/\r/' "$root/guest/$script" > "$out/$script"
done
echo "built $out: d3dprobe.exe, d3dtest.exe, d3d9test.exe, hostcat.exe, wininfo.exe, umdcaps.exe, d3d9trace.exe, modes.exe, videoprobe.exe and their .cmd scripts"
echo "(in the guest, from the tools disk: D:\\run.cmd runs d3dtest, D:\\ninetest.cmd d3d9test, D:\\diag.cmd sends dxdiag's report,"
echo " D:\\info.cmd the visible windows, D:\\events.cmd the latest errors, D:\\probe.cmd runs d3dprobe)"
