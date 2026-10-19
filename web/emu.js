// Embedder for emu.wasm. Runs in Node (tests) and in a browser Worker (the real thing).
// The same class is used by both; only the way bytes are obtained differs.

const enc = new TextEncoder();
const dec = new TextDecoder();

export const EV = { YIELD: 0, EXITED: 1, IDLE: 2, HYPER: 3, CRASH: 4, PROBE: 5 };

// Feature probes for the JIT: generated code uses wasm tail calls (region chaining) and SIMD (inline SSE2 integer ops).
// Engines without them still run the JIT, just without those parts.
const WASM_TAIL_CALL = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 127, 3, 3, 2, 0, 0, 7, 5, 1, 1, 103, 0, 1, 10, 11, 2, 4, 0, 65, 1, 11, 4, 0, 18, 0, 11]);
const WASM_SIMD = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 7, 5, 1, 1, 103, 0, 0, 10, 22, 1, 20, 0, 253, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11]);
const supports = (bytes) => { try { return WebAssembly.validate(bytes); } catch { return false; } };

export class Emulator {
  static async create({ wasm, onConsole, onLog, realtimeBaseMs }) {
    const self = new Emulator();
    self.onConsole = onConsole || (() => {});
    self.onLog = onLog || ((s) => console.error(s));
    self.t0 = (typeof performance !== 'undefined' ? performance : { now: () => Date.now() }).now();
    self.realtimeBaseMs = realtimeBaseMs ?? Date.now();
    const perf = typeof performance !== 'undefined' ? performance : { now: () => Date.now() };
    const imports = {
      env: {
        host_log: (p, n) => { p >>>= 0; self.onLog(dec.decode(self.u8().subarray(p, p + n))); },
        host_now_ns: () => Math.floor((perf.now() - self.t0) * 1e6) + 1e9,
        host_realtime_ns: () => (self.realtimeBaseMs + (perf.now() - self.t0)) * 1e6,
        host_random: (p, n) => {
          p >>>= 0;
          const view = self.u8().subarray(p, p + n);
          // crypto.getRandomValues is capped at 64 KiB per call
          for (let o = 0; o < n; o += 65536) globalThis.crypto.getRandomValues(view.subarray(o, Math.min(n, o + 65536)));
        },
        host_console: (fd, p, n) => { p >>>= 0; self.onConsole(fd, self.u8().slice(p, p + n)); },
        // ---- JIT (docs/JIT.md): compile a module of translated regions and park its functions in the
        // emulator's own indirect function table so Rust can call_indirect them.
        host_jit_install: (ptr, len, n, out) => self._jitInstall(ptr, len, n, out),
        host_jit_free: (slot) => {
          self.table.set(slot, null);
          self.jitFree.push(slot);
        },
      },
    };
    self.jitFree = [];
    self.jitModules = 0;
    self.jitCompileMs = 0;
    const mod = wasm instanceof WebAssembly.Module ? wasm : await WebAssembly.compile(wasm);
    const inst = await WebAssembly.instantiate(mod, imports);
    self.x = inst.exports;
    self.mem = inst.exports.memory;
    self.table = inst.exports.__indirect_function_table;
    return self;
  }

  _jitInstall(ptr, len, n, out) {
    ptr >>>= 0; out >>>= 0;
    const slots = [];
    try {
      const t0 = performance.now();
      const bytes = new Uint8Array(this.mem.buffer, ptr, len).slice(); // copy out of linear memory
      if (this.onJitModule) this.onJitModule(bytes); // debugging hook (node-run.mjs: EMU_JIT_DUMP=dir)
      const m = new WebAssembly.Module(bytes);
      const x = this.x;
      const inst = new WebAssembly.Instance(m, {
        env: { memory: this.mem, table: this.table, exec_one: x.jit_exec_one, load_slow: x.jit_load_slow, store_slow: x.jit_store_slow, mat: x.jit_mat, sys_time: x.jit_sys_time, fist_fild: x.jit_fist_fild, fcomip: x.jit_fcomip, cvtss2si: x.jit_cvtss2si, mm_op: x.jit_mm_op },
      });
      for (let i = 0; i < n; i++) {
        if (typeof inst.exports['r' + i] !== 'function') throw new Error('missing region export r' + i);
      }
      for (let i = 0; i < n; i++) {
        const f = inst.exports['r' + i];
        let slot = this.jitFree.pop();
        if (slot === undefined) slot = this.table.grow(1, f);
        else this.table.set(slot, f);
        slots.push(slot);
      }
      new Uint32Array(this.mem.buffer, out, n).set(slots);
      this.jitModules++;
      this.jitCompileMs += performance.now() - t0;
      return 0;
    } catch (e) {
      for (const slot of slots) { this.table.set(slot, null); this.jitFree.push(slot); }
      this.onLog('[jit] install failed: ' + (e && e.stack || e));
      return 1;
    }
  }

