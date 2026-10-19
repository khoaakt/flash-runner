// One-line embed:  <script type="module" src="/web/loader.js"></script>
// Replaces <embed>/<object> SWF elements with a canvas driven by the emulated Flash runtime.

import { createFrameServices, watchGamepads } from './frame-services.js';

const WORKER_URL = new URL('./worker.js', import.meta.url);

const IS_MAC = typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform || navigator.userAgent || '');
const BUTTON = (b) => (b === 0 ? 0 : b === 1 ? 1 : b === 2 ? 2 : -1);
function mods(e) {
  return (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) | (e.metaKey ? 8 : 0) |
    (e.buttons & 1 ? 64 : 0) | (e.buttons & 4 ? 128 : 0) | (e.buttons & 2 ? 256 : 0) | (e.getModifierState?.('CapsLock') ? 512 : 0) | (e.getModifierState?.('NumLock') ? 1024 : 0);
}

export class FlashPlayer {
  constructor(el, opts = {}) {
    this.el = el;
    const w = parseInt(el.getAttribute('width') || el.width || 640, 10) || 640;
    const h = parseInt(el.getAttribute('height') || el.height || 480, 10) || 480;
    this.size = { w, h };
    this.opts = opts;
    this.canvas = document.createElement('canvas');
    this.canvas.width = w; this.canvas.height = h;
    this.canvas.tabIndex = 0;
    this.canvas.style.cssText = opts.fill ? 'width:100%;height:100%;display:block;outline:none' : `width:${w}px;height:${h}px;display:block;background:#000;outline:none`;
    this.status = document.createElement('div');
    this.status.style.cssText = 'font:12px/1.4 ui-monospace,monospace;color:#888;padding:4px 0';
    this.wrap = document.createElement('div');
    if (opts.fill) this.wrap.style.cssText = 'width:100%;height:100%';
    this.wrap.append(this.canvas);
    if (opts.statusBar !== false) this.wrap.append(this.status);
    if (opts.host) opts.host.append(this.wrap); else el.replaceWith(this.wrap);
    this.listeners = [];
    this.services = { ...createFrameServices(this), ...(opts.services || {}) };   // what the worker may ask this frame to do (web/frame-services.js)
  }

  async start() {
    const el = this.el;
    const src = el.getAttribute('src') || el.getAttribute('data') || '';
    const args = {};
    for (const a of el.attributes) if (!['width', 'height', 'style', 'class'].includes(a.name)) args[a.name] = a.value;
    for (const p of el.querySelectorAll?.('param') || []) args[p.name] = p.value;
    if (src) args.src = new URL(src, document.baseURI).href;
    const worker = this.worker = new Worker(WORKER_URL, { type: 'module' });
    const off = this.canvas.transferControlToOffscreen();
    const assets = this.opts.assets || { wasm: '/assets/emu.wasm', rootfs: '/assets/rootfs.tar', host: '/assets/pepper-host', plugin: this.opts.plugin || '/vendor/plugin.so' };
    worker.onmessage = (e) => this.onWorker(e.data);
    const wantAudio = this.opts.audio !== false && typeof AudioContext !== 'undefined';
    if (wantAudio) this.initAudio().catch((e) => console.warn('audio init failed', e));
    const pluginBytes = this.opts.pluginBytes || null;
    worker.postMessage({ type: 'boot', log: this.opts.log, aot: this.opts.aot, lir: this.opts.lir, jitIntrinsics: this.opts.jitIntrinsics, jitTrustLoads: this.opts.jitTrustLoads, netCache: this.opts.netCache, wantAudio, assets, pluginBytes, persist: this.opts.persist, language: this.opts.language, memMB: this.opts.memMB, storageName: this.opts.storageName, canvas: off, args, size: this.size, baseUrl: this.opts.baseUrl || document.baseURI, documentUrl: this.opts.pageUrl || location.href, pageBridge: !!this.opts.pageBridge, force2d: !!this.opts.force2d, noSse: this.opts.sse2 === false, trace: !!this.opts.trace, gpu3d: !!this.opts.gpu3d, jit: this.opts.jit, profile: this.opts.profile, fpsLog: !!this.opts.fpsLog, sockBridge: !!this.opts.sockBridge, screen: { w: screen.width, h: screen.height } }, [off]);
    this.bindInput();
    this.stopGamepads = watchGamepads((pads) => worker.postMessage({ type: 'gamepads', pads }));   // PPB_Gamepad::Sample reads the latest of these
    this.status.textContent = 'starting…';
  }

