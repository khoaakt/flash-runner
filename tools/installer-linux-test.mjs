// Checks dist/flash-runner_*_{amd64,arm64}.deb (tools/build-installer-linux.mjs) with podman, never touching this machine:
//   - the package contents, with dpkg-deb in a container: the CRX (signature, ID, version, same files as dist/extension), the four external
//     extension JSON files, the helper binary (byte-identical to native/out) and its native messaging manifests, modes and owners
//   - dpkg -i / -r / -P in a Debian container of the package's own architecture (amd64 runs emulated), and replacing the standalone flash-helper deb
//   - native/helper-test.mjs against the installed helper (the container's architecture), in the node image
//   - Chromium in a container installs the extension from the JSON files, enabled, and drops it again after `dpkg -r` (Chrome's documented
//     behaviour on Linux; Chromium is not Chrome, and no Chrome for Linux was available to test)
//   node tools/installer-linux-test.mjs [--no-chromium]
import { spawnSync, execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, statSync, existsSync, writeFileSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, dirname, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readCrx3, extensionId } from './installer/crx.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(root, 'dist');
let ok = true;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + String(extra).slice(0, 600))); ok &&= !!cond; };
const walk = (dir, base = dir, out = new Map()) => { for (const e of readdirSync(dir, { withFileTypes: true })) { const f = join(dir, e.name); if (e.isDirectory()) walk(f, base, out); else if (e.name !== '.DS_Store') out.set(relative(base, f), f); } return out; };
const sha = (f) => createHash('sha256').update(readFileSync(f)).digest('hex');
const IMAGES = { amd64: ['linux/amd64', 'debian:bookworm-slim'], arm64: ['linux/arm64', 'docker.io/arm64v8/debian:bookworm-slim'] };
const podman = (platform, image, mounts, script) => spawnSync('podman', ['run', '--rm', '--platform', platform, ...mounts.flatMap((m) => ['-v', m]), image, 'sh', '-c', script], { encoding: 'utf8', maxBuffer: 1 << 27 });

const work = mkdtempSync(join(dist, '.deb-test-'));   // under the repo so podman can mount it
process.on('exit', () => rmSync(work, { recursive: true, force: true }));
const debs = {};
for (const arch of Object.keys(IMAGES)) {
  const found = readdirSync(dist).filter((f) => new RegExp(`^flash-runner_[\\d.]+_${arch}\\.deb$`).test(f)).sort((a, b) => statSync(join(dist, b)).mtimeMs - statSync(join(dist, a)).mtimeMs);
  check(`dist has a ${arch} package`, found.length > 0, 'run: node tools/build-installer-linux.mjs');
  if (found.length) debs[arch] = found[0];
}
const source = JSON.parse(readFileSync(join(root, 'dist/extension/manifest.json'), 'utf8'));
const id = extensionId(Buffer.from(source.key, 'base64'));
const want = walk(join(root, 'dist/extension'));
const pem = join(root, 'vendor-private', 'extension-key.pem');
const secret = existsSync(pem) ? Buffer.from(readFileSync(pem, 'utf8').split('\n').filter((l) => l && !l.startsWith('-----'))[1] || '') : null;

