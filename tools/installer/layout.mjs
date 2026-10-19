// Where the macOS installer (tools/build-installer.mjs) puts things, and the build version it stamps into the installed manifest.
// Shared by the build and by tools/installer-test.mjs; the shell scripts in this directory (preinstall, Uninstall.command) spell the same paths out.
export const INSTALL_DIR = '/Applications/Flash Runner';
export const EXTENSION_DIR = `${INSTALL_DIR}/extension`;

// The unpacked extension has no auto-update, so each installer carries its own version: major.minor from the source manifest, then the
// whole days since 2026-01-01 and the minute of the day (UTC). Chrome shows it on the extension's card, which is how you tell that a
// reload picked up the new files. Every part is an integer <= 65535 without leading zeros, as Chrome requires.
export function stampVersion(version, now = new Date()) {
  const [major, minor] = String(version).split('.').map(Number);
  const since = now.getTime() - Date.UTC(2026, 0, 1);
  const days = Math.floor(since / 864e5), minutes = Math.floor((since % 864e5) / 6e4);
  if (![major, minor].every((n) => Number.isInteger(n) && n >= 0 && n <= 65535) || days < 0 || days > 65535) throw new Error(`cannot stamp version ${version} at ${now.toISOString()}`);
  return `${major}.${minor}.${days}.${minutes}`;
}