  async initAudio() {
    const ctx = this.audioCtx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
    await ctx.audioWorklet.addModule(new URL('./audio-worklet.js', import.meta.url));
    let sab = null;
    if (typeof SharedArrayBuffer !== 'undefined' && self.crossOriginIsolated) {
      const cap = ctx.sampleRate;                       // 1 s ring
      sab = new SharedArrayBuffer(16 + cap * 2 * 4);
      new Int32Array(sab)[2] = cap;
    }
    const node = new AudioWorkletNode(ctx, 'flash-audio', { outputChannelCount: [2], processorOptions: { sab } });
    node.connect(ctx.destination);
    if (!sab) node.port.onmessage = (e) => this.worker.postMessage({ type: 'audio-level', frames: e.data });
    this.audioNode = node;
    this.worker.postMessage({ type: 'audio', sab, rate: ctx.sampleRate });
    const resume = () => { if (ctx.state !== 'running') ctx.resume(); };
    for (const t of ['pointerdown', 'keydown', 'touchstart']) addEventListener(t, resume, { passive: true });
  }

  onWorker(m) {
    switch (m.type) {
      case 'audio-data': this.audioNode?.port.postMessage(m.f, [m.f.buffer]); break;
      case 'log': if (this.opts.log !== false) { console.log('[flash]', m.s); this.opts.onLog?.(m.s); } break;   // opts.log === false: the extension's "Debug log" setting is off
      case 'ready': this.status.textContent = `running (${m.presenter})`; this.opts.onReady?.(m); break;
      case 'stats': this.status.textContent = `${m.fps.toFixed(0)} fps · ${m.calls} PPB calls`; break;
      case 'page': this.pageRequest(m); break;
      case 'svc': this.service(m); break;
      case 'sock': this.opts.sockBridge?.(m.msg); break;
      case 'invoke-result': { const r = this.invocations?.get(m.id); this.invocations?.delete(m.id); r?.(m); break; }
      case 'plugin-message': this.opts.onMessage?.(m.value); break;
      case 'snapshot': { const r = this.snapshots?.get(m.id); this.snapshots?.delete(m.id); if (r) m.error ? r.reject(new Error(m.error)) : r.resolve({ w: m.w, h: m.h, data: new Uint8ClampedArray(m.data) }); break; }
      case 'cursor': this.canvas.style.cursor = m.css; break;
      case 'navigate': this.opts.onNavigate ? this.opts.onNavigate(m.url, m.target, m.user) : window.open(m.url, m.target || '_blank'); break;
      case 'fullscreen': this.setFullscreen(!!m.on); break;
      case 'menu': this.showMenu(m.id, m.items, m.x, m.y); break;
      case 'clipboard': navigator.clipboard?.writeText(m.text).catch(() => {}); break;
      case 'error': this.status.textContent = 'error: ' + m.message.split('\n')[0]; console.error(m.message); this.opts.onError?.(m); break;
      case 'exit': this.status.textContent = `exited (${m.code})${m.crash ? ' — crash' : ''}`; this.opts.onExit?.(m); break;
    }
  }

