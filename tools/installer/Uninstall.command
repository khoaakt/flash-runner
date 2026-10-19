#!/bin/sh
# Removes what FlashRunner.pkg installed: double-click it, or run it with sudo. The extension is removed from the browser separately
# (chrome://extensions -> Remove on the "flash-runner" card), because Chrome loaded it from the folder this deletes.
# FLASH_UNINSTALL_ROOT=<dir> (tools/installer-test.mjs) does the same removals below <dir> and leaves the installer receipts alone.
set -eu
root="${FLASH_UNINSTALL_ROOT:-}"
if [ -z "$root" ] && [ "$(id -u)" != 0 ]; then
  echo "Administrator rights are needed to remove the installed files."
  exec sudo /bin/sh "$0" "$@"
fi
# the browsers' system-wide native messaging directories that FlashHelper.pkg registers the helper in (tools/build-helper.sh);
# the ._ files are AppleDouble leftovers that package carries
for d in "Library/Google/Chrome" "Library/Application Support/Chromium" "Library/Microsoft/Edge" \
         "Library/Application Support/BraveSoftware/Brave-Browser" "Library/Application Support/Vivaldi"; do
  rm -f "$root/$d/NativeMessagingHosts/com.flashrunner.sockets.json" "$root/$d/NativeMessagingHosts/._com.flashrunner.sockets.json"
done
rm -rf "$root/Library/Application Support/FlashRunnerHelper"
if [ -z "$root" ]; then
  pkgutil --forget com.flashrunner.helper >/dev/null 2>&1 || true
  pkgutil --forget com.flashrunner.extension >/dev/null 2>&1 || true
fi
echo "Removed the online game helper and its browser registrations."
echo "Removing the extension folder. Remove the extension's card in chrome://extensions too."
rm -rf "$root/Applications/Flash Runner"
exit 0
