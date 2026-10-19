// Drive a real Chrome with the unpacked extension installed through CDP (pipe transport, no dependencies).
//   node tools/ext-test.mjs [--runtime path/to/libpepflashplayer.so] [--seconds 12] [--shots dir] [--headed]
//     [--setting key=<json>]  (repeatable) extra chrome.storage.local values, e.g. jitTrustLoads=false; only the keys of runtime-store.js DEFAULTS reach the player
//     [--cpu]                 CPU profile of every worker (the emulator) -> <shots dir>/worker-N.cpuprofile; it perturbs timing, so use it to attribute, never to time
//     [--expect-pass]         with --page: the movie must have painted its green PASS square (tests/swf-src probes) by the final screenshot
//     [--no-shots]            with --page: print the [flash] log lines after the wait and exit (no screenshots, no other checks)
//     [--url <https://...>]   open any address instead of a page of this repository, wait, and screenshot it (custom-before.png in the shots dir);
//                             --clicks "x,y;x,y" then clicks the page itself first (page coordinates); --hold <s> keeps the browser open afterwards
//     [--size 1280x800]       browser window size (default: Chrome's)
// Chrome 137+ ignores --load-extension in branded builds; `Extensions.loadUnpacked` over --remote-debugging-pipe
// with --enable-unsafe-extension-debugging is the supported way to install an unpacked extension for testing.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, mkdirSync, existsSync, statSync, createReadStream } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import http from 'node:http';
import { probeResult } from './lib/png-probe.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (n, d) => { const i = process.argv.indexOf(n); return i < 0 ? d : (process.argv[i + 1] ?? true); };
const runtimePath = resolve(arg('--runtime', join(root, 'vendor-private/libpepflashplayer.so')));
const seconds = +arg('--seconds', 12);
const shots = arg('--shots', null);
const shotsDir = shots || join(root, 'tests/out');   // screenshots land in the git-ignored tests/out unless --shots says otherwise
const headed = process.argv.includes('--headed');
const openUrl = arg('--url', null);                // a page elsewhere: the extension plays whatever Flash it finds
const customPage = arg('--page', null) || (openUrl ? '(url)' : null);
const windowSize = arg('--size', null);
const extraSettings = {};                          // --setting key=<json> (repeatable): added to the chrome.storage.local seeds, e.g. --setting jitTrustLoads=false
process.argv.forEach((a, i) => { if (a === '--setting') { const kv = process.argv[i + 1] || ''; const j = kv.indexOf('='); extraSettings[kv.slice(0, j)] = JSON.parse(kv.slice(j + 1)); } });
const cpuProfile = process.argv.includes('--cpu');   // CPU profile of every worker (the emulator) -> <shots dir>/worker-N.cpuprofile; it perturbs timing: attribution runs only
const noShots = process.argv.includes('--no-shots'); // with --page: print the log summary after the wait and exit (no screenshots, no other checks)
const clip = arg('--clip', null);                 // text placed on the clipboard (from inside the player frame) before the clicks
const keys = arg('--keys', '');                  // comma list of DOM key names pressed (with real key events) after the clicks, e.g. a,Enter,ArrowLeft
const rightClick = arg('--rightclick', '');       // "x,y" page coordinates of a right click (Flash context menu) done last
const evalBefore = arg('--pre-eval', null);        // JS evaluated in the custom page right before the input events
const evalPost = arg('--post-eval', null);         // JS evaluated in the custom page after the input events
const frameEval = arg('--frame-eval', null);       // JS evaluated inside the player iframe (before, then after the input events)
const expectEval = arg('--expect', null);          // regex the --eval result (as JSON text) must match; exit status reflects it
const evalAfter = arg('--eval', null);           // JS evaluated in the custom page after the wait; result printed
const drag = arg('--drag', '');                    // "x1,y1,x2,y2" page coordinates: press, drag and release the left button (select text with the mouse)
const menuClick = arg('--menu-click', null);       // label of an item of Flash's right-click menu, clicked with real input events after --rightclick
const expectClipboard = arg('--expect-clipboard', null);   // text the movie must have written to the clipboard (needs --spy-clipboard)
const clicks = arg('--clicks', '');            // "x,y;x,y" page coordinates clicked (with real input events) after the first wait          // e.g. tests/ext/compat.html: just load it, wait, screenshot
const CHROME = process.env.CHROME || (process.platform === 'win32' ? 'C:/Program Files/Google/Chrome/Application/chrome.exe' : '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
const ANGLE = process.env.ANGLE_BACKEND || (process.platform === 'win32' ? 'd3d11' : 'metal');   // the graphics backend ANGLE uses: Metal on macOS, Direct3D 11 on Windows
const MIME = { '.html': 'text/html', '.swf': 'application/x-shockwave-flash', '.js': 'text/javascript', '.mp3': 'audio/mpeg', '.so': 'application/octet-stream' };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = http.createServer((req, res) => {
  const p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p === '/crossdomain.xml') { res.writeHead(200, { 'content-type': 'text/x-cross-domain-policy' }); res.end('<?xml version="1.0"?><cross-domain-policy><allow-access-from domain="*" secure="false"/><allow-http-request-headers-from domain="*" headers="*"/></cross-domain-policy>'); return; }
  if (p === '/echo-ref') { res.writeHead(200, { 'content-type': 'text/plain', 'access-control-allow-origin': '*' }); res.end(`method=${req.method} referer=${req.headers.referer || '-'} origin=${req.headers.origin || '-'}`); return; }
  const file = p === '/runtime.so' ? runtimePath : join(root, p);
  if (!file.startsWith(root) && file !== runtimePath) { res.writeHead(403).end(); return; }
  if (!existsSync(file) || !statSync(file).isFile()) { res.writeHead(404).end(); return; }
  const extra = p === '/tests/ext/csp.html' ? { 'content-security-policy': "default-src 'self'; script-src 'self'; frame-src 'self'; object-src 'none'; connect-src 'self'; style-src 'self' 'unsafe-inline'" } : {};
  // EXT_TEST_VALIDATORS=1: answer like a normal static server (ETag + Last-Modified, 304 on a conditional request), to exercise the extension's download cache;
  // EXT_TEST_REQLOG=1: print every request (and whether it was conditional)
  const st = statSync(file), v = process.env.EXT_TEST_VALIDATORS ? { etag: `"${st.size.toString(16)}-${Math.floor(st.mtimeMs).toString(16)}"`, 'last-modified': st.mtime.toUTCString() } : {};
  if (process.env.EXT_TEST_REQLOG && !/\.html$/.test(p)) console.log('[req]', req.method, p, req.headers['if-none-match'] ? '(conditional)' : '');
  if (v.etag && req.headers['if-none-match'] === v.etag) { res.writeHead(304, { ...v, ...extra }); res.end(); return; }
  res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream', 'content-length': st.size, ...v, ...extra });
  createReadStream(file).pipe(res);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

const profileDir = mkdtempSync(join(tmpdir(), 'flashrunner-ext-'));
let sockServer = null, sockPort = 0;
if (process.argv.includes('--native')) {          // install the socket helper into this throw-away profile only
  const { execFileSync } = await import('node:child_process');
  const cfgFile = join(profileDir, 'sockets.json'); writeFileSync(cfgFile, JSON.stringify({ allowPrivate: true })); process.env.FLASHRUNNER_SOCKETS_CONFIG = cfgFile;   // the test server is on loopback
  execFileSync('sh', [join(root, 'tools/install-native-host.sh'), '--dir', join(profileDir, 'NativeMessagingHosts')], { stdio: 'ignore' });
}
if (process.argv.includes('--native')) {          // echo + socket policy server the sock.swf probe talks to
  const net = await import('node:net');
  sockServer = net.createServer((c) => { let first = true; c.on('data', (d) => { const t = d.toString('latin1'); if (process.env.EXT_TEST_LOGS) console.log('[sock-server] received ' + JSON.stringify(t.slice(0, 40))); if (first && t.startsWith('<policy-file-request/>')) { c.end('<?xml version="1.0"?><cross-domain-policy><allow-access-from domain="*" to-ports="*"/></cross-domain-policy>\0'); return; } first = false; c.write(d); }); });
  await new Promise((r) => sockServer.listen(0, '127.0.0.1', r)); sockPort = sockServer.address().port;
}
const noHelper = process.argv.includes('--no-helper');   // run sock.swf with NO helper registered in the profile: the notice must appear
const sockQuery = sockPort || (noHelper ? 59999 : 0);     // a high port nothing listens on: Flash itself refuses ports below 1024 without a policy file, long before the helper
const chrome = spawn(CHROME, [
  `--user-data-dir=${profileDir}`, '--remote-debugging-pipe', '--enable-unsafe-extension-debugging',
  '--no-first-run', '--no-default-browser-check', '--autoplay-policy=no-user-gesture-required', '--enable-unsafe-webgpu', `--use-angle=${ANGLE}`, '--ignore-gpu-blocklist',
  ...(windowSize ? [`--window-size=${windowSize.replace('x', ',')}`] : []), ...(headed ? [] : ['--headless=new']), 'about:blank',
], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'] });
const cleanup = () => { if (process.env.EXT_TEST_LOGS) console.log('--- console (' + logs.length + ' lines) ---\n' + logs.map((l) => String(l).slice(0, 220)).join('\n')); try { chrome.kill('SIGKILL'); } catch {} server.close(); try { sockServer?.close(); } catch {} };
process.on('exit', cleanup);
setTimeout(() => { console.error('WATCHDOG: test exceeded 170 s'); cleanup(); process.exit(3); }, 170000).unref();

// ---- CDP over pipe (NUL-terminated JSON)
let nextId = 0; const pending = new Map(); const handlers = []; let buf = '';
chrome.stdio[4].on('data', (d) => {
  buf += d.toString('utf8');
  let i; while ((i = buf.indexOf('\0')) >= 0) {
    const m = JSON.parse(buf.slice(0, i)); buf = buf.slice(i + 1);
    if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.rej(new Error(`${p.method}: ${m.error.message}`)) : p.res(m.result); }
    else handlers.forEach((h) => h(m));
  }
});
const send = (method, params = {}, sessionId) => new Promise((res, rej) => {
  const id = ++nextId; pending.set(id, { res, rej, method });
  chrome.stdio[3].write(JSON.stringify({ id, method, params, sessionId }) + '\0');
  setTimeout(() => { if (pending.has(id)) { pending.delete(id); rej(new Error(`${method}: timeout`)); } }, 30000);
});
const logs = [];
const sessions = new Map();
handlers.push((m) => {
  if (m.method === 'Runtime.consoleAPICalled') logs.push((m.params.args || []).map((a) => a.value ?? a.description ?? '').join(' ') + (process.env.EXT_TEST_LOGS ? '   <- ' + (m.params.stackTrace?.callFrames?.[0]?.url || '?') : ''));
  if (m.method === 'Runtime.exceptionThrown') logs.push('EXC ' + (m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text));
  if (m.method === 'Target.attachedToTarget') {
    const sid = m.params.sessionId; sessions.set(m.params.targetInfo.targetId, { sid, url: m.params.targetInfo.url, type: m.params.targetInfo.type });
    send('Runtime.enable', {}, sid).catch(() => {});
    send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sid).catch(() => {});
    if (cpuProfile && m.params.targetInfo.type === 'worker') send('Profiler.enable', {}, sid).then(() => send('Profiler.setSamplingInterval', { interval: 1000 }, sid)).then(() => send('Profiler.start', {}, sid)).catch(() => {});
    send('Runtime.runIfWaitingForDebugger', {}, sid).catch(() => {});
  }
});
async function stopProfiles() {
  if (!cpuProfile) return;
  mkdirSync(shotsDir, { recursive: true });
  let n = 0;
  for (const [, ss] of sessions) if (ss.type === 'worker') { try { const { profile } = await send('Profiler.stop', {}, ss.sid); const f = join(shotsDir, `worker-${n++}.cpuprofile`); writeFileSync(f, JSON.stringify(profile)); console.log('cpuprofile', f); } catch {} }
}
const target = async (targetId) => {
  const sessionId = (await send('Target.attachToTarget', { targetId, flatten: true })).sessionId;
  await send('Runtime.enable', {}, sessionId); await send('Page.enable', {}, sessionId);
  await send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sessionId).catch(() => {});
  return {
    send: (method, params) => send(method, params, sessionId),
    eval: async (expression) => { const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId); if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text); return r.result.value; },
  };
};

