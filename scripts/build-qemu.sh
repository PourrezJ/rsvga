#!/usr/bin/env bash
# Builds QEMU with rsvga's patches (patches/qemu/*.patch) into
# target/qemu/install, without touching the system QEMU.
#
# The source tarball is pinned by SHA-256. That hash was taken from a tarball
# whose signature was checked against the QEMU release key
# CEAC C9E1 5534 EBAB B82D  3FA0 3353 C9CE F108 B584 (Michael Roth).
#
# Build dependencies on Debian 13:
#   sudo apt install libpixman-1-dev libslirp-dev libgtk-3-dev libfdt-dev
#
# Usage: scripts/build-qemu.sh   (then target/qemu/install/bin/qemu-system-x86_64)
set -euo pipefail

VERSION=11.1.2
SHA256=731b5681e4bb18be313231579b8efd0296c5b015fa36dc533874b639ba838016

root=$(cd "$(dirname "$0")/.." && pwd)
work=$root/target/qemu
tarball=$work/dl/qemu-$VERSION.tar.xz

mkdir -p "$work/dl"
if [[ ! -s $tarball ]]; then
  curl -fL --proto '=https' -o "$tarball.partial" "https://download.qemu.org/qemu-$VERSION.tar.xz"
  mv "$tarball.partial" "$tarball"
fi
echo "$SHA256  $tarball" | sha256sum --check --quiet -

echo "== extracting QEMU $VERSION"
rm -rf "$work/src" "$work/build"
mkdir -p "$work/src" "$work/build"
tar -xJf "$tarball" -C "$work/src" --strip-components=1

for patch in "$root"/patches/qemu/*.patch; do
  echo "== applying ${patch##*/}"
  patch --directory="$work/src" --strip=1 --forward --quiet < "$patch"
done

echo "== configuring"
cd "$work/build"
"$work/src/configure" \
  --prefix="$work/install" \
  --target-list=x86_64-softmmu \
  --with-pkgversion="rsvga patched" \
  --disable-download \
  --disable-docs \
  --disable-tools \
  --disable-werror \
  --enable-kvm \
  --enable-pixman \
  --enable-gtk \
  --enable-slirp \
  --enable-vhost-user \
  --enable-fdt=system \
  --audio-drv-list=pa,alsa

echo "== building"
make -j"$(nproc)"
make install

"$work/install/bin/qemu-system-x86_64" --version
echo "== done: $work/install/bin/qemu-system-x86_64"
