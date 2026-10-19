#!/bin/sh
# Compile tests/progs/<glob>.{c,cpp} inside the i386 container as static + dynamic binaries, and record
# the container's own stdout/exit code as the expected result. Default glob: everything.
#   tools/build-progs.sh 'x87_*'
# Per-program extra flags: first line comment   // flags: -msse2 -mfpmath=sse
# Files named lib*.c are built as shared objects (out/libNAME.so) and not executed.
set -eu
cd "$(dirname "$0")/.."
mkdir -p tests/out
PAT="${1:-*}"
docker run --rm --platform linux/386 -v "$PWD/tests:/work" flashrunner-i386-dev sh -c '
  cd /work
  for c in progs/'"$PAT"'.c progs/'"$PAT"'.cpp; do
    [ -f "$c" ] || continue
    n=$(basename $c); n=${n%.*}
    case $c in *.cpp) CC=g++;; *) CC=gcc;; esac
    flags=$(head -1 $c | sed -n "s|^// flags: ||p")
    case $n in
      lib*) $CC -O1 $flags -shared -fPIC -o out/$n.so $c 2>&1 | head -20; continue;;
    esac
    case $n in dyn_*) ;; *) $CC -O1 $flags -static -o out/$n.static $c -lm -lpthread -ldl 2>&1 | head -20;; esac
    $CC -O1 $flags -o out/$n.dyn $c -lm -lpthread -ldl -Wl,-rpath,/work 2>&1 | head -20
    for v in static dyn; do
      [ -f out/$n.$v ] || continue
      if [ -f progs/$n.expected ]; then   # hand-written expectation (oracle unusable, e.g. qemu-user pthread_cancel)
        cp progs/$n.expected out/$n.$v.expected; echo 0 > out/$n.$v.code; continue
      fi
      ./out/$n.$v > out/$n.$v.expected 2>out/$n.$v.expected.err; echo $? > out/$n.$v.code || true
    done
  done
'