  /** JIT tier-up threshold: 0 = off, 1 = compile every block at first sight, N = after N executions (100 is a good default). */
  setJit(threshold, { profChains = false, intrinsics = true, trustLoads = false } = {}) {
    if (this.x.emu_set_jit_features) this.x.emu_set_jit_features(supports(WASM_TAIL_CALL) ? 1 : 0, supports(WASM_SIMD) ? 1 : 0);
    // per-chain retired-instruction counters (jitHotReport): a 64-bit memory read-modify-write at every chain entry, ~3 % of run time, so only on request
    this.setJitProfChains(profChains);
    if (this.x.emu_set_jit_intrinsics) this.x.emu_set_jit_intrinsics(intrinsics ? 1 : 0);
    if (this.x.emu_set_jit_trust_loads) this.x.emu_set_jit_trust_loads(trustLoads ? 1 : 0);
    this.x.emu_set_jit(threshold >>> 0);
  }
  /** Copy already-compiled callees of at most n instructions into the calling region (0 = off). */
  setJitDup(n) { if (this.x.emu_set_jit_dup) this.x.emu_set_jit_dup(n >>> 0); }
  /** Region selection: successors join a region when they ran >= threshold/div times (0 = all decoded successors). */
  setJitRegionHotDiv(div) { if (this.x.emu_set_jit_region_hot_div) this.x.emu_set_jit_region_hot_div(div >>> 0); }
  /** Count retired instructions per compiled chain (before the code gets hot); jitHotReport(n) lists the hottest. */
  setJitProfChains(on) { this.profChains = !!on; if (this.x.emu_set_jit_prof_chains) this.x.emu_set_jit_prof_chains(on ? 1 : 0); }
  jitHotReport(n = 25) { this.x.emu_jit_hot_report(n); return this.evMsg(); }
  /** Offline artifacts embed instance-specific memory addresses. Compile for this layout after boot/spawn. */
  aotLayout() { this.x.emu_aot_layout(); return JSON.parse(this.evMsg()); }
  lirCompletions() { return this.x.emu_lir_completed(); }
  checkGuestReadable(address, size) {
    if (!Number.isSafeInteger(address) || !Number.isSafeInteger(size) || address < 0 || size < 0
      || this.x.emu_guest_readable(address, size) !== 1) throw new RangeError('guest capture range is not readable');
  }
  /** Debug/capture boundary, exact bytes required; guest state is untouched and resumes once. */
  setEntryProbe(eip, id, expected = new Uint8Array()) {
    if (!(expected instanceof Uint8Array) || expected.length > 256) throw new TypeError('invalid probe byte guard');
    const p = this._put(expected);
    try {
      if (this.x.emu_set_entry_probe(eip >>> 0, id >>> 0, p, expected.length) < 0) throw new Error('entry probe rejected');
    } finally { this.x.emu_free(p, expected.length); }
  }
  /** Only load locally trusted emu-aot artifacts: byte guards are compatibility checks, NOT authentication. */
  installAot(bundle) {
    if (!(bundle instanceof Uint8Array)) throw new TypeError('AOT bundle must be a Uint8Array');
    if (!bundle.length || bundle.length > 80 * 1024 * 1024) throw new Error('invalid AOT bundle size');
    const p = this._put(bundle);
    try {
      const n = this.x.emu_install_aot(p, bundle.length);
      if (n < 0) throw new Error('AOT installation rejected: ' + this.evMsg());
      return n;
    } finally { this.x.emu_free(p, bundle.length); }
  }
  installPortableAot(bundle, loadBias) {
    if (!(bundle instanceof Uint8Array) || !bundle.length || bundle.length > 80 * 1024 * 1024) throw new TypeError('invalid portable AOT bundle');
    const p = this._put(bundle);
    try {
      const n = this.x.emu_install_portable_aot(p, bundle.length, loadBias >>> 0);
      if (n < 0) throw new Error('portable AOT installation rejected: ' + this.evMsg());
      return n;
    } finally { this.x.emu_free(p, bundle.length); }
  }
  installCapturedSubLeaf(entry, core, nativeBytes) {
    const p = this._put(nativeBytes);
    try {
      const n = this.x.emu_install_lir_sub_leaf(entry >>> 0, core >>> 0, p, nativeBytes.length);
      if (n < 0) throw new Error('captured LIR leaf rejected: ' + this.evMsg());
      return n;
    } finally { this.x.emu_free(p, nativeBytes.length); }
  }
  installCapturedIntegerLeaf(entry, core, nativeBytes, source, offsets, argc) {
    if (!(nativeBytes instanceof Uint8Array) || nativeBytes.length > 4096 || typeof source !== 'string'
      || !Array.isArray(offsets) || offsets.length > 16) throw new TypeError('invalid captured integer leaf');
    const text = enc.encode(source), locations = new Uint8Array(offsets.length * 4);
    const view = new DataView(locations.buffer);
    offsets.forEach((n, i) => view.setUint32(4 * i, n, true));
    const p = this._put(nativeBytes), s = this._put(text), a = this._put(locations);
    try {
      const n = this.x.emu_install_lir_integer_leaf(entry >>> 0, core >>> 0, p, nativeBytes.length, s, text.length, a, offsets.length, argc);
      if (n < 0) throw new Error(this.evMsg());
      return n;
    } finally { this.x.emu_free(p, nativeBytes.length); this.x.emu_free(s, text.length); this.x.emu_free(a, locations.length); }
  }
  installCapturedLoop(entry, core, nativeBytes, metadata) {
    if (!(nativeBytes instanceof Uint8Array) || nativeBytes.length > 4096 || !(metadata instanceof Uint8Array) || metadata.length > 16384) throw new TypeError('invalid captured loop');
    const p = this._put(nativeBytes), m = this._put(metadata);
    try {
      const n = this.x.emu_install_lir_counted_loop(entry >>> 0, core >>> 0, p, nativeBytes.length, m, metadata.length);
      if (n < 0) throw new Error(this.evMsg());
      return n;
    } finally { this.x.emu_free(p, nativeBytes.length); this.x.emu_free(m, metadata.length); }
  }
  installCapturedNumber(entry, core, nativeBytes, source, offsets, argc) {
    if (!(nativeBytes instanceof Uint8Array) || nativeBytes.length > 4096 || typeof source !== 'string'
      || !Array.isArray(offsets) || offsets.length < 1 || offsets.length > 8) throw new TypeError('invalid captured Number leaf');
    const text = enc.encode(source), locations = new Uint8Array(offsets.length * 4), view = new DataView(locations.buffer);
    offsets.forEach((value, i) => view.setUint32(i * 4, value, true));
    const p = this._put(nativeBytes), s = this._put(text), a = this._put(locations);
    try {
      const n = this.x.emu_install_lir_number(entry >>> 0, core >>> 0, p, nativeBytes.length, s, text.length, a, offsets.length, argc);
      if (n < 0) throw new Error(this.evMsg()); return n;
    } finally { this.x.emu_free(p, nativeBytes.length); this.x.emu_free(s, text.length); this.x.emu_free(a, locations.length); }
  }
  installCapturedMemoryEffects(entry, core, nativeBytes, source, bindings, fields, stores, argc) {
    if (!(nativeBytes instanceof Uint8Array) || nativeBytes.length > 4096 || typeof source !== 'string'
      || !Array.isArray(bindings) || bindings.length > 16 || !Array.isArray(fields) || fields.length > 16
      || !Array.isArray(stores) || stores.length > 8 || stores.some((pair) => !Array.isArray(pair) || pair.length !== 2)) throw new TypeError('invalid captured receiver memory');
    const words = (values) => {
      const bytes = new Uint8Array(values.length * 4), view = new DataView(bytes.buffer);
      values.forEach((value,i) => view.setUint32(i*4,value,true)); return bytes;
    };
    const text = enc.encode(source), argumentsBytes = words(bindings), fieldBytes = words(fields), storeBytes = words(stores.flat());
    const p = this._put(nativeBytes), s = this._put(text), a = this._put(argumentsBytes), f = this._put(fieldBytes), e = this._put(storeBytes);
    try {
      const n = this.x.emu_install_lir_memory(entry >>> 0,core >>> 0,p,nativeBytes.length,s,text.length,
        a,bindings.length,f,fields.length,e,stores.length,argc);
      if (n < 0) throw new Error(this.evMsg()); return n;
    } finally { this.x.emu_free(p,nativeBytes.length); this.x.emu_free(s,text.length); this.x.emu_free(a,argumentsBytes.length); this.x.emu_free(f,fieldBytes.length); this.x.emu_free(e,storeBytes.length); }
  }
  /** Compile the supported pure normalized LIR subset, NOT a raw Flash LIns buffer.
   * Returns trace(...args) -> [status, payload:bigint]. Guard exits require host-owned restart.
   */
  compileLir(source) {
    if (typeof source !== 'string') throw new TypeError('LIR source must be text');
    const bytes = enc.encode(source);
    if (bytes.length > 1024 * 1024) throw new Error('LIR source exceeds 1 MiB');
    const p = this._put(bytes);
    try {
      const n = this.x.emu_compile_lir(p, bytes.length);
      if (n < 0) throw new Error('LIR compilation failed: ' + this.evMsg());
      const ptr = this.x.emu_ev_msg_ptr() >>> 0;
      const wasm = this.u8().slice(ptr, ptr + n);
      return new WebAssembly.Instance(new WebAssembly.Module(wasm)).exports.trace;
    } finally { this.x.emu_free(p, bytes.length); }
  }
  /** Collect dispatcher phase timings for jitReport() (small overhead). */
  setEipProf(on) { if (this.x.emu_set_eip_prof) this.x.emu_set_eip_prof(on ? 1 : 0); }
  eipProfReport(n = 400) { if (!this.x.emu_eip_prof_report) return ''; this.x.emu_eip_prof_report(n); return this.evMsg(); }
  setJitProfile(on) { if (this.x.emu_set_jit_profile) this.x.emu_set_jit_profile(on ? 1 : 0); }
  jitReport() {
    this.x.emu_jit_report();
    return this.evMsg() + (this.profChains ? '\n' + this.jitHotReport(30) : '') + `\n[jit] modules=${this.jitModules} wasm compile+instantiate ${this.jitCompileMs.toFixed(1)} ms`;
  }

