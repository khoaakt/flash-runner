// Build the macOS installer dist/FlashRunner.pkg: the unpacked extension (installed in /Applications/Flash Runner/extension) and the
// online game helper (native/out/FlashHelper.pkg, reused as it is) as two choices of one installer; the helper is selected by default.
//   node tools/build-extension.mjs && node tools/build-installer.mjs
// Prerequisites: dist/extension (build-extension.mjs) and native/out (tools/build-helper.sh); macOS, for pkgbuild / productbuild.
// An installer cannot switch the extension on: Chrome 155 installs a CRX registered through External Extensions, then shows it as
// "not from the Chrome Web Store" with a locked toggle. So the extension is still added once by hand (chrome://extensions -> Load
// unpacked), which the last page of the installer explains. tools/installer-test.mjs checks the package.
import { mkdirSync, mkdtempSync, rmSync, existsSync, readFileSync, writeFileSync, chmodSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { INSTALL_DIR, EXTENSION_DIR, stampVersion } from './installer/layout.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const res = join(root, 'tools', 'installer');
const out = join(root, 'dist', 'FlashRunner.pkg');
if (process.platform !== 'darwin') { console.error('the installer is built with pkgbuild and productbuild: run this on macOS'); process.exit(2); }
const need = (p, hint) => { if (!existsSync(join(root, p))) { console.error(`missing ${p} — ${hint}`); process.exit(2); } };
need('dist/extension/manifest.json', 'run: node tools/build-extension.mjs');
need('native/out/FlashHelper.pkg', 'run: tools/build-helper.sh');
need('native/out/flash-helper-darwin', 'run: tools/build-helper.sh');

const manifest = JSON.parse(readFileSync(join(root, 'dist/extension/manifest.json'), 'utf8'));
if (!manifest.key) { console.error('dist/extension/manifest.json has no "key": without it the extension ID differs from the one the helper allows'); process.exit(2); }
const version = stampVersion(manifest.version);
const helperVersion = execFileSync(join(root, 'native/out/flash-helper-darwin'), ['--version']).toString().trim();

const work = mkdtempSync(join(tmpdir(), 'flash-runner-installer-'));
try {
  // the extension folder, with this installer's version in its manifest (the unpacked dist/extension keeps the source version)
  // Not fs.cpSync: it copies extended attributes, and pkgbuild stores every com.apple.provenance as a ._ entry in the package (xattr -d
  // silently keeps that attribute, and a process that is itself provenance-tracked tags the files it creates anyway, so ._ entries can
  // remain, and pkgbuild then prints "write: Permission denied"). The postinstall script deletes any ._ file the Installer leaves behind.
  const stage = join(work, 'root');
  const target = join(stage, EXTENSION_DIR);
  mkdirSync(dirname(target), { recursive: true });
  execFileSync('ditto', ['--norsrc', '--noextattr', '--noacl', join(root, 'dist/extension'), target]);
  writeFileSync(join(target, 'manifest.json'), JSON.stringify({ ...manifest, version }, null, 2) + '\n');
  const install = (from, to) => { writeFileSync(to, readFileSync(join(res, from))); chmodSync(to, 0o755); };
  const fill = (s) => s.replaceAll('@VERSION@', version).replaceAll('@HELPER_VERSION@', helperVersion).replaceAll('@EXTENSION_DIR@', EXTENSION_DIR).replaceAll('@INSTALL_DIR@', INSTALL_DIR);
  install('Uninstall.command', join(stage, INSTALL_DIR, 'Uninstall.command'));
  writeFileSync(join(stage, INSTALL_DIR, 'README.txt'), fill(readFileSync(join(res, 'README.txt'), 'utf8')));

  const scripts = join(work, 'scripts'), parts = join(work, 'parts'), resources = join(work, 'resources');
  for (const d of [scripts, parts, resources]) mkdirSync(d);
  install('preinstall', join(scripts, 'preinstall'));
  install('postinstall', join(scripts, 'postinstall'));
  execFileSync('pkgbuild', ['--quiet', '--root', stage, '--identifier', 'com.flashrunner.extension', '--version', version, '--install-location', '/',
    '--ownership', 'recommended', '--scripts', scripts, join(parts, 'FlashRunnerExtension.pkg')], { stdio: 'inherit' });
  execFileSync('ditto', ['--norsrc', '--noextattr', '--noacl', join(root, 'native/out/FlashHelper.pkg'), join(parts, 'FlashHelper.pkg')]);

  for (const f of ['welcome.html', 'conclusion.html']) writeFileSync(join(resources, f), fill(readFileSync(join(res, f), 'utf8')));
  writeFileSync(join(work, 'Distribution.xml'), fill(readFileSync(join(res, 'Distribution.xml'), 'utf8')));

  mkdirSync(dirname(out), { recursive: true });
  rmSync(out, { force: true });
  execFileSync('productbuild', ['--distribution', join(work, 'Distribution.xml'), '--resources', resources, '--package-path', parts, out], { stdio: 'inherit' });
} finally {
  rmSync(work, { recursive: true, force: true });
}
console.log(`built ${out}  (${(statSync(out).size / 1048576).toFixed(1)} MiB, extension ${version}, helper ${helperVersion}; unsigned: a downloaded copy needs Privacy & Security -> Open Anyway)`);
