// Boot the real Flash PPAPI plugin in Node and see how far it gets:
//   node web/flash-smoke.mjs [--swf path.swf] [--trace] [--seconds N] [--png out.ppm]
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import http from 'node:http';
import { Emulator } from './emu.js';
import { Pepper } from './pepper.js';
import { NodeSockets } from './socket-node.mjs';
import { createNanojitCapture } from './nanojit-capture.js';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i < 0 ? d : (argv[i + 1] ?? true); };
const flashvars = opt('--flashvars', '');
const swfPath = opt('--swf', null), trace = argv.includes('--trace'), seconds = +opt('--seconds', 20), out = opt('--png', null);
const plugin = opt('--plugin', null) ? new URL('file://' + (opt('--plugin').startsWith('/') ? opt('--plugin') : process.cwd() + '/' + opt('--plugin'))) : new URL('../vendor-private/libpepflashplayer.so', import.meta.url);
if (!existsSync(plugin)) { console.error('put the licensed runtime at vendor-private/libpepflashplayer.so'); process.exit(2); }

const wasm = readFileSync(process.env.EMU_WASM_FILE || new URL('./emu.wasm', import.meta.url));
const t0 = performance.now();
const emu = await Emulator.create({ wasm, onConsole: (fd, b) => process.stderr.write(`[guest${fd}] ` + Buffer.from(b).toString()), onLog: (s) => process.stderr.write(s + '\n') });
emu.vfsNew();
emu.loadTar(readFileSync(new URL('../rootfs/out/rootfs.tar', import.meta.url)));
emu.addFile('/opt/pepper-host', readFileSync(new URL('../guest/out/pepper-host', import.meta.url)));
emu.addFile('/opt/plugin.so', readFileSync(plugin));
emu.boot(+(process.env.EMU_MEM || 768), +(process.env.EMU_CPUID || 0));
const jitN = process.env.EMU_JIT === undefined ? 100 : +process.env.EMU_JIT;   // default on, like the browser worker; EMU_JIT=0 = interpreter
if (process.env.EMU_JIT_DUMP) { let nd = 0; emu.onJitModule = (b) => writeFileSync(`${process.env.EMU_JIT_DUMP}/jit${nd++}.wasm`, b); }   // every generated module
if (process.env.EMU_TRACE_SYS) emu.x.emu_set_trace(1);   // log every guest syscall
if (jitN) emu.setJit(jitN, { profChains: !!process.env.EMU_JIT_PROFCHAINS, intrinsics: !process.env.EMU_JIT_NOINTR, trustLoads: !!process.env.EMU_JIT_TRUST });   // EMU_JIT_PROFCHAINS=1: per-chain retired-instruction counters, hottest printed at the end
emu.spawn('/opt/pepper-host', ['/opt/pepper-host'], ['HOME=/home/flash', 'LANG=C', 'TZ=UTC', 'FW_ARENA_MB=' + (process.env.FW_ARENA_MB || 64)]);
const capturePath = opt('--nanojit-capture', null);
const nanojit = capturePath || argv.includes('--lir') ? await createNanojitCapture({ emu, plugin: readFileSync(plugin), log: console.log, lower: argv.includes('--lir'), families: opt('--lir-families', null) ?? undefined, fullCapture: !!capturePath, maxMethods: +opt('--capture-methods', 32) }) : null;
const portableAotPath = opt('--portable-aot', null);
const portableAot = portableAotPath ? readFileSync(portableAotPath) : null;
const portableIndexPath = opt('--portable-index', null);
const portableBundles = portableIndexPath ? JSON.parse(readFileSync(portableIndexPath, 'utf8')).artifacts
  .map((artifact) => readFileSync(new URL(artifact.file, pathToFileURL(resolve(portableIndexPath))))) : [];
const configuredAot = !!portableAot || portableBundles.length > 0;