  u8() {
    if (!this._u8 || this._u8.buffer !== this.mem.buffer) this._u8 = new Uint8Array(this.mem.buffer);
    return this._u8;
  }
  dv() {
    if (!this._dv || this._dv.buffer !== this.mem.buffer) this._dv = new DataView(this.mem.buffer);
    return this._dv;
  }

  // Pointers the wasm module returns are i32: once linear memory passes 2 GiB they read as negative numbers here and every
  // typed-array/DataView use of them throws. All pointer-returning exports go through `>>> 0`; passing the unsigned value
  // back into an export is lossless (ToInt32 restores the bit pattern).
  _alloc(n) { return this.x.emu_alloc(n) >>> 0; }
  _put(bytes) {
    const p = this._alloc(bytes.length);
    this.u8().set(bytes, p);
    return p;
  }

  vfsNew() { this.x.emu_vfs_new(); }
  loadTar(bytes) {
    const p = this._put(bytes);
    this.x.emu_vfs_load_tar(p, bytes.length);
    this.x.emu_free(p, bytes.length);
  }
  addFile(path, bytes, mode = 0o755) {
    const pb = enc.encode(path);
    const pp = this._put(pb), dp = this._put(bytes);
    this.x.emu_vfs_add_file(pp, pb.length, dp, bytes.length, mode);
    this.x.emu_free(pp, pb.length);
    this.x.emu_free(dp, bytes.length);
  }
  boot(memMB = 512, cpuidExtra = 0) { this.x.emu_boot(memMB, cpuidExtra); }

