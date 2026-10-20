// Worker: owns the emulator, the Pepper host and the canvas. The page talks to it with postMessage.
import { Emulator } from './emu.js';
import { Pepper } from './pepper.js';
import { Presenter } from './present.js';
import { IdbPersist } from './persist.js';
import { NetCache } from './net-cache.js';
import { BridgeSockets } from './socket-bridge.js';
import { preparePluginAccelerators } from './plugin-accelerators.js';
import { createNanojitCapture } from './nanojit-capture.js';
import { WORKER_LIR_FAMILIES, resolveLirFamilies } from './lir-policy.js';

let totalFrames = 0, pep = null, instance = 1, presenter = null, canvasSize = { w: 640, h: 480 };
let audioSink = null, audioReadyResolve = null;
const audioReady = new Promise((r) => { audioReadyResolve = r; });

// Output sinks for PPB_Audio: a SharedArrayBuffer ring drained by the AudioWorklet (preferred),
// or a message-based queue when the page is not cross-origin isolated.
class RingSink {
  constructor(sab, rate) { this.h = new Int32Array(sab, 0, 4); this.d = new Float32Array(sab, 16); this.cap = Atomics.load(this.h, 2); this.rate = rate; }
  targetFrames() { return Math.min(this.cap >> 1, Math.round(this.rate * 0.1)); }
  bufferedFrames() { let a = Atomics.load(this.h, 0) - Atomics.load(this.h, 1); if (a < 0) a += this.cap; return a; }
  write(f, srcRate) {
    f = resample(f, srcRate, this.rate);
    const n = f.length >> 1; let w = Atomics.load(this.h, 0);
    if (this.cap - this.bufferedFrames() - 1 < n) return;      // ring full: drop (should not happen with pacing)
    for (let i = 0; i < n; i++) { this.d[w * 2] = f[i * 2]; this.d[w * 2 + 1] = f[i * 2 + 1]; if (++w >= this.cap) w = 0; }
    Atomics.store(this.h, 0, w);
  }
}
class MessageSink {
  constructor(rate) { this.rate = rate; this.level = 0; }
  targetFrames() { return Math.round(this.rate * 0.12); }
  bufferedFrames() { return this.level; }
  write(f, srcRate) { f = resample(f, srcRate, this.rate); this.level += f.length >> 1; self.postMessage({ type: 'audio-data', f }, [f.buffer]); }
}
function resample(f, from, to) {
  if (from === to) return f;
  const n = Math.floor((f.length >> 1) * to / from), out = new Float32Array(n * 2);
  for (let i = 0; i < n; i++) {
    const p = i * from / to, i0 = Math.floor(p), i1 = Math.min(i0 + 1, (f.length >> 1) - 1), t = p - i0;
    out[i * 2] = f[i0 * 2] * (1 - t) + f[i1 * 2] * t; out[i * 2 + 1] = f[i0 * 2 + 1] * (1 - t) + f[i1 * 2 + 1] * t;
  }
  return out;
}
const post = (type, data = {}, transfer = []) => self.postMessage({ type, ...data }, transfer);
// The extension's "Debug log" setting: off means no message is posted (and the guest console is not decoded). Standalone
// pages leave `log` undefined and keep logging.
let logging = true;
const log = (s) => { if (logging) post('log', { s }); };
// Give the event loop a turn (fetch completions, input messages) through one reused MessageChannel: creating a channel per yield was ~100 ports/s of garbage.
const yieldChannel = new MessageChannel();
let yieldPending = null;
yieldChannel.port1.onmessage = () => { const r = yieldPending; yieldPending = null; if (r) r(); };
const yieldToEventLoop = () => new Promise((r) => { yieldPending = r; yieldChannel.port2.postMessage(0); });

