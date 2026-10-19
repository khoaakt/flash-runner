#!/bin/sh
# Compile guest-side programs (pepper-host, test plugins) with the i386 toolchain in the container.
set -eu
cd "$(dirname "$0")/.."
python3 tools/gen-ppapi.py
mkdir -p guest/out
docker run --rm --platform linux/386 -v "$PWD:/proj" flashrunner-i386-dev sh -c '
  cd /proj/guest/pepper-host && make 2>&1 | tail -20
  for p in /proj/guest/test-plugin/*.c; do [ -f "$p" ] || continue; n=$(basename $p .c); gcc -O1 -g -shared -fPIC -I/proj/third_party -o /proj/guest/out/lib$n.so $p; gcc -O1 -g -shared -fPIC -DFAKEPLUG_FP -I/proj/third_party -o /proj/guest/out/lib$n-fp.so $p; done
'
ls -la guest/out
