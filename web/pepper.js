// Pepper (PPAPI) host. Receives PPB_* calls trapped by the guest stubs (see guest/pepper-host,
// tools/gen-ppapi.py) and implements them on browser primitives. One instance per plugin module.

import { METHODS, PPB, PPP } from './ppapi-sigs.js';
import { EV } from './emu.js';

import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, VT, BLOCK } from './pp-consts.js';
export * from './pp-consts.js';

const HC = { LOG: 0x10000, NEXT_TASK: 0x10001, INIT: 0x10002, READY: 0x10003, PLUGIN_ARG: 0x10004, PLUGIN_LOADED: 0x10006 };
const retView = new DataView(new ArrayBuffer(8));   // scratch for f/d returns in finishCall (synchronous, never re-entered): a fresh DataView + ArrayBuffer per call showed up in a Chrome CPU profile of a Date-heavy movie
const WORDS = { i: 1, p: 1, f: 1, d: 2, I: 2, V: 4, C: 3, A: 2, P: 2, F: 2, T: 7 };   // T = PP_TouchPoint, 28 bytes
const utf8dec = new TextDecoder();
const utf8enc = new TextEncoder();

class Arena {            // first-fit allocator over a guest memory block the guest donated at startup
  constructor(base, size) { this.free = [[base, size]]; this.sizes = new Map(); }
  alloc(n) {
    n = (n + 15) & ~15;
    for (let i = 0; i < this.free.length; i++) {
      const [s, l] = this.free[i];
      if (l >= n) {
        if (l === n) this.free.splice(i, 1); else this.free[i] = [s + n, l - n];
        this.sizes.set(s, n);
        return s;
      }
    }
    throw new Error('guest arena exhausted');
  }
  release(p) {
    const n = this.sizes.get(p); if (!n) return;
    this.sizes.delete(p);
    this.free.push([p, n]); this.free.sort((a, b) => a[0] - b[0]);
    for (let i = 0; i + 1 < this.free.length;) {   // coalesce
      if (this.free[i][0] + this.free[i][1] === this.free[i + 1][0]) { this.free[i][1] += this.free[i + 1][1]; this.free.splice(i + 1, 1); } else i++;
    }
  }
}

export class Pepper {
  constructor(emu, opts = {}) {
    this.emu = emu;
    this.opts = opts;
    this.log = opts.log || ((s) => console.error('[pepper] ' + s));
    this.onMessage = opts.onMessage || (() => {});
    this.onFrame = opts.onFrame || (() => {});
    this.onCursor = opts.onCursor || (() => {});
    this.res = new Map();        // PP_Resource -> object
    this.nextRes = 3;            // 1 = main message loop, 2 = audio callback loop (reserved, see pepper-host.c)
    this.vars = new Map();       // var id -> {refs, type, ...}
    this.nextVar = 1;
    this.loops = new Map();      // loop resource -> {queue, waiting, quit, thread}
    this.threadLoop = new Map(); // tid -> loop resource
    this.pendingCalls = new Map();
    this.nextTask = 1;
    this.unimpl = new Set();
    this.instances = new Map();
    this.mainTid = 0;
    this.ready = null;
    this.modulePath = '';
    this.t0 = performance.now();
    this.impl = this.buildImpl();
    this.stats = { calls: 0, byName: new Map() };
    this.tm = { emu: 0, yld: 0, idle: 0 };   // worker time split since the last frame (opts.stallLog / profile): guest, event-loop turns, waiting for a timer
    this.prof = opts.profile ? { emuMs: 0, hyperMs: 0, idleMs: 0, yieldMs: 0, runs: 0, hypers: 0, byName: new Map() } : null;   // opts.profile: time split for the diagnostics log
    this.loops.set(1, { queue: [], waiting: null, thread: 0, main: true });
    this.loops.set(2, { queue: [], waiting: null, thread: 0, audio: true });
    this.res.set(1, { type: 'messageloop' });
  }