  bindInput() {
    const c = this.canvas, send = (ev) => this.worker.postMessage({ type: 'input', ev });
    const on = (t, f, o) => { c.addEventListener(t, f, o); this.listeners.push([t, f]); };
    const pos = (e) => { const r = c.getBoundingClientRect(); return [Math.round((e.clientX - r.left) * (this.size.w / r.width)), Math.round((e.clientY - r.top) * (this.size.h / r.height))]; };
    const mouse = (etype) => (e) => {
      const [x, y] = pos(e);
      if (etype === 0) c.focus();
      send({ etype, ts: e.timeStamp / 1000, mods: mods(e), mouse: { button: etype === 2 || etype === 3 || etype === 4 ? (e.buttons & 1 ? 0 : e.buttons & 4 ? 1 : e.buttons & 2 ? 2 : -1) : BUTTON(e.button), x, y, clicks: e.detail || 1, mx: e.movementX | 0, my: e.movementY | 0 } });
      if (etype === 0 || etype === 1) e.preventDefault();
    };
    on('mousedown', mouse(0)); on('mouseup', mouse(1)); on('mousemove', mouse(2)); on('mouseenter', mouse(3)); on('mouseleave', mouse(4));
    on('contextmenu', (e) => {                    // PP_INPUTEVENT_TYPE_CONTEXTMENU: Flash answers with its own PPB_Flash_Menu
      const [x, y] = pos(e);
      const ask = () => send({ etype: 10, ts: e.timeStamp / 1000, mods: mods(e), mouse: { button: 2, x, y, clicks: 1, mx: 0, my: 0 } });
      // Flash enables "Paste" only if the clipboard has text when it builds the menu, and the worker knows the clipboard only
      // after a Ctrl+V or a copy made inside Flash. Hand it the current text first (as the Ctrl+V path does).
      if (navigator.clipboard?.readText) navigator.clipboard.readText().then((text) => this.worker.postMessage({ type: 'clipboard-text', text }), () => {}).finally(ask);
      else ask();
      e.preventDefault();
    });
    on('wheel', (e) => {
      const k = e.deltaMode === 1 ? 1 : 1 / 100;
      send({ etype: 5, ts: e.timeStamp / 1000, mods: mods(e), wheel: { dx: -e.deltaX * (e.deltaMode === 1 ? 16 : 1), dy: -e.deltaY * (e.deltaMode === 1 ? 16 : 1), tx: -e.deltaX * k, ty: -e.deltaY * k, page: e.deltaMode === 2 } });
      e.preventDefault();
    }, { passive: false });
    const sendKey = (e, m) => {
      if (e.type === 'keydown') {
        send({ etype: 6, ts: e.timeStamp / 1000, mods: m, key: { code: e.keyCode, text: '', codeStr: e.code } });
        if (e.key.length === 1 && !e.ctrlKey && !e.metaKey) send({ etype: 9, ts: e.timeStamp / 1000, mods: m, key: { code: e.keyCode, text: e.key, codeStr: e.code } });
        else if (e.key === 'Enter') send({ etype: 9, ts: e.timeStamp / 1000, mods: m, key: { code: 13, text: '\r', codeStr: e.code } });
      } else send({ etype: 8, ts: e.timeStamp / 1000, mods: m, key: { code: e.keyCode, text: '', codeStr: e.code } });
    };
    const key = (e) => {
      let m = mods(e) | (e.repeat ? 32 : 0) | (e.location === 3 ? 16 : 0);
      // The Linux Flash build handles edit shortcuts on Ctrl; on macOS people press Cmd, so present Cmd+A/C/V/X/Y/Z as Ctrl
      if (IS_MAC && e.metaKey && !e.ctrlKey && /^[acvxyz]$/i.test(e.key)) m = (m & ~8) | 2;
      if (e.type === 'keydown' && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'v' && navigator.clipboard?.readText) {
        // Flash asks for the clipboard while handling Ctrl+V, so hand it the text before the key event arrives.
        navigator.clipboard.readText().then((text) => this.worker.postMessage({ type: 'clipboard-text', text }), () => {}).finally(() => sendKey(e, m));
        e.preventDefault();
        return;
      }
      sendKey(e, m);
      if (!(e.ctrlKey || e.metaKey) || e.key === 'Control') e.preventDefault();
    };
    on('keydown', key); on('keyup', key);
    on('focus', () => this.worker.postMessage({ type: 'focus', has: true }));
    on('blur', () => this.worker.postMessage({ type: 'focus', has: false }));
  }