for (const [arch, deb] of Object.entries(debs)) {
  const [platform, image] = IMAGES[arch], version = deb.match(/_([\d.]+)_/)[1], dir = join(work, arch);
  mkdirSync(dir);
  console.log(`--- ${deb}`);
  // contents
  const info = podman(platform, image, [`${dist}:/d:ro`, `${dir}:/w`], `dpkg-deb -I /d/${deb} && dpkg-deb -c /d/${deb} > /w/listing.txt && dpkg-deb -x /d/${deb} /w/x`);
  check('dpkg-deb reads the package', info.status === 0, info.stderr);
  const control = info.stdout;
  check('control: name, version, architecture', /Package: flash-runner\b/.test(control) && control.includes(`Version: ${version}`) && control.includes(`Architecture: ${arch}`), control);
  check('control: it replaces the standalone flash-helper package', /Conflicts: flash-helper/.test(control) && /Replaces: flash-helper/.test(control) && /Provides: flash-helper/.test(control));
  const listing = readFileSync(join(dir, 'listing.txt'), 'utf8').split('\n').filter(Boolean).map((l) => l.trim().split(/\s+/));
  const files = listing.filter((c) => c[0].startsWith('-')), modeOf = (p) => files.find((c) => c[5] === p || c[5] === '.' + p)?.[0];
  check('every file is owned by root', files.length > 10 && files.every((c) => c[1] === 'root/root'), files.find((c) => c[1] !== 'root/root')?.join(' '));
  const x = join(dir, 'x');
  const crxPath = join(x, 'opt/flash-runner/flash-runner.crx');
  check('the CRX is installed and world-readable', existsSync(crxPath) && modeOf('/opt/flash-runner/flash-runner.crx') === '-rw-r--r--', modeOf('/opt/flash-runner/flash-runner.crx'));
  let crx;
  try { crx = readCrx3(readFileSync(crxPath)); check(`the CRX signature verifies and its ID is ${id}`, crx.id === id, crx.id); } catch (e) { check('the CRX signature verifies', false, e.message); }
  if (crx) {
    const zip = join(dir, 'crx.zip'), unpacked = join(dir, 'crx');
    writeFileSync(zip, crx.archive); mkdirSync(unpacked);
    execFileSync('unzip', ['-q', zip, '-d', unpacked]);
    const got = walk(unpacked), installed = JSON.parse(readFileSync(join(unpacked, 'manifest.json'), 'utf8'));
    const missing = [...want.keys()].filter((k) => !got.has(k)), extra = [...got.keys()].filter((k) => !want.has(k));
    check(`the CRX holds the same ${want.size} files as dist/extension`, !missing.length && !extra.length, JSON.stringify({ missing: missing.slice(0, 5), extra: extra.slice(0, 5) }));
    const differ = [...want].filter(([k, f]) => k !== 'manifest.json' && got.has(k) && sha(f) !== sha(got.get(k))).map(([k]) => k);
    check('every file but manifest.json has the same bytes', !differ.length, differ.slice(0, 5).join(', '));
    check('manifest.json differs only in its version, which is the package version', JSON.stringify({ ...source, version: 'v' }) === JSON.stringify({ ...installed, version: 'v' }) && installed.version === version, installed.version);
    const jsons = ['opt/google/chrome/extensions', 'usr/share/google-chrome/extensions', 'usr/share/microsoft-edge/extensions', 'usr/share/chromium/extensions'].map((d) => join(x, d, `${id}.json`));
    check('Chrome, Edge and Chromium each get a JSON naming the CRX and its version', jsons.every((f) => existsSync(f) && JSON.stringify(JSON.parse(readFileSync(f, 'utf8'))) === JSON.stringify({ external_crx: '/opt/flash-runner/flash-runner.crx', external_version: version })), jsons.filter((f) => !existsSync(f)).join(', '));
    check('those JSON files are world-readable', ['opt/google/chrome/extensions', 'usr/share/google-chrome/extensions', 'usr/share/microsoft-edge/extensions', 'usr/share/chromium/extensions'].every((d) => modeOf(`/${d}/${id}.json`) === '-rw-r--r--'));
  }
  const helper = join(x, 'usr/lib/flash-helper/flash-helper');
  check('the helper is byte-identical to native/out and executable', existsSync(helper) && sha(helper) === sha(join(root, `native/out/flash-helper-linux-${arch}`)) && modeOf('/usr/lib/flash-helper/flash-helper') === '-rwxr-xr-x');
  const natives = ['etc/opt/chrome/native-messaging-hosts', 'etc/chromium/native-messaging-hosts', 'etc/opt/edge/native-messaging-hosts', 'etc/brave/native-messaging-hosts'].map((d) => join(x, d, 'com.flashrunner.sockets.json'));
  check('the helper is registered for Chrome, Chromium, Edge and Brave and allows only this extension', natives.every((f) => { if (!existsSync(f)) return false; const m = JSON.parse(readFileSync(f, 'utf8')); return m.name === 'com.flashrunner.sockets' && m.type === 'stdio' && m.path === '/usr/lib/flash-helper/flash-helper' && JSON.stringify(m.allowed_origins) === JSON.stringify([`chrome-extension://${id}/`]); }), natives.filter((f) => !existsSync(f)).join(', '));
  check('README and copyright (with the Adobe notice) are installed', existsSync(join(x, 'usr/share/doc/flash-runner/README')) && /Adobe/.test(readFileSync(join(x, 'usr/share/doc/flash-runner/copyright'), 'utf8')) && !/@[A-Z]+@/.test(readFileSync(join(x, 'usr/share/doc/flash-runner/README'), 'utf8')));
  const all = walk(x);
  check('no key file in the package', ![...all.keys()].some((k) => /\.pem$|extension-key/.test(k)) && (!secret || ![...all.values()].some((f) => readFileSync(f).includes(secret))));

  // install, remove, purge
  const ids = ['opt/google/chrome/extensions', 'usr/share/google-chrome/extensions', 'usr/share/microsoft-edge/extensions', 'usr/share/chromium/extensions'].map((d) => `/${d}/${id}.json`).join(' ');
  const natives2 = ['etc/opt/chrome', 'etc/chromium', 'etc/opt/edge', 'etc/brave'].map((d) => `/${d}/native-messaging-hosts/com.flashrunner.sockets.json`).join(' ');
  const standalone = arch === 'amd64' ? `dpkg -i /n/flash-helper_amd64.deb >/dev/null && dpkg -s flash-helper | grep -q 'Status: install ok installed' && echo standalone-helper-installed;` : '';
  const run = podman(platform, image, [`${dist}:/d:ro`, `${join(root, 'native/out')}:/n:ro`], `set -e; ${standalone}
    dpkg -i /d/${deb} 2>&1 | grep -E 'Removing|Unpacking flash-runner|Setting up flash-runner|installed|Restart' || true
    for f in /opt/flash-runner/flash-runner.crx /usr/lib/flash-helper/flash-helper ${ids} ${natives2}; do test -f "$f" || { echo "missing after install: $f"; exit 1; }; done
    echo helper-version=$(/usr/lib/flash-helper/flash-helper --version)
    ${arch === 'amd64' ? "dpkg -s flash-helper >/dev/null 2>&1 && echo standalone-still-installed || echo standalone-replaced" : ''}
    dpkg -s flash-runner | grep -E '^(Status|Provides)'
    dpkg -r flash-runner >/dev/null
    for f in /opt/flash-runner/flash-runner.crx /usr/lib/flash-helper/flash-helper ${ids} ${natives2}; do test ! -e "$f" || { echo "left after remove: $f"; exit 1; }; done
    dpkg -P flash-runner >/dev/null; echo remove-ok`);
  const out = run.stdout + run.stderr;
  check(`dpkg -i installs, dpkg -r removes every file, dpkg -P purges (${arch} container)`, run.status === 0 && /remove-ok/.test(out) && /helper-version=\d+/.test(out), out);
  if (arch === 'amd64') check('installing it replaces the standalone flash-helper package', /standalone-helper-installed/.test(out) && /standalone-replaced/.test(out), out);
}

