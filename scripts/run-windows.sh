#!/usr/bin/env bash
# Runs a Windows VM on rsvga: the rsvga window shows the display and sends
# keyboard and mouse to the guest; QEMU is the patched build from
# scripts/build-qemu.sh.
#
# The disk is always used through a qcow2 overlay (default
# target/vm/overlay.qcow2, created on first use), so the original image is
# never written. Delete the overlay to start again from the original disk.
# The overlay is refused if the base image changes afterwards (another VM
# booted it), since its contents would no longer match.
#
# Usage:
#   scripts/run-windows.sh --disk WINDOWS.qcow2 --vars OVMF_VARS.fd \
#       [--overlay FILE] [--mem 6144] [--cpus 4] [--trace]
#
# Requires: target/qemu/install (build-qemu.sh), target/firmware (build-firmware.sh),
# and rsvga built with the window: cargo build --release -p rsvga-server --features window
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
qemu=$root/target/qemu/install/bin/qemu-system-x86_64
rsvga=$root/target/release/rsvga
rom=$root/target/firmware/rsvga-gop.efi
ovmf_code=/usr/share/OVMF/OVMF_CODE_4M.fd

disk= vars= overlay=$root/target/vm/overlay.qcow2 mem=6144 cpus=4 trace=0
while (($#)); do
  case $1 in
    --disk) disk=$2; shift 2 ;;
    --vars) vars=$2; shift 2 ;;
    --overlay) overlay=$2; shift 2 ;;
    --mem) mem=$2; shift 2 ;;
    --cpus) cpus=$2; shift 2 ;;
    --trace) trace=1; shift ;;
    *) sed -n '2,17p' "$0"; exit 2 ;;
  esac
done
[[ -n $disk && -n $vars ]] || { sed -n '2,17p' "$0"; exit 2; }
for f in "$qemu" "$rsvga" "$rom" "$ovmf_code" "$disk" "$vars"; do
  [[ -e $f ]] || { echo "missing: $f" >&2; exit 1; }
done

state=$(dirname "$overlay")
mkdir -p "$state"
# An overlay is only valid while its base image is unchanged: remember the
# base's size and modification time, and refuse to run if they moved.
stamp=${overlay%.qcow2}.base
base_id() { stat -c '%n %s %Y' "$(realpath "$disk")"; }
if [[ ! -e $overlay ]]; then
  qemu-img create -q -f qcow2 -b "$(realpath "$disk")" -F qcow2 "$overlay"
  base_id > "$stamp"
  echo "new overlay: $overlay (backed by $disk)"
elif [[ ! -e $stamp ]]; then
  base_id > "$stamp"
elif [[ "$(base_id)" != "$(cat "$stamp")" ]]; then
  echo "the base image changed since $overlay was created: its contents no longer" >&2
  echo "match. Delete $overlay (and ${overlay%.qcow2}.*) to start over." >&2
  exit 1
fi
nvram=${overlay%.qcow2}.vars.fd
[[ -e $nvram ]] || cp "$vars" "$nvram"

run=${XDG_RUNTIME_DIR:-/tmp}
sock=$run/rsvga-vm.sock qmp=$run/rsvga-vm-qmp.sock
rm -f "$sock" "$qmp"

RSVGA_TRACE=$trace "$rsvga" --socket "$sock" --rom "$rom" --window --qmp "$qmp" \
  2> "$state/rsvga.log" &
rsvga_pid=$!
trap 'kill $rsvga_pid 2>/dev/null; rm -f "$sock" "$qmp"' EXIT
for _ in $(seq 50); do [[ -S $sock ]] && break; sleep 0.1; done

echo "Windows starts in the rsvga window (logs: $state/rsvga.log)."
"$qemu" -name rsvga-windows \
  -machine q35,accel=kvm,memory-backend=ram -cpu host -smp "$cpus" -m "$mem" \
  -object memory-backend-memfd,id=ram,size="${mem}M",share=on \
  -rtc base=localtime -nodefaults -vga none -display none -serial null \
  -drive "if=pflash,format=raw,unit=0,readonly=on,file=$ovmf_code" \
  -drive "if=pflash,format=raw,unit=1,file=$nvram" \
  -device ich9-ahci,id=sata \
  -drive "if=none,id=osdisk,file=$overlay,format=qcow2,cache=writeback,discard=unmap" \
  -device ide-hd,drive=osdisk,bus=sata.0 \
  -device qemu-xhci -device usb-tablet \
  -netdev user,id=net0 -device e1000e,netdev=net0 \
  -qmp "unix:$qmp,server=on,wait=off" \
  -device "{\"driver\":\"vfio-user-pci\",\"socket\":{\"type\":\"unix\",\"path\":\"$sock\"}}"