  /** Flash's right-click menu as an HTML popup; reports the chosen item id (or null when dismissed). */
  showMenu(id, items, x, y) {
    const doc = this.canvas.ownerDocument, host = this.wrap;
    const rect = this.canvas.getBoundingClientRect();
    const reply = (selected) => { menu.remove(); if (selected != null) this.canvas.focus?.(); doc.removeEventListener('pointerdown', away, true); doc.removeEventListener('keydown', esc, true); this.worker.postMessage({ type: 'menu-result', id, selected }); };
    const menu = doc.createElement('div');
    menu.style.cssText = `position:fixed;z-index:2147483647;min-width:180px;max-width:320px;background:#fff;color:#202124;border:1px solid #c4c7c5;border-radius:6px;box-shadow:0 4px 16px rgba(0,0,0,.28);padding:4px 0;font:13px/1.35 system-ui,sans-serif;user-select:none`;
    const build = (list, container) => {
      for (const it of list) {
        if (it.type === 2) { const hr = doc.createElement('div'); hr.style.cssText = 'height:1px;background:#e0e0e0;margin:4px 0'; container.append(hr); continue; }
        const row = doc.createElement('div');
        row.style.cssText = `padding:5px 14px 5px 26px;position:relative;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;${it.enabled ? 'cursor:default' : 'color:#9aa0a6'}`;
        row.textContent = (it.name || '').replace(/&/g, '');
        if (it.type === 1 && it.checked) { const c = doc.createElement('span'); c.textContent = '\u2713'; c.style.cssText = 'position:absolute;left:9px'; row.append(c); }
        if (it.enabled && it.type !== 3) { row.onpointerenter = () => (row.style.background = '#e8f0fe'); row.onpointerleave = () => (row.style.background = ''); row.onclick = () => reply(it.id); }
        if (it.type === 3 && it.submenu) { row.textContent = row.textContent + '  \u25b8'; row.style.color = '#5f6368'; container.append(row); build(it.submenu.map((s) => ({ ...s, name: '    ' + s.name })), container); continue; }
        container.append(row);
      }
    };
    build(items, menu);
    // Choosing an item must not move keyboard focus off the movie: a blur is reported to Flash as "focus lost", and a text
    // field that lost focus has no selection left for Cut/Copy/Delete to act on. (Chrome's own plugin menu never touches focus.)
    menu.addEventListener('mousedown', (e) => e.preventDefault());
    host.append(menu);
    const w = menu.offsetWidth, h = menu.offsetHeight, vw = doc.documentElement.clientWidth, vh = doc.documentElement.clientHeight;
    menu.style.left = Math.max(0, Math.min(rect.left + x, vw - w - 2)) + 'px';
    menu.style.top = Math.max(0, Math.min(rect.top + y, vh - h - 2)) + 'px';
    const away = (e) => { if (!menu.contains(e.target)) reply(null); };
    const esc = (e) => { if (e.key === 'Escape') { e.preventDefault(); reply(null); } };
    setTimeout(() => { doc.addEventListener('pointerdown', away, true); doc.addEventListener('keydown', esc, true); }, 0);
  }