  spawn(path, argv, env) {
    const blob = (a) => enc.encode(a.map((s) => s + '\0').join(''));
    const pb = enc.encode(path), ab = blob(argv), eb = blob(env);
    const pp = this._put(pb), ap = this._put(ab), ep = this._put(eb);
    const rc = this.x.emu_spawn(pp, pb.length, ap, ab.length, ep, eb.length);
    this.x.emu_free(pp, pb.length); this.x.emu_free(ap, ab.length); this.x.emu_free(ep, eb.length);
    if (rc !== 0) throw new Error('spawn failed: ' + this.evMsg());
  }

  evMsg() {
    const p = this.x.emu_ev_msg_ptr() >>> 0, n = this.x.emu_ev_msg_len();
    return dec.decode(this.u8().subarray(p, p + n));
  }

  run(sliceInsns = 200000, slices = 64) {
    if (this.observingProbe) throw new Error('emulator scheduling reentry from observation callback');
    const t = this.x.emu_run(sliceInsns, slices);
    const arg = this.x.emu_ev_arg();
    return { type: t, arg, msg: t === EV.CRASH || t === EV.PROBE ? this.evMsg() : '' };
  }

  guest() {
    return new Uint8Array(this.mem.buffer, this.x.emu_guest_base() >>> 0, this.x.emu_guest_size());
  }
  // ---- guest filesystem access (kernel-side VFS) ----
  _str(s) { const b = enc.encode(s); return [this._put(b), b.length]; }
  _fs(fn, path, ...rest) { const [p, n] = this._str(path); try { return fn(p, n, ...rest); } finally { this.x.emu_free(p, n); } }
  openFd(path, flags, mode = 0o644) { return this._fs((p, n) => this.x.emu_open_fd(p, n, flags, mode), path); }
  mkdirp(path) { return this._fs((p, n) => this.x.emu_mkdir_p(p, n), path); }
  unlinkPath(path, recursive = false) { return this._fs((p, n) => this.x.emu_unlink(p, n, recursive ? 1 : 0), path); }
  stat(path) {
    const out = this._alloc(16);
    try {
      const rc = this._fs((p, n) => this.x.emu_stat(p, n, out), path);
      if (rc < 0) return null;
      const dv = this.dv();
      return { mode: dv.getUint32(out, true), size: dv.getUint32(out + 4, true) + dv.getUint32(out + 8, true) * 2 ** 32, mtime: dv.getUint32(out + 12, true) };
    } finally { this.x.emu_free(out, 16); }
  }
  readdir(path) {
    const rc = this._fs((p, n) => this.x.emu_readdir(p, n), path);
    if (rc < 0) return null;
    const parts = this.evMsg().split('\0'); const out = [];
    for (let i = 0; i + 1 < parts.length; i += 2) out.push({ name: parts[i], dir: parts[i + 1] === 'D' });
    return out;
  }
  rename(a, b) { const [p, n] = this._str(a), [q, m] = this._str(b); try { return this.x.emu_rename(p, n, q, m); } finally { this.x.emu_free(p, n); this.x.emu_free(q, m); } }
  writeFile(path, bytes) { const [p, n] = this._str(path); const d = this._put(bytes); try { return this.x.emu_write_file(p, n, d, bytes.length); } finally { this.x.emu_free(p, n); this.x.emu_free(d, bytes.length); } }
  dumpTree(prefix) {
    const cnt = this._fs((p, n) => this.x.emu_dump_tree(p, n), prefix);
    const ptr = this.x.emu_ev_msg_ptr() >>> 0, len = this.x.emu_ev_msg_len();
    const buf = this.u8().slice(ptr, ptr + len), dv = new DataView(buf.buffer);
    const files = []; let o = 0;
    for (let i = 0; i < cnt; i++) {
      const pl = dv.getUint32(o, true); o += 4; const path = dec.decode(buf.subarray(o, o + pl)); o += pl;
      const dl = dv.getUint32(o, true); o += 4; files.push([path, buf.slice(o, o + dl)]); o += dl;
    }
    return files;
  }
  reg(tid, i) { return this.x.emu_reg_get(tid, i) >>> 0; }
  setReg(tid, i, v) { this.x.emu_reg_set(tid, i, v >>> 0); }
}