  // ---- guest memory access ---------------------------------------------------------------
  get g() {
    const b = this.emu.mem.buffer;
    if (this._gb !== b) {
      this._gb = b;
      this._base = this.emu.x.emu_guest_base() >>> 0;   // unsigned: the base can lie above 2 GiB, where an i32 reads negative
      this._size = this.emu.x.emu_guest_size();
      this._dv = new DataView(b);
      this._u8 = new Uint8Array(b);
    }
    return this;
  }
  // Plugin-supplied pointers are untrusted: every accessor is confined to guest RAM so a bad pointer can
  // never read or write the emulator's own state that shares this wasm memory.
  chk(a, n) { if (!(a >= 0 && n >= 0 && a + n <= this._size)) throw new RangeError(`guest pointer ${a >>> 0}+${n} outside guest memory`); }
  u32(a) { this.g.chk(a, 4); return this._dv.getUint32(this._base + a, true); }
  i32(a) { this.g.chk(a, 4); return this._dv.getInt32(this._base + a, true); }
  f32(a) { this.g.chk(a, 4); return this._dv.getFloat32(this._base + a, true); }
  f64(a) { this.g.chk(a, 8); return this._dv.getFloat64(this._base + a, true); }
  w32(a, v) { this.g.chk(a, 4); this._dv.setUint32(this._base + a, v >>> 0, true); }
  wi32(a, v) { this.g.chk(a, 4); this._dv.setInt32(this._base + a, v | 0, true); }
  wf32(a, v) { this.g.chk(a, 4); this._dv.setFloat32(this._base + a, v, true); }
  wf64(a, v) { this.g.chk(a, 8); this._dv.setFloat64(this._base + a, v, true); }
  bytes(a, n) { this.g.chk(a, n); return this._u8.subarray(this._base + a, this._base + a + n); }
  cstr(a) {
    this.g.chk(a, 1);
    const u = this._u8, b = this._base, lim = Math.min(this._size - a, 1 << 20);
    let n = 0; while (n < lim && u[b + a + n]) n++;
    return utf8dec.decode(u.subarray(b + a, b + a + n));
  }
  /** Store-only helpers: callers that only WRITE guest memory use these so a remote backend need not fetch the old contents first. */
  putBytes(a, u8) { this.bytes(a, u8.length).set(u8); }
  zeroBytes(a, n) { this.bytes(a, n).fill(0); }
  bytesW(a, n) { return this.bytes(a, n); }            // a view the caller will overwrite completely
  ro(a, n) { return this.bytes(a, n); }                // a view the caller only reads
  /** Blend colour (R,G,B) with base alpha A through a coverage mask (bw x bh bytes) into premultiplied BGRA pixels at `addr`, rows `stride` bytes apart. */
  blendCoverage(addr, stride, bw, bh, R, G, B, A, cov) {
    const dst = this.bytes(addr, stride * (bh - 1) + bw * 4);
    for (let y = 0; y < bh; y++) {
      for (let x = 0; x < bw; x++) {
        const a = (cov[y * bw + x] * A / 255) | 0; if (!a) continue;
        const di = y * stride + x * 4, ia = 255 - a;
        dst[di] = (B * a / 255 + dst[di] * ia / 255 + 0.5) | 0;
        dst[di + 1] = (G * a / 255 + dst[di + 1] * ia / 255 + 0.5) | 0;
        dst[di + 2] = (R * a / 255 + dst[di + 2] * ia / 255 + 0.5) | 0;
        dst[di + 3] = (a + dst[di + 3] * ia / 255 + 0.5) | 0;
      }
    }
  }
  alloc(n) { return this.arena.alloc(n); }
  allocBytes(bytes) { const p = this.alloc(bytes.length + 1); this.putBytes(p, bytes); this.putBytes(p + bytes.length, new Uint8Array(1)); return p; }
  allocCStr(s) { return this.allocBytes(utf8enc.encode(s)); }

  // ---- resources & vars ------------------------------------------------------------------
  newRes(obj) { const id = this.nextRes++; obj.refs = 1; this.res.set(id, obj); return id; }
  getRes(id, type) { const r = this.res.get(id); return r && (!type || r.type === type) ? r : null; }

