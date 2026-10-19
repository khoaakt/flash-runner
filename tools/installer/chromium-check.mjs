// Runs INSIDE the test container (tools/installer-linux-test.mjs): starts Chromium headless on a fresh profile, opens chrome://extensions and prints the
// state of the Flash runtime extension as JSON. `present` waits for it (an external extension is added at startup); `absent` checks it is not there.
//   node chromium-check.mjs <present|absent> <extension id> <profile dir>
import { spawn } from 'node:child_process';
import { readFileSync, existsSync, rmSync } from 'node:fs';
const [mode, id, profile] = process.argv.slice(2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
rmSync(`${profile}/DevToolsActivePort`, { force: true });
const chromium = spawn('chromium', ['--headless=new', '--no-sandbox', `--user-data-dir=${profile}`, '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check', 'about:blank'], { stdio: 'ignore' });
let port; for (let i = 0; i < 120 && !port; i++) { await sleep(250); const f = `${profile}/DevToolsActivePort`; if (existsSync(f)) port = readFileSync(f, 'utf8').split('\n')[0]; }
if (!port) { console.log(JSON.stringify({ error: 'chromium did not start' })); process.exit(2); }
const ver = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const ws = new WebSocket(ver.webSocketDebuggerUrl); await new Promise((r) => (ws.onopen = r));
let n = 0; const pending = new Map(); ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } };
const send = (method, params = {}, sessionId) => new Promise((r) => { const i = ++n; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const t = await send('Target.createTarget', { url: 'chrome://extensions' });
const a = await send('Target.attachToTarget', { targetId: t.result.targetId, flatten: true });
await sleep(1500);
const list = async () => { const r = await send('Runtime.evaluate', { expression: `chrome.developerPrivate.getExtensionsInfo({ includeDisabled: true }).then((a) => JSON.stringify(a.filter((x) => x.id === ${JSON.stringify(id)}).map((x) => ({ version: x.version, state: x.state, location: x.location, suspicious: x.disableReasons.suspiciousInstall, corrupt: x.disableReasons.corruptInstall }))))`, awaitPromise: true, returnByValue: true }, a.result.sessionId); return JSON.parse(r.result?.result?.value || '[]'); };
let found = [];
for (let i = 0; i < (mode === 'present' ? 60 : 8); i++) { found = await list(); if (found.length) break; await sleep(500); }
console.log(JSON.stringify({ mode, found }));
ws.close(); chromium.kill(); await sleep(500);
process.exit(mode === 'present' ? (found.length === 1 && found[0].state === 'ENABLED' ? 0 : 1) : (found.length === 0 ? 0 : 1));
