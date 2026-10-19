#!/bin/sh
# Wall-clock comparison of the interpreter and the JIT under Node.
#   tools/bench-jit.sh [prog ...]          default: the bench_* programs
#   JITS="0 50 1" tools/bench-jit.sh ...   thresholds to compare (0 = interpreter only)
#   RUNS=3                                 best-of-N
#   EMU_WASM=path                          wasm build to use (default web/emu.wasm)
cd "$(dirname "$0")/.."
JITS="${JITS:-0 50}"
RUNS="${RUNS:-3}"
PROGS="$*"
[ -n "$PROGS" ] || PROGS="bench_int bench_fib bench_mem bench_chase"
printf "%-16s" "program"
for j in $JITS; do printf " %10s" "jit=$j"; done
printf "\n"
for p in $PROGS; do
  f=tests/out/$p.static
  [ -f "$f" ] || f=tests/out/$p.dyn
  printf "%-16s" "$p"
  for j in $JITS; do
    best=999999
    for r in $(seq $RUNS); do
      ms=$(EMU_STATS=1 node web/node-run.mjs --jit $j --rootfs rootfs/out/rootfs.tar -- $f 2>&1 >/dev/null | sed -n 's/.*wasm elapsed \([0-9]*\) ms.*/\1/p')
      [ -n "$ms" ] && [ "$ms" -lt "$best" ] && best=$ms
    done
    printf " %8s ms" "$best"
  done
  printf "\n"
done
