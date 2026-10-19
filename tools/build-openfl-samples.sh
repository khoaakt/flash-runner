#!/bin/sh
# Build OpenFL's sample games for the Flash target (Haxe -> AS3 SWF) into tests/swf-samples/, which tests/ext/*.html and tools/swf-batch.mjs use.
# The samples are fetched from upstream (openfl-samples, via haxelib) and built here; they are NOT vendored in this repository, because their
# art, audio and video belong to their authors under their own terms. tests/openfl/ and tests/swf-samples/ are git-ignored.
#   tools/build-openfl-samples.sh HelloTriangle BunnyMark PiratePig ...
# Needs: haxe (brew install haxe), then: haxelib setup .haxelib-global; haxelib install openfl lime openfl-samples lime-samples actuate ...
set -u
cd "$(dirname "$0")/.."
mkdir -p tests/openfl tests/swf-samples
cd tests/openfl
export HAXELIB_PATH="$PWD/../../.haxelib-global"
for s in "$@"; do
  [ -d "$s" ] || haxelib run openfl create "$s" >/dev/null 2>&1
  ( cd "$s" && haxelib run openfl build flash 2>&1 | grep -E "Error|error:" | head -3 )
  f=$(ls "$s"/Export/flash/bin/*.swf 2>/dev/null | head -1)
  [ -n "$f" ] && cp "$f" ../swf-samples/ && echo "built $s -> $(basename $f)" || echo "FAILED $s"
done
