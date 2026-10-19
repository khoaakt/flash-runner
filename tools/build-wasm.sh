#!/bin/sh
# Build the emulator for wasm32 and optimize it into web/emu.wasm.
#   CARGO_TARGET_DIR=...  use a private cargo target dir (avoids lock contention with other builds)
#   WASM_OUT=path         write the optimized module somewhere other than web/emu.wasm
set -eu
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"   # absent on Windows (cargo is on PATH there)
TD="${CARGO_TARGET_DIR:-target}"
OUT="${WASM_OUT:-web/emu.wasm}"
CARGO_TARGET_DIR="$TD" cargo build --release --target wasm32-unknown-unknown -p emu --lib
mkdir -p "$(dirname "$OUT")"
wasm-opt -O3 --enable-bulk-memory --enable-sign-ext --enable-nontrapping-float-to-int --enable-mutable-globals \
  "$TD/wasm32-unknown-unknown/release/emu.wasm" -o "$OUT"
ls -l "$OUT"
