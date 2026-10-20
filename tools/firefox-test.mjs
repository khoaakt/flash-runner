// Drive a real Firefox (through geckodriver, WebDriver over HTTP, no npm dependencies) with the Firefox build of the extension installed as a
// temporary add-on, open a test page and report what happened.
//   node tools/build-extension.mjs --firefox
//   node tools/firefox-test.mjs --geckodriver <path> [--firefox <firefox binary>] [--addon dist/firefox] [--page tests/ext/compat.html]
//                               [--seconds 15] [--shots dir] [--headed] [--setting key=<json>]
//                               [--clicks "x,y;x,y"] [--keys "a,b,Enter"]   real mouse clicks (page coordinates) and key presses after the first wait
//                               [--url <https://...>] [--size 1280x800]     a page elsewhere instead of one of this repository; window size
//                               [--page-eval <js>] [--frame-eval <js>] [--expect-pass]         JS run inside the player frame after the wait; the movie must have painted its PASS square
//                               [--keep-open [minutes]] [--also page,page]   with --headed: after the checks leave the window open (default 30 minutes, or until it
//                                                                            is closed) with the add-on installed and --also pages in more tabs, to try it by hand
// Needs: Firefox 128 or newer and geckodriver (https://github.com/mozilla/geckodriver/releases). Exit status 0 when every check passed.
import { spawn } from 'node:child_process';
import { createReadStream, existsSync, mkdirSync, statSync, writeFileSync, readFileSync, readdirSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { decodePacket, encodePacket, parseHello, responderHello, CHUNK } from './lib/rtmfp-mini.mjs';
import { ARM_FIREFOX, REPORT_FIREFOX, describePdf, expectedPrint } from './lib/print-spy.mjs';
import { probeResult } from './lib/png-probe.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (n, d) => { const i = process.argv.indexOf(n); return i < 0 ? d : (process.argv[i + 1] ?? true); };
const geckodriver = arg('--geckodriver', process.env.GECKODRIVER);
if (!geckodriver) { console.error('give --geckodriver <path> (or set GECKODRIVER)'); process.exit(2); }
const firefox = arg('--firefox', process.env.FIREFOX || (process.platform === 'win32' ? join(process.env.LOCALAPPDATA || '', 'Mozilla Firefox', 'firefox.exe') : undefined));
const addon = resolve(arg('--addon', join(root, 'dist/firefox')));
let page = arg('--page', 'tests/ext/compat.html');   // --sockets replaces it with tests/ext/sock.html
const pickText = arg('--pick-text', null);          // content of the file the player frame's open dialog is answered with
const expectDownload = arg('--expect-download', null);   // regex a downloaded file's content must match
const expectPrint = process.argv.includes('--expect-print');   // the movie's PrintJob must hand the frame a PDF with a red 100x100 image on a Letter page
const saveDir = arg('--save-dir', null) ? resolve(arg('--save-dir', null)) : null;            // downloads of the player frame's save dialog go here
const UDP_RECV = /\[udp\] first datagram received: \d+ bytes from 127\.0\.0\.1:/, UDP_LINE = /\[udp\]/;
const withUdp = process.argv.includes('--udp-server');   // run tests/ext/rtmfp.html against a local UDP server that answers like the first step of an RTMFP server (needs the helper installed for Firefox, with UDP support)
const withSockets = process.argv.includes('--sockets');   // run tests/ext/sock.html against a local echo server through the native helper (needs the helper installed for Firefox)
const seconds = +arg('--seconds', 15);
const shots = resolve(arg('--shots', join(root, 'tests/out')));
const headed = process.argv.includes('--headed');
const clicks = String(arg('--clicks', '')), keys = String(arg('--keys', ''));
const openUrl = arg('--url', null), size = arg('--size', null);
const pageEval = arg('--page-eval', null), frameEval = arg('--frame-eval', null), expectPass = process.argv.includes('--expect-pass');
const keepOpenArg = arg('--keep-open', null), keepOpen = keepOpenArg !== null;
const keepMinutes = keepOpen ? (Number.isFinite(+keepOpenArg) && +keepOpenArg > 0 ? +keepOpenArg : 30) : 0;
const also = String(arg('--also', '')).split(',').filter(Boolean);
const settings = { enabled: true, openSwfLinks: true, autoplayMax: 8, clickToPlay: false, log: true, fpsLog: true, diag: true };
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
  const csp = new URL(req.url, 'http://x').searchParams.get('csp') === 'noeval' ? { 'content-security-policy': "script-src 'self' 'unsafe-inline'" } : {};   // a CSP that allows scripts but not eval
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream', 'content-length': statSync(file).size, ...csp });
  createReadStream(file).pipe(res);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

if (saveDir) mkdirSync(saveDir, { recursive: true });
const udpSeen = []; let udpServer = null;
if (withUdp) {
  const { createSocket } = await import('node:dgram');
  const { tmpdir } = await import('node:os');
  const cfg = join(tmpdir(), 'flash-runner-ff-udp.json'); writeFileSync(cfg, JSON.stringify({ allowPrivate: true, allowUDP: true })); process.env.FLASHRUNNER_SOCKETS_CONFIG = cfg;
  udpServer = createSocket('udp4');
  udpServer.on('message', (msg, rinfo) => {
    let p = null; try { p = decodePacket(msg); } catch { /* not RTMFP */ }
    udpSeen.push({ len: msg.length, from: rinfo.port, sid: p?.sid, checksumOk: p?.ok, chunks: (p?.chunks || []).map((c) => c.type.toString(16)) });
    const hello = p?.chunks.find((c) => c.type === CHUNK.HELLO);
    if (hello) udpServer.send(encodePacket([responderHello(parseHello(hello.data).tag)], p.timestamp ?? 0), rinfo.port, rinfo.address);
  });
  await new Promise((r) => udpServer.bind(0, '127.0.0.1', r));
  page = 'tests/ext/rtmfp.html?url=' + encodeURIComponent('rtmfp://127.0.0.1:' + udpServer.address().port + '/app');
}
let sockServer = null, sockSeen = '';
if (withSockets) {    // the helper refuses loopback unless told otherwise; geckodriver, Firefox and the helper inherit this variable
  const { createServer: tcp } = await import('node:net');
  const { tmpdir } = await import('node:os');
  const cfg = join(tmpdir(), 'flash-runner-ff-sockets.json'); writeFileSync(cfg, JSON.stringify({ allowPrivate: true })); process.env.FLASHRUNNER_SOCKETS_CONFIG = cfg;
  sockServer = tcp((c) => c.on('data', (d) => {
    const t = d.toString('latin1'); sockSeen += t;
    if (t.startsWith('<policy-file-request/>')) c.end('<?xml version="1.0"?><cross-domain-policy><allow-access-from domain="*" to-ports="*"/></cross-domain-policy>\0'); else c.write(d);
  }));
  await new Promise((r) => sockServer.listen(0, '127.0.0.1', r));
  page = 'tests/ext/sock.html?sock=' + sockServer.address().port;
}

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
if (!keepOpen) setTimeout(() => { console.error('WATCHDOG: test exceeded 170 s'); cleanup().then(() => process.exit(3)); }, 170000).unref();

for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${gdPort}/status`); break; } catch { await sleep(200); } }
const prefs = {
  ...(process.argv.includes('--fake-media') ? { 'media.navigator.streams.fake': true, 'media.navigator.permission.disabled': true } : {}),   // a test pattern and a beep instead of real devices, and no permission prompt
  ...(saveDir ? { 'browser.download.folderList': 2, 'browser.download.dir': saveDir, 'browser.download.useDownloadDir': true, 'browser.helperApps.neverAsk.saveToDisk': 'application/octet-stream', 'browser.download.manager.showWhenStarting': false } : {}),
  'extensions.webextensions.uuids': JSON.stringify({ [GECKO_ID]: UUID }),
  'devtools.console.stdout.content': true,       // page and worker console output goes to Firefox's stdout, which geckodriver forwards
  'media.autoplay.default': 0,
  'xpinstall.signatures.required': false,
  'dom.gamepad.test.enabled': true,             // navigator.requestGamepadServiceTest(): virtual pads for tests/ext/gamepad.html
};
const session = await wd('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'firefox', acceptInsecureCerts: true,
  'moz:firefoxOptions': { ...(firefox ? { binary: firefox } : {}), args: headed ? [] : ['-headless'], prefs, log: { level: 'info' } } } } });
sid = session.sessionId;
console.log('Firefox', session.capabilities.browserVersion, '| geckodriver session', sid.slice(0, 8));
const S = `/session/${sid}`;
const exec = async (script, args = []) => wd('POST', `${S}/execute/async`, { script: `const done = arguments[arguments.length - 1]; Promise.resolve((async () => { ${script} })()).then((v) => done(v), (e) => done({ __error: String(e && e.message ? e.name + ': ' + e.message : e) }));`, args });

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
  const port = await exec(`const p = chrome.runtime.connectNative('com.flashrunner.sockets'); const got = await new Promise((res) => { p.onMessage.addListener((m) => res(m)); p.onDisconnect.addListener((q) => res({ disconnected: String(q && q.error && q.error.message) })); p.postMessage({ t: 'hello', rid: 2 }); setTimeout(() => res({ timeout: true }), 4000); }); try { p.disconnect(); } catch {} return got;`);
  check('a long-lived native port to the helper works', !!port && port.ok === true, JSON.stringify(port));
  const runtimes = await exec(`const m = await import('./runtime-store.js'); const r = await m.getActiveRuntime(); return r ? { bundled: !!r.bundled, size: r.size, id: r.id } : null;`);
  check('a bundled Flash runtime is available', !!runtimes && !runtimes.__error && runtimes.bundled, JSON.stringify(runtimes));

  // the movie page, in the first tab
  await wd('POST', `${S}/window`, { handle: mainTab });
  if (size) { const [w, h] = String(size).split('x').map(Number); await wd('POST', `${S}/window/rect`, { width: w, height: h }); }
  await wd('POST', `${S}/url`, { url: openUrl || `${base}/${page}` });
  await sleep(seconds * 1000);
  const state = await exec(`const f = [...document.querySelectorAll('iframe[data-flash-runner]')]; return { frames: f.length, src: f[0] && f[0].src, embeds: document.querySelectorAll('embed,object').length, ready: window.__flashRunnerReady || null };`);
  check('the Flash element was replaced by a player iframe', state && state.frames >= 1, JSON.stringify(state));
  check('the player frame is served from the add-on', !!state?.src && state.src.startsWith(EXT + '/'), String(state?.src));
  if (refHits.length) {
    const want = `${base}/${page}`.split('?')[0];
    console.log('   /echo-ref saw', JSON.stringify(refHits));
    check('Referer and Origin of the movie requests are those of the embedding page', refHits.every((h) => h.referer.startsWith(want) && h.origin === base), JSON.stringify(refHits));
  }
  if (pageEval) console.log('   page eval =>', JSON.stringify(await exec(pageEval)));
  if (frameEval) {   // run code in the player frame (a cross-origin iframe of the add-on): switch into it, run, switch back
    const el = await wd('POST', `${S}/element`, { using: 'css selector', value: 'iframe[data-flash-runner]' });
    await wd('POST', `${S}/frame`, { id: el });
    const r = await exec(frameEval);
    await wd('POST', `${S}/frame/parent`, {});
    console.log('   frame eval =>', JSON.stringify(r));
  }
  const inPlayerFrame = async (script) => {
    const el = await wd('POST', `${S}/element`, { using: 'css selector', value: 'iframe[data-flash-runner]' });
    await wd('POST', `${S}/frame`, { id: el });
    try { return await exec(script); } finally { await wd('POST', `${S}/frame/parent`, {}); }
  };
  if (expectPrint) console.log('   print spy =>', JSON.stringify(await inPlayerFrame('return ' + ARM_FIREFOX)));
  if (pickText !== null) {   // headless Firefox cancels a file picker at once and cannot be driven: give the player frame's <input type=file> a file the moment it is added (before the cancel arrives) and fire change
    console.log('   file picker hook =>', JSON.stringify(await inPlayerFrame(`new MutationObserver((muts, obs) => { for (const m of muts) for (const n of m.addedNodes) if (n.localName === 'input' && n.type === 'file') { const dt = new DataTransfer(); dt.items.add(new File([${JSON.stringify(pickText)}], 'pick.txt', { type: 'text/plain' })); n.files = dt.files; document.documentElement.setAttribute('data-fed', 'accept=' + n.accept + ' multiple=' + n.multiple); n.dispatchEvent(new Event('change')); } }).observe(document.body, { childList: true }); return 'armed';`)));
  }
  if (clicks || keys) {
    const KEY = { Enter: '\uE007', Backspace: '\uE003', Tab: '\uE004', Escape: '\uE00C', ArrowLeft: '\uE012', ArrowRight: '\uE014', Space: ' ' };
    const mouse = clicks.split(';').filter(Boolean).flatMap((c) => { if (c.startsWith('wait:')) return [{ type: 'pause', duration: Number(c.slice(5)) }]; const [x, y] = c.split(',').map(Number); return [{ type: 'pointerMove', x, y, origin: 'viewport' }, { type: 'pointerDown', button: 0 }, { type: 'pause', duration: 80 }, { type: 'pointerUp', button: 0 }, { type: 'pause', duration: 300 }]; });
    if (mouse.length) await wd('POST', `${S}/actions`, { actions: [{ type: 'pointer', id: 'mouse', parameters: { pointerType: 'mouse' }, actions: mouse }] });
    const kb = keys.split(',').filter(Boolean).flatMap((k) => [{ type: 'keyDown', value: KEY[k] ?? k }, { type: 'keyUp', value: KEY[k] ?? k }, { type: 'pause', duration: 60 }]);
    if (kb.length) await wd('POST', `${S}/actions`, { actions: [{ type: 'key', id: 'kb', actions: kb }] });
    await sleep(1500);
    console.log('   sent', clicks ? 'clicks ' + clicks : '', keys ? 'keys ' + keys : '');
  }
  if (pickText !== null) { const fed = await inPlayerFrame("return document.documentElement.getAttribute('data-fed') || 'the picker was never given a file'"); console.log('   file picker =>', JSON.stringify(fed)); check('the player frame opened a file input and it was answered', /^accept=/.test(String(fed)), String(fed)); }
  if (expectPrint) { await sleep(2500); const r = describePdf(await inPlayerFrame('return ' + REPORT_FIREFOX)); console.log('   print report =>', JSON.stringify(r)); check('the PrintJob of the movie gave the frame a Letter-page PDF with its red 100x100 image', expectedPrint(r), JSON.stringify(r)); }
  if (saveDir) { await sleep(2500); const got = readdirSync(saveDir).filter((n) => !/.part$/.test(n)); const files = got.map((n) => [n, readFileSync(join(saveDir, n), 'utf8').slice(0, 80)]); console.log('   downloaded files:', JSON.stringify(files)); if (expectDownload) check('Flash saved a file through the browser download', files.some(([, t]) => new RegExp(expectDownload).test(t)), JSON.stringify(files)); }
  mkdirSync(shots, { recursive: true });
  const png = await wd('GET', `${S}/screenshot`);
  const shotFile = join(shots, 'firefox-page.png'); writeFileSync(shotFile, Buffer.from(png, 'base64'));
  if (expectPass) {   // a movie can start late on a slow machine (a slow start was seen to paint 6 s after the wait): look again for up to 8 s before failing
    let r = probeResult(shotFile), late = 0;
    while (!r.pass && late < 8000) { await sleep(2000); late += 2000; writeFileSync(shotFile, Buffer.from(await wd('GET', `${S}/screenshot`), 'base64')); r = probeResult(shotFile); }
    check('the movie painted its PASS square', r.pass, JSON.stringify(r));
    if (late) console.log(`   (the square was ${r.pass ? 'found' : 'still missing'} ${late / 1000} s after the wait)`);
  }
  console.log('   screenshot', shotFile);
  if (withUdp) {
    const all = gdLog.join('');
    console.log('   udp datagrams the server saw:', JSON.stringify(udpSeen.slice(0, 4)), 'count', udpSeen.length);
    check('Flash sent RTMFP hello datagrams through the helper (valid checksum, chunk 0x30)', udpSeen.length >= 1 && udpSeen[0].checksumOk && udpSeen[0].chunks.includes('30'), JSON.stringify(udpSeen.slice(0, 3)));
    check('after our responder hello Flash sent its initial keying chunk (0x38)', udpSeen.some((u) => u.chunks.includes('38')), JSON.stringify(udpSeen.slice(0, 6)));
    check('Flash read the reply through the helper', UDP_RECV.test(all), all.split(String.fromCharCode(10)).filter((l) => UDP_LINE.test(l)).join(' | ') || 'no [udp] lines');
  }
  if (withSockets) {
    const port = sockServer.address().port, all = gdLog.join('');
    check('a Flash socket reaches the helper from the player frame (through the background relay)', all.includes(`[sock] connect 127.0.0.1:${port} -> ok`), all.split('\n').filter((l) => /\[sock\]/.test(l)).join('\n') || 'no [sock] lines');
    check('the local server received the movie\'s bytes', sockSeen.includes('hello socket'), JSON.stringify(sockSeen.slice(0, 80)));
  }
  const log = gdLog.join('').split('\n').filter((l) => /\[flash\]|PPP_InitializeModule|JavaScript error|Error:/i.test(l));
  check('the emulated Flash runtime initialised', log.some((l) => /PPP_InitializeModule -> 0/.test(l)), log.slice(-8).join('\n'));
  console.log(log.slice(0, 14).map((l) => '   ' + l.slice(0, 200)).join('\n'));
  if (keepOpen) {
    const first = await wd('GET', `${S}/window`);
    for (const p of also) {
      const { handle } = await wd('POST', `${S}/window/new`, { type: 'tab' });
      await wd('POST', `${S}/window`, { handle });
      await wd('POST', `${S}/url`, { url: `${base}/${p}` });
    }
    await wd('POST', `${S}/window`, { handle: first });
    console.log(`\nFirefox stays open for up to ${keepMinutes} minutes (close the window to end). Pages: ${[page, ...also].join(', ')}`);
    const until = Date.now() + keepMinutes * 60000;
    while (Date.now() < until) { try { await wd('GET', `${S}/window/handles`); } catch { break; } await sleep(2000); }
  }
} catch (e) {
  check('test run', false, e && e.stack || String(e));
}
if (process.env.FF_TEST_LOG) writeFileSync(process.env.FF_TEST_LOG, gdLog.join(''));
console.log(failed ? `${failed} FAILED` : 'all ok');
await cleanup();
process.exit(failed ? 1 : 0);