  mkVar(v) {                // JS value -> {type, lo, hi} PP_Var triple
    if (v === undefined) return { type: VT.UNDEFINED, lo: 0, hi: 0 };
    if (v === null) return { type: VT.NULL, lo: 0, hi: 0 };
    if (typeof v === 'boolean') return { type: VT.BOOL, lo: v ? 1 : 0, hi: 0 };
    if (typeof v === 'number' && Number.isInteger(v) && Math.abs(v) < 2 ** 31) return { type: VT.INT32, lo: v | 0, hi: 0 };
    if (typeof v === 'number') { const b = new DataView(new ArrayBuffer(8)); b.setFloat64(0, v, true); return { type: VT.DOUBLE, lo: b.getUint32(0, true), hi: b.getUint32(4, true) }; }
    if (typeof v === 'string') { const id = this.nextVar++; this.vars.set(id, { refs: 1, type: VT.STRING, s: v, ptr: 0, len: 0 }); return { type: VT.STRING, lo: id, hi: 0 }; }
    if (v instanceof Uint8Array) { const id = this.nextVar++; this.vars.set(id, { refs: 1, type: VT.ARRAY_BUFFER, data: v, ptr: 0 }); return { type: VT.ARRAY_BUFFER, lo: id, hi: 0 }; }
    throw new Error('mkVar: unsupported ' + typeof v);
  }
  readVar(a) { return { type: this.u32(a), lo: this.u32(a + 8), hi: this.u32(a + 12) }; }
  writeVar(a, v) { this.w32(a, v.type); this.w32(a + 4, 0); this.w32(a + 8, v.lo); this.w32(a + 12, v.hi); }
  varValue(v) {
    switch (v.type) {
      case VT.UNDEFINED: return undefined;
      case VT.NULL: return null;
      case VT.BOOL: return !!v.lo;
      case VT.INT32: return v.lo | 0;
      case VT.DOUBLE: { const b = new DataView(new ArrayBuffer(8)); b.setUint32(0, v.lo, true); b.setUint32(4, v.hi, true); return b.getFloat64(0, true); }
      case VT.STRING: return this.vars.get(v.lo)?.s ?? '';
      case VT.ARRAY_BUFFER: return this.vars.get(v.lo)?.data;
      default: return `<var type ${v.type}>`;
    }
  }
  addRefVar(v) { if (v.type >= VT.STRING) { const e = this.vars.get(v.lo); if (e) e.refs++; } }
  releaseVar(v) {
    if (v.type < VT.STRING) return;
    const e = this.vars.get(v.lo);
    if (e && --e.refs <= 0) { if (e.ptr) this.arena.release(e.ptr); if (e.obj && e.obj.remote > 2) this.releaseRemote?.(e.obj.remote); this.vars.delete(v.lo); }
  }

  // ---- task loops ------------------------------------------------------------------------
  loop(id) { let l = this.loops.get(id); if (!l) { l = { queue: [], waiting: null, thread: 0 }; this.loops.set(id, l); } return l; }

  /** Queue `fn(...args)` on a message loop. Returns a promise for the guest return value. */
  postTask(loopId, fn, args, delayMs = 0, retVar = false, opts = {}) {
    return new Promise((resolve) => {
      const t = { id: this.nextTask++, fn, args, resolve, retVar, post: opts.post, sync: opts.sync };
      const push = () => { const l = this.loop(loopId); l.queue.push(t); this.pump(l); };
      if (delayMs > 0) setTimeout(push, delayMs); else push();
    });
  }
  pump(l) {
    if (!l.waiting) return;
    if (l.waiting.flash?.quit) { const w = l.waiting; l.waiting = null; w.flash.quit = false; this.resume(w.tid, 1); return; }
    if (!l.queue.length) return;
    const t = l.queue.shift();
    const w = l.waiting; l.waiting = null;
    if (t.quit) {                                       // PostQuit is ordered behind everything posted before it
      if (t.destroy) { l.destroyed = true; if (this.threadLoop.get(w.tid) === t.loopId) this.threadLoop.delete(w.tid); this.releaseRes(t.loopId); }
      this.resume(w.tid, 1);
      return;
    }
    this.writeTask(w.ptr, t);
    this.resume(w.tid, 0);
  }
  writeTask(p, t) {
    this.w32(p, t.id); this.w32(p + 4, t.fn); this.w32(p + 8, t.args.length | (t.retVar ? 0x80000000 : 0));
    t.args.forEach((a, i) => this.w32(p + 12 + i * 4, a));
    this.w32(p + 12 + 14 * 4, t.post ? t.post.addr : 0); this.w32(p + 12 + 14 * 4 + 4, t.post ? t.post.len : 0);   // guest range the completion reads back (the audio buffer); 0 = none
    this.pendingCalls.set(t.id, t);
  }
  resume(tid, ret) { this.emu.x.emu_host_complete(tid, ret >>> 0); this.wakeRun && this.wakeRun(); }

