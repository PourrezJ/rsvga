#!/usr/bin/env python3
"""Smoke test: rsvga behind a real QEMU, driven like a guest driver would.

Starts `rsvga`, then a disk-less q35/KVM/OVMF VM with the card attached over
vfio-user, and uses the QEMU monitor's port I/O commands (`o`, `i`) to talk
to the SVGA registers through the real KVM -> QEMU -> vfio-user path,
including the unaligned 32-bit value port at BAR0 + 1.

With --firmware, the card also serves its UEFI driver (target/firmware/
rsvga-gop.efi) from its option ROM, and the test instead checks that OVMF
draws its console through it.

Usage: scripts/smoke-qemu.py [--firmware] [--qemu PATH] [--rsvga PATH] [--keep DIR]
Exit status 0 means every check passed.
"""

import argparse
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import zlib
from functools import partial

print = partial(print, flush=True)
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OVMF_CODE = Path("/usr/share/OVMF/OVMF_CODE_4M.fd")
OVMF_VARS = Path("/usr/share/OVMF/OVMF_VARS_4M.fd")

SVGA_ID_2 = 0x90000002
REG = {"ID": 0, "ENABLE": 1, "WIDTH": 2, "HEIGHT": 3, "BITS_PER_PIXEL": 7,
       "BYTES_PER_LINE": 12, "FB_START": 13, "VRAM_SIZE": 15, "MEM_START": 18}