// helper-test.mjs against the installed helper, in the node image of each architecture (the container's own architecture picks the package)
const NODE = { amd64: ['linux/amd64', 'docker.io/library/node:24-bookworm'], arm64: ['linux/arm64', 'docker.io/arm64v8/node:24-bookworm'] };
for (const [arch, [platform, image]] of Object.entries(NODE)) {
  const ranHelper = spawnSync('podman', ['run', '--rm', '--platform', platform, '-v', `${dist}:/d:ro`, '-v', `${join(root, 'native/helper-test.mjs')}:/t/helper-test.mjs:ro`, image, 'sh', '-c',
    'A=$(dpkg --print-architecture); dpkg -i /d/flash-runner_*_$A.deb >/dev/null && echo arch=$A && node /t/helper-test.mjs /usr/lib/flash-helper/flash-helper'], { encoding: 'utf8', maxBuffer: 1 << 26 });
  check(`native/helper-test.mjs passes against the installed helper (${ranHelper.stdout.match(/arch=(\w+)/)?.[1] || '?'}, ${arch === 'amd64' ? 'emulated' : 'native'})`, ranHelper.status === 0 && /all ok/.test(ranHelper.stdout) && ranHelper.stdout.includes(`arch=${arch}`), (ranHelper.stdout + ranHelper.stderr).split('\n').filter((l) => /FAIL|rror/.test(l)).join('; ') || ranHelper.stderr.slice(-300));
}

// Chromium picks the extension up from the JSON files, enabled and without a prompt; it is dropped again after the package is removed
if (process.argv.includes('--no-chromium')) console.log('SKIP Chromium (--no-chromium)');
else {
  const r = spawnSync('podman', ['run', '--rm', '--platform', 'linux/arm64', '-v', `${dist}:/d:ro`, '-v', `${join(root, 'tools/installer/chromium-check.mjs')}:/t/check.mjs:ro`, 'docker.io/arm64v8/node:24-bookworm', 'sh', '-c',
    `apt-get update -qq >/dev/null && apt-get install -y -qq chromium >/dev/null 2>&1; A=$(dpkg --print-architecture); dpkg -i /d/flash-runner_*_$A.deb >/dev/null
     echo "[1] $(node /t/check.mjs present ${id} /tmp/profile)"; echo "[1] exit=$?"
     dpkg -r flash-runner >/dev/null
     echo "[2] $(node /t/check.mjs absent ${id} /tmp/profile)"; echo "[2] exit=$?"`], { encoding: 'utf8', maxBuffer: 1 << 26, timeout: 900000 });
  const first = r.stdout.match(/\[1\] (\{.*\})/)?.[1], second = r.stdout.match(/\[2\] (\{.*\})/)?.[1];
  const present = first ? JSON.parse(first).found?.[0] : null;
  check('Chromium installs the extension from the package by itself: one card, ENABLED, at the package version', !!present && present.state === 'ENABLED' && /^\d+\.\d+\.\d+\.\d+$/.test(present.version) && !present.suspicious && !present.corrupt, first || (r.stdout + r.stderr).slice(-400));
  check('after dpkg -r Chromium removes it again at its next start', !!second && JSON.parse(second).found?.length === 0, second || (r.stdout + r.stderr).slice(-400));
}
console.log(ok ? 'all ok' : 'FAILED');
process.exit(ok ? 0 : 1);