// Scripting bridge to the embedding page (ExecuteScript / window objects): request/reply over the main thread.
const sockBridge = new BridgeSockets((msg) => post('sock', { msg }));          // raw TCP through the native helper (see player.js)
const pageWaiters = new Map();
let pageSeq = 0;
// Requests to the player frame's main thread for what a worker cannot do itself (pickers, camera, print): see web/frame-services.js.
const svcWaiters = new Map();
let svcSeq = 0;
const frameService = {
  request: (op, args = {}) => new Promise((resolve) => { const id = ++svcSeq; svcWaiters.set(id, resolve); post('svc', { id, op, args }); }),   // resolves { ok, value | error }
};
const pageBridge = {
  request: (msg) => new Promise((resolve) => { const id = ++pageSeq; pageWaiters.set(id, resolve); post('page', { id, msg }); }),
  post: (msg) => post('page', { id: 0, msg }),
};

async function fetchBytes(url, label) {
  const t0 = performance.now();
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${label || url}: HTTP ${r.status}`);
  const b = new Uint8Array(await r.arrayBuffer());
  log(`loaded ${label || url} (${(b.length / 1048576).toFixed(1)} MiB, ${(performance.now() - t0).toFixed(0)} ms)`);
  return b;
}

async function boot(m) {
  const { assets, canvas, args, size, baseUrl, documentUrl, memMB } = m;
  logging = m.log !== false;
  canvasSize = size;
  presenter = await Presenter.create(canvas, { log, force2d: m.force2d });
  log(`presenter: ${presenter.kind}`);
  const [wasm, rootfs, host, plugin] = await Promise.all([
    fetchBytes(assets.wasm, 'emu.wasm'), fetchBytes(assets.rootfs, 'rootfs.tar'),
    fetchBytes(assets.host, 'pepper-host'),
    m.pluginBytes ? Promise.resolve(new Uint8Array(m.pluginBytes)) : fetchBytes(assets.plugin, 'plugin'),   // extension: runtime comes from IndexedDB
  ]);
  const emu = await Emulator.create({ wasm, onConsole: (fd, b) => { if (logging) log(`[guest fd${fd}] ${new TextDecoder().decode(b).trimEnd()}`); }, onLog: log });
  const acceleratorAssets = new URL('./runtime-aot/manifest.json', new URL(assets.wasm, self.location.href));
  const accelerators = await preparePluginAccelerators({ emu, plugin, manifestUrl: acceleratorAssets.href, log, enabled: m.jit !== 0 && m.aot !== false });
  const nanojit = m.jit !== 0 && m.lir !== false
    ? await createNanojitCapture({ emu, plugin, log, lower: true, families: resolveLirFamilies(m.lirFamilies, WORKER_LIR_FAMILIES) }).catch((e) => { log(`[nanojit] native fallback: ${e.message || e}`); return null; })
    : null;
  emu.vfsNew();
  emu.loadTar(rootfs);
  emu.addFile('/opt/pepper-host', host);
  emu.addFile('/opt/plugin.so', plugin);
  const bootT0 = performance.now();
  emu.boot(memMB || 768, m.noSse ? 0x80000000 : 0);
  log(`[boot] guest memory ${memMB || 768} MiB ready in ${(performance.now() - bootT0).toFixed(0)} ms`);
  // Checked guest loads are the correctness default. The unsafe diagnostic
  // shortcut must be explicitly requested, never inferred from an absent flag.
  if (m.jit !== 0) emu.setJit(m.jit || 100, { profChains: m.profile === 'deep', intrinsics: m.jitIntrinsics !== false, trustLoads: m.jitTrustLoads === true });
  emu.spawn('/opt/pepper-host', ['/opt/pepper-host'], ['HOME=/home/flash', 'LANG=C', 'TZ=UTC', `FW_ARENA_MB=${Math.max(32, Math.min(256, ((memMB || 768) / 5) | 0))}`, ...(m.profile ? ['FW_MAPS_SEC=5'] : [])]);
  const persist = m.persist === false ? null : await IdbPersist.open(m.storageName || 'flash-runner-storage').catch(() => null);
  // static downloads (SWF/XML/images/sound): memory + IndexedDB cache with HTTP validation (web/net-cache.js); m.netCache === false turns it off, m.netCacheClear empties it
  const netCache = m.netCache === false ? null : await NetCache.open({ name: m.netCacheName || 'flash-runner-netcache', memBytes: (m.netCacheMemMB || 64) << 20, dbBytes: (m.netCacheDbMB || 512) * 1048576 }).catch(() => null);
  if (netCache && m.netCacheClear) await netCache.clear();
  if (m.wantAudio) await Promise.race([audioReady, new Promise((r) => setTimeout(r, 3000))]);
  let frames = 0, t0 = performance.now();
  let lirCompletionReported = false;
  let lastFrame = 0, smooth = 40;                       // frame pacing: a frame that comes > 150 ms after an animating one is a freeze worth explaining
  const countFrame = () => {
    if (!lirCompletionReported && nanojit?.stats.lowered) {
      const completed = emu.lirCompletions();
      if (completed > 0n) { log(`[nanojit] completed_native_leaf_calls=${completed}`); lirCompletionReported = true; }
    }
    frames++; totalFrames++;
    const now = performance.now();
    if (m.fpsLog || m.profile) {
      const gap = lastFrame ? now - lastFrame : 0; lastFrame = now;
      const tm = pep.tm || { emu: 0, yld: 0, idle: 0 };
      if (gap > 150 && smooth < 80) log(`[stall] ${gap.toFixed(0)} ms without a frame: guest ${tm.emu.toFixed(0)} ms, event-loop turns ${tm.yld.toFixed(0)} ms, idle ${tm.idle.toFixed(0)} ms, other ${(gap - tm.emu - tm.yld - tm.idle).toFixed(0)} ms (host calls, GC)`);
      if (gap > 0) smooth = smooth * 0.9 + Math.min(gap, 400) * 0.1;
      tm.emu = tm.yld = tm.idle = 0;
    }
    if (now - t0 > 1000) { post('stats', { fps: frames * 1000 / (now - t0), calls: pep.stats.calls }); frames = 0; t0 = now; }
  };
  pep = new Pepper(emu, {
    onPluginLoaded: (image) => { nanojit?.onPluginLoaded(image); return accelerators.onPluginLoaded(image); },
    onEntryProbe: (probe) => nanojit?.onEntryProbe(probe),
    persist, netCache, language: m.language, numCores: 1,
    gpu3d: !!m.gpu3d,                                   // PPB_Flash::GetSetting: advertise Stage3D / GPU compositing
    onSwap: (i, glCanvas, w, h) => { presenter.presentCanvas(glCanvas, w, h); countFrame(); },   // PPB_Graphics3D::SwapBuffers
    baseUrl, documentUrl, trace: !!m.trace, screenSize: m.screen,
    service: frameService,
    page: m.pageBridge ? pageBridge : null, profile: m.profile, stallLog: !!m.fpsLog || !!m.profile, sockets: m.sockBridge ? sockBridge : null, sampleRate: audioSink?.rate || 48000, get audioSink() { return audioSink; },
    yield: yieldToEventLoop,
    onMessage: (i, v) => post('plugin-message', { value: typeof v === 'object' ? String(v) : v }),
    onFrame: (i, buf, w, h) => { presenter.present(buf, w, h); countFrame(); },
    onCursor: (css) => post('cursor', { css }),
    onNavigate: (url, target, user) => post('navigate', { url, target, user }),
    onFullscreen: (inst, on) => post('fullscreen', { on }),
    onClipboardWrite: (text) => post('clipboard', { text }),
    onMenu: (inst, id, items, x, y) => post('menu', { id, items, x, y }),
    log,
  });
  pep.modulePath = '/opt/plugin.so';
  if (m.fpsLog && !m.profile) {                         // hidden setting `fpsLog`: only the frame rate, every 3 s (the diagnostics log times every hypercall and costs a few percent)
    let last = performance.now(), frames0 = 0;
    setInterval(() => { const now = performance.now(), fr = totalFrames - frames0; frames0 = totalFrames; log(`[fps] ${(fr * 1000 / (now - last)).toFixed(1)}`); last = now; }, 3000);
  }
  if (m.profile) { self.__pep = pep; self.__emu = emu; self.__accelerators = accelerators.state; self.__nanojit = nanojit; }   // handles for external probe harnesses
  if (m.profile) {                                      // diagnostics: periodic one-line stats on the console (extension setting "diag")
    if (m.profile === 'deep' && emu) emu.setJitProfile(true);       // phase timers cost ~18% (performance.now per dispatch): only on request
    let last = performance.now(), n = 0, frames0 = 0;
    setInterval(() => {
      const now = performance.now(), dt = now - last; last = now;
      const p = pep.profSnapshot(); if (!p) return;
      const fr = totalFrames - frames0; frames0 = totalFrames;
      log(`[prof] ${(fr * 1000 / dt).toFixed(1)} fps | per ${dt.toFixed(0)} ms: emu ${p.emuMs.toFixed(0)} hyper ${p.hyperMs.toFixed(0)} idle ${p.idleMs.toFixed(0)} yield ${p.yieldMs.toFixed(0)} | hypercalls ${p.hypers} | mem ${emu ? (emu.mem.buffer.byteLength / 1048576).toFixed(0) + ' MiB' : 'cx'} | top: ${p.top.join(' ')}`);
      if (pep.pendingFetches?.size) log(`[net] pending: ${[...pep.pendingFetches.values()].slice(0, 6).join(' ')}`);
      if (emu && ++n % 3 === 0) {
        log(emu.jitReport());
        if (nanojit) log(`[nanojit] capture stats ${JSON.stringify(nanojit.stats)} completed_native_leaf_calls=${emu.lirCompletions()}`);
      }
    }, 3000);
  }
  const ready = pep.whenReady();
  const runP = pep.run();
  await ready;
  log(`PPP_InitializeModule -> ${pep.initResult}`);
  if (pep.initResult !== 0) { post('error', { message: 'plugin refused to initialise' }); return; }
  await pep.restoreStorage();                          // Shared Objects saved in an earlier session
  await pep.createInstance(instance, args, size);
  post('ready', { presenter: presenter.kind });
  const r = await runP;
  post('exit', { code: r, crash: pep.crashed || '' });
}

self.onmessage = (e) => {
  const m = e.data;
  if (m.type === 'boot') boot(m).catch((err) => post('error', { message: String(err && err.stack || err) }));
  else if (m.type === 'input' && pep) pep.sendInput(instance, m.ev);
  else if (m.type === 'message' && pep) pep.postMessageToPlugin(instance, m.value);
  else if (m.type === 'focus' && pep) pep.changeFocus(instance, !!m.has);
  else if (m.type === 'menu-result' && pep) pep.menuResult(m.id, m.selected);
  else if (m.type === 'clipboard-text' && pep) pep.clipboardText = String(m.text ?? '');
  else if (m.type === 'sock-in') sockBridge.handle(m.msg);
  else if (m.type === 'svc-reply') { const w = svcWaiters.get(m.id); svcWaiters.delete(m.id); w?.(m); }
  else if (m.type === 'gamepads' && pep) pep.gamepads = m.pads;
  else if (m.type === 'media' && pep?.mediaIn) pep.mediaIn(m);          // a camera or microphone chunk from the frame (frame-media.js)
  else if (m.type === 'page-reply') { const w = pageWaiters.get(m.id); pageWaiters.delete(m.id); w?.(m.reply); }
  else if (m.type === 'invoke' && pep) pep.invokeInstance(instance, m.name, m.args || []).then((r) => post('invoke-result', { id: m.id, ...r }), (e) => post('invoke-result', { id: m.id, ok: false, exception: String(e) }));
  else if (m.type === 'audio') { audioSink = m.sab ? new RingSink(m.sab, m.rate) : new MessageSink(m.rate); audioReadyResolve(); }
  else if (m.type === 'audio-level' && audioSink instanceof MessageSink) audioSink.level = m.frames;
  else if (m.type === 'view' && pep) pep.changeView(instance, m.rect, m.opts || {});
  else if (m.type === 'snapshot') presenter?.snapshot().then((s) => post('snapshot', { w: s.w, h: s.h, data: s.data.buffer, id: m.id }, [s.data.buffer]), (e) => post('snapshot', { error: String(e), id: m.id }));
};