  // ---- hypercall dispatch ----------------------------------------------------------------
  handleHyper(tid) {
    if (this.prof) { const nr = this.emu.reg(tid, 0); const t0 = performance.now(); try { return this._handleHyper(tid); } finally { const d = performance.now() - t0; this.prof.hyperMs += d; this.prof.hypers++; const m = nr < 0x10000 ? METHODS[nr] : null; const k = m ? `${m.iface.split(';')[0]}::${m.name}` : `svc${(nr >>> 0).toString(16)}`; const e = this.prof.byName.get(k) || [0, 0]; e[0]++; e[1] += d; this.prof.byName.set(k, e); } }
    return this._handleHyper(tid);
  }
  _handleHyper(tid) {
    const nr = this.emu.reg(tid, 0);
    if (nr >= 0x10000) return this.service(tid, nr);
    const m = METHODS[nr];
    if (!m) { this.log(`bad PPB method id ${nr}`); this.emu.setReg(tid, 0, 0); return; }
    this.stats.calls++;
    let p = this.emu.reg(tid, 4) + 4;     // skip return address
    let hidden = 0;
    if ('VPFT'.includes(m.r)) { hidden = this.u32(p); p += 4; }
    const args = [];
    for (const c of m.a) {
      switch (c) {
        case 'i': case 'p': args.push(this.u32(p)); break;
        case 'f': args.push(this.f32(p)); break;
        case 'd': args.push(this.f64(p)); break;
        case 'I': args.push(BigInt(this.u32(p)) | (BigInt(this.u32(p + 4)) << 32n)); break;
        case 'V': args.push(this.readVar(p)); break;
        case 'C': args.push({ func: this.u32(p), user: this.u32(p + 4), flags: this.u32(p + 8) }); break;
        case 'A': args.push({ func: this.u32(p), user: this.u32(p + 4) }); break;
        case 'P': case 'F': args.push([this.i32(p), this.i32(p + 4)]); break;
        case 'T': args.push(this.ro(p, 28).slice()); break;
      }
      p += WORDS[c] * 4;
    }
    const base = m.iface.split(';')[0];
    const impl = this.impl[m.iface] || this.impl[base] || this.impl[base.replace('(Dev)', '')];
    const fn = impl && impl[m.name];
    let ret;
    if (!fn) {
      const key = m.iface + '::' + m.name;
      if (!this.unimpl.has(key)) { this.unimpl.add(key); this.log(`UNIMPLEMENTED ${key}(${m.an.join(', ')})`); }
      ret = m.r === 'V' ? this.mkVar(undefined) : 0;
    } else {
      try { ret = fn.call(this, tid, ...args); }
      catch (e) { this.log(`${m.iface}::${m.name} threw ${e.stack || e}`); ret = m.r === 'V' ? this.mkVar(undefined) : PP_ERROR_FAILED; }
    }
    if (this.opts.trace) this.log(`${m.iface}::${m.name}(${args.map((a) => (typeof a === 'object' ? JSON.stringify(a, (k, v) => (typeof v === 'bigint' ? Number(v) : v)) : a)).join(', ')}) -> ${ret === BLOCK ? 'BLOCK' : JSON.stringify(ret)}`);
    if (ret === BLOCK) { this.emu.x.emu_host_block(tid); return; }
    if (ret && typeof ret.then === 'function') {
      this.emu.x.emu_host_block(tid);
      const tb = this.opts.stallLog ? performance.now() : 0;
      ret.then((v) => {
        // a call that parked a guest thread: when it is the main thread this is a freeze of the whole movie until the answer comes (page bridge, page script, ...)
        if (tb && performance.now() - tb > 40) this.log(`[blocked] ${m.iface}::${m.name} held ${tid === this.mainTid ? 'the main thread' : 'thread ' + tid} for ${(performance.now() - tb).toFixed(0)} ms`);
        this.finishCall(tid, m, hidden, v); this.emu.x.emu_host_complete(tid, this.emu.reg(tid, 0)); this.wakeRun && this.wakeRun();
      });
      return;
    }
    this.finishCall(tid, m, hidden, ret);
  }

