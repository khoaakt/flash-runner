#!/bin/sh
# Developer install (or removal) of the native messaging host that gives the extension raw TCP sockets; users get the packaged installers instead.
#   tools/install-native-host.sh                 install for every Chromium-family browser found
#   tools/install-native-host.sh --uninstall     remove it again
#   tools/install-native-host.sh --dir <dir>     install into one NativeMessagingHosts directory (e.g. a test profile)
# It writes a small JSON manifest (allowing only this extension's id) pointing at the helper binary in native/out. Nothing else on the system is changed.
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
EXT_ID="njekchgflloenifdhaehploldjanaonk"
NAME="com.flashrunner.sockets"
mode=install; dirs=""
while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) mode=uninstall ;;
    --dir) shift; dirs="$1" ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
if [ -z "$dirs" ]; then
  if [ "$(uname -s)" = Darwin ]; then b="$HOME/Library/Application Support"; else b="$HOME/.config"; fi
  list="$(printf '%s\n' "$b/Google/Chrome" "$b/Google/Chrome Beta" "$b/google-chrome" "$b/Chromium" "$b/chromium" "$b/BraveSoftware/Brave-Browser" "$b/Microsoft Edge" "$b/microsoft-edge" "$b/Vivaldi")"
  dirs="$(printf '%s\n' "$list" | while IFS= read -r c; do [ -d "$c" ] && printf '%s\n' "$c/NativeMessagingHosts"; done)"
  [ -n "$dirs" ] || { echo "no Chromium-family browser profile directory found; use --dir" >&2; exit 1; }
fi
if [ "$mode" = uninstall ]; then
  echo "$dirs" | while IFS= read -r d; do [ -n "$d" ] && rm -f "$d/$NAME.json" && echo "removed $d/$NAME.json"; done
  exit 0
fi
# the helper built by tools/build-helper.sh for this machine
case "$(uname -s)" in Darwin) wrapper="$here/native/out/flash-helper-darwin" ;; *) a=$(uname -m); case $a in x86_64) a=amd64 ;; aarch64|arm64) a=arm64 ;; esac; wrapper="$here/native/out/flash-helper-linux-$a" ;; esac
[ -x "$wrapper" ] || { echo "helper not built: run tools/build-helper.sh first" >&2; exit 1; }
echo "$dirs" | while IFS= read -r d; do
  [ -n "$d" ] || continue
  mkdir -p "$d"
  cat > "$d/$NAME.json" <<JSON
{
  "name": "$NAME",
  "description": "Raw TCP sockets for the flash-runner extension",
  "path": "$wrapper",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
JSON
  echo "installed $d/$NAME.json"
done
echo "helper: $wrapper"
