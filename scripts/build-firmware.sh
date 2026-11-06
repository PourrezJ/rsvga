#!/usr/bin/env bash
# Builds the card's UEFI GOP driver (firmware/gop) and copies it to
# target/firmware/rsvga-gop.efi, ready for `rsvga --rom`.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
rustup target add x86_64-unknown-uefi >/dev/null
cargo test --quiet --manifest-path "$root/firmware/gop/Cargo.toml" --target "$(rustc -vV | sed -n 's/^host: //p')"
cargo build --release --manifest-path "$root/firmware/gop/Cargo.toml" --target x86_64-unknown-uefi
mkdir -p "$root/target/firmware"
cp "$root/firmware/gop/target/x86_64-unknown-uefi/release/rsvga-gop.efi" "$root/target/firmware/rsvga-gop.efi"
echo "== done: $root/target/firmware/rsvga-gop.efi ($(stat -c %s "$root/target/firmware/rsvga-gop.efi") bytes)"