  finishCall(tid, m, hidden, ret) {
    const e = this.emu;
    switch (m.r) {
      case 'v': break;
      case 'i': case 'p': e.setReg(tid, 0, (ret === true ? 1 : ret === false ? 0 : ret) >>> 0); break;
      case 'f': retView.setFloat32(0, ret, true); e.setReg(tid, 0, retView.getUint32(0, true)); break;
      case 'd': retView.setFloat64(0, ret, true); e.setReg(tid, 0, retView.getUint32(0, true)); e.setReg(tid, 2, retView.getUint32(4, true)); break;
      case 'I': { const v = BigInt.asUintN(64, BigInt(ret)); e.setReg(tid, 0, Number(v & 0xffffffffn)); e.setReg(tid, 2, Number(v >> 32n)); break; }
      case 'V': this.writeVar(hidden, ret); e.setReg(tid, 0, hidden); break;
      case 'P': this.wi32(hidden, ret[0]); this.wi32(hidden + 4, ret[1]); e.setReg(tid, 0, hidden); break;
      case 'F': this.wf32(hidden, ret[0]); this.wf32(hidden + 4, ret[1]); e.setReg(tid, 0, hidden); break;
      case 'T': { const t = new Uint8Array(28); if (ret) t.set(ret.subarray ? ret.subarray(0, 28) : ret); this.putBytes(hidden, t); e.setReg(tid, 0, hidden); break; }
    }
  }

  service(tid, nr) {
    const r = (i) => this.emu.reg(tid, i);
    switch (nr) {
      case HC.LOG: this.log('guest: ' + utf8dec.decode(this.ro(r(3), r(1)))); this.emu.setReg(tid, 0, 0); return;
      case HC.INIT:
        this.arena = new Arena(r(3), r(1));
        this.scratch = r(2);
        this.mainTid = tid;
        this.loops.get(1).thread = tid;
        this.threadLoop.set(tid, 1);
        this.emu.setReg(tid, 0, 1);
        return;
      case HC.PLUGIN_ARG: {
        const b = utf8enc.encode(this.modulePath);
        this.putBytes(r(1), b);
        this.emu.setReg(tid, 0, b.length);
        return;
      }
      case HC.READY:
        this.getPPP = r(3);
        this.initResult = r(1) | 0;
        this.emu.setReg(tid, 0, 0);
        if (this.ready) this.ready();
        return;
      case HC.PLUGIN_LOADED: {
        this.pluginImage = { loadBias: r(3) >>> 0, initialize: r(1) >>> 0, getInterface: r(2) >>> 0 };
        try {
          // Intentionally synchronous: the guest is paused at this hypercall and no
          // plugin thread may run until all guards/registrations have completed.
          const count = this.opts.onPluginLoaded?.(this.pluginImage) || 0;
          if (typeof count?.then === 'function') throw new Error('onPluginLoaded must be synchronous');
          this.emu.setReg(tid, 0, Number(count) >>> 0);
        } catch (e) {
          this.log(`[accel] plugin installation skipped: ${e.message || e}`);
          this.emu.setReg(tid, 0, 0);
        }
        return;
      }
      case HC.NEXT_TASK: {
        const loopId = r(3), ptr = r(1), prevId = r(2), lo = r(6), hi = r(7);
        const prev = this.pendingCalls.get(prevId);
        if (prev) { this.pendingCalls.delete(prevId); prev.resolve({ lo, hi }); if (prev.sync) prev.sync(lo, hi); }   // sync: runs while the shipped range is still readable
        const flash = this.getRes(loopId, 'flashloop');
        if (flash) {                                      // nested Flash message loop: runs the thread's own loop
          flash.parent = this.threadLoop.get(tid) || 1; flash.tid = tid;
          if (flash.quit) { flash.quit = false; this.emu.setReg(tid, 0, 1); return; }
          const pl = this.loop(flash.parent);
          pl.waiting = { tid, ptr, flash };
          this.emu.x.emu_host_block(tid);
          this.pump(pl);
          return;
        }
        const l = this.loop(loopId);
        l.thread = tid; this.threadLoop.set(tid, loopId);
        l.waiting = { tid, ptr };
        this.emu.x.emu_host_block(tid);
        this.pump(l);
        return;
      }
      default:
        this.log(`unknown host call ${nr.toString(16)}`);
        this.emu.setReg(tid, 0, 0);
    }
  }

