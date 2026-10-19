// Checks dist/FlashRunner.pkg (tools/build-installer.mjs) without installing it: pkgutil expands it into a scratch directory, so nothing here
// needs root or touches /Applications or /Library. It covers the layout the installer would lay down, the helper it carries (against native/out
// and native/helper-test.mjs), the install scripts, and the extension loaded in a real Chrome from a copy of the install path.
//   node tools/installer-test.mjs [--no-chrome]
// The one thing it cannot do is run the Installer itself: double-click the package and look at /Applications/Flash Runner.
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, statSync, existsSync, writeFileSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, dirname, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { INSTALL_DIR, EXTENSION_DIR } from './installer/layout.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const pkg = join(root, 'dist', 'FlashRunner.pkg');
if (process.platform !== 'darwin' || !existsSync(pkg)) { console.error('needs macOS and dist/FlashRunner.pkg: run node tools/build-extension.mjs && node tools/build-installer.mjs'); process.exit(2); }
let ok = true;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + extra)); ok &&= !!cond; };

const work = mkdtempSync(join(tmpdir(), 'flash-installer-test-'));
process.on('exit', () => rmSync(work, { recursive: true, force: true }));
const walk = (dir, base = dir, out = new Map()) => {      // relative path -> absolute path of every file (not directories, not .DS_Store, which pkgbuild drops)
  for (const e of readdirSync(dir, { withFileTypes: true })) { const f = join(dir, e.name); if (e.isDirectory()) walk(f, base, out); else if (e.name !== '.DS_Store') out.set(relative(base, f), f); }
  return out;
};
const sha = (f) => createHash('sha256').update(readFileSync(f)).digest('hex');
const idOf = (key) => [...createHash('sha256').update(Buffer.from(key, 'base64')).digest().subarray(0, 16)].map((b) => String.fromCharCode(97 + (b >> 4)) + String.fromCharCode(97 + (b & 15))).join('');
const run = (cmd, args, env = {}) => spawnSync(cmd, args, { env: { ...process.env, ...env }, encoding: 'utf8', maxBuffer: 1 << 26 });

const x = join(work, 'x');
execFileSync('pkgutil', ['--expand-full', pkg, x], { stdio: 'ignore' });   // prints "write: Permission denied" for com.apple.provenance; harmless
const extPkg = join(x, 'FlashRunnerExtension.pkg'), helperPkg = join(x, 'FlashHelper.pkg');
const extRoot = join(extPkg, 'Payload'), helperRoot = join(helperPkg, 'Payload'), extDir = join(extRoot, EXTENSION_DIR);

