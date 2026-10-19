#!/bin/sh
# Build the socket helper (native/helper, Go) for every platform and package the installers the extension bundles (tools/build-extension.mjs):
#   native/out/FlashHelper-Setup.exe    Windows: double-click installs it for the current user (registry, no administrator)
#   native/out/FlashHelper.pkg          macOS: Installer package (binary + manifests for all users)
#   native/out/flash-helper_amd64.deb   Linux: Debian package
# plus the bare binaries flash-helper-darwin (universal), flash-helper-linux-{amd64,arm64}. Needs Go and, for the .deb, podman
# (dpkg-deb runs in a Debian container). Unsigned: Windows SmartScreen and macOS Gatekeeper warn once; sign + notarize for release.
set -eu
cd "$(dirname "$0")/.."
OUT="$PWD/native/out"
mkdir -p "$OUT"
VER=$(cd native/helper && go run . --version)
build() { GOOS=$1 GOARCH=$2 CGO_ENABLED=0 go build -C native/helper -trimpath -ldflags '-s -w' -o "$OUT/$3" . ; }
build windows amd64 FlashHelper-Setup.exe
build darwin amd64 flash-helper-darwin-amd64
build darwin arm64 flash-helper-darwin-arm64
lipo -create -output "$OUT/flash-helper-darwin" "$OUT/flash-helper-darwin-amd64" "$OUT/flash-helper-darwin-arm64"
rm -f "$OUT/flash-helper-darwin-amd64" "$OUT/flash-helper-darwin-arm64"
build linux amd64 flash-helper-linux-amd64
build linux arm64 flash-helper-linux-arm64

# macOS package: everything under /Library so every user of the machine gets it, no script needed
R="$OUT/pkgroot"; rm -rf "$R"
BIN="/Library/Application Support/FlashRunnerHelper/flash-helper"
mkdir -p "$R$(dirname "$BIN")"; cp "$OUT/flash-helper-darwin" "$R$BIN"; chmod 755 "$R$BIN"
for d in "Library/Google/Chrome/NativeMessagingHosts" "Library/Application Support/Chromium/NativeMessagingHosts" "Library/Microsoft/Edge/NativeMessagingHosts" \
         "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts" "Library/Application Support/Vivaldi/NativeMessagingHosts"; do
  mkdir -p "$R/$d"; "$OUT/flash-helper-darwin" manifest "$BIN" > "$R/$d/com.flashrunner.sockets.json"
done
pkgbuild --quiet --root "$R" --identifier com.flashrunner.helper --version "$VER" --install-location / --ownership recommended "$OUT/FlashHelper.pkg"
rm -rf "$R"

# Debian package: binary + system-wide manifests (/etc/.../native-messaging-hosts is read for every user)
D="$OUT/debroot"; rm -rf "$D"
LBIN="/usr/lib/flash-helper/flash-helper"
mkdir -p "$D/DEBIAN" "$D$(dirname "$LBIN")"; cp "$OUT/flash-helper-linux-amd64" "$D$LBIN"; chmod 755 "$D$LBIN"
for d in etc/opt/chrome/native-messaging-hosts etc/chromium/native-messaging-hosts etc/opt/edge/native-messaging-hosts etc/brave/native-messaging-hosts; do
  mkdir -p "$D/$d"; "$OUT/flash-helper-darwin" manifest "$LBIN" > "$D/$d/com.flashrunner.sockets.json"
done
cat > "$D/DEBIAN/control" <<EOF
Package: flash-helper
Version: $VER
Section: web
Priority: optional
Architecture: amd64
Maintainer: flash-runner <noreply@localhost>
Description: Socket helper for the flash-runner browser extension
 Lets Flash content played by the extension open the raw TCP sockets
 (flash.net.Socket, XMLSocket) that a browser cannot open itself.
EOF
podman run --rm --platform linux/amd64 -v "$OUT:/out" debian:bookworm-slim sh -c 'dpkg-deb --build --root-owner-group /out/debroot /out/flash-helper_amd64.deb >/dev/null'
rm -rf "$D"
ls -la "$OUT"