  // ---- driving the emulator --------------------------------------------------------------
  /** Boot-time: wait for the plugin to finish PPP_InitializeModule. */
  whenReady() { return new Promise((res) => { this.ready = res; }); }

  async run(shouldStop = () => false) {
    const emu = this.emu;
    const yieldNow = this.opts.yield || (() => new Promise((r) => setTimeout(r, 0)));
    let wake = null;
    this.wakeRun = () => { if (wake) { const w = wake; wake = null; w(); } };
    for (;;) {
      if (shouldStop()) return;
      const timed = this.prof || this.opts.stallLog;
      const pt = timed ? performance.now() : 0;
      // 96 slices of 100 k instructions ~ 8 ms of guest time between returns to the event loop (it was 16 = 1.3 ms: 8 % of the worker's time went into yielding)
      let ev;
      try {
        ev = emu.run(this.opts.sliceInsns || 100000, this.opts.slices || 96);
      } catch (e) {
        // a guest load outside guest RAM in the loads-unchecked performance mode (docs/JIT.md), or a bug: the same outcome as a SIGSEGV that nobody handles
        this.log('GUEST CRASH: ' + (e && e.message || e));
        this.crashed = String(e && e.message || e);
        return -1;
      }
      if (timed) { const d = performance.now() - pt; this.tm.emu += d; if (this.prof) { this.prof.emuMs += d; this.prof.runs++; } }
      switch (ev.type) {
        case EV.PROBE: {
          const probe = JSON.parse(ev.msg);
          try {
            emu.observingProbe = true;
            const result = this.opts.onEntryProbe?.(probe, emu);
            if (typeof result?.then === 'function') throw new Error('onEntryProbe must be synchronous');
          } catch (error) {
            this.log(`[nanojit] observer disabled: ${error.message || error}`);
            this.opts.onEntryProbe = null;
          } finally { emu.observingProbe = false; }
          break;
        }
        case EV.HYPER: this.handleHyper(ev.arg); break;
        case EV.EXITED: this.log('guest exited with ' + ev.arg); this.exited = ev.arg; return ev.arg;
        case EV.CRASH: this.log('GUEST CRASH: ' + ev.msg); this.crashed = ev.msg; return -1;
        case EV.IDLE: {
          // Everything is parked: wait for a timer, a JS-side completion, or the guest deadline.
          const waitMs = ev.arg < 0 ? 1e9 : Math.max(0, (ev.arg - ((performance.now() - emu.t0) * 1e6 + 1e9)) / 1e6);
          const it = timed ? performance.now() : 0;
          await new Promise((r) => { wake = r; setTimeout(r, Math.min(waitMs, 50)); });
          if (timed) { const d = performance.now() - it; this.tm.idle += d; if (this.prof) this.prof.idleMs += d; }
          break;
        }
        default: { const yt = timed ? performance.now() : 0; await yieldNow(); if (timed) { const d = performance.now() - yt; this.tm.yld += d; if (this.prof) this.prof.yieldMs += d; } }
      }
    }
  }

  /** Diagnostics: time split since the last call (ms) and the busiest PPB methods. */
  profSnapshot() {
    const p = this.prof; if (!p) return null;
    const top = [...p.byName].sort((a, b) => b[1][1] - a[1][1]).slice(0, 8).map(([k, v]) => `${k}=${v[0]}x/${v[1].toFixed(0)}ms`);
    const snap = { emuMs: p.emuMs, hyperMs: p.hyperMs, idleMs: p.idleMs, yieldMs: p.yieldMs, runs: p.runs, hypers: p.hypers, top };
    p.emuMs = p.hyperMs = p.idleMs = p.yieldMs = p.runs = p.hypers = 0; p.byName.clear();
    return snap;
  }

  /** Call a guest function pointer on a message loop (default: main). */
  call(fn, args, loop = 1) { return this.postTask(loop, fn, args); }