// --- the installer's own pages and choices
const distribution = readFileSync(join(x, 'Distribution'), 'utf8');
check('the online game helper is a choice, selected by default', /<choice id="helper"[^>]*start_selected="true"/.test(distribution));
check('the extension is a choice, selected by default', /<choice id="extension"[^>]*start_selected="true"/.test(distribution));
check('both packages are in the installer', distribution.includes('#FlashHelper.pkg') && distribution.includes('#FlashRunnerExtension.pkg') && existsSync(extRoot) && existsSync(helperRoot));
const pages = ['welcome.html', 'conclusion.html'].map((f) => readFileSync(join(x, 'Resources', f), 'utf8'));
check('no template placeholder is left in the pages or the distribution', ![...pages, distribution].some((t) => /@[A-Z_]+@/.test(t)));
check('the last page tells where the extension is and how to load it', pages[1].includes(EXTENSION_DIR) && /Load unpacked/.test(pages[1]) && /Developer mode/.test(pages[1]));
const visible = (html) => html.replace(/<style[\s\S]*?<\/style>/g, '').replace(/<[^>]+>/g, ' ').replace(/&[#\w]+;/g, ' ').replace(/\s+/g, ' ').trim();
check(`the last page is short enough to read without scrolling (${visible(pages[1]).length} characters of text)`, visible(pages[1]).length < 450, visible(pages[1]));
const readme = existsSync(join(extRoot, INSTALL_DIR, 'README.txt')) ? readFileSync(join(extRoot, INSTALL_DIR, 'README.txt'), 'utf8') : '';
check('README.txt next to the extension covers loading, updating and removing', readme.includes(EXTENSION_DIR) && /Load unpacked/.test(readme) && /Updating/.test(readme) && /Uninstall\.command/.test(readme) && !/@[A-Z_]+@/.test(readme));
check('the first page says Adobe Flash Player is bundled for personal use', /Adobe Flash Player/.test(pages[0]) && /personal use/.test(pages[0]));

// --- the extension
const source = JSON.parse(readFileSync(join(root, 'dist/extension/manifest.json'), 'utf8'));
const installed = JSON.parse(readFileSync(join(extDir, 'manifest.json'), 'utf8'));
const [major, minor] = source.version.split('.');
check('installed manifest carries a stamped build version (major.minor.days.minutes)', new RegExp(`^${major}\\.${minor}\\.\\d+\\.\\d+$`).test(installed.version) && installed.version !== source.version, installed.version);
const declared = readFileSync(join(extPkg, 'PackageInfo'), 'utf8').match(/<pkg-info[^>]* version="([^"]+)"/)?.[1];
check('the package and the distribution declare that same version', declared === installed.version && distribution.includes(`version="${installed.version}"`), `${declared} vs ${installed.version}`);
const id = idOf(installed.key || '');
check('the extension ID (from the manifest key) is the one the helper allows', !!installed.key && installed.key === JSON.parse(readFileSync(join(root, 'extension/manifest.json'), 'utf8')).key && readFileSync(join(root, 'native/helper/main.go'), 'utf8').includes(`chrome-extension://${id}/`), id);
const want = walk(join(root, 'dist/extension')), got = walk(extDir);
const missing = [...want.keys()].filter((k) => !got.has(k)), extra = [...got.keys()].filter((k) => !want.has(k));
check(`${want.size} files, the same set as dist/extension`, !missing.length && !extra.length && want.size > 50, JSON.stringify({ missing: missing.slice(0, 5), extra: extra.slice(0, 5) }));
const differ = [...want].filter(([k, f]) => k !== 'manifest.json' && got.has(k) && sha(f) !== sha(got.get(k))).map(([k]) => k);
check('every file but manifest.json has the same bytes', !differ.length, differ.slice(0, 5).join(', '));
check('manifest.json differs only in its version', JSON.stringify({ ...source, version: 'v' }) === JSON.stringify({ ...installed, version: 'v' }));
check('no ._ file in the extension folder', ![...got.keys()].some((k) => k.split('/').some((p) => p.startsWith('._'))));
const pem = join(root, 'vendor-private', 'extension-key.pem');
const secret = existsSync(pem) ? Buffer.from(readFileSync(pem, 'utf8').split('\n').filter((l) => l && !l.startsWith('-----'))[1] || '') : null;
const everything = walk(x);
check('no key file in the installer', ![...everything.keys()].some((k) => /\.pem$|extension-key/.test(k)));
check(secret ? 'the extension signing key is nowhere in the installer' : 'the extension signing key is nowhere in the installer (key file absent: skipped)', !secret || ![...everything.values()].some((f) => readFileSync(f).includes(secret)));

// --- the online game helper
const binary = join(helperRoot, 'Library/Application Support/FlashRunnerHelper/flash-helper');
check('the helper binary is byte-identical to native/out/flash-helper-darwin', existsSync(binary) && sha(binary) === sha(join(root, 'native/out/flash-helper-darwin')));
const manifests = [...walk(helperRoot).keys()].filter((k) => k.endsWith('/com.flashrunner.sockets.json'));
const registered = manifests.map((k) => JSON.parse(readFileSync(join(helperRoot, k), 'utf8')));
check('registered for Chrome, Chromium, Edge, Brave and Vivaldi', ['Google/Chrome', 'Chromium', 'Microsoft/Edge', 'BraveSoftware', 'Vivaldi'].every((b) => manifests.some((k) => k.includes(b))), manifests.join(', '));
check('every registration allows only this extension and points at the installed helper',
  registered.length === manifests.length && registered.every((m) => m.name === 'com.flashrunner.sockets' && m.type === 'stdio' && m.path === '/Library/Application Support/FlashRunnerHelper/flash-helper' && JSON.stringify(m.allowed_origins) === JSON.stringify([`chrome-extension://${id}/`])), JSON.stringify(registered[0]));
const helperRun = run('node', [join(root, 'native/helper-test.mjs'), binary]);
check('native/helper-test.mjs passes against the packaged helper', helperRun.status === 0 && /all ok/.test(helperRun.stdout), (helperRun.stdout + helperRun.stderr).split('\n').filter((l) => /FAIL/.test(l)).join('; '));

// --- the install scripts (never run as shipped: preinstall would delete a real installed copy)
const executable = (f) => existsSync(f) && (statSync(f).mode & 0o111) !== 0;
check('preinstall, postinstall and Uninstall.command are executable', executable(join(extPkg, 'Scripts/preinstall')) && executable(join(extPkg, 'Scripts/postinstall')) && executable(join(extRoot, INSTALL_DIR, 'Uninstall.command')));
check('preinstall clears exactly the extension folder', readFileSync(join(extPkg, 'Scripts/preinstall'), 'utf8').includes(`rm -rf "${EXTENSION_DIR}"`));
const scratch = join(work, 'post/extension');
mkdirSync(join(scratch, 'web'), { recursive: true });
for (const f of ['manifest.json', 'web/a.js', '._manifest.json', 'web/._a.js']) writeFileSync(join(scratch, f), 'x');
const post = run('/bin/sh', [join(extPkg, 'Scripts/postinstall')], { FLASH_EXTENSION_DIR: scratch });
const left = [...walk(scratch).keys()].sort();
check('postinstall deletes ._ files and nothing else', post.status === 0 && JSON.stringify(left) === JSON.stringify(['manifest.json', 'web/a.js']), JSON.stringify(left));
check('postinstall without an extension folder is a no-op', run('/bin/sh', [join(extPkg, 'Scripts/postinstall')], { FLASH_EXTENSION_DIR: join(work, 'absent') }).status === 0);
check('postinstall defaults to the extension folder', readFileSync(join(extPkg, 'Scripts/postinstall'), 'utf8').includes(`:-${EXTENSION_DIR}}`));

// Uninstall.command on a copy of both payloads (plus the ._ leftovers FlashHelper.pkg is known to carry)
const fake = join(work, 'fake-root');
mkdirSync(fake);
execFileSync('ditto', [extRoot, fake]); execFileSync('ditto', [helperRoot, fake]);
for (const m of manifests) writeFileSync(join(fake, dirname(m), '._com.flashrunner.sockets.json'), 'x');
const before = walk(fake).size;
const un = run('/bin/sh', [join(fake, INSTALL_DIR, 'Uninstall.command')], { FLASH_UNINSTALL_ROOT: fake });
const remaining = [...walk(fake).keys()];
check(`Uninstall.command removes all ${before} installed files and registrations`, un.status === 0 && before > 50 && remaining.length === 0, `${un.stderr.trim()} left: ${remaining.slice(0, 5).join(', ')}`);

// --- the extension in a real Chrome, loaded from the install path's layout (a path with a space in it), with its bundled 32.0.0.363 runtime
const chrome = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
if (process.argv.includes('--no-chrome') || !existsSync(chrome)) console.log('SKIP extension in Chrome (' + (process.argv.includes('--no-chrome') ? '--no-chrome' : 'no Chrome found') + ')');
else {
  const e2e = run('node', [join(root, 'tools/ext-test.mjs'), '--bundled', '--native', '--page', 'tests/ext/sock.html', '--seconds', '15', '--shots', join(work, 'shots')], { EXT_DIR: extDir });
  const lines = e2e.stdout.split('\n').filter((l) => /^(PASS|FAIL) /.test(l));
  const failed = lines.filter((l) => l.startsWith('FAIL'));
  check(`the extension from ${EXTENSION_DIR}'s layout passes ext-test in Chrome (${lines.length - failed.length} checks, incl. a socket echo through the helper)`, e2e.status === 0 && lines.length >= 8 && !failed.length, failed.join(' | ') || e2e.stderr.slice(-300));
}
console.log(ok ? 'all ok' : 'FAILED');
process.exit(ok ? 0 : 1);