  /** Resize the plugin view (CSS pixels). */
  resize(w, h, opts = {}) {
    this.size = { w, h };
    // size changes caused by entering/leaving browser fullscreen must keep telling Flash whether the view is fullscreen,
    // otherwise a ResizeObserver tick during the transition looks like "fullscreen ended" and Flash drops out of it
    if (opts.fullscreen === undefined) opts = { ...opts, fullscreen: !!this.canvas.ownerDocument.fullscreenElement };
    this.worker?.postMessage({ type: 'view', rect: { x: 0, y: 0, w, h }, opts });
  }
  setFullscreen(on) {
    const doc = this.canvas.ownerDocument;
    if (on) {
      this.canvas.requestFullscreen?.().then(() => {
        const onChange = () => { if (!doc.fullscreenElement) { doc.removeEventListener('fullscreenchange', onChange); this.resize(this.prevSize.w, this.prevSize.h); this.worker?.postMessage({ type: 'view', rect: { x: 0, y: 0, w: this.prevSize.w, h: this.prevSize.h }, opts: { fullscreen: false } }); } };
        this.prevSize = { ...this.size };
        doc.addEventListener('fullscreenchange', onChange);
        this.resize(screen.width, screen.height, { fullscreen: true });
      }).catch(() => {});
    } else if (doc.fullscreenElement) doc.exitFullscreen();
  }
  postMessage(v) { this.worker.postMessage({ type: 'message', value: v }); }
  /** Worker -> this frame: run services[m.op](m.args, { active }) and answer with { ok, value | error }; id 0 = fire and forget. */
  service(m) {
    const answer = (r) => { if (m.id) this.worker?.postMessage({ type: 'svc-reply', id: m.id, ...r }); };
    const fn = this.services[m.op];
    if (!fn) return answer({ ok: false, error: 'no such service: ' + m.op });
    const ctx = { active: !!navigator.userActivation?.isActive };
    Promise.resolve().then(() => fn.call(this, m.args || {}, ctx)).then((value) => answer({ ok: true, value }), (e) => answer({ ok: false, error: String(e && e.message || e) }));
  }
  /** Worker -> embedding page scripting request (opts.pageBridge(msg) -> Promise<reply>); id 0 = fire and forget. */
  pageRequest(m) {
    const bridge = this.opts.pageBridge;
    if (!bridge) { if (m.id) this.worker.postMessage({ type: 'page-reply', id: m.id, reply: { ok: false, error: 'no page' } }); return; }
    Promise.resolve(bridge(m.msg, !!m.id)).then((reply) => { if (m.id) this.worker.postMessage({ type: 'page-reply', id: m.id, reply }); },
      (e) => { if (m.id) this.worker.postMessage({ type: 'page-reply', id: m.id, reply: { ok: false, error: String(e) } }); });
  }
  /** Deliver a socket-helper reply/event to the worker (opts.sockBridge sends requests out). */
  sockIn(msg) { this.worker?.postMessage({ type: 'sock-in', msg }); }
  /** Call a function the movie exposed with ExternalInterface.addCallback: Promise<{ ok, value, exception }> (strings/numbers/bools). */
  invoke(name, args = []) {
    this.invocations ||= new Map(); this.invokeId = (this.invokeId || 0) + 1;
    return new Promise((resolve) => { this.invocations.set(this.invokeId, resolve); this.worker.postMessage({ type: 'invoke', id: this.invokeId, name, args }); });
  }
  /** Read back what the presenter currently shows: Promise<{w, h, data: Uint8ClampedArray RGBA}> (the canvas itself is owned by the worker). */
  snapshot() {
    this.snapshots ||= new Map(); this.snapshotId = (this.snapshotId || 0) + 1;
    return new Promise((resolve, reject) => { this.snapshots.set(this.snapshotId, { resolve, reject }); this.worker.postMessage({ type: 'snapshot', id: this.snapshotId }); });
  }
  destroy() { this.worker?.terminate(); this.wrap.remove(); }
}

export function autoEmbed(opts = {}) {
  const els = document.querySelectorAll('embed[src$=".swf" i], embed[type="application/x-shockwave-flash"], object[type="application/x-shockwave-flash"]');
  return [...els].map((el) => { const p = new FlashPlayer(el, opts); p.start(); return p; });
}

if (document.currentScript === null && typeof window !== 'undefined' && !window.__flashNoAuto && location.protocol !== 'chrome-extension:') {   // the extension pages create players explicitly
  const run = () => { window.flashPlayers = autoEmbed(window.flashOptions || {}); };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', run); else run();
}