let ok = true;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + extra)); ok &&= !!cond; };

// ---- install the extension
let extId = null;
try { extId = (await send('Extensions.loadUnpacked', { path: process.env.EXT_DIR ? resolve(process.env.EXT_DIR) : join(root, 'dist/extension') })).id; } catch (e) { console.log('loadUnpacked failed:', e.message); }
check('extension installed via Extensions.loadUnpacked', !!extId, 'is this Chrome build blocking extension debugging?');
if (!extId) { cleanup(); process.exit(1); }
console.log('extension id', extId);
await sleep(800);

async function newTab(url) { const { targetId } = await send('Target.createTarget', { url }); return target(targetId); }

// ---- install the runtime through the options page (same code path as the file picker)
const opts = await newTab(`chrome-extension://${extId}/options/options.html`);
await sleep(600);
// the three storage seeds below turn the Debug log on (ext-test reads the player's log lines) unless --no-log
const zipPath = arg('--zip', null);            // install the runtime through the options page's file input from a .zip instead
let installed;
if (process.argv.includes('--bundled')) {                // run with the Flash runtime shipped inside the extension instead of installing a file
  installed = await opts.eval(`(async () => {
    const m = await import('./runtime-store.js');
    await chrome.storage.local.set({ enabled: true, openSwfLinks: true, autoplayMax: 8, clickToPlay: false, gpu3d: ${process.argv.includes('--gpu3d')}, diag: ${process.argv.includes('--diag')}, fpsLog: ${process.argv.includes('--fps-log')}, log: ${!process.argv.includes('--no-log')}, ...${JSON.stringify(extraSettings)} });
    const r = await m.getActiveRuntime();
    return r && r.bundled ? r.size + ' ' + r.sha256.slice(0, 12) : 'ERR no bundled runtime';
  })()`).catch((e) => 'ERR ' + e.message);
} else if (zipPath) {
  await opts.send('DOM.enable');
  const doc = await opts.send('DOM.getDocument', {});
  const { nodeId } = await opts.send('DOM.querySelector', { nodeId: doc.root.nodeId, selector: '#file' });
  await opts.send('DOM.setFileInputFiles', { nodeId, files: [resolve(zipPath)] });
  await sleep(4000);
  installed = await opts.eval(`(async () => { const m = await import('./runtime-store.js'); const r = await m.getRuntime(); await chrome.storage.local.set({ enabled: true, openSwfLinks: true, autoplayMax: 8, clickToPlay: false, gpu3d: ${process.argv.includes('--gpu3d')}, diag: ${process.argv.includes('--diag')}, fpsLog: ${process.argv.includes('--fps-log')}, log: ${!process.argv.includes('--no-log')}, ...${JSON.stringify(extraSettings)} }); return r ? r.size + ' ' + r.sha256.slice(0, 12) + ' ' + r.name : 'none'; })()`).catch((e) => 'ERR ' + e.message);
  installed = installed.replace(/^(\d+ [0-9a-f]{12}) .*$/, '$1');
} else installed = await opts.eval(`(async () => {
  const m = await import('./runtime-store.js');
  const bytes = new Uint8Array(await (await fetch(${JSON.stringify(base + '/runtime.so')})).arrayBuffer());
  const r = await m.setRuntimeFromBytes(bytes, 'libpepflashplayer.so');
  await chrome.storage.local.set({ enabled: true, openSwfLinks: true, autoplayMax: 8, clickToPlay: false, gpu3d: ${process.argv.includes('--gpu3d')}, diag: ${process.argv.includes('--diag')}, fpsLog: ${process.argv.includes('--fps-log')}, log: ${!process.argv.includes('--no-log')}, ...${JSON.stringify(extraSettings)} });
  return r.size + ' ' + r.sha256.slice(0, 12);
})()`).catch((e) => 'ERR ' + e.message);
check('runtime accepted by the options page', /^\d+ [0-9a-f]{12}$/.test(installed), installed);
await sleep(300);
check('options page shows the installed runtime', /Installed|No runtime/.test(await opts.eval(`document.getElementById('status').textContent`)));
const rejected = await opts.eval(`(async () => { const m = await import('./runtime-store.js'); try { await m.setRuntimeFromBytes(new Uint8Array(2 << 20), 'x'); return 'accepted'; } catch (e) { return e.message; } })()`);
check('non-ELF file is rejected', /not an ELF/.test(rejected), rejected);
if (existsSync(join(process.env.EXT_DIR ? resolve(process.env.EXT_DIR) : join(root, 'dist/extension'), 'helper'))) {   // the bundled installers must be downloadable from the settings page
  const sizes = await opts.eval(`Promise.all(['helper/FlashHelper-Setup.exe', 'helper/FlashHelper.pkg', 'helper/flash-helper_amd64.deb'].map((p) => fetch(chrome.runtime.getURL(p)).then((r) => r.ok ? r.arrayBuffer().then((b) => b.byteLength) : 'HTTP ' + r.status, (e) => 'ERR ' + e.message))).then((a) => a.join(' '))`).catch((e) => 'ERR ' + e.message);
  check('helper installers are packaged and reachable from the settings page', /^\d{6,} \d{6,} \d{6,}$/.test(sizes), sizes);
}
{
  const text = await opts.eval('document.body.innerText');
  check('settings page has no "Advanced" disclosure or developer command', !/Advanced|install-native-host/.test(text) && /Connection/.test(text), text.slice(0, 200));
}

