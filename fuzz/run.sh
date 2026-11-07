#!/usr/bin/env bash
# Runs a fuzz target with libFuzzer on the stable toolchain (no cargo-fuzz or
# nightly needed). Safe Rust needs coverage guidance, not AddressSanitizer.
# Usage: fuzz/run.sh <fifo|decode|dx> [seconds]
set -euo pipefail
cd "$(dirname "$0")"
target="${1:?usage: $0 <fifo|decode|dx> [seconds]}"
seconds="${2:-60}"
triple="$(rustc -vV | sed -n 's/^host: //p')"
export RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares --cfg fuzzing -Cdebug-assertions -Coverflow-checks"
# An explicit --target keeps RUSTFLAGS away from build scripts.
cargo build --release --target "$triple" --bin "$target"
mkdir -p "corpus/$target" artifacts
exec "target/$triple/release/$target" -max_total_time="$seconds" -max_len=8192 \
  -artifact_prefix="artifacts/$target-" "corpus/$target"
