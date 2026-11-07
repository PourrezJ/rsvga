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

x86_64-w64-mingw32-gcc -O2 -Wall -Wextra -o "$out/d3dprobe.exe" \
    "$root/guest/d3dprobe/d3dprobe.c" -ld3d11 -ldxgi -ld3d9 -ldxguid -luuid
echo "built $out/d3dprobe.exe"