const viewerFile = arg('--viewer-file', null);   // open the viewer without a movie and feed it a local .swf through its file input
if (viewerFile) {
  const vt = await newTab(`chrome-extension://${extId}/player/viewer.html`);
  await sleep(1500);
  await vt.send('DOM.enable');
  const doc = await vt.send('DOM.getDocument', {});
  const { nodeId } = await vt.send('DOM.querySelector', { nodeId: doc.root.nodeId, selector: 'input[type=file]' });
  check('viewer without a movie offers a file picker', !!nodeId);
  await vt.send('DOM.setFileInputFiles', { nodeId, files: [resolve(viewerFile)] });
  await sleep(seconds * 1000);
  mkdirSync(shotsDir, { recursive: true });
  const png = await vt.send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(shotsDir, 'viewer-local.png'), Buffer.from(png.data, 'base64'));
  console.log('screenshot', join(shotsDir, 'viewer-local.png'));
  console.log('title:', await vt.eval('document.title'));
  console.log(logs.filter((l) => /\[flash\]|EXC|error/i.test(l)).slice(-6).join('\n'));
  cleanup(); process.exit(ok ? 0 : 1);
}

if (customPage) {
  const cp = await newTab(openUrl || `${base}/${customPage}${sockQuery ? (customPage.includes('?') ? '&' : '?') + 'sock=' + sockQuery : ''}`);
  await sleep(seconds * 1000);
  await stopProfiles();
  if (noShots) { console.log(logs.filter((l) => /\[flash\]|EXC/.test(l)).join('\n')); cleanup(); process.exit(0); }
  mkdirSync(shotsDir, { recursive: true });
  const shot = async (name) => { const png = await cp.send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(shotsDir, name), Buffer.from(png.data, 'base64')); };
  if (process.argv.includes('--twice')) { await cp.send('Page.navigate', { url: 'about:blank' }); await sleep(1500); await cp.send('Page.navigate', { url: `${base}/${customPage}${sockQuery ? '?sock=' + sockQuery : ''}` }); await sleep(seconds * 1000); }
  if (openUrl) {                                   // another site: optional clicks on the page itself (e.g. a cookie banner), one screenshot, done
    for (const c of String(clicks).split(';').filter(Boolean)) {
      const [x, y] = c.split(',').map(Number);
      for (const type of ['mousePressed', 'mouseReleased']) await cp.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
      await sleep(800);
    }
    await shot('custom-before.png');
    console.log('screenshot', join(shotsDir, 'custom-before.png'));
    const hold = +arg('--hold', 0);                // keep the browser open this many seconds more, e.g. for tools/window-shot.ps1 to photograph the window
    if (hold > 0) { console.log('holding the browser open for', hold, 's'); await sleep(hold * 1000); }
    cleanup(); process.exit(0);
  }
  await shot('custom-before.png');
  if (sockPort) {                                 // sock.swf reports what happened through navigateToURL: the tab Chrome opened for it carries the result
    const { targetInfos } = await send('Target.getTargets');
    const rep = targetInfos.map((t) => t.url).find((u) => u.startsWith('http://report.invalid/'));
    const text = rep ? decodeURIComponent(rep.split('?')[1] || '').replace(/\+/g, ' ') : '';
    check('socket echo through the native helper', /connect=ok\|data=hello socket/.test(text), text || 'no report from the movie');
  }
  if (evalAfter) { for (let k = 0; k < 20; k++) { await sleep(1000); if (await cp.eval('!!window.done').catch(() => false)) break; } const ev = JSON.stringify(await cp.eval(evalAfter).catch((e) => 'ERR ' + e.message), null, 1); console.log('eval =>', ev); if (expectEval) check('--eval result matches /' + expectEval + '/', new RegExp(expectEval, 's').test(ev), ev.slice(0, 300)); }
  if (evalBefore) console.log('pre-eval =>', JSON.stringify(await cp.eval(evalBefore).catch((e) => 'ERR ' + e.message)));
  // CDP input sent to the page target is not routed into out-of-process iframes (the extension player), so drive the
  // player frame through its own target session, translating page coordinates into frame coordinates.
  const frameRect = JSON.parse(await cp.eval(`JSON.stringify((() => { const f = document.querySelector('iframe[data-flash-runner]'); const r = f && f.getBoundingClientRect(); return r ? { x: r.left, y: r.top } : null; })())`).catch(() => 'null'));
  const playerSession = [...sessions.values()].reverse().find((v) => v.type === 'iframe' && (!v.url || /\/player\/player\.html/.test(v.url)));
  const inp = (method, params) => (playerSession && frameRect ? send(method, params, playerSession.sid) : cp.send(method, params));
  const pt = (x, y) => (playerSession && frameRect ? [x - frameRect.x, y - frameRect.y] : [x, y]);
  if (process.env.DEBUG_SESSIONS) console.log([...sessions.values()].map((v) => v.type + ' ' + v.url).join('\n'));
  console.log('input route:', playerSession && frameRect ? 'player frame session' : 'page target (no OOPIF session found)');
  const inFrame = async (expr) => { if (!playerSession) return 'no player session'; const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true }, playerSession.sid); return r.exceptionDetails ? 'ERR ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text) : r.result.value; };
  if (frameEval) console.log('frame pre-eval =>', JSON.stringify(await inFrame(frameEval)));
  if ((keys || menuClick || rightClick) && playerSession) {   // a headless test frame is not focused: without this the keys never reach the movie
    await cp.send('Emulation.setFocusEmulationEnabled', { enabled: true }).catch(() => {}); await send('Emulation.setFocusEmulationEnabled', { enabled: true }, playerSession.sid).catch(() => {}); await cp.send('Page.bringToFront').catch(() => {});
  }
  if (process.argv.includes('--spy-clipboard')) {   // record every navigator.clipboard.writeText the player makes, and whether it succeeded
    await inFrame(`window.__cb = []; const w = navigator.clipboard.writeText.bind(navigator.clipboard); navigator.clipboard.writeText = (t) => { window.__cb.push('write:' + t); return w(t).then(() => { window.__cb.push('ok'); }, (e) => { window.__cb.push('ERR ' + e); throw e; }); }; 1`);
    await send('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'] }).catch(() => {});
  }
  if (noHelper || sockPort) {                     // a movie opened a socket: with no helper registered the user must be told, with one nobody must be bothered
    const noticeNow = async () => JSON.parse(await inFrame(`JSON.stringify({ notice: !!document.querySelector('[data-helper-notice]'), canvas: !!document.querySelector('canvas') })`));
    const badge = () => opts.eval(`(async () => { const [t] = await chrome.tabs.query({ url: ${JSON.stringify(base + '/*')} }); return t ? await chrome.action.getBadgeText({ tabId: t.id }) : 'no tab'; })()`);
    const n = await noticeNow().catch((e) => ({ err: e.message })), b = await badge();
    if (noHelper) {
      check('no helper: a notice bar is shown over the movie', n.notice === true, JSON.stringify(n));
      check('no helper: the movie itself is still there (not replaced by the notice)', n.canvas === true, JSON.stringify(n));
      check('no helper: the toolbar button is marked with "!"', b === '!', JSON.stringify(b));
      // the popup of that tab offers the install; opened as a normal tab, so tabs.query is pointed at the test page
      const pop = await newTab('about:blank');
      await pop.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (globalThis.chrome?.tabs) { const q = chrome.tabs.query.bind(chrome.tabs); chrome.tabs.query = (i) => q({}).then((ts) => ts.filter((t) => t.url.startsWith(${JSON.stringify(base)}))); }` });
      await pop.send('Page.navigate', { url: `chrome-extension://${extId}/popup/popup.html` });
      await sleep(1500);
      check('no helper: the popup says the page wants to play online and offers "Install helper"', await pop.eval(`!!document.querySelector('[data-testid=helper-needed] button')`));
      if (shots) { const png = await pop.send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(shots, 'popup-helper.png'), Buffer.from(png.data, 'base64')); }
      await pop.eval(`setTimeout(() => document.querySelector('[data-testid=helper-needed] button').click(), 0), 0`);
      await sleep(1500);
      // the install opened on its own at first run, so a settings tab exists already: it must be reused (scrolled to Connection), not duplicated
      const optionTabs = (await send('Target.getTargets')).targetInfos.filter((t) => t.type === 'page' && /\/options\/options\.html/.test(t.url));
      const anchored = optionTabs.find((t) => t.url.endsWith('#connection'));
      const where = anchored ? await target(anchored.targetId).then((t) => t.eval(`JSON.stringify({ top: Math.round(document.getElementById('connection').getBoundingClientRect().top), h: innerHeight })`)).then(JSON.parse, () => ({})) : {};
      check('"Install helper" brings up the settings page at its Connection section, in an existing tab', optionTabs.length === 2 && !!anchored && where.top >= 0 && where.top < where.h, JSON.stringify({ tabs: optionTabs.map((t) => t.url.split('/').pop()), where }));
      await inFrame(`document.querySelector('[data-helper-notice] button:last-child').click()`);
      check('the notice can be dismissed', (await noticeNow()).notice === false);
    } else {
      check('helper installed: no notice and no "!" while the sockets work', n.notice === false && b === '', JSON.stringify({ n, b }));
    }
  }
  let i = 0;
  for (const c of String(clicks).split(';').filter(Boolean)) {
    const [px, py] = c.split(',').map(Number); const [x, y] = pt(px, py);
    await inp('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
    await inp('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', buttons: 1, clickCount: 1 });
    await inp('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', buttons: 0, clickCount: 1 });
    await sleep(700); i++;
  }
  if (i) await sleep(2500);
  if (drag) {
    const [x1, y1, x2, y2] = drag.split(',').map(Number); const [a, b] = pt(x1, y1), [c, d] = pt(x2, y2);
    await inp('Input.dispatchMouseEvent', { type: 'mouseMoved', x: a, y: b });
    await inp('Input.dispatchMouseEvent', { type: 'mousePressed', x: a, y: b, button: 'left', buttons: 1, clickCount: 1 });
    for (let k = 1; k <= 8; k++) await inp('Input.dispatchMouseEvent', { type: 'mouseMoved', x: a + (c - a) * k / 8, y: b + (d - b) * k / 8, button: 'left', buttons: 1 });
    await inp('Input.dispatchMouseEvent', { type: 'mouseReleased', x: c, y: d, button: 'left', buttons: 0, clickCount: 1 });
    await sleep(500);
  }
  if (clip && playerSession) {
    await cp.send('Emulation.setFocusEmulationEnabled', { enabled: true }).catch(() => {}); await send('Emulation.setFocusEmulationEnabled', { enabled: true }, playerSession.sid).catch(() => {}); await cp.send('Page.bringToFront').catch(() => {});
    await send('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'] }).catch(() => {});
    const r = await send('Runtime.evaluate', { expression: `navigator.clipboard.writeText(${JSON.stringify(clip)}).then(() => 'ok', (e) => 'ERR ' + e)`, awaitPromise: true, userGesture: true }, playerSession.sid);
    console.log('clipboard write =>', JSON.stringify(r.result.value ?? r.exceptionDetails?.text));
  }

  const KEY = { Enter: [13, '\r'], ArrowLeft: [37], ArrowRight: [39], ArrowUp: [38], ArrowDown: [40], ' ': [32, ' '], Escape: [27] };
  for (let k of String(keys).split(',').filter(Boolean)) {
    const mod = k.startsWith('^') ? +(process.env.KEYMOD || (process.platform === 'darwin' ? 4 : 2)) : 0;   // ^v = Cmd/Ctrl+V
    if (mod) k = k.slice(1);
    const code = KEY[k]?.[0] ?? k.toUpperCase().charCodeAt(0), text = KEY[k]?.[1] ?? (k.length === 1 ? k : undefined);
    const base = { key: k, code: k.length === 1 ? 'Key' + k.toUpperCase() : k, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code };
    await inp('Input.dispatchKeyEvent', { type: text && !mod ? 'keyDown' : 'rawKeyDown', ...base, modifiers: mod, text: mod ? undefined : text });
    await inp('Input.dispatchKeyEvent', { type: 'keyUp', ...base, modifiers: mod });
    await sleep(400);
  }
  if (rightClick) { const [rx, ry] = rightClick.split(',').map(Number); const [x, y] = pt(rx, ry); await inp('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y }); await inp('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', buttons: 2, clickCount: 1 }); await inp('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', buttons: 0, clickCount: 1 }); await sleep(1500); }
  if (menuClick) {
    const item = JSON.parse(await inFrame(`JSON.stringify((() => { const row = [...document.querySelectorAll('div')].find((d) => d.style.cursor === 'default' && d.textContent.replace(/\\u2713/g, '').trim() === ${JSON.stringify(menuClick)}); if (!row) return { menu: [...document.querySelectorAll('div')].filter((d) => d.style.padding && d.style.cursor).map((d) => d.textContent) }; const b = row.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 }; })())`));
    check(`Flash's context menu offers "${menuClick}"`, item && item.x !== undefined, JSON.stringify(item));
    if (item?.x !== undefined) {
      for (const [type, buttons] of [['mouseMoved', 0], ['mousePressed', 1], ['mouseReleased', 0]]) await send('Input.dispatchMouseEvent', { type, x: item.x, y: item.y, button: 'left', buttons, clickCount: 1 }, playerSession.sid);
      await sleep(1500);
    }
  }
  if (keys || rightClick) await sleep(1500);
  if (process.argv.includes('--spy-clipboard')) {
    const calls = await inFrame('JSON.stringify(window.__cb)');
    console.log('clipboard calls =>', calls);
    if (expectClipboard !== null) check(`the movie wrote "${expectClipboard}" to the clipboard`, calls.includes('write:' + expectClipboard) && calls.includes('"ok"') && !calls.includes('ERR'), calls);
  }
  if (frameEval) console.log('frame post-eval =>', JSON.stringify(await inFrame('window.__fe')));
  if (evalPost) console.log('post-eval =>', JSON.stringify(await cp.eval(evalPost).catch((e) => 'ERR ' + e.message)));
  await shot('custom.png');
  if (process.argv.includes('--expect-pass')) { const r = probeResult(join(shotsDir, 'custom.png')); check('the movie painted its PASS square', r.pass, JSON.stringify(r)); }
  if (process.argv.includes('--expect-accelerators')) {
    check('packaged AOT installed into the real plugin', logs.some((l) => /\[aot\] plugin base=.*installed=[1-9]/.test(l)), logs.slice(-20).join('\n'));
    check('captured real NanoJIT method replaced by wasm', logs.some((l) => /LIR->wasm installed method=/.test(l)), logs.slice(-20).join('\n'));
    check('LIR replacement completed real method calls (not only guard retries)', logs.some((l) => /completed_native_leaf_calls=[1-9]/.test(l)), logs.slice(-20).join('\n'));
    if (process.argv.includes('--expect-loops')) check('bounded captured loop replaced in Chrome', logs.some((l) => /LIR->wasm installed method=.*loop=(sum|xorshift)/.test(l)), logs.slice(-20).join('\n'));
    if (process.argv.includes('--expect-memory')) check('captured receiver writes executed in Chrome', logs.some((l) => /LIR->wasm installed method=.*memory=write/.test(l)), logs.slice(-20).join('\n'));
    if (process.argv.includes('--expect-aot-fallback')) check('serialized offline fallback executed in Chrome', logs.some((l) => /serialized_fallback_calls=[1-9]\d*/.test(l)), logs.slice(-20).join('\n'));
  }
  console.log('screenshot', join(shotsDir, 'custom.png'));
  console.log(logs.filter((l) => process.env.FULLLOG || /\[flash\]|EXC|error/i.test(l)).slice(-(+process.env.LOGLINES || 40)).join('\n'));
  cleanup(); process.exit(ok ? 0 : 1);
}

// ---- the test page with three kinds of Flash markup
const page = await newTab(`${base}/tests/ext/page.html`);
await sleep(2500);
const info = await page.eval(`document.getElementById('info').textContent`);
check('navigator.plugins advertises Shockwave Flash', /plugin=Shockwave Flash 32\.0 r0 mime=true/.test(info), info);
const state = await page.eval(`JSON.stringify({ iframes: [...document.querySelectorAll('iframe[data-flash-runner]')].map(f => ({ id: f.id, w: f.getAttribute('width'), h: f.getAttribute('height') })), leftovers: document.querySelectorAll('embed, object').length })`);
const st = JSON.parse(state);
check('three Flash elements replaced by player iframes (embed, object+embed, dynamically added object)', st.iframes.length === 3 && st.leftovers === 0, state);
check('ids and sizes preserved', st.iframes.some((f) => f.id === 'game1' && f.w === '320' && f.h === '240') && st.iframes.some((f) => f.id === 'game2' && f.w === '200'), state);

await sleep(seconds * 1000);
const booted = logs.filter((l) => /PPP_InitializeModule -> 0/.test(l)).length;
check('emulated Flash runtime initialised inside the iframe(s)', booted >= 1, logs.filter((l) => /flash|PPP|error|EXC/i.test(l)).slice(-12).join('\n'));
if (shots) { mkdirSync(shots, { recursive: true }); const png = await page.send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(shots, 'page.png'), Buffer.from(png.data, 'base64')); console.log('screenshot', join(shots, 'page.png')); }

