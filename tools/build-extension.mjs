// Assemble the unpacked Chrome extension into dist/extension (load it from chrome://extensions → Load unpacked).
//   node tools/build-extension.mjs [--zip] [--firefox]
//   --firefox also writes dist/firefox: the same extension with the manifest changes Firefox needs (tools/firefox/manifest-overlay.json, tools/firefox/overlay.mjs).
// Prerequisites: tools/build-wasm.sh, rootfs/build.sh, tools/build-guest.sh.
import { cpSync, mkdirSync, rmSync, existsSync, readdirSync, writeFileSync, readFileSync, statSync } from 'node:fs';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = process.env.EXT_DIR ? resolve(process.env.EXT_DIR) : join(root, 'dist', 'extension');   // EXT_DIR: build somewhere else (the same variable is read by ext-test.mjs)
const need = (p) => { if (!existsSync(join(root, p))) { console.error(`missing ${p} — run the build steps in README.md first`); process.exit(2); } };
need('web/emu.wasm'); need('rootfs/out/rootfs.tar'); need('guest/out/pepper-host');

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync(join(root, 'extension'), out, { recursive: true });

// Popup and settings page: built from ui/ (Vite + React + shadcn/ui); the built pages land where the manifest expects them.
const ui = join(root, 'ui');
if (!existsSync(join(ui, 'node_modules'))) { console.error('ui/node_modules is missing — run: cd ui && npm ci'); process.exit(2); }
execFileSync('npm', ['run', 'build', '--silent'], { cwd: ui, stdio: 'inherit', shell: process.platform === 'win32' });
cpSync(join(ui, 'dist', 'popup'), join(out, 'popup'), { recursive: true });
cpSync(join(ui, 'dist', 'options', 'options.html'), join(out, 'options', 'options.html'));
cpSync(join(ui, 'dist', 'ui-assets'), join(out, 'ui-assets'), { recursive: true });

// shared runtime code (single source of truth is web/)
const WEB = ['emu.js', 'pepper.js', 'pp-consts.js', 'ppapi-sigs.js', 'ppb-impl.js', 'ppb-net.js', 'ppb-audio.js', 'ppb-flash.js', 'ppb-misc.js',
  'ppb-gles2.js', 'ppb-graphics3d.js', 'gles2-context.js', 'present.js', 'persist.js', 'net-cache.js', 'scripting.js', 'ppb-sockets.js', 'socket-bridge.js', 'worker.js', 'loader.js', 'frame-services.js', 'audio-worklet.js', 'plugin-accelerators.js', 'nanojit-capture.js', 'nanojit-leaf.js', 'nanojit-loop.js', 'nanojit-memory.js', 'nanojit-number.js', 'lir-policy.js'];
