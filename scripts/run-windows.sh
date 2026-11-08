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
#       [--overlay FILE] [--mem 6144] [--cpus 4] [--trace] [--diagnose] \
#       [--kvm-cpu] [--guest-tools] [--guestinfo 'KEY=VALUE;...'] [--no-3d] \
#       [-- EXTRA QEMU ARGUMENTS...]
#
# 3D is on: Direct3D 9, 10 and 11 (feature level 11_0) run on the host GPU
# through Vulkan, with VMware's driver in WDDM 2.0. --no-3d (or RSVGA_3D=0)
# keeps the device 2D: the driver runs display-only and Windows renders 3D
# in software. Without a Vulkan GPU, rsvga stays 2D by itself.
#
# The CPU shows VMware's hypervisor signature ("VMwareVMware", through the
# Hyper-V vendor leaf; KVM's own leaves are hidden): the VMware Tools
# installer refuses other hypervisors (VM_CheckRequirements, error 1602).
# --kvm-cpu keeps KVM's signature instead. The Tools also need the host KVM
# module parameter enable_vmware_backdoor=Y to reach the VMware port from
# user mode. With them, resizing the rsvga window resizes Windows.
#
# Sleep (S3) and hibernation (S4) are hidden from the guest: without S4,
# Windows cannot use Fast Startup, so every boot is a full one and the
# display driver always starts from scratch (a resumed driver keeps the
# choices it made against the previous device).
#
# --trace logs register accesses and FIFO commands (rsvga.log; RSVGA_TRACE=0
# in the environment keeps the server quiet, to measure), and the
# VMware backdoor traffic, driver log messages included (qemu.log).
# --diagnose also saves each guest cursor and a screen capture per second in
# the overlay's folder (cursors/, screen.png).
#
# --guest-tools gives the guest target/guest-tools (scripts/build-guest-tools.sh)
# as a read-only FAT disk. --guestinfo sets VMware guestinfo values the
# drivers read, e.g. their log levels:
#   --guestinfo 'guestinfo.svga.wddm.miniportLogging=TRUE;guestinfo.loglevel.vm3d.all=10'
#
# A QEMU (HMP) monitor listens on $XDG_RUNTIME_DIR/rsvga-vm-hmp.sock, to look
# at a stuck guest from outside, e.g.:
#   echo 'info registers' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/rsvga-vm-hmp.sock
#
# Requires: target/qemu/install (build-qemu.sh), target/firmware (build-firmware.sh),
# and rsvga built with the window: cargo build --release -p rsvga-server --features window
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
qemu=$root/target/qemu/install/bin/qemu-system-x86_64
rsvga=$root/target/release/rsvga
rom=$root/target/firmware/rsvga-gop.efi
ovmf_code=/usr/share/OVMF/OVMF_CODE_4M.fd

# The comment block at the top of this file.
usage() { awk 'NR > 1 && !/^#/ { exit } NR > 1' "$0"; exit 2; }

disk= vars= overlay=$root/target/vm/overlay.qcow2 mem=6144 cpus=4 trace=0 diagnose=0
guest_tools=0 guestinfo=
cpu=host,kvm=off,hv-relaxed,hv-vendor-id=VMwareVMware
while (($#)); do
  case $1 in
    --disk) disk=$2; shift 2 ;;
    --vars) vars=$2; shift 2 ;;
    --overlay) overlay=$2; shift 2 ;;
    --mem) mem=$2; shift 2 ;;
    --cpus) cpus=$2; shift 2 ;;
    --kvm-cpu) cpu=host; shift ;;
    --trace) trace=1; shift ;;
    --diagnose) diagnose=1; trace=1; shift ;;
    --guest-tools) guest_tools=1; shift ;;
    --guestinfo) guestinfo=$2; shift 2 ;;
    --no-3d) export RSVGA_3D=0; shift ;;
    --) shift; break ;;
    *) usage ;;
  esac
done
[[ -n $disk && -n $vars ]] || usage
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
sock=$run/rsvga-vm.sock qmp=$run/rsvga-vm-qmp.sock hmp=$run/rsvga-vm-hmp.sock
rm -f "$sock" "$qmp" "$hmp"

# The VMware backdoor answers GET_SVGA_CAPABILITIES like the device (the
# VMware driver asks it before using the card) and opens GuestRPC, the
# channel VMware drivers log through (patch 0002).
mapfile -t vmport < <("$rsvga" --vmport-args)
qemu_trace=()
if ((trace)); then
  qemu_trace=(-trace vmport_svga_capabilities -trace "vmport_guestrpc_*" -D "$state/qemu.log")
fi

if [[ -n $guestinfo ]]; then
  vmport+=(-global "vmport.guestinfo=$guestinfo")
fi
if ((guest_tools)); then
  tools=$root/target/guest-tools
  [[ -d $tools ]] || { echo "missing: $tools (scripts/build-guest-tools.sh)" >&2; exit 1; }
  # snapshot=on: vvfat is read-only, which ide-hd would refuse otherwise.
  vmport+=(-drive "if=none,id=guesttools,format=raw,snapshot=on,file=fat:$tools"
           -device ide-hd,drive=guesttools,bus=sata.1)
fi

extra=()
if ((diagnose)); then
  export RSVGA_CURSOR_DUMP=$state/cursors
  extra=(--snapshot "$state/screen.png")
fi
RSVGA_TRACE=${RSVGA_TRACE:-$trace} "$rsvga" --socket "$sock" --rom "$rom" --window --qmp "$qmp" "${extra[@]}" \
  2> "$state/rsvga.log" &
rsvga_pid=$!
trap 'kill $rsvga_pid 2>/dev/null; rm -f "$sock" "$qmp" "$hmp"' EXIT
for _ in $(seq 50); do [[ -S $sock ]] && break; sleep 0.1; done

echo "Windows starts in the rsvga window (logs: $state/rsvga.log)."
"$qemu" -name rsvga-windows \
  -machine q35,accel=kvm,memory-backend=ram -cpu "$cpu" -smp "$cpus" -m "$mem" \
  -global ICH9-LPC.disable_s3=1 -global ICH9-LPC.disable_s4=1 \
  -object memory-backend-memfd,id=ram,size="${mem}M",share=on \
  -rtc base=localtime -nodefaults -vga none -display none -serial null \
  -drive "if=pflash,format=raw,unit=0,readonly=on,file=$ovmf_code" \
  -drive "if=pflash,format=raw,unit=1,file=$nvram" \
  -device ich9-ahci,id=sata \
  -drive "if=none,id=osdisk,file=$overlay,format=qcow2,cache=writeback,discard=unmap" \
  -device ide-hd,drive=osdisk,bus=sata.0 \
  -device qemu-xhci -device usb-tablet \
  -netdev user,id=net0 -device e1000e,netdev=net0 \
  -qmp "unix:$qmp,server=on,wait=off" -monitor "unix:$hmp,server=on,wait=off" \
  -device "{\"driver\":\"vfio-user-pci\",\"socket\":{\"type\":\"unix\",\"path\":\"$sock\"}}" \
  "${vmport[@]}" "${qemu_trace[@]}" "$@"
