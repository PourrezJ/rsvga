#!/usr/bin/env bash
# Runs one of the guest tools in the test VM started by
#   run-windows.sh --diagnose --guest-tools
# and prints what it reported to the host.
#
#   scripts/guest-run.sh [--now] [--admin] NAME [SECONDS]
#
# NAME is a script of the tools disk (target/guest-tools, drive D: in the
# guest): run (d3dtest), diag (dxdiag), probe (d3dprobe), info (visible
# windows), events (latest errors), nine (d3dprobe and the VMware debug
# driver's user-mode log), umd (umdcaps: the Direct3D 9 driver's answers),
# dxg (enables the graphics kernel's event channels; with --admin), trace
# (d3d9trace: what the Direct3D 9 runtime asks the driver), retrace (the
# same, with the display mode formats rewritten), modes (resolution
# changes and back), reboot (restarts Windows: nothing to wait for),
# videoprobe (video processor blits, to decode the driver's commands).
#
# The script waits for the desktop (unless --now), opens Win+R, types
# D:\NAME.cmd through the QEMU monitor and waits up to SECONDS (default
# 180) for the tool's last line in qemu.log, where the VMware backdoor
# messages land.
#
# --admin runs it elevated (Ctrl+Shift+Enter, then Alt+Y on the UAC
# prompt), for what needs administrator rights (the graphics kernel's
# event channels).
#
# Keys are typed for the guest's AZERTY (French) layout: NAME must be
# lowercase letters.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
log=$root/target/vm/rsvga.log
qemu_log=$root/target/vm/qemu.log
hmp=$XDG_RUNTIME_DIR/rsvga-vm-hmp.sock

now=0 admin=0
while [[ ${1:-} == --* ]]; do
  case $1 in
    --now) now=1 ;;
    --admin) admin=1 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
name=${1:?usage: guest-run.sh [--now] [--admin] NAME [SECONDS]}
seconds=${2:-180}
[[ $name =~ ^[a-z]+$ ]] || { echo "NAME must be lowercase letters" >&2; exit 2; }

case $name in
  run) tool=d3dtest done='d3dtest: summary\|d3dtest: FAIL setup' ;;
  probe) tool=d3dprobe done='d3dprobe: done' ;;
  info) tool=wininfo done='wininfo: --- end' ;;
  diag | events | dxg | heaven | heavenrun) tool=hostcat done='hostcat: --- end\|hostcat: cannot' ;;
  umd) tool=umdcaps done='umdcaps: --- end' ;;
  trace | retrace) tool=d3d9trace done='d3d9trace: --- end' ;;
  ninetest) tool=d3d9test done='d3d9test: summary\|d3d9test: FAIL setup' ;;
  modes) tool=modes done='modes: --- end' ;;
  videoprobe) tool=videoprobe done='videoprobe: --- end' ;;
  nine) tool='\(d3dprobe\|hostcat\)' done='hostcat: --- end\|hostcat: cannot' ;;
  *) tool=$name done="$name: " ;;
esac

key() {
  printf 'sendkey %s\n' "$1" | socat -t0.3 - "UNIX-CONNECT:$hmp" >/dev/null
  sleep "${2:-0.3}"
}
# US key positions of AZERTY letters.
azerty() {
  case $1 in a) echo q ;; q) echo a ;; z) echo w ;; w) echo z ;; m) echo semicolon ;; *) echo "$1" ;; esac
}

if ((!now)); then
  # The desktop: DWM drawing (3D) or presenting (2D).
  ready() { (($(grep -ac 'DX [0-9] DrawIndexed\|GB UpdateScreenTarget' "$log" 2>/dev/null || true) > 200)); }
  for _ in $(seq 100); do ready && break; sleep 3; done
  ready || { echo "no desktop" >&2; exit 1; }
  sleep 25
fi
before=$(grep -ac "log $tool" "$qemu_log" 2>/dev/null || true)
key meta_l-r 3
key end; key shift-home; key backspace
for k in d dot alt_r-8; do key "$k"; done # D:\
for c in $(grep -o . <<<"$name"); do key "$(azerty "$c")"; done
for k in shift-comma c semicolon d; do key "$k"; done # .cmd
if ((admin)); then
  key ctrl-shift-ret 4
  key alt-y
else
  key ret
fi
# Only lines after `before`: the tool may have run before in this VM.
lines() { grep -a "log $tool" "$qemu_log" | tail -n +$((before + 1)); }
for _ in $(seq $((seconds / 2))); do
  lines | grep -q "$done" && break
  sleep 2
done
lines | sed "s/^.*log $tool: //"
