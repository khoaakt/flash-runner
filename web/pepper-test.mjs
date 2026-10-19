// End-to-end test of the Pepper bridge with the fake plugin (no Adobe code involved):
//   node web/pepper-test.mjs [--plugin path/to/lib.so] [--trace]
import { readFileSync } from 'node:fs';
import { Emulator } from './emu.js';
import { Pepper } from './pepper.js';
import http from 'node:http';

const argv = process.argv.slice(2);
let pluginPath = new URL(process.env.FP ? '../guest/out/libfakeplug-fp.so' : '../guest/out/libfakeplug.so', import.meta.url).pathname, trace = false;
for (let i = 0; i < argv.length; i++) { if (argv[i] === '--plugin') pluginPath = argv[++i]; if (argv[i] === '--trace') trace = true; }

const wasm = readFileSync(new URL('./emu.wasm', import.meta.url));
const emu = await Emulator.create({ wasm, onConsole: (fd, b) => process.stderr.write(b), onLog: (s) => process.stderr.write(s + '\n') });
// EMU_HEAP_PAD_MB=N: reserve linear memory first so every later pointer lies above 2 GiB (see web/node-run.mjs)
for (let left = +(process.env.EMU_HEAP_PAD_MB || 0); left > 0; left -= 256) emu.x.emu_alloc(Math.min(left, 256) * 1048576);
emu.vfsNew();
emu.loadTar(readFileSync(new URL('../rootfs/out/rootfs.tar', import.meta.url)));
emu.addFile('/work/pepper-host', readFileSync(new URL('../guest/out/pepper-host', import.meta.url)));
emu.addFile('/work/plugin.so', readFileSync(pluginPath));
emu.boot(512, 0);
emu.spawn('/work/pepper-host', ['/work/pepper-host'], ['HOME=/home/flash', 'LANG=C']);

const server = http.createServer((req, res) => { res.setHeader('content-type', 'text/plain'); res.end('payload from the page origin\n'); });
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}/`;
const messages = [], frames = [];
const sinkFrames = []; let buffered = 0;
const audioSink = { targetFrames: () => 4800, bufferedFrames: () => buffered, write: (f) => { sinkFrames.push(f); buffered += f.length / 2; if (buffered > 20000) buffered = 20000; } };
const pep = new Pepper(emu, { trace, baseUrl: base, audioSink, onMessage: (i, m) => { messages.push(m); console.log('  plugin says:', JSON.stringify(m)); }, onFrame: (i, buf, w, h) => frames.push({ w, h, px: buf.slice() }) });
pep.modulePath = '/work/plugin.so';
const ready = pep.whenReady();
let stop = false;
const runP = pep.run(() => stop);
await ready;
console.log('plugin initialised, PPP_InitializeModule ->', pep.initResult);
const t0 = performance.now();
await pep.createInstance(1, { src: 'demo.swf', quality: 'high', fetch: base + 'data.txt', audio: 1 }, { w: 64, h: 48 });
console.log('instance created');
await new Promise((r) => { const t = setInterval(() => { if (frames.length >= 3 || performance.now() - t0 > 8000) { clearInterval(t); r(); } }, 20); });
await pep.postMessageToPlugin(1, 'hello bridge');
await pep.sendInput(1, { etype: 0, ts: 1, mods: 0, mouse: { button: 0, x: 11, y: 22, clicks: 1, mx: 0, my: 0 } });
await new Promise((r) => setTimeout(r, 200));
stop = true;

let ok = true;
const check = (name, cond) => { console.log((cond ? 'PASS ' : 'FAIL ') + name); ok &&= cond; };
check('3 frames flushed', frames.length >= 3);
const px = (f, x, y) => Array.from(frames[f].px.subarray((y * frames[f].w + x) * 4, (y * frames[f].w + x) * 4 + 4));
if (frames.length >= 2) {
  check('frame0 (0,0) = BGRA 0,0,0,255', JSON.stringify(px(0, 0, 0)) === '[0,0,0,255]');
  const w = 64, h = 48, f = 1;
  const x = 10, y = 20;
  const want = [(f * 90) & 255, (y * 255 / h) | 0, ((x * 255 / w | 0) + f * 40) & 255, 255];
  check(`frame1 (${x},${y}) = ${want}`, JSON.stringify(px(1, x, y)) === JSON.stringify(want));
}
check('didcreate message with args', messages.some((m) => /didcreate argc=4 src=demo.swf quality=high fetch=http/.test(m)));
check('view message 64x48', messages.some((m) => /view 64x48 fullscreen=0 visible=1/.test(m)));
check('flush callbacks ran', messages.filter((m) => /^flushed frame=\d result=0/.test(m)).length >= 3);
check('echo of postMessage', messages.includes('echo:hello bridge'));
check('mouse event delivered', messages.some((m) => /mouse type=0 at 11,22 button=0/.test(m)));
check('audio callbacks produced PCM', sinkFrames.length >= 3 && Math.max(...sinkFrames[0].slice(0, 200)) > 0.2);
check('audio cb message', messages.some((m) => /^audio cb 3 frames=\d+/.test(m)));
check('URLLoader fetched the body', messages.some((m) => /^url body \(29 bytes\): payload from the page origin/.test(m)));
if (process.env.FP) check('time_ok=1', messages.includes('time_ok=1'));
console.log('PPB calls:', pep.stats.calls, 'unimplemented:', [...pep.unimpl].join(', ') || 'none');
server.close();
process.exit(ok ? 0 : 1);
