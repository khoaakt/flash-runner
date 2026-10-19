#!/bin/sh
# Differential test: run every compiled test program in the emulator and compare stdout + exit code
# with what the real i386 container produced (tests/out/<name>.<variant>.expected / .code).
#   tools/difftest.sh [name-glob] [--sse2]   e.g. tools/difftest.sh 'x87_*' --sse2
# Prerequisite: tools/build-progs.sh (needs podman running + image flashrunner-i386-dev + rootfs/build.sh).
cd "$(dirname "$0")/.."
PAT="${1:-*}"
shift 2>/dev/null
EXTRA="$*"
RUN=./target/debug/emu-run
# RUNNER=wasm runs the same programs inside the wasm build under Node (needs web/emu.wasm: tools/build-wasm.sh)
#   DIFFTEST_SKIP='glob glob'  skip programs whose name matches (with EMU_JIT_TRUST=1: the ones that rely on faulting loads)
#   JIT=N      wasm runner only: force the JIT (1 = translate everything at first sight, N = after N executions)
#   EMU_WASM=p wasm runner only: use another wasm build than web/emu.wasm
[ "${RUNNER:-}" = wasm ] && RUN="perl tools/with-timeout.pl ${DIFFTEST_TIMEOUT:-900} node web/node-run.mjs"   # (a hung test shows up as a failure instead of blocking the run)
[ -n "${JIT:-}" ] && export EMU_JIT="$JIT"
[ "${RUNNER:-}" = wasm ] || [ -x $RUN ] || { echo "build first: cargo build -p emu --bin emu-run"; exit 2; }
pass=0; fail=0; failed=""
for exp in tests/out/$PAT.expected; do
  base=${exp%.expected}            # tests/out/hello.static
  # DIFFTEST_SKIP="jit_mem* jit_signal*": programs left out (e.g. those that rely on faulting loads when the JIT runs with the loads-unchecked mode, EMU_JIT_TRUST=1)
  skip=0; for g in ${DIFFTEST_SKIP:-}; do case $(basename $base) in $g) skip=1;; esac; done; [ $skip = 1 ] && continue
  [ -f "$base" ] || continue
  want_code=$(cat $base.code 2>/dev/null || echo 0)
  got=$($RUN --rootfs rootfs/out/rootfs.tar $EXTRA -- $base 2>$base.got.err); got_code=$?
  if [ "$got" = "$(cat $exp)" ] && [ "$got_code" = "$want_code" ]; then pass=$((pass+1)); else fail=$((fail+1)); failed="$failed $(basename $base)"; fi
done
echo "pass=$pass fail=$fail"
[ -n "$failed" ] && echo "FAILED:$failed" && exit 1
exit 0