  async createInstance(instance, args = {}, size = { w: 640, h: 480 }) {
    const ppInst = this.u32 && await this.call(this.getPPP, [this.allocCStr('PPP_Instance;1.1')]);
    this.pppInstance = ppInst.lo;                       // pointer to PPP_Instance_1_1
    const names = Object.keys(args), argn = this.alloc(4 * (names.length + 1)), argv = this.alloc(4 * (names.length + 1));
    names.forEach((k, i) => { this.w32(argn + i * 4, this.allocCStr(k)); this.w32(argv + i * 4, this.allocCStr(String(args[k]))); });
    const inst = { id: instance, g2d: 0, size, view: 0, pppInput: 0, inputClasses: 0, focus: false, srcUrl: args.src ? new URL(args.src, this.opts.baseUrl || 'http://localhost/').href : '' };
    this.instances.set(instance, inst);
    const ok = await this.call(this.u32(this.pppInstance + 0), [instance, names.length, argn, argv]);
    const inputIface = await this.call(this.getPPP, [this.allocCStr('PPP_InputEvent;0.1')]);
    inst.pppInput = inputIface.lo;
    const msgIface = await this.call(this.getPPP, [this.allocCStr('PPP_Messaging;1.0')]);
    inst.pppMessaging = msgIface.lo;
    await this.changeView(instance, { x: 0, y: 0, w: size.w, h: size.h });
    return ok.lo;
  }

  async changeView(instance, r, opts = {}) {
    const inst = this.instances.get(instance);
    inst.size = { w: r.w, h: r.h };
    inst.fullscreen = !!opts.fullscreen;                // PPB_FlashFullscreen::IsFullscreen answers from the last view
    const view = this.newRes({ type: 'view', rect: r, fullscreen: !!opts.fullscreen, visible: opts.visible !== false, pageVisible: opts.pageVisible !== false, scale: opts.scale || 1 });
    const old = inst.view; inst.view = view;
    await this.call(this.u32(this.pppInstance + 8), [instance, view]);
    if (old) this.releaseRes(old);
  }

  /** Tell the plugin the instance gained/lost keyboard focus (PPP_Instance::DidChangeFocus). */
  changeFocus(instance, has) {
    const inst = this.instances.get(instance);
    if (!inst || inst.focus === has || !this.pppInstance) return Promise.resolve();
    inst.focus = has;
    return this.call(this.u32(this.pppInstance + 12), [instance, has ? 1 : 0]);
  }

  postMessageToPlugin(instance, value) {
    const inst = this.instances.get(instance);
    if (!inst?.pppMessaging) return;
    const v = this.mkVar(value);
    const a = this.alloc(16); this.writeVar(a, v);
    // PPP_Messaging::HandleMessage(instance, PP_Var) — PP_Var is passed by value (4 words).
    return this.call(this.u32(inst.pppMessaging), [instance, v.type, 0, v.lo, v.hi]).then(() => { this.arena.release(a); this.releaseVar(v); });
  }

  /** Persist module-local (Flash Shared Object) storage through opts.persist = { load(), save(files) }. */
  async restoreStorage() {
    const p = this.opts.persist; if (!p) return;
    const files = await p.load();
    // one unreadable entry must not keep the whole plugin from starting
    for (const [path, bytes] of files || []) {
      try { this.emu.mkdirp(path.slice(0, path.lastIndexOf('/'))); this.emu.writeFile(path, bytes); }
      catch (e) { this.log(`[storage] restore of ${path} failed: ${e && e.message || e}`); }
    }
  }
  persistStorage() {
    const p = this.opts.persist; if (!p) return;
    // runs from a timer: a failure here (bad snapshot, IndexedDB refusing a write) is logged and retried on the next change, never thrown into the page
    try { p.save(this.emu.dumpTree('/home/flash/.flash/ModuleLocal')); }
    catch (e) { this.log(`[storage] persist failed: ${e && e.message || e}`); }
  }

  releaseRes(id) {
    const r = this.res.get(id);
    if (!r) return;
    if (--r.refs <= 0) { r.onFree && r.onFree(); this.res.delete(id); }
  }

  /** Deliver a DOM-derived input event to the plugin (see buildInputEvent). */
  async sendInput(instance, ev) {
    const inst = this.instances.get(instance);
    if (!inst?.pppInput) return PP_ERROR_FAILED;
    const res = this.newRes({ type: 'inputevent', ...ev });
    const r = await this.call(this.u32(inst.pppInput), [instance, res]);
    this.releaseRes(res);
    return r.lo;
  }

  buildImpl() { return buildImpl(this); }
}

import { buildImpl } from './ppb-impl.js';
import { installScripting } from './scripting.js';
installScripting(Pepper.prototype);
