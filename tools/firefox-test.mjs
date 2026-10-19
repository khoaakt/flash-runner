// Drive a real Firefox (through geckodriver, WebDriver over HTTP, no npm dependencies) with the Firefox build of the extension installed as a
// temporary add-on, open a test page and report what happened.
//   node tools/build-extension.mjs --firefox
//   node tools/firefox-test.mjs --geckodriver <path> [--firefox <firefox binary>] [--addon dist/firefox] [--page tests/ext/compat.html]
//                               [--seconds 15] [--shots dir] [--headed] [--setting key=<json>]
//                               [--clicks "x,y;x,y"] [--keys "a,b,Enter"]   real mouse clicks (page coordinates) and key presses after the first wait
// Needs: Firefox 128 or newer and geckodriver (https://github.com/mozilla/geckodriver/releases). Exit status 0 when every check passed.
import { spawn } from 'node:child_process';
import { createReadStream, existsSync, mkdirSync, statSync, writeFileSync, readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (n, d) => { const i = process.argv.indexOf(n); return i < 0 ? d : (process.argv[i + 1] ?? true); };
const geckodriver = arg('--geckodriver', process.env.GECKODRIVER);
if (!geckodriver) { console.error('give --geckodriver <path> (or set GECKODRIVER)'); process.exit(2); }
const firefox = arg('--firefox', process.env.FIREFOX || (process.platform === 'win32' ? join(process.env.LOCALAPPDATA || '', 'Mozilla Firefox', 'firefox.exe') : undefined));
const addon = resolve(arg('--addon', join(root, 'dist/firefox')));
const page = arg('--page', 'tests/ext/compat.html');
const seconds = +arg('--seconds', 15);
const shots = resolve(arg('--shots', join(root, 'tests/out')));
const headed = process.argv.includes('--headed');
const clicks = String(arg('--clicks', '')), keys = String(arg('--keys', ''));
const settings = { enabled: true, openSwfLinks: true, autoplayMax: 8, clickToPlay: false, log: true, fpsLog: true };
process.argv.forEach((a, i) => { if (a === '--setting') { const kv = process.argv[i + 1] || ''; const j = kv.indexOf('='); settings[kv.slice(0, j)] = JSON.parse(kv.slice(j + 1)); } });
// the add-on's moz-extension:// host is a random UUID per profile unless the profile pins it
const GECKO_ID = JSON.parse(readFileSync(join(root, 'tools/firefox/manifest-overlay.json'), 'utf8')).set.browser_specific_settings.gecko.id;
const UUID = '5a1f0c7e-3b2d-4e6a-9f10-2c8d7e4b6a11';
const EXT = `moz-extension://${UUID}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const check = (name, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + name + (ok ? '' : '  ' + extra)); if (!ok) failed++; };

const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.swf': 'application/x-shockwave-flash', '.png': 'image/png', '.json': 'application/json', '.mp4': 'video/mp4', '.txt': 'text/plain' };
const refHits = [];   // what /echo-ref saw: tests/swf/net.swf asks it for its own Referer and Origin
const server = createServer((req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p === '/echo-ref') { refHits.push({ method: req.method, referer: req.headers.referer || '-', origin: req.headers.origin || '-' }); res.writeHead(200, { 'content-type': 'text/plain', 'access-control-allow-origin': '*' }); res.end(`referer=${req.headers.referer || '-'} origin=${req.headers.origin || '-'}`); return; }
  const file = join(root, p);
  if (!file.startsWith(root) || !existsSync(file) || !statSync(file).isFile()) { res.writeHead(404).end(); return; }
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream', 'content-length': statSync(file).size });
  createReadStream(file).pipe(res);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

const gdPort = 4400 + Math.floor(Math.random() * 500);
const gdLog = [];
const gd = spawn(geckodriver, ['--port', String(gdPort), '--log', 'warn', '--allow-system-access'], { stdio: ['ignore', 'pipe', 'pipe'] });
gd.stdout.on('data', (d) => gdLog.push(String(d))); gd.stderr.on('data', (d) => gdLog.push(String(d)));
const wd = async (method, path, body) => {
  const r = await fetch(`http://127.0.0.1:${gdPort}${path}`, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  const j = await r.json();
  if (j.value?.error) throw new Error(`${method} ${path}: ${j.value.error}: ${j.value.message}`);
  return j.value;
};
let sid = null;
const cleanup = async () => { try { if (sid) await wd('DELETE', `/session/${sid}`); } catch {} try { gd.kill(); } catch {} server.close(); };
process.on('exit', () => { try { gd.kill(); } catch {} });
setTimeout(() => { console.error('WATCHDOG: test exceeded 170 s'); cleanup().then(() => process.exit(3)); }, 170000).unref();

