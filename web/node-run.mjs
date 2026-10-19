// node web/node-run.mjs [--rootfs tar] [--sse2] [--mem MB] [--trace] [--jit N] [--aot trusted-bundle] prog [args...]
//   --jit N (or env EMU_JIT=N): 0 = interpreter only, 1 = translate every block at first sight, N = after N executions.
//   env EMU_WASM=path overrides web/emu.wasm; EMU_STATS=1 prints timing and JIT statistics.
// Runs a guest program inside the wasm build (V8) — the portability check for tests/difftest.
import { readFileSync, readdirSync, writeFileSync, mkdirSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { Emulator, EV } from './emu.js';
import v8 from 'node:v8';
// node can deadlock in process.exit(): the main thread joins the V8 worker threads while one of them (a concurrent baseline compile) waits for a GC
// that only the main thread could run (seen as a test run that never ends). Compiling baseline code on the main thread avoids it.
v8.setFlagsFromString('--no-concurrent-sparkplug');

const argv = process.argv.slice(2);
let rootfs = null, noSse = false, mem = 512, trace = false;
let aot = null;
let jit = process.env.EMU_JIT !== undefined ? +process.env.EMU_JIT : 0;
while (argv.length && argv[0].startsWith('--')) {
  const a = argv.shift();
  if (a === '--rootfs') rootfs = argv.shift();
  else if (a === '--sse2') { /* default now */ }
  else if (a === '--no-sse2') noSse = true;
  else if (a === '--mem') mem = +argv.shift();
  else if (a === '--trace') trace = true;
  else if (a === '--jit') jit = +argv.shift();
  else if (a === '--aot') aot = argv.shift();
  else if (a === '--') break;
}
const prog = argv[0];
const wasm = readFileSync(process.env.EMU_WASM || new URL('./emu.wasm', import.meta.url));
const out = [];
const emu = await Emulator.create({
  wasm,
  onConsole: (fd, b) => (fd === 1 ? process.stdout : process.stderr).write(b),
  onLog: (s) => process.stderr.write(s + '\n'),
});
// regression switch: EMU_HEAP_PAD_MB=N reserves N MB of linear memory first, so every pointer handed out afterwards (VFS buffers,
// guest RAM, JIT modules) lies above 2 GiB when N is ~2100 — the range where wasm i32 pointers read as negative numbers in JS.
// (in 256 MB pieces: a single Rust allocation above isize::MAX = 2 GiB - 1 panics)
for (let left = +(process.env.EMU_HEAP_PAD_MB || 0); left > 0; left -= 256) emu.x.emu_alloc(Math.min(left, 256) * 1048576);
emu.vfsNew();
if (rootfs) emu.loadTar(readFileSync(rootfs));
const dir = dirname(prog);
for (const f of readdirSync(dir)) if (f.endsWith('.so')) emu.addFile('/work/' + f, readFileSync(join(dir, f)));
const guestPath = '/work/' + basename(prog);
emu.addFile(guestPath, readFileSync(prog));
emu.boot(mem, noSse ? 0x80000000 : 0);
if (trace) emu.x.emu_set_trace(1);
if (jit) emu.setJit(jit, { profChains: !!process.env.EMU_JIT_PROFCHAINS, intrinsics: !process.env.EMU_JIT_NOINTR, trustLoads: !!process.env.EMU_JIT_TRUST }); // EMU_JIT_PROFCHAINS=1: per-chain counters, printed with EMU_STATS=1
if (jit && process.env.EMU_JIT_DUP) emu.setJitDup(+process.env.EMU_JIT_DUP); // test switch: copy small compiled callees into callers (instruction cap)
if (jit && process.env.EMU_JIT_DIV) emu.setJitRegionHotDiv(+process.env.EMU_JIT_DIV); // test switch: region merge hotness divisor
// test switches: pretend the engine lacks wasm tail calls (EMU_JIT_NOCHAIN=1) and/or SIMD (EMU_JIT_NOSIMD=1)
if (jit && (process.env.EMU_JIT_NOCHAIN || process.env.EMU_JIT_NOSIMD)) emu.x.emu_set_jit_features(process.env.EMU_JIT_NOCHAIN ? 0 : 1, process.env.EMU_JIT_NOSIMD ? 0 : 1);
if (process.env.EMU_JIT_DUMP) {
  // EMU_JIT_DUMP=dir: write every translated module to dir/jitN.wasm (inspect with wasm2wat / wasm-objdump)
  mkdirSync(process.env.EMU_JIT_DUMP, { recursive: true });
  let n = 0;
  emu.onJitModule = (b) => writeFileSync(`${process.env.EMU_JIT_DUMP}/jit${n++}.wasm`, b);
}
if (process.env.EMU_EIPPROF) emu.setEipProf(true);   // test switch: guest address sampling histogram, printed at the end
emu.spawn(guestPath, [guestPath, ...argv.slice(1)], ['PATH=/usr/bin:/bin', 'HOME=/home/flash', 'LANG=C']);
if (aot) {
  if (!jit) throw new Error('--aot requires --jit N');
  const count = emu.installAot(readFileSync(aot));
  process.stderr.write(`[aot] installed ${count} offline regions\n`);
}
const t0 = performance.now();
let code = 0;
for (;;) {
  const ev = emu.run(200000, 64);
  if (ev.type === EV.EXITED) { code = ev.arg; break; }
  if (ev.type === EV.CRASH) { process.stderr.write('[emu] CRASH: ' + ev.msg + '\n'); code = 139; break; }
  if (ev.type === EV.IDLE) {
    const now = emu.x.emu_ev_arg();
    if (ev.arg < 0) { process.stderr.write('[emu] deadlock\n'); code = 124; break; }
    const waitMs = Math.max(0, (ev.arg - (performance.now() - emu.t0) * 1e6 - 1e9) / 1e6);
    await new Promise((r) => setTimeout(r, Math.min(waitMs, 50)));
  }
  if (ev.type === EV.HYPER) { process.stderr.write('[emu] unhandled hypercall\n'); code = 123; break; }
}
if (process.env.EMU_STATS) {
  process.stderr.write(`[emu] wasm elapsed ${(performance.now() - t0).toFixed(0)} ms\n`);
  if (jit) process.stderr.write(emu.jitReport() + '\n');
  if (jit && process.env.EMU_JIT_PROFCHAINS) process.stderr.write(emu.jitHotReport(8) + '\n');
}
if (process.env.EMU_EIPPROF) process.stderr.write(emu.eipProfReport(10) + '\n');
// stdout/stderr may be pipes: process.exit() would drop whatever the reader has not consumed yet (truncated output under load)
await new Promise((r) => process.stdout.write('', r));
await new Promise((r) => process.stderr.write('', r));
process.exit(code);
