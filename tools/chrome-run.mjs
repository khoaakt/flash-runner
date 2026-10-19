// Drive a page in headless Chrome over CDP in REAL time (web/gl-test.html needs it: the emulator runs in a Worker and
// `--virtual-time-budget --dump-dom` ends before the worker finishes). macOS Chrome path; no dependencies (Node >= 22).
//   node web/serve.mjs --port 8181 &
//   node tools/chrome-run.mjs http://localhost:8181/web/gl-test.html            # waits for window.__done, prints #out + console
//   node tools/chrome-run.mjs http://localhost:8181/web/gl-test.html?force2d    # 2D presenter instead of WebGPU
//   node tools/chrome-run.mjs http://localhost:8181/web/ppb-gles2-selftest.html # emulator-free handler test
//   options: --wait "<js>" (default window.__done)  --eval "<js>" (default #out text)  --timeout ms  --flags "<chrome flags>"  --shot out.png
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const args = process.argv.slice(2);
const url = args[0];
const opt = (n, d) => { const i = args.indexOf(n); return i >= 0 ? args[i + 1] : d; };
const waitExpr = opt('--wait', 'window.__done'), evalExpr = opt('--eval', 'document.getElementById("out") && document.getElementById("out").textContent');
const timeout = +opt('--timeout', 60000);
const extra = (opt('--flags', '') || '').split(' ').filter(Boolean);
const port = 9300 + Math.floor(Math.random() * 500);
const dir = mkdtempSync(join(tmpdir(), 'cdp-'));
const chrome = spawn('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${dir}`, '--enable-unsafe-webgpu', '--use-angle=metal', '--enable-webgl', '--ignore-gpu-blocklist',
  '--no-first-run', '--no-default-browser-check', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--autoplay-policy=no-user-gesture-required', ...extra, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
let errtxt = ''; chrome.stderr.on('data', (d) => { errtxt += d; });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function getJSON(u) { for (let i = 0; i < 100; i++) { try { return await (await fetch(u)).json(); } catch { await sleep(100); } } throw new Error('no chrome'); }
const targets = await getJSON(`http://127.0.0.1:${port}/json`);
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const pend = new Map(); const logs = [];
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); }
  else if (d.method === 'Runtime.consoleAPICalled') logs.push(`[console.${d.params.type}] ` + d.params.args.map((a) => a.value ?? a.description ?? '').join(' '));
  else if (d.method === 'Runtime.exceptionThrown') logs.push('[exception] ' + (d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text));
  else if (d.method === 'Log.entryAdded') logs.push(`[log.${d.params.entry.level}] ${d.params.entry.text} ${d.params.entry.url || ''}`);
  else if (d.method === 'Target.attachedToTarget' || d.method === 'Target.receivedMessageFromTarget') { /* workers */ }
};
const send = (method, params = {}) => new Promise((res) => { const i = ++id; pend.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
await send('Runtime.enable'); await send('Log.enable'); await send('Page.enable');
await send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
await send('Page.navigate', { url });
const t0 = Date.now(); let ok = false;
while (Date.now() - t0 < timeout) {
  const r = await send('Runtime.evaluate', { expression: `!!(${waitExpr})`, returnByValue: true });
  if (r.result?.result?.value === true) { ok = true; break; }
  await sleep(200);
}
const r = await send('Runtime.evaluate', { expression: evalExpr, returnByValue: true, awaitPromise: true });
console.log(ok ? '--- done' : '--- TIMEOUT');
console.log(typeof r.result?.result?.value === 'string' ? r.result.result.value : JSON.stringify(r.result));
if (logs.length) console.log('--- console\n' + logs.slice(0, 80).join('\n'));
if (args.includes('--shot')) { const s = await send('Page.captureScreenshot', { format: 'png' }); (await import('node:fs')).writeFileSync(opt('--shot', 'shot.png'), Buffer.from(s.result.data, 'base64')); }
ws.close(); chrome.kill('SIGKILL');
process.exit(0);
