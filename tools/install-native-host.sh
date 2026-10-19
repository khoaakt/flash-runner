#!/bin/sh
# Developer install (or removal) of the native messaging host that gives the extension raw TCP sockets; users get the packaged installers instead.
#   tools/install-native-host.sh                 install for every Chromium-family browser and Firefox found
#   tools/install-native-host.sh --uninstall     remove it again
#   tools/install-native-host.sh --dir <dir>     install into one NativeMessagingHosts directory (e.g. a test profile)
#   tools/install-native-host.sh --firefox-dir <dir>   the same for Firefox, whose manifest names the add-on id instead of the extension origin
# It writes a small JSON manifest (allowing only this extension's id in Chrome and its add-on id in Firefox) pointing at the helper binary in native/out. Nothing else on the system is changed.
set -eu
here="$(cd "$(dirname "$0")/.." && pwd)"
EXT_ID="njekchgflloenifdhaehploldjanaonk"
FF_ID="flash-runner@khoaakt.github.io"   # browser_specific_settings.gecko.id in tools/firefox/manifest-overlay.json
NAME="com.flashrunner.sockets"
mode=install; dirs=""; ffdirs=""
while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) mode=uninstall ;;
    --dir) shift; dirs="$1" ;;
    --firefox-dir) shift; ffdirs="$1" ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
if [ -z "$dirs" ] && [ -z "$ffdirs" ]; then
  if [ "$(uname -s)" = Darwin ]; then b="$HOME/Library/Application Support"; else b="$HOME/.config"; fi
  list="$(printf '%s\n' "$b/Google/Chrome" "$b/Google/Chrome Beta" "$b/google-chrome" "$b/Chromium" "$b/chromium" "$b/BraveSoftware/Brave-Browser" "$b/Microsoft Edge" "$b/microsoft-edge" "$b/Vivaldi")"
  dirs="$(printf '%s\n' "$list" | while IFS= read -r c; do [ -d "$c" ] && printf '%s\n' "$c/NativeMessagingHosts"; done)"
  if [ "$(uname -s)" = Darwin ]; then ffroot="$b/Mozilla"; ffdir="$ffroot/NativeMessagingHosts"; else ffroot="$HOME/.mozilla"; ffdir="$ffroot/native-messaging-hosts"; fi
  [ -d "$ffroot" ] && ffdirs="$ffdir"
  [ -n "$(printf '%s%s' "$dirs" "$ffdirs" | tr -d '[:space:]')" ] || { echo "no Chromium-family or Firefox profile directory found; use --dir" >&2; exit 1; }
fi
if [ "$mode" = uninstall ]; then
  printf '%s\n%s\n' "$dirs" "$ffdirs" | while IFS= read -r d; do [ -n "$d" ] && rm -f "$d/$NAME.json" && echo "removed $d/$NAME.json"; done
  exit 0
fi
# the helper built by tools/build-helper.sh for this machine
case "$(uname -s)" in Darwin) wrapper="$here/native/out/flash-helper-darwin" ;; *) a=$(uname -m); case $a in x86_64) a=amd64 ;; aarch64|arm64) a=arm64 ;; esac; wrapper="$here/native/out/flash-helper-linux-$a" ;; esac
[ -x "$wrapper" ] || { echo "helper not built: run tools/build-helper.sh first" >&2; exit 1; }
write_manifest() {   # $1 = directory, $2 = the line naming who may start the helper
  mkdir -p "$1"
  cat > "$1/$NAME.json" <<JSON
{
  "name": "$NAME",
  "description": "Raw TCP sockets for the flash-runner extension",
  "path": "$wrapper",
  "type": "stdio",
  $2
}
JSON
  echo "installed $1/$NAME.json"
}
echo "$dirs" | while IFS= read -r d; do [ -n "$d" ] && write_manifest "$d" "\"allowed_origins\": [\"chrome-extension://$EXT_ID/\"]"; done
# Firefox reads allowed_extensions and rejects a manifest that has the Chromium key
echo "$ffdirs" | while IFS= read -r d; do [ -n "$d" ] && write_manifest "$d" "\"allowed_extensions\": [\"$FF_ID\"]"; done
echo "helper: $wrapper"
