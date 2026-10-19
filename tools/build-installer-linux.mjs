// Build the Linux packages dist/flash-runner_<version>_{amd64,arm64}.deb: the extension as a signed CRX that Chrome, Chromium and Edge pick up by
// themselves, plus the online game helper (the same files as native/out/flash-helper_amd64.deb, which this package replaces).
//   node tools/build-extension.mjs && node tools/build-installer-linux.mjs        (also needs tools/build-helper.sh's native/out, and podman)
// Linux is the one platform where a file can install an off-store extension: Chrome reads <dir>/<extension id>.json with external_crx and
// external_version and installs it with no prompt (developer.chrome.com "Install extensions"; Windows and macOS refuse external_crx).
// The CRX is signed with vendor-private/extension-key.pem so the extension keeps its ID, which is what the helper's allowed_origins names.
// tools/installer-linux-test.mjs checks the packages in containers.
import { mkdirSync, mkdtempSync, rmSync, existsSync, readFileSync, writeFileSync, cpSync, chmodSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { stampVersion } from './installer/layout.mjs';
import { packDirectory } from './installer/crx.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const res = join(root, 'tools', 'installer', 'linux');
const need = (p, hint) => { if (!existsSync(join(root, p))) { console.error(`missing ${p} — ${hint}`); process.exit(2); } };
need('dist/extension/manifest.json', 'run: node tools/build-extension.mjs');
need('vendor-private/extension-key.pem', 'the CRX must be signed with the extension key, or the extension gets another ID');
const ARCHES = { amd64: 'native/out/flash-helper-linux-amd64', arm64: 'native/out/flash-helper-linux-arm64' };
for (const p of Object.values(ARCHES)) need(p, 'run: tools/build-helper.sh');
// the `manifest <path>` subcommand writes the native messaging manifest; any helper build that runs on this machine will do
const cliPath = process.platform === 'darwin' ? 'native/out/flash-helper-darwin' : `native/out/flash-helper-linux-${process.arch === 'arm64' ? 'arm64' : 'amd64'}`;
need(cliPath, 'run: tools/build-helper.sh');
const cli = join(root, cliPath);

const manifest = JSON.parse(readFileSync(join(root, 'dist/extension/manifest.json'), 'utf8'));
if (!manifest.key) { console.error('dist/extension/manifest.json has no "key"'); process.exit(2); }
const version = stampVersion(manifest.version);

const HELPER = '/usr/lib/flash-helper/flash-helper';
const CRX = '/opt/flash-runner/flash-runner.crx';
// where each browser looks for external extension files (Chrome and Edge: developer.chrome.com / learn.microsoft.com; Chromium: Debian's directory)
const EXTERNAL = ['opt/google/chrome/extensions', 'usr/share/google-chrome/extensions', 'usr/share/microsoft-edge/extensions', 'usr/share/chromium/extensions'];
// system-wide native messaging directories, the same ones the standalone helper package uses
const NATIVE = ['etc/opt/chrome/native-messaging-hosts', 'etc/chromium/native-messaging-hosts', 'etc/opt/edge/native-messaging-hosts', 'etc/brave/native-messaging-hosts'];

const work = mkdtempSync(join(root, 'dist', '.deb-work-'));   // under the repo: podman can mount it
try {
  const ext = join(work, 'extension');
  cpSync(join(root, 'dist/extension'), ext, { recursive: true });
  writeFileSync(join(ext, 'manifest.json'), JSON.stringify({ ...manifest, version }, null, 2) + '\n');
  const { crx, id } = packDirectory(ext, readFileSync(join(root, 'vendor-private/extension-key.pem'), 'utf8'));
  const notice = readFileSync(join(root, 'dist/extension/NOTICE.txt'), 'utf8');
  const fill = (s) => s.replaceAll('@VERSION@', version).replaceAll('@CRX@', CRX).replaceAll('@ID@', id);
  mkdirSync(join(root, 'dist'), { recursive: true });

  for (const [arch, helper] of Object.entries(ARCHES)) {
    const pkg = join(work, `pkg-${arch}`), put = (path, data, mode = 0o644) => { const f = join(pkg, path); mkdirSync(dirname(f), { recursive: true }); writeFileSync(f, data); chmodSync(f, mode); };
    put(CRX.slice(1), crx);
    for (const d of EXTERNAL) put(`${d}/${id}.json`, JSON.stringify({ external_crx: CRX, external_version: version }, null, 2) + '\n');
    put(HELPER.slice(1), readFileSync(join(root, helper)), 0o755);
    const nativeManifest = execFileSync(cli, ['manifest', HELPER]);
    for (const d of NATIVE) put(`${d}/com.flashrunner.sockets.json`, nativeManifest);
    put('usr/share/doc/flash-runner/README', fill(readFileSync(join(res, 'README'), 'utf8')));
    put('usr/share/doc/flash-runner/copyright', notice);
    put('DEBIAN/postinst', fill(readFileSync(join(res, 'postinst'), 'utf8')), 0o755);
    put('DEBIAN/control', fill(readFileSync(join(res, 'control'), 'utf8')).replaceAll('@ARCH@', arch)
      .replaceAll('@SIZE@', execFileSync('du', ['-sk', pkg]).toString().split('\t')[0]));
    const name = `flash-runner_${version}_${arch}.deb`;
    rmSync(join(root, 'dist', name), { force: true });
    // dpkg-deb runs in a Debian container (not available on macOS); --root-owner-group makes every file root's whatever user staged it
    execFileSync('podman', ['run', '--rm', '--platform', 'linux/amd64', '-v', `${work}:/work`, '-v', `${join(root, 'dist')}:/out`, 'debian:bookworm-slim', 'sh', '-c',
      `cd /work/pkg-${arch} && find . -type d -exec chmod 755 {} + && dpkg-deb --build --root-owner-group . /out/${name} >/dev/null`], { stdio: 'inherit' });
    console.log(`built dist/${name}  (${(statSync(join(root, 'dist', name)).size / 1048576).toFixed(1)} MiB, extension ${version}, id ${id})`);
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}