// ---- a page with a strict CSP (frame-src 'self') refuses the extension iframe: expect the viewer-button fallback
const csp = await newTab(`${base}/tests/ext/csp.html`);
await sleep(5000);
const cspState = await csp.eval(`JSON.stringify({ iframes: document.querySelectorAll('iframe[data-flash-runner]').length, placeholders: document.querySelectorAll('[data-flash-runner=placeholder]').length, text: (document.querySelector('[data-flash-runner=placeholder]') || {}).textContent || '' })`);
const cs = JSON.parse(cspState);
check('strict-CSP page: Flash still shows (player iframe, or the viewer-button fallback if the CSP refuses it)', (cs.iframes === 1 && cs.placeholders === 0) || (cs.iframes === 0 && cs.placeholders === 1 && /Play Flash in viewer/.test(cs.text)), cspState);
console.log('   csp page result:', cs.iframes === 1 ? 'iframe loaded despite CSP' : 'fallback placeholder');

// ---- direct .swf navigation goes through the viewer
const direct = await newTab(`${base}/tests/swf/testsrc.swf`);
await sleep(4000);
const loc = await direct.eval('location.href').catch(() => '');
check('opening a .swf URL redirects to the extension viewer', /chrome-extension:\/\/.*\/player\/viewer\.html\?src=/.test(loc), loc);
if (shots) { const png = await direct.send('Page.captureScreenshot', { format: 'png' }); writeFileSync(join(shots, 'viewer.png'), Buffer.from(png.data, 'base64')); }

console.log('\nconsole sample:\n' + logs.filter((l) => /\[flash\]/.test(l)).slice(0, 10).join('\n'));
cleanup();
process.exit(ok ? 0 : 1);