class Monitor:
    """Minimal HMP client over a unix socket."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        for _ in range(100):
            try:
                self.sock.connect(path)
                break
            except OSError:
                time.sleep(0.1)
        self.sock.settimeout(5)
        self._read_prompt()

    def _read_prompt(self):
        data = b""
        while not data.endswith(b"(qemu) "):
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("QEMU closed the monitor")
            data += chunk
        return data.decode(errors="replace")

    def quit(self):
        self.sock.sendall(b"quit\n")
        self.sock.close()

    def cmd(self, line):
        self.sock.sendall(line.encode() + b"\n")
        out = self._read_prompt().replace("\r", "")
        lines = [l for l in out.split("\n") if l and "(qemu)" not in l and line not in l]
        return "\n".join(l.strip() for l in lines if not l.startswith("\x1b"))


def bar0(monitor):
    for line in monitor.cmd("info pci").splitlines():
        if line.startswith("BAR0: I/O at "):
            return int(line.split()[3], 16)
    raise RuntimeError("the SVGA card has no I/O BAR in 'info pci'")


def lit_pixels(png):
    """Counts non-black bytes in one of rsvga's PNG snapshots."""
    data, pos, idat = png.read_bytes(), 8, b""
    while pos < len(data):
        size = int.from_bytes(data[pos:pos + 4], "big")
        if data[pos + 4:pos + 8] == b"IDAT":
            idat += data[pos + 8:pos + 8 + size]
        pos += 12 + size
    width = int.from_bytes(data[16:20], "big")
    raw = zlib.decompress(idat)
    rows = [raw[i + 1:i + 1 + 3 * width] for i in range(0, len(raw), 1 + 3 * width)]
    return sum(1 for row in rows for byte in row if byte)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu", default=ROOT / "target/qemu/install/bin/qemu-system-x86_64")
    parser.add_argument("--rsvga", default=ROOT / "target/release/rsvga")
    parser.add_argument("--keep", help="keep logs and the snapshot in this directory")
    parser.add_argument("--firmware", action="store_true", help="test the UEFI driver in the option ROM")
    parser.add_argument("--rom", default=ROOT / "target/firmware/rsvga-gop.efi")
    args = parser.parse_args()

    runtime = Path(os.environ.get("XDG_RUNTIME_DIR", tempfile.gettempdir()))
    work = Path(tempfile.mkdtemp(prefix="rsvga-smoke-"))
    sock, mon = runtime / f"rsvga-smoke-{os.getpid()}.sock", runtime / f"rsvga-smoke-{os.getpid()}.mon"
    shutil.copy(OVMF_VARS, work / "vars.fd")
    snapshot = work / "frame.png"

    command = [str(args.rsvga), "--socket", str(sock), "--snapshot", str(snapshot)]
    if args.firmware:
        command += ["--rom", str(args.rom)]
    rsvga = subprocess.Popen(command, stderr=open(work / "rsvga.log", "w"))
    for _ in range(50):
        if sock.exists():
            break
        time.sleep(0.1)
    qemu = subprocess.Popen([
        str(args.qemu), "-machine", "q35,accel=kvm,memory-backend=mem", "-cpu", "host", "-m", "512M",
        "-object", "memory-backend-memfd,id=mem,size=512M,share=on",
        "-nodefaults", "-vga", "none", "-display", "none", "-serial", "null",
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,unit=1,file={work / 'vars.fd'}",
        "-device", f'{{"driver":"vfio-user-pci","socket":{{"type":"unix","path":"{sock}"}}}}',
        "-monitor", f"unix:{mon},server=on,wait=off",
    ], stdout=subprocess.DEVNULL, stderr=open(work / "qemu.log", "w"))

    failures = []

    def check(name, got, want):
        ok = got == want
        print(f"{'ok  ' if ok else 'FAIL'} {name}: {got:#x}" + ("" if ok else f" (want {want:#x})"))
        if not ok:
            failures.append(name)

    try:
        if args.firmware:
            firmware_checks(work / "rsvga.log", snapshot, failures)
            return finish(failures)
        time.sleep(1.0)  # let OVMF enumerate PCI and assign the BARs
        monitor = Monitor(str(mon))
        for _ in range(50):
            try:
                port = bar0(monitor)
                break
            except RuntimeError:
                time.sleep(0.2)
        else:
            raise RuntimeError("OVMF never assigned the I/O BAR")
        print(f"SVGA I/O ports at {port:#x}")

        def write(reg, value):
            monitor.cmd(f"o /w {port:#x} {REG[reg]}")
            monitor.cmd(f"o /w {port + 1:#x} {value:#x}")

        def read(reg):
            monitor.cmd(f"o /w {port:#x} {REG[reg]}")
            out = monitor.cmd(f"i /w {port + 1:#x}")
            return int(out.rsplit("=", 1)[1], 16)

        check("ID through the unaligned value port", read("ID"), SVGA_ID_2)
        check("VRAM_SIZE", read("VRAM_SIZE"), 64 << 20)
        for reg, value in (("WIDTH", 800), ("HEIGHT", 600), ("BITS_PER_PIXEL", 32), ("ENABLE", 1)):
            write(reg, value)
        check("BYTES_PER_LINE after setting 800x600x32", read("BYTES_PER_LINE"), 3200)
        fb = read("FB_START")
        print(f"     FB_START = {fb:#x}")
        if fb == 0:
            failures.append("FB_START")
        time.sleep(1.5)  # one snapshot period
        log = (work / "rsvga.log").read_text()
        if "mode 800x600 32 bpp" not in log:
            failures.append("mode change seen by rsvga")
            print("FAIL rsvga did not log the 800x600 mode")
        else:
            print("ok   rsvga switched to 800x600 32 bpp")
        if snapshot.exists() and snapshot.read_bytes()[16:24] == (800).to_bytes(4, "big") + (600).to_bytes(4, "big"):
            print(f"ok   snapshot is 800x600")
        else:
            failures.append("snapshot")
            print("FAIL no 800x600 snapshot")
        monitor.quit()
    finally:
        for proc in (qemu, rsvga):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        for path in (sock, mon):
            path.unlink(missing_ok=True)
        if args.keep:
            shutil.copytree(work, args.keep, dirs_exist_ok=True)
        shutil.rmtree(work, ignore_errors=True)

    return finish(failures)


def finish(failures):
    print("PASS" if not failures else f"FAILED: {', '.join(failures)}")
    return 1 if failures else 0


def firmware_checks(log_path, snapshot, failures):
    """OVMF loads the ROM driver, which sets 1024x768; the console then picks
    1280x800 and draws the shell banner into the framebuffer."""
    deadline = time.time() + 30
    while time.time() < deadline:
        log = log_path.read_text()
        if "mode 1280x800 32 bpp" in log and snapshot.exists() and lit_pixels(snapshot) > 1000:
            break
        time.sleep(0.5)
    log = log_path.read_text()
    for name, ok in (
        ("option ROM served", re.search(r"option ROM: \d+ bytes", log) is not None),
        ("driver set its boot mode (1024x768)", "mode 1024x768 32 bpp" in log),
        ("console mode (1280x800)", "mode 1280x800 32 bpp" in log),
        ("console text drawn", snapshot.exists() and lit_pixels(snapshot) > 1000),
    ):
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        if not ok:
            failures.append(name)


if __name__ == "__main__":
    sys.exit(main())