for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${gdPort}/status`); break; } catch { await sleep(200); } }
const prefs = {
  'extensions.webextensions.uuids': JSON.stringify({ [GECKO_ID]: UUID }),
  'devtools.console.stdout.content': true,       // page and worker console output goes to Firefox's stdout, which geckodriver forwards
  'media.autoplay.default': 0,
  'xpinstall.signatures.required': false,
};
const session = await wd('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'firefox', acceptInsecureCerts: true,
  'moz:firefoxOptions': { ...(firefox ? { binary: firefox } : {}), args: headed ? [] : ['-headless'], prefs, log: { level: 'info' } } } } });
sid = session.sessionId;
console.log('Firefox', session.capabilities.browserVersion, '| geckodriver session', sid.slice(0, 8));
const S = `/session/${sid}`;
const exec = async (script, args = []) => wd('POST', `${S}/execute/async`, { script: `const done = arguments[arguments.length - 1]; Promise.resolve((async () => { ${script} })()).then((v) => done(v), (e) => done({ __error: String(e && e.stack || e) }));`, args });

try {
  const id = await wd('POST', `${S}/moz/addon/install`, { path: addon, temporary: true });
  check('add-on installed as a temporary add-on', id === GECKO_ID, `got ${id}`);

  // settings page: stored settings seed, and what the browser granted
  // WebDriver may not navigate to moz-extension:// itself: open the page from the browser's privileged context, then drive that tab as usual
  const mainTab = await wd('GET', `${S}/window`);
  await wd('POST', `${S}/moz/context`, { context: 'chrome' });
  await wd('POST', `${S}/execute/sync`, { script: 'const win = Services.wm.getMostRecentWindow("navigator:browser"); win.gBrowser.selectedTab = win.gBrowser.addTab(arguments[0], { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() });', args: [`${EXT}/options/options.html`] });
  await wd('POST', `${S}/moz/context`, { context: 'content' });
  await sleep(1500);
  const handles = await wd('GET', `${S}/window/handles`);
  const optTab = handles.find((h) => h !== mainTab);
  await wd('POST', `${S}/window`, { handle: optTab });
  const opt = await exec(`await chrome.storage.local.set(${JSON.stringify(settings)}); const perms = await chrome.permissions.getAll(); return { title: document.title, origins: perms.origins, perms: perms.permissions, text: document.body.innerText.slice(0, 200) };`);
  check('settings page opens and has storage', !!opt && !opt.__error, JSON.stringify(opt));
  console.log('   host permissions granted:', JSON.stringify(opt?.origins), '| permissions:', JSON.stringify(opt?.perms));
  check('host permission for all sites is granted', (opt?.origins || []).some((o) => o === '<all_urls>' || o === '*://*/*'), 'content scripts will not run without it');
  const hello = await exec(`return await chrome.runtime.sendNativeMessage('com.flashrunner.sockets', { t: 'hello', rid: 1 });`);
  check('the socket helper answers through Firefox native messaging', !!hello && hello.ok === true && hello.version === 2, JSON.stringify(hello));
  const runtimes = await exec(`const m = await import('./runtime-store.js'); const r = await m.getActiveRuntime(); return r ? { bundled: !!r.bundled, size: r.size, id: r.id } : null;`);
  check('a bundled Flash runtime is available', !!runtimes && !runtimes.__error && runtimes.bundled, JSON.stringify(runtimes));

  // the movie page, in the first tab
  await wd('POST', `${S}/window`, { handle: mainTab });
  await wd('POST', `${S}/url`, { url: `${base}/${page}` });
  await sleep(seconds * 1000);
  const state = await exec(`const f = [...document.querySelectorAll('iframe[data-flash-runner]')]; return { frames: f.length, src: f[0] && f[0].src, embeds: document.querySelectorAll('embed,object').length, ready: window.__flashRunnerReady || null };`);
  check('the Flash element was replaced by a player iframe', state && state.frames >= 1, JSON.stringify(state));
  check('the player frame is served from the add-on', !!state?.src && state.src.startsWith(EXT + '/'), String(state?.src));
  if (refHits.length) {
    const want = `${base}/${page}`.split('?')[0];
    console.log('   /echo-ref saw', JSON.stringify(refHits));
    check('Referer and Origin of the movie requests are those of the embedding page', refHits.every((h) => h.referer.startsWith(want) && h.origin === base), JSON.stringify(refHits));
  }
  if (clicks || keys) {
    const KEY = { Enter: '\uE007', Backspace: '\uE003', Tab: '\uE004', Escape: '\uE00C', ArrowLeft: '\uE012', ArrowRight: '\uE014', Space: ' ' };
    const mouse = clicks.split(';').filter(Boolean).flatMap((c) => { const [x, y] = c.split(',').map(Number); return [{ type: 'pointerMove', x, y, origin: 'viewport' }, { type: 'pointerDown', button: 0 }, { type: 'pause', duration: 80 }, { type: 'pointerUp', button: 0 }, { type: 'pause', duration: 300 }]; });
    if (mouse.length) await wd('POST', `${S}/actions`, { actions: [{ type: 'pointer', id: 'mouse', parameters: { pointerType: 'mouse' }, actions: mouse }] });
    const kb = keys.split(',').filter(Boolean).flatMap((k) => [{ type: 'keyDown', value: KEY[k] ?? k }, { type: 'keyUp', value: KEY[k] ?? k }, { type: 'pause', duration: 60 }]);
    if (kb.length) await wd('POST', `${S}/actions`, { actions: [{ type: 'key', id: 'kb', actions: kb }] });
    await sleep(1500);
    console.log('   sent', clicks ? 'clicks ' + clicks : '', keys ? 'keys ' + keys : '');
  }
  mkdirSync(shots, { recursive: true });
  const png = await wd('GET', `${S}/screenshot`);
  const shotFile = join(shots, 'firefox-page.png'); writeFileSync(shotFile, Buffer.from(png, 'base64'));
  console.log('   screenshot', shotFile);
  const log = gdLog.join('').split('\n').filter((l) => /\[flash\]|PPP_InitializeModule|JavaScript error|Error:/i.test(l));
  check('the emulated Flash runtime initialised', log.some((l) => /PPP_InitializeModule -> 0/.test(l)), log.slice(-8).join('\n'));
  console.log(log.slice(0, 14).map((l) => '   ' + l.slice(0, 200)).join('\n'));
} catch (e) {
  check('test run', false, e && e.stack || String(e));
}
if (process.env.FF_TEST_LOG) writeFileSync(process.env.FF_TEST_LOG, gdLog.join(''));
console.log(failed ? `${failed} FAILED` : 'all ok');
await cleanup();
process.exit(failed ? 1 : 0);