mkdirSync(join(out, 'web'), { recursive: true });
for (const f of WEB) cpSync(join(root, 'web', f), join(out, 'web', f));
// every relative import of a bundled module must itself be bundled (a missing one only fails at runtime in the worker)
for (const f of WEB) for (const m of readFileSync(join(root, 'web', f), 'utf8').matchAll(/from '(\.\.?\/[\w./-]+\.js)'|import\('(\.\.?\/[\w./-]+\.js)'\)/g)) {
  const dep = join(dirname(f), m[1] || m[2]).replace(/^\.\//, '');
  if (dep.startsWith('..')) continue;
  if (!WEB.includes(dep)) { console.error(`web/${f} imports ./${dep}, which is not in the extension file list (tools/build-extension.mjs)`); process.exit(1); }
}

mkdirSync(join(out, 'assets'), { recursive: true });
cpSync(process.env.EMU_WASM || join(root, 'web/emu.wasm'), join(out, 'assets/emu.wasm'));   // EMU_WASM: build against a fixed emulator while web/emu.wasm is being worked on
cpSync(join(root, 'rootfs/out/rootfs.tar'), join(out, 'assets/rootfs.tar'));
cpSync(join(root, 'guest/out/pepper-host'), join(out, 'assets/pepper-host'));
const aotSource = process.env.AOT_DIR ? resolve(process.env.AOT_DIR) : join(root, 'web/runtime-aot');
execFileSync(process.execPath, [join(root, 'tools/build-plugin-aot.mjs'), '--out', aotSource], { cwd: root, stdio: 'inherit' });
// Ship only executable bundles referenced by the current image manifest. Offline
// .wasm/.json inspection outputs and stale builds remain local, not triplicated
// in the installable extension.
const aotTarget = join(out, 'assets/runtime-aot');
const aotManifest = JSON.parse(readFileSync(join(aotSource, 'manifest.json'), 'utf8'));
mkdirSync(aotTarget, { recursive: true });

// Adobe Flash Player runtimes shipped inside the extension: ia32 PPAPI builds from before Adobe's end-of-life block (kept out of the repo in vendor-private/).
const RUNTIMES = [
  { id: '32.0.0.363', file: 'libpepflashplayer-32.0.0.363.so' },
  { id: '26.0.0.151', file: 'libpepflashplayer-26.0.0.151.so' },
];
mkdirSync(join(out, 'assets', 'runtimes'), { recursive: true });
const bundled = [];
for (const r of RUNTIMES) {
  const src = join(root, 'vendor-private', r.file);
  if (!existsSync(src)) { console.error(`missing vendor-private/${r.file} — the extension bundles the Flash runtimes from there`); process.exit(2); }
  const bytes = readFileSync(src);
  if (!(bytes.length > 64 && bytes[0] === 0x7f && bytes[1] === 0x45 && bytes[2] === 0x4c && bytes[3] === 0x46 && bytes[4] === 1 && bytes.readUInt16LE(18) === 3)) { console.error(`vendor-private/${r.file} is not a 32-bit x86 ELF`); process.exit(2); }
  const file = `flash-${r.id}.so`;
  writeFileSync(join(out, 'assets', 'runtimes', file), bytes);
  bundled.push({ id: r.id, name: `Adobe Flash Player ${r.id}`, file, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
}
writeFileSync(join(out, 'assets', 'runtimes', 'runtimes.json'), JSON.stringify(bundled, null, 2));
const packagedAot = { ...aotManifest, images: aotManifest.images.filter((image) => bundled.some((runtime) => runtime.sha256 === image.sha256)) };
writeFileSync(join(aotTarget, 'manifest.json'), JSON.stringify(packagedAot));
for (const image of packagedAot.images) for (const artifact of image.artifacts) {
  if (!/^[a-zA-Z0-9.-]+\.aot(?:\.gz)?$/.test(artifact.file)) throw new Error('invalid generated AOT filename');
  cpSync(join(aotSource, artifact.file), join(aotTarget, artifact.file));
}

// Socket helper installers (tools/build-helper.sh): the settings page offers the one for the user's OS as a download
const HELPERS = ['FlashHelper-Setup.exe', 'FlashHelper.pkg', 'flash-helper_amd64.deb'];
mkdirSync(join(out, 'helper'), { recursive: true });
for (const f of HELPERS) {
  const src = join(root, 'native', 'out', f);
  if (!existsSync(src)) {
    if (process.env.NO_HELPER) { console.log(`helper installer ${f} not bundled (NO_HELPER)`); continue; }
    console.error(`missing native/out/${f} — run tools/build-helper.sh (needs Go and podman; NO_HELPER=1 builds without the installers)`); process.exit(2);
  }
  cpSync(src, join(out, 'helper', f));
}

writeFileSync(join(out, 'NOTICE.txt'), `flash-runner
=============
This build contains Adobe Flash Player (assets/runtimes/*.so: 32.0.0.363 and 26.0.0.151, ia32 PPAPI). Adobe Flash Player is proprietary
software of Adobe Inc. / HARMAN; it is included unmodified and is not covered by this project's licence. It is bundled for personal use:
check the licence terms before passing this build to anyone else. This project is not affiliated with or endorsed by Adobe.

Bundled third-party components
- assets/rootfs.tar: a minimal Debian GNU/Linux (bookworm, i386) userland — GNU C Library, libstdc++, libgcc_s (LGPL-2.1+/GPL with
  runtime exception). Source: https://packages.debian.org/source/bookworm/glibc and /gcc-12
- assets/emu.wasm: x86-32 emulator (Rust). Uses iced-x86 (MIT).
- PPAPI interface tables derived from Chromium's ppapi headers (BSD-3-Clause, The Chromium Authors).
- Settings and popup: React, Radix UI, Tailwind CSS, shadcn/ui, lucide icons, sonner (MIT).
- helper/: the socket helper (Go standard library; golang.org/x/sys, BSD-3-Clause) packaged as installers for Windows, macOS and Linux.
`);
const size = (p) => { let t = 0; const walk = (d) => { for (const e of readdirSync(d, { withFileTypes: true })) { const f = join(d, e.name); e.isDirectory() ? walk(f) : (t += statSync(f).size); } }; walk(p); return t; };
console.log(`built ${out}  (${(size(out) / 1048576).toFixed(1)} MiB)`);
// Zip a directory's contents: `zip` where it exists, otherwise tar's zip writer (Windows ships bsdtar).
// On Windows the tar on PATH may be GNU tar (Git Bash), which cannot write zip: use the system's bsdtar.
const TAR = process.platform === 'win32' ? join(process.env.SystemRoot || 'C:/Windows', 'System32', 'tar.exe') : 'tar';
function makeZip(dir, file) {
  rmSync(file, { force: true });
  try { execFileSync('zip', ['-qr', file, '.'], { cwd: dir }); } catch (e) { if (e.code !== 'ENOENT') throw e; execFileSync(TAR, ['--format', 'zip', '-cf', file, '-C', dir, ...readdirSync(dir)]); }
}
if (process.argv.includes('--zip')) {
  const zip = join(root, 'dist', 'flash-runner-extension.zip');
  makeZip(out, zip);
  console.log('zip:', zip);
}

if (process.argv.includes('--firefox')) {
  const ffOut = process.env.FIREFOX_DIR ? resolve(process.env.FIREFOX_DIR) : join(root, 'dist', 'firefox');
  rmSync(ffOut, { recursive: true, force: true });
  cpSync(out, ffOut, { recursive: true });
  const { firefoxManifest, gecko } = await import('./firefox/overlay.mjs');
  writeFileSync(join(ffOut, 'manifest.json'), JSON.stringify(firefoxManifest(JSON.parse(readFileSync(join(ffOut, 'manifest.json'), 'utf8'))), null, 2) + '\n');
  console.log(`built ${ffOut}  (Firefox ${gecko.strict_min_version}+, add-on id ${gecko.id})`);
  if (process.argv.includes('--zip')) {
    const xpi = join(root, 'dist', 'flash-runner-firefox.xpi');
    makeZip(ffOut, xpi);
    console.log('xpi (unsigned):', xpi);
  }
}