const server = http.createServer((req, res) => {
  if (swfPath && req.url.startsWith('/movie.swf')) { res.setHeader('content-type', 'application/x-shockwave-flash'); res.end(readFileSync(swfPath)); }
  else if (req.url.startsWith('/data.txt')) { res.setHeader('content-type', 'text/plain'); res.end('payload from the page origin\n'); }
  else if (swfPath && req.url.split('?')[0] !== '/' && existsSync(new URL('.' + decodeURIComponent(req.url.split('?')[0]), 'file://' + swfPath.replace(/[^/]*$/, '').replace(/^(?!\/)/, process.cwd() + '/')).pathname)) {
    const fp = new URL('.' + decodeURIComponent(req.url.split('?')[0]), 'file://' + swfPath.replace(/[^/]*$/, '').replace(/^(?!\/)/, process.cwd() + '/')).pathname;
    res.setHeader('content-type', /\.mp4$/.test(fp) ? 'video/mp4' : 'application/octet-stream'); res.end(readFileSync(fp));
  } else if (false) { res.setHeader('content-type', 'application/x-shockwave-flash'); res.end(readFileSync(swfPath)); }
  else { res.statusCode = 404; res.end('nope'); }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;

let frames = 0, last = null;
// --audio: a simulated real-time output device so audio-clocked streams (NetStream) advance like they would in a browser
const simSink = (() => { let peak = 0, written = 0, t0 = performance.now(); const rate = 48000; const played = () => Math.min(written, Math.floor((performance.now() - t0) / 1000 * rate)); return { rate, targetFrames: () => Math.round(rate * 0.12), bufferedFrames: () => written - played(), write(f) { if (written === played()) t0 = performance.now() - played() / rate * 1000; written += f.length >> 1; for (let i = 0; i < f.length; i++) if (Math.abs(f[i]) > peak) peak = Math.abs(f[i]); }, stats: () => ({ frames: written, peak }) }; })();
// Yield through a MessageChannel like web/worker.js: setTimeout(0) costs ~15 ms per turn on Windows (timer tick), which would dominate the timings.
const yieldChannel = new MessageChannel(); let yieldPending = null;
yieldChannel.port1.on('message', () => { const r = yieldPending; yieldPending = null; if (r) r(); });
yieldChannel.port1.unref();
const yieldToEventLoop = () => new Promise((r) => { yieldPending = r; yieldChannel.port2.postMessage(0); });
const pep = new Pepper(emu, { yield: yieldToEventLoop, sockets: process.env.NO_SOCKETS ? undefined : new NodeSockets(), audioSink: argv.includes('--audio') ? simSink : undefined, sampleRate: 48000, trace, baseUrl: base, onNavigate: (u, t, user) => console.log('NAVIGATE ->', decodeURIComponent(u).replace(/\|/g, ' | '), 'target=', t, 'user=', user), onCursor: (c) => console.log('cursor ->', c), onMenu: (inst, id, items, x, y) => { console.log('MENU', id, JSON.stringify(items.map((i) => i.name)), x, y); setTimeout(() => pep.menuResult(id, null), 100); }, documentUrl: base + 'index.html', onMessage: (i, m) => console.log('plugin message:', m),
  onPluginLoaded: (image) => {
    nanojit?.onPluginLoaded(image);
    if (portableAot) { const n = emu.installPortableAot(portableAot, image.loadBias); console.log(`[aot] installed ${n} static plugin regions before PPP_InitializeModule`); return n; }
    if (portableBundles.length) {
      const started = performance.now(); let count = 0;
      for (const bundle of portableBundles) count += emu.installPortableAot(bundle, image.loadBias);
      console.log(`[aot] installed ${count} static plugin regions in ${(performance.now() - started).toFixed(1)} ms before PPP_InitializeModule`);
      return count;
    }
    return 0;
  },
  onEntryProbe: (event) => nanojit?.onEntryProbe(event),
  onFrame: (i, buf, w, h) => { frames++; last = { w, h, px: buf.slice() }; } });
pep.modulePath = '/opt/plugin.so';
const ready = pep.whenReady();
let stop = false;
const runP = pep.run(() => stop);
await Promise.race([ready, runP.then(() => { throw new Error('guest ended before init: ' + (pep.crashed || pep.exited)); })]);
console.log(`PPP_InitializeModule -> ${pep.initResult}  (${((performance.now() - t0) / 1000).toFixed(1)} s)`);
if (pep.initResult === 0) {
  const r = await Promise.race([pep.createInstance(1, swfPath ? { src: base + 'movie.swf', type: 'application/x-shockwave-flash', quality: 'high', ...(flashvars ? { flashvars } : {}), ...Object.fromEntries(String(opt('--attrs', '')).split(';').filter(Boolean).map((kv) => kv.split('='))) } : { type: 'application/x-shockwave-flash' }, { w: 400, h: 300 }), new Promise((r) => setTimeout(() => r('timeout'), seconds * 1000))]);
  console.log('createInstance ->', r);
  if (argv.includes('--wait-for-pass')) {
    const started = performance.now();
    while (performance.now() - started < seconds * 1000) {
      if (last) {
        const at = (260 * last.w + 360) * 4;
        if (last.px[at] === 0 && last.px[at + 1] === 255 && last.px[at + 2] === 0) break;
      }
      await new Promise((r) => setTimeout(r, 10));
    }
    console.log(`[integration] movie completion ${(performance.now() - started).toFixed(1)} ms`);
  } else {
  await new Promise((r) => setTimeout(r, seconds * 1000));
  }
  if (argv.includes('--probe-obj')) {
    const oid = await pep.getInstanceObject(1);
    console.log('instance object var', oid);
    for (const n of (process.env.PROBE_NAMES || 'helloFromBrowser,CallFunction,nonexistent').split(',')) console.log('HasMethod', n, JSON.stringify(await pep.pluginObjectOp(oid, 'HasMethod', n)), 'HasProperty', JSON.stringify(await pep.pluginObjectOp(oid, 'HasProperty', n)));
    console.log('names', JSON.stringify(await pep.pluginObjectOp(oid, 'GetAllPropertyNames')));
    console.log('CallFunction echo', JSON.stringify(await pep.pluginObjectOp(oid, 'Call', 'CallFunction', ['<invoke name="echo" returntype="javascript"><arguments><string>hi there</string></arguments></invoke>'])));
    console.log('CallFunction sum', JSON.stringify(await pep.pluginObjectOp(oid, 'Call', 'CallFunction', ['<invoke name="sum" returntype="javascript"><arguments><number>2</number><number>40.5</number></arguments></invoke>'])));
    console.log('CallFunction obj', JSON.stringify(await pep.pluginObjectOp(oid, 'Call', 'CallFunction', ['<invoke name="obj" returntype="javascript"><arguments><object><property id="a"><number>1</number></property><property id="b"><array><property id="0"><string>x</string></property></array></property></object></arguments></invoke>'])));
    console.log('CallFunction nosuch', JSON.stringify(await pep.pluginObjectOp(oid, 'Call', 'CallFunction', ['<invoke name="nosuch" returntype="javascript"><arguments></arguments></invoke>'])));
    console.log('Call echo direct', JSON.stringify(await pep.pluginObjectOp(oid, 'Call', 'echo', ['direct hello'])));
    console.log('GetProperty echo', JSON.stringify(await pep.pluginObjectOp(oid, 'GetProperty', 'echo')));
    await new Promise((r) => setTimeout(r, 1500));
    console.log('frames after call', frames);
  }
  if (argv.includes('--call')) {
    const object = await pep.getInstanceObject(1);
    const name = String(opt('--call', 'selfTest'));
    console.log('integration callback', name, JSON.stringify(await pep.pluginObjectOp(object, 'Call', name, [])));
  }
  if (argv.includes('--ctx')) {
    const [cx, cy] = String(opt('--ctx', '200,150')).split(',').map(Number);
    const ev = (etype, button, clicks, mods = 0) => ({ etype, ts: performance.now() / 1000, mods, mouse: { button, x: cx, y: cy, clicks, mx: 0, my: 0 } });
    for (const e of [ev(3, -1, 0), ev(2, -1, 0), ev(0, 2, 1, 256), ev(10, 2, 1, 256), ev(1, 2, 1, 0)]) { const r = await pep.sendInput(1, e); console.log('input type', e.etype, '->', r); await new Promise((r) => setTimeout(r, 300)); }
    await new Promise((r) => setTimeout(r, 1500));
  }
  if (argv.includes('--click')) {
    const [cx, cy] = String(opt('--click', '262,172')).split(',').map(Number);
    const ev = (etype, button, clicks) => ({ etype, ts: performance.now() / 1000, mods: 0, mouse: { button, x: cx, y: cy, clicks, mx: 0, my: 0 } });
    for (const e of [ev(3, -1, 0), ev(2, -1, 0), ev(0, 0, 1), ev(1, 0, 1)]) { const r = await pep.sendInput(1, e); console.log('input type', e.etype, '->', r); await new Promise((r) => setTimeout(r, 300)); }
    await new Promise((r) => setTimeout(r, 1500));
  }
}
stop = true;
if (nanojit) { if (capturePath) writeFileSync(capturePath, JSON.stringify({ format: 'flash-runner-nanojit-capture-v1', stats: nanojit.stats, captures: nanojit.captures })); console.log('[nanojit] capture stats', JSON.stringify(nanojit.stats)); console.log(emu.jitReport().split('\n').filter((line) => /\[accel\]|\[aot\]|offline regions|promoted_to_jit|completed_native_leaf_calls/.test(line)).join('\n')); }
else if (configuredAot) console.log(emu.jitReport().split('\n').filter((line) => /\[accel\]|\[aot\]|offline regions|promoted_to_jit/.test(line)).join('\n'));
if (process.env.EMU_JIT_PROFCHAINS) console.log(emu.jitHotReport(+process.env.EMU_JIT_PROFCHAINS > 1 ? +process.env.EMU_JIT_PROFCHAINS : 30));
if (process.env.EMU_DUMP_GUEST) for (const spec of process.env.EMU_DUMP_GUEST.split(';')) { const [a, n, f] = spec.split(','); writeFileSync(f, emu.guest().slice(parseInt(a, 16), parseInt(a, 16) + parseInt(n))); }   // addr(hex),len,file;...: raw guest memory at the end
if (process.env.JIT_REPORT_FULL) console.log(emu.jitReport());   // complete [jit]/[accel] report (dispatcher exits, exec_one by mnemonic, ...)
if (process.env.JIT_REPORT) console.log(emu.jitReport().split('\n').filter((l) => /exec_one by mnemonic|guest ins retired|helper calls/.test(l)).join('\n').slice(0, 1500));
if (argv.includes('--audio')) console.log(`audio: ${simSink.stats().frames} frames (${(simSink.stats().frames / 48000).toFixed(2)} s) peak=${simSink.stats().peak.toFixed(3)}`);
console.log(`frames=${frames} ppb calls=${pep.stats.calls} unimplemented=${[...pep.unimpl].join(', ') || 'none'} crashed=${pep.crashed ? 'YES' : 'no'}`);
if (argv.includes('--read-timing-pixel') && last) {
  const at = (241 * last.w + 341) * 4;
  console.log(`[integration] guest workload ${last.px[at + 2] * 65536 + last.px[at + 1] * 256 + last.px[at]} ms`);
  if (/lir-arithmetic/.test(swfPath || '')) {
    const hotAt = (241 * last.w + 331) * 4;
    console.log(`[integration] typed hot calls ${last.px[hotAt + 2] * 65536 + last.px[hotAt + 1] * 256 + last.px[hotAt]} ms`);
  }
}
if (argv.includes('--expect-pixel')) {
  const [xs, ys, wanted] = String(opt('--expect-pixel')).split(',');
  const x = Number(xs), y = Number(ys);
  if (!last || x < 0 || y < 0 || x >= last.w || y >= last.h) throw new Error('expected pixel is unavailable');
  const at = (y * last.w + x) * 4;
  const actual = [last.px[at + 2], last.px[at + 1], last.px[at]].map((b) => b.toString(16).padStart(2, '0')).join('');
  if (actual !== wanted.toLowerCase()) throw new Error(`pixel ${x},${y}: expected ${wanted}, got ${actual}`);
  console.log(`PASS expected pixel ${x},${y}=${actual}`);
}
if (out && last) { const { w, h, px } = last; const rgb = Buffer.alloc(w * h * 3); for (let i = 0; i < w * h; i++) { rgb[i * 3] = px[i * 4 + 2]; rgb[i * 3 + 1] = px[i * 4 + 1]; rgb[i * 3 + 2] = px[i * 4]; } writeFileSync(out, Buffer.concat([Buffer.from(`P6 ${w} ${h} 255\n`), rgb])); console.log('wrote', out); }
server.close();
process.exit(0);
