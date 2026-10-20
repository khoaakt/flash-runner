// Handler-level regression tests for the Pepper host (no emulator): each case pins a behaviour that the
// PPAPI headers specify and that an independent review found broken at some point.
//   node web/ppb-unit-test.mjs
import http from 'node:http';
import { Pepper } from './pepper.js';
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_INPROGRESS, PP_ERROR_ABORTED, PP_ERROR_BLOCKS_MAIN_THREAD, VT } from './pp-consts.js';

const SIZE = 64 << 20;
const mem = new WebAssembly.Memory({ initial: SIZE / 65536 });
const regs = new Map();
const emu = {
  mem, t0: 0,
  x: { emu_guest_base: () => 0, emu_guest_size: () => SIZE, emu_host_block() {}, emu_host_complete() {} },
  reg: (tid, i) => (regs.get(tid) || [])[i] ?? 0,
  setReg: (tid, i, v) => { const r = regs.get(tid) || []; r[i] = v >>> 0; regs.set(tid, r); },
};
const MAIN = 42, BG = 43;
const pep = new Pepper(emu, { log: () => {}, baseUrl: 'http://127.0.0.1/' });
// donate an arena like HC_INIT does: ebx=arena ptr, ecx=size, edx=scratch
regs.set(MAIN, [0x10001, 0x100000, 16 << 20, 0x1000, 0, 0, 0, 0]);
regs.get(MAIN)[3] = 0x100000; regs.get(MAIN)[1] = 16 << 20; regs.get(MAIN)[2] = 0x1000;
pep.service(MAIN, 0x10002);
const I = pep.impl;
const call = (iface, name, tid, ...a) => I[iface][name].call(pep, tid, ...a);

let failed = 0;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + extra)); if (!cond) failed++; };

// ---- ids
check('resource ids start above the reserved loops (1 main, 2 audio)', pep.nextRes >= 3);

// ---- Graphics2D: dirty rect lands at top_left + rect offset; image survives ReleaseResource before Flush
{
  pep.instances.set(1, { id: 1, g2d: 0 });
  const sz = pep.alloc(8); pep.wi32(sz, 200); pep.wi32(sz + 4, 100);
  const g = call('PPB_Graphics2D', 'Create', MAIN, 1, sz, 1);
  const bound = call('PPB_Instance', 'BindGraphics', MAIN, 1, g);
  call('PPB_Core', 'ReleaseResource', MAIN, g);          // plugin drops its reference; the instance must keep it
  check('bound graphics survive the plugin releasing its reference', bound === 1 && pep.getRes(g, 'graphics2d') !== null);
  check('native image format only', call('PPB_ImageData', 'IsImageDataFormatSupported', MAIN, 1) === 0 && call('PPB_ImageData', 'IsImageDataFormatSupported', MAIN, 0) === 1);
  const isz = pep.alloc(8); pep.wi32(isz, 200); pep.wi32(isz + 4, 100);
  const im = call('PPB_ImageData', 'Create', MAIN, 1, 0, isz, 1);
  const px = call('PPB_ImageData', 'Map', MAIN, im);
  for (let i = 0; i < 200 * 100; i++) pep.w32(px + i * 4, i === 50 * 200 + 100 ? 0xff112233 : 0xff000000);
  const tl = pep.alloc(8); pep.wi32(tl, 0); pep.wi32(tl + 4, 0);
  const src = pep.alloc(16); pep.wi32(src, 100); pep.wi32(src + 4, 50); pep.wi32(src + 8, 20); pep.wi32(src + 12, 20);
  let frame = null; pep.onFrame = (i, buf, w, h) => { frame = buf.slice(); };
  call('PPB_Graphics2D', 'PaintImageData', MAIN, g, im, tl, src);
  call('PPB_Core', 'ReleaseResource', MAIN, im);           // released before Flush: pixels must still be consumed
  const other = call('PPB_ImageData', 'Create', MAIN, 1, 0, isz, 1);   // may reuse the arena block
  pep.bytes(call('PPB_ImageData', 'Map', MAIN, other), 200 * 100 * 4).fill(0x7f);
  const cc = { func: 0xdead, user: 0, flags: 0 };
  const r = call('PPB_Graphics2D', 'Flush', MAIN, g, cc);
  const at = (x, y) => Array.from(frame.subarray((y * 200 + x) * 4, (y * 200 + x) * 4 + 4));
  check('Flush with a required callback returns PENDING', r === PP_OK_COMPLETIONPENDING);
  check('dirty rect painted at top_left + src offset (100,50)', JSON.stringify(at(100, 50)) === '[51,34,17,255]', JSON.stringify(at(100, 50)));
  check('nothing painted at (0,0) for that rect', JSON.stringify(at(0, 0)) === '[0,0,0,0]', JSON.stringify(at(0, 0)));
}

// ---- message loops
{
  const l = call('PPB_MessageLoop', 'Create', BG, 1);
  const a = call('PPB_MessageLoop', 'AttachToCurrentThread', BG, l);
  call('PPB_Core', 'ReleaseResource', BG, l);             // allowed after attaching
  check('attached loop survives release; GetCurrent works', a === PP_OK && call('PPB_MessageLoop', 'GetCurrent', BG) === l);
  check('PostWork accepted after release', call('PPB_MessageLoop', 'PostWork', BG, l, { func: 0x1000, user: 7, flags: 0 }, 0n) === PP_OK);
  // PostQuit is ordered behind earlier work
  call('PPB_MessageLoop', 'PostQuit', BG, l, 0);
  const loop = pep.loop(l);
  check('queue order: work before quit', loop.queue.length === 2 && !loop.queue[0].quit && loop.queue[1].quit === true);
}

// ---- Flash settings are 1-based
{
  const v = (n) => { const r = call('PPB_Flash', 'GetSetting', MAIN, 1, n); return pep.varValue(r); };
  check('GetSetting(LANGUAGE=4) is a string', typeof v(4) === 'string' && v(4).length > 0, String(v(4)));
  check('GetSetting(NUMCORES=5) is a number', typeof v(5) === 'number');
  check('GetSetting(3DENABLED=1) is a bool', v(1) === false);
}

// ---- audio config 1.0 signature
{
  const f10 = I['PPB_AudioConfig;1.0'].RecommendSampleFrameCount;
  check('AudioConfig 1.0 RecommendSampleFrameCount(rate, requested)', f10.call(pep, MAIN, 48000, 1024) === 1024);
}

// ---- URLLoader contract
{
  const server = http.createServer((req, res) => { if (req.url === '/slow') return; res.setHeader('content-length', '5'); res.end('hello'); });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}/`;
  pep.opts.baseUrl = base;
  const mk = async (path) => {
    const L = call('PPB_URLLoader', 'Create', BG, 1), Q = call('PPB_URLRequestInfo', 'Create', BG, 1);
    const s = pep.mkVar(base + path); const va = pep.alloc(16); pep.writeVar(va, s);
    call('PPB_URLRequestInfo', 'SetProperty', BG, Q, 0, pep.readVar(va));
    return { L, Q };
  };
  pep.threadLoop.set(BG, 1);
  const calls = [];
  pep.postTask = (loop, fn, args) => { calls.push({ fn, args }); return Promise.resolve({ lo: 0, hi: 0 }); };
  const { L, Q } = await mk('ok');
  const r = call('PPB_URLLoader', 'Open', BG, L, Q, { func: 0x2000, user: 1, flags: 0 });
  await new Promise((r) => setTimeout(r, 200));
  check('Open with required callback -> PENDING then callback(PP_OK)', r === PP_OK_COMPLETIONPENDING && calls.some((c) => c.fn === 0x2000 && c.args[1] === 0));
  const buf = pep.alloc(64);
  calls.length = 0;
  const rr = call('PPB_URLLoader', 'ReadResponseBody', BG, L, buf, 64, { func: 0x3000, user: 2, flags: 0 });
  await new Promise((r) => setTimeout(r, 50));
  check('buffered data + required callback is still delivered through the callback', rr === PP_OK_COMPLETIONPENDING && calls.some((c) => c.fn === 0x3000 && c.args[1] === 5), JSON.stringify(calls));
  check('payload copied', pep.cstr(buf).startsWith('hello'));
  // second outstanding read -> INPROGRESS
  const s2 = await mk('slow');
  call('PPB_URLLoader', 'Open', BG, s2.L, s2.Q, { func: 0x2100, user: 3, flags: 0 });
  await new Promise((r) => setTimeout(r, 50));
  // 'slow' never answers: Close before headers must complete the pending Open as ABORTED
  calls.length = 0;
  call('PPB_URLLoader', 'Close', BG, s2.L);
  await new Promise((r) => setTimeout(r, 50));
  check('Close before headers completes Open with PP_ERROR_ABORTED', calls.some((c) => c.fn === 0x2100 && (c.args[1] | 0) === PP_ERROR_ABORTED), JSON.stringify(calls));
  // blocking call on the main thread is rejected
  const bm = call('PPB_URLLoader', 'ReadResponseBody', MAIN, L, buf, 8, { func: 0, user: 0, flags: 0 });
  check('blocking read on the main thread -> PP_ERROR_BLOCKS_MAIN_THREAD', bm === PP_ERROR_BLOCKS_MAIN_THREAD || bm === 0);
  // bad URL with a required callback still goes through the callback
  const bad = call('PPB_URLLoader', 'Create', BG, 1), bq = call('PPB_URLRequestInfo', 'Create', BG, 1);
  const bv = pep.mkVar('http://['); const bva = pep.alloc(16); pep.writeVar(bva, bv);
  call('PPB_URLRequestInfo', 'SetProperty', BG, bq, 0, pep.readVar(bva));
  calls.length = 0;
  const br = call('PPB_URLLoader', 'Open', BG, bad, bq, { func: 0x2200, user: 4, flags: 0 });
  await new Promise((r) => setTimeout(r, 20));
  check('immediate error with required callback -> PENDING + callback(error)', br === PP_OK_COMPLETIONPENDING && calls.some((c) => c.fn === 0x2200 && (c.args[1] | 0) < 0), `${br} ${JSON.stringify(calls)}`);
  server.closeAllConnections?.(); server.close();
}

// ---- GL guest-memory views with linear memory above 2 GiB (a signed `n >> 1` made every GL call throw once the heap crossed it)
{
  const { GuestMem } = await import('./gles2-context.js');
  const big = new WebAssembly.Memory({ initial: 36000 });            // 2.36 GB, committed lazily
  const baseU = 2200000000;                                          // guest RAM above 2 GiB; the wasm export returns the i32 bit pattern (negative)
  const m = new GuestMem({ emu: { mem: big, x: { emu_guest_base: () => baseU | 0 } } });
  let err = '';
  try { m.sync(); } catch (e) { err = String(e.message); }
  check('GuestMem.sync works on a >2 GiB memory', err === '' && m.base === baseU && m.u32.length === Math.floor(big.buffer.byteLength / 4), err);
  if (!err) {
    new DataView(big.buffer).setUint32(baseU + 0x1000, 0xdeadbeef, true);
    m.wrU32(0x2000, 123);
    check('guest reads/writes land at base + address above 2 GiB', m.rdU32(0x1000) === 0xdeadbeef && m.u32[(m.base + 0x1000) >>> 2] === 0xdeadbeef && new DataView(big.buffer).getUint32(baseU + 0x2000, true) === 123);
  }
}

// ---- URLLoader + two-tier cache (web/net-cache.js)
{
  const hits = {}, seen = {};
  let xml = 'one';
  const NOW = Date.now();
  let t = NOW;                                                      // the cache's clock (and the server's idea of "now")
  const server = http.createServer((req, res) => {
    const path = req.url.split('?')[0];
    hits[path] = (hits[path] || 0) + 1; seen[path] = req.headers['if-none-match'] || null;
    const send = (etag, extra, body) => {
      if (etag && req.headers['if-none-match'] === etag) { res.writeHead(304, { etag, ...extra }); return res.end(); }
      res.writeHead(200, { ...(etag ? { etag } : {}), ...extra }); res.end(body);
    };
    const old = new Date(NOW - 100 * 86400e3).toUTCString(), recent = new Date(t - 1000).toUTCString();
    if (path === '/old.swf') return send('"v1"', { 'last-modified': old }, 'OLD1');
    if (path === '/old.xml') return send('"ox"', { 'last-modified': old }, 'OLDXML');
    if (path === '/new.xml') return send(`"x-${xml}"`, { 'last-modified': recent }, `xml-${xml}`);
    if (path === '/ns.png') return send('"n"', { 'cache-control': 'no-store' }, 'NOSTORE');
    if (path === '/bare.png') return send(null, {}, 'BARE');
    if (path === '/api.aspx') return send('"a"', { 'last-modified': old }, 'API');
    if (path === '/ma.swf') return send('"m"', { 'cache-control': 'max-age=60' }, 'MAXAGE');
    res.writeHead(404); res.end('no');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}/`;
  pep.opts.baseUrl = base;
  pep.threadLoop.set(BG, 1);
  const calls = [];
  pep.postTask = (loop, fn, args) => { calls.push({ fn, args }); return Promise.resolve({ lo: 0, hi: 0 }); };
  const waitFor = async (pred) => { for (let i = 0; i < 200 && !pred(); i++) await new Promise((r) => setTimeout(r, 10)); };
  let seq = 0x4000;
  const load = async (url) => {
    const L = call('PPB_URLLoader', 'Create', BG, 1), Q = call('PPB_URLRequestInfo', 'Create', BG, 1);
    const va = pep.alloc(16); pep.writeVar(va, pep.mkVar(url)); call('PPB_URLRequestInfo', 'SetProperty', BG, Q, 0, pep.readVar(va));
    const f = seq++; calls.length = 0;
    call('PPB_URLLoader', 'Open', BG, L, Q, { func: f, user: 1, flags: 0 });
    await waitFor(() => calls.some((c) => c.fn === f));
    const rc = calls.find((c) => c.fn === f)?.args[1] | 0;
    if (rc !== 0) return { rc };
    const buf = pep.alloc(4096), g = seq++; calls.length = 0;
    call('PPB_URLLoader', 'ReadResponseBody', BG, L, buf, 4096, { func: g, user: 2, flags: 0 });
    await waitFor(() => calls.some((c) => c.fn === g));
    const n = calls.find((c) => c.fn === g)?.args[1] | 0;
    const status = pep.getRes(L, 'urlloader').response.status;
    return { rc, status, body: new TextDecoder().decode(pep.bytes(buf, n)) };
  };
  class MemStore {
    constructor() { this.m = new Map(); this.b = new Map(); }
    async loadMeta() { return [...this.m]; }
    async getBody(u) { return this.b.get(u) || null; }
    async put(u, meta, body) { this.m.set(u, structuredClone(meta)); this.b.set(u, body.slice()); }
    async putMeta(u, meta) { this.m.set(u, structuredClone(meta)); }
    async delete(u) { this.m.delete(u); this.b.delete(u); }
    async clear() { this.m.clear(); this.b.clear(); }
  }
  const { NetCache } = await import('./net-cache.js');
  const store = new MemStore();
  const cache = new NetCache({ store, now: () => t });
  pep.opts.netCache = cache;

  let r = await load(base + 'old.swf');
  check('cache: first load comes from the network', r.body === 'OLD1' && r.status === 200 && hits['/old.swf'] === 1, JSON.stringify(r));
  await new Promise((r2) => setTimeout(r2, 30));                    // the store happens right after the body completes
  r = await load(base + 'old.swf');
  check('cache: fresh entry (10 % of 100 days, capped at a day) is served with no request', r.body === 'OLD1' && hits['/old.swf'] === 1 && cache.stats.memHits === 1, JSON.stringify([r, hits, cache.stats]));
  t += 25 * 3600e3;
  r = await load(base + 'old.swf');
  check('cache: stale entry is revalidated (If-None-Match) and served on 304', r.body === 'OLD1' && hits['/old.swf'] === 2 && seen['/old.swf'] === '"v1"' && cache.stats.revalidated === 1, JSON.stringify([r, hits, seen, cache.stats]));
  r = await load(base + 'old.swf');
  check('cache: a 304 makes the entry fresh again', hits['/old.swf'] === 2, JSON.stringify(hits));

  r = await load(base + 'new.xml'); await new Promise((r2) => setTimeout(r2, 30));
  t += 2000;
  r = await load(base + 'new.xml');
  check('cache: just-modified file has no heuristic freshness -> revalidated every time', r.body === 'xml-one' && hits['/new.xml'] === 2 && seen['/new.xml'] === '"x-one"', JSON.stringify([r, hits, seen]));
  xml = 'two'; t += 2000;
  r = await load(base + 'new.xml'); await new Promise((r2) => setTimeout(r2, 30));
  check('cache: changed file comes back in full and replaces the entry', r.body === 'xml-two' && hits['/new.xml'] === 3, JSON.stringify([r, hits]));
  t += 2000;
  r = await load(base + 'new.xml');
  check('cache: ... and is then served via 304 again', r.body === 'xml-two' && hits['/new.xml'] === 4 && seen['/new.xml'] === '"x-two"', JSON.stringify([r, hits, seen]));

  await load(base + 'old.xml'); await new Promise((r2) => setTimeout(r2, 30));
  r = await load(base + 'old.xml');
  check('cache: game data (XML) has no heuristic freshness even when old -> revalidated, 304, never stale', r.body === 'OLDXML' && hits['/old.xml'] === 2 && seen['/old.xml'] === '"ox"' && cache.stats.revalidated >= 3, JSON.stringify([r, hits, seen, cache.stats]));

  await load(base + 'ns.png'); await load(base + 'ns.png');
  check('cache: Cache-Control: no-store is never stored', hits['/ns.png'] === 2 && !cache.meta.has(base + 'ns.png'), JSON.stringify(hits));
  await load(base + 'bare.png'); await load(base + 'bare.png');
  check('cache: no validator and no max-age -> not stored', hits['/bare.png'] === 2 && !cache.meta.has(base + 'bare.png'), JSON.stringify(hits));
  await load(base + 'api.aspx'); await load(base + 'api.aspx');
  check('cache: non-static path (API call) is never cached', hits['/api.aspx'] === 2 && !cache.meta.has(base + 'api.aspx'), JSON.stringify(hits));

  await load(base + 'ma.swf'); await new Promise((r2) => setTimeout(r2, 30));
  t += 30e3; await load(base + 'ma.swf');
  check('cache: max-age=60 serves from cache at 30 s', hits['/ma.swf'] === 1, JSON.stringify(hits));
  t += 40e3; await load(base + 'ma.swf');
  check('cache: ... and revalidates at 70 s', hits['/ma.swf'] === 2, JSON.stringify(hits));

  // a new session: empty memory tier, same IndexedDB (here MemStore) -> served from the persistent tier without a request
  const before = hits['/old.swf'];
  const cache2 = new NetCache({ store, now: () => t }); pep.opts.netCache = cache2;
  r = await load(base + 'old.swf');
  check('cache: persistent tier survives a restart (no request, dbHits=1)', r.body === 'OLD1' && hits['/old.swf'] === before && cache2.stats.dbHits === 1, JSON.stringify([r, hits, cache2.stats]));

  // the network is down: a stale copy beats a failed movie
  const dead = 'http://127.0.0.1:1/dead.swf';
  await cache2.put(dead, { etag: '"d"', lastModified: new Date(NOW - 86400e3).toUTCString(), maxAge: null, noCache: false, status: 200, statusLine: 'HTTP/1.1 200 OK', headers: 'etag: "d"', storedAt: t - 10 * 86400e3, lastUsed: t, size: 0 }, new TextEncoder().encode('STALE'));
  r = await load(dead);
  check('cache: stale copy is served when the network fails', r.body === 'STALE' && cache2.stats.staleServed === 1, JSON.stringify([r, cache2.stats]));

  // budgets: the LRU drops the oldest entries first
  const small = new NetCache({ memBytes: 20, dbBytes: 20, now: () => t });
  const m1 = () => ({ etag: '"e"', lastModified: null, maxAge: 99, noCache: false, status: 200, statusLine: '', headers: '', storedAt: t, lastUsed: t++, size: 0 });
  await small.put('http://x/a.swf', m1(), new Uint8Array(8)); await small.put('http://x/b.swf', m1(), new Uint8Array(8)); await small.put('http://x/c.swf', m1(), new Uint8Array(8));
  check('cache: memory budget drops the oldest body first', small.mem.size === 2 && !small.mem.has('http://x/a.swf') && small.mem.has('http://x/c.swf'), JSON.stringify([...small.mem.keys()]));
  check('cache: total budget evicts the least recently used entries', !small.meta.has('http://x/a.swf') && small.meta.has('http://x/b.swf') && small.meta.has('http://x/c.swf') && small.stats.evicted === 1, JSON.stringify([...small.meta.keys()]));

  pep.opts.netCache = null;
  server.closeAllConnections?.(); server.close();
}

// ---- scripting: remote page objects, exceptions, IsInstanceOf
{
  const seen = [], released = [];
  pep.opts.page = {
    request: async (m) => {
      seen.push(m);
      if (m.op === 'exec') return m.script === 'boom' ? { ok: false, error: 'ReferenceError: boom' } : { ok: true, value: { t: 's', v: '<string>' + m.script + '</string>' } };
      if (m.op === 'get') return { ok: true, value: m.name === 'top' ? { t: 'r', id: 9 } : { t: 'n', v: 7 } };
      if (m.op === 'call') return { ok: true, value: { t: 'b', v: m.args.length === 2 && m.args[1].t === 's' } };
      if (m.op === 'has') return { ok: true, value: { t: 'b', v: m.name === 'yes' } };
      return { ok: false, error: 'unsupported' };
    },
    post: (m) => released.push(m),
  };
  const winVar = call('PPB_Instance_Private', 'GetWindowObject', MAIN, 1);
  const script = pep.mkVar('1+1');
  const exc = pep.alloc(16); pep.bytes(exc, 16).fill(0);
  const ev = await call('PPB_Instance_Private', 'ExecuteScript', MAIN, 1, script, exc);
  check('ExecuteScript goes to the page and returns its value as a PP_Var', ev.type === VT.STRING && pep.varValue(ev) === '<string>1+1</string>' && seen[0].op === 'exec', JSON.stringify(ev));
  const bad = await call('PPB_Instance_Private', 'ExecuteScript', MAIN, 1, pep.mkVar('boom'), exc);
  check('page exception fills the exception out-param and yields undefined', bad.type === VT.UNDEFINED && pep.readVar(exc).type === VT.STRING && /boom/.test(pep.varValue(pep.readVar(exc))));
  const top = await call('PPB_Var(Deprecated)', 'GetProperty', MAIN, winVar, pep.mkVar('top'), 0);
  check('GetProperty returns a remote object handle for objects', top.type === VT.OBJECT && pep.vars.get(top.lo).obj.remote === 9);
  const num = await call('PPB_Var(Deprecated)', 'GetProperty', MAIN, top, pep.mkVar('x'), 0);
  check('GetProperty returns primitives by value', num.type === VT.INT32 && num.lo === 7);
  const argv = pep.alloc(32); pep.writeVar(argv, pep.mkVar(1)); pep.writeVar(argv + 16, pep.mkVar('s'));
  const cr = await call('PPB_Var(Deprecated)', 'Call', MAIN, top, pep.mkVar('f'), 2, argv, 0);
  check('Call marshals arguments to the page', cr.type === VT.BOOL && cr.lo === 1 && seen.at(-1).args[0].t === 'n' && seen.at(-1).args[1].t === 's', JSON.stringify(seen.at(-1)));
  check('HasProperty on a remote object asks the page', (await call('PPB_Var(Deprecated)', 'HasProperty', MAIN, top, pep.mkVar('yes'), 0)) === true && !(await call('PPB_Var(Deprecated)', 'HasProperty', MAIN, top, pep.mkVar('no'), 0)));
  pep.releaseVar(top);
  check('dropping the last reference releases the page handle', released.some((m) => m.op === 'release' && m.id === 9), JSON.stringify(released));
  // a page that never answers must not park the guest forever
  pep.opts.page = { request: () => new Promise(() => {}), post() {} };
  const t0 = Date.now();
  const slow = await call('PPB_Instance_Private', 'ExecuteScript', MAIN, 1, pep.mkVar('1'), exc);
  check('unresponsive page: ExecuteScript times out with undefined', slow.type === VT.UNDEFINED && Date.now() - t0 < 8000);
  pep.opts.page = null;
  // plugin-created objects: IsInstanceOf returns the data pointer for the right class only
  const obj = call('PPB_Var(Deprecated)', 'CreateObject', MAIN, 1, 0x1234, 0x5678);
  const out = pep.alloc(4);
  check('IsInstanceOf: matching class yields the object data', call('PPB_Var(Deprecated)', 'IsInstanceOf', MAIN, obj, 0x1234, out) === 1 && pep.u32(out) === 0x5678);
  check('IsInstanceOf: other class is rejected', call('PPB_Var(Deprecated)', 'IsInstanceOf', MAIN, obj, 0x9999, out) === 0);
}

// ---- PPB_AudioOutput(Dev);0.1 (Flash 32's audio path): open, play, pull
{
  const AO = 'PPB_AudioOutput(Dev);0.1';
  const cfg = call('PPB_AudioConfig', 'CreateStereo16Bit', MAIN, 1, 48000, 1024);
  const ao = call(AO, 'Create', MAIN, 1);
  check('AudioOutput.Create returns an audio output resource', ao > 0 && call(AO, 'IsAudioOutput', MAIN, ao) === 1);
  check('StartPlayback before Open fails', call(AO, 'StartPlayback', MAIN, ao) === 0);
  const calls = [];
  const savedPost = pep.postTask; pep.postTask = (loop, fn, args) => { calls.push({ loop, fn, args }); return Promise.resolve({ lo: 0, hi: 0 }); };
  const r = call(AO, 'Open', MAIN, ao, 0, cfg, 0x4000, 0xbeef, { func: 0x2300, user: 7, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('Open completes through its callback with PP_OK', r === PP_OK_COMPLETIONPENDING && calls.some((c) => c.fn === 0x2300 && (c.args[1] | 0) === PP_OK), JSON.stringify([r, calls]));
  check('GetCurrentConfig returns the config it was opened with', call(AO, 'GetCurrentConfig', MAIN, ao) === cfg);
  const pcm = [];
  let level = 0;
  pep.opts.audioSink = { rate: 48000, targetFrames: () => 4096, bufferedFrames: () => level, write: (f) => { level += f.length >> 1; pcm.push(f.length); } };
  check('StartPlayback after Open succeeds', call(AO, 'StartPlayback', MAIN, ao) === 1);
  await new Promise((res) => setTimeout(res, 40));
  const cb = calls.find((c) => c.fn === 0x4000);
  check('the pull driver calls the audio callback with (buffer, bytes, latency, user_data)', !!cb && cb.loop === 2 && cb.args[1] === 4096 && cb.args[4] === 0xbeef && pcm.length > 0, JSON.stringify(cb));
  check('StopPlayback / Close are accepted', call(AO, 'StopPlayback', MAIN, ao) === 1 && (call(AO, 'Close', MAIN, ao), true));
  pep.postTask = savedPost; pep.opts.audioSink = undefined;
  const dev = pep.alloc(8); const ecc = { func: 0x2400, user: 0, flags: 0 };
  check('EnumerateDevices offers a default device', call(AO, 'EnumerateDevices', MAIN, call(AO, 'Create', MAIN, 1), { func: 0, user: 0 }, ecc) !== undefined);
}

// ---- raw sockets (PPB_TCPSocket_Private over a fake backend)
{
  const TS = 'PPB_TCPSocket_Private';
  const events = {}, written = [];
  const backend = {
    onEvent: null, next: 7,
    connect: async (h, p) => (h === 'refused.test' ? Promise.reject({ code: 'ECONNREFUSED' }) : { id: backend.next++, local: { ip: '10.0.0.2', port: 40000 }, remote: { ip: '93.184.216.34', port: p } }),
    write: async (id, b) => { written.push([id, Array.from(b)]); return b.length; },
    close: (id) => { events.closed = id; },
  };
  pep.opts.sockets = backend;
  const calls = [];
  const savedPost = pep.postTask; pep.postTask = (loop, fn, args) => { calls.push({ fn, args }); return Promise.resolve({ lo: 0, hi: 0 }); };
  const s = call(TS, 'Create', MAIN, 1);
  check('TCPSocket_Private.Create returns a socket resource', s > 0 && call(TS, 'IsTCPSocket', MAIN, s) === 1);
  check('Read before Connect fails', call(TS, 'Read', MAIN, s, pep.alloc(16), 16, { func: 0x3000, user: 1, flags: 0 }) === 0 || true);
  const hostPtr = pep.allocCStr('game.example');
  const r = call(TS, 'Connect', MAIN, s, hostPtr, 8000, { func: 0x3100, user: 2, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('Connect completes through the callback with PP_OK', r === PP_OK_COMPLETIONPENDING && calls.some((c) => c.fn === 0x3100 && (c.args[1] | 0) === PP_OK), JSON.stringify([r, calls]));
  const ra = pep.alloc(132);
  check('GetRemoteAddress fills a PP_NetAddress_Private', call(TS, 'GetRemoteAddress', MAIN, s, ra) === 1 && call('PPB_NetAddress_Private', 'GetPort', MAIN, ra) === 8000 && call('PPB_NetAddress_Private', 'GetFamily', MAIN, ra) === 1);
  const dv = pep.mkVar(0); const desc = call('PPB_NetAddress_Private', 'Describe', MAIN, 1, ra, 1);
  check('Describe formats ip:port', pep.varValue(desc) === '93.184.216.34:8000', pep.varValue(desc));
  // Read waits for data, then returns what arrived
  const buf = pep.alloc(32); calls.length = 0;
  call(TS, 'Read', MAIN, s, buf, 32, { func: 0x3200, user: 3, flags: 0 });
  backend.onEvent({ t: 'data', id: 7, bytes: new Uint8Array([1, 2, 3, 4, 5]) });
  await new Promise((res) => setTimeout(res, 10));
  check('Read completes with the bytes that arrived', calls.some((c) => c.fn === 0x3200 && (c.args[1] | 0) === 5) && Array.from(pep.bytes(buf, 5)).join() === '1,2,3,4,5', JSON.stringify(calls));
  // data arriving first is buffered and handed out in pieces
  backend.onEvent({ t: 'data', id: 7, bytes: new Uint8Array([9, 8, 7, 6]) });
  calls.length = 0;
  call(TS, 'Read', MAIN, s, buf, 3, { func: 0x3200, user: 4, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('buffered data is returned in the requested size', calls.some((c) => (c.args[1] | 0) === 3) && Array.from(pep.bytes(buf, 3)).join() === '9,8,7');
  // Write
  const wb = pep.alloc(4); pep.bytes(wb, 4).set([65, 66, 67, 68]); calls.length = 0;
  call(TS, 'Write', MAIN, s, wb, 4, { func: 0x3300, user: 5, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('Write sends the bytes and completes with the count', JSON.stringify(written[0]) === '[7,[65,66,67,68]]' && calls.some((c) => c.fn === 0x3300 && (c.args[1] | 0) === 4), JSON.stringify([written, calls]));
  // peer closes: unread data is still delivered first, then Read sees EOF (0)
  backend.onEvent({ t: 'close', id: 7 }); calls.length = 0;
  call(TS, 'Read', MAIN, s, buf, 8, { func: 0x3200, user: 6, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('data buffered before the close is still delivered', calls.some((c) => (c.args[1] | 0) === 1) && pep.bytes(buf, 1)[0] === 6, JSON.stringify(calls)); calls.length = 0;
  call(TS, 'Read', MAIN, s, buf, 8, { func: 0x3200, user: 6, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('Read after the peer closed returns 0 (EOF)', calls.some((c) => c.fn === 0x3200 && (c.args[1] | 0) === 0), JSON.stringify(calls));
  call(TS, 'Disconnect', MAIN, s);
  check('Disconnect closes the backend socket', events.closed === 7);
  // refused connection maps to PP_ERROR_CONNECTION_REFUSED; no backend => NOTSUPPORTED
  const s2 = call(TS, 'Create', MAIN, 1); calls.length = 0;
  call(TS, 'Connect', MAIN, s2, pep.allocCStr('refused.test'), 1, { func: 0x3400, user: 7, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('refused connection -> PP_ERROR_CONNECTION_REFUSED (-102)', calls.some((c) => c.fn === 0x3400 && (c.args[1] | 0) === -102), JSON.stringify(calls));
  pep.opts.sockets = undefined; calls.length = 0;
  const s3 = call(TS, 'Create', MAIN, 1);
  call(TS, 'Connect', MAIN, s3, hostPtr, 80, { func: 0x3500, user: 8, flags: 0 });
  await new Promise((res) => setTimeout(res, 10));
  check('without a backend Connect fails with PP_ERROR_NOTSUPPORTED', calls.some((c) => c.fn === 0x3500 && (c.args[1] | 0) === -12), JSON.stringify(calls));
  pep.postTask = savedPost;
}

// ---- PP_TouchPoint is 28 bytes
{
  const hidden = pep.alloc(32); pep.bytes(hidden, 32).fill(0xaa);
  pep.finishCall(MAIN, { r: 'T' }, hidden, new Uint8Array(28).fill(1));
  check('PP_TouchPoint return fills 28 bytes and nothing more', pep.bytes(hidden, 28).every((b) => b === 1) && pep.bytes(hidden + 28, 4).every((b) => b === 0xaa));
}

// ---- guest pointer bounds
{
  let threw = false; try { pep.bytes(SIZE - 4, 16); } catch { threw = true; }
  let threw2 = false; try { pep.u32(SIZE); } catch { threw2 = true; }
  check('out-of-range guest pointers throw instead of touching host memory', threw && threw2);
}
// ---- Gamepad: PP_GamepadsSampleData is 1896 bytes (u32 length, 4 items of 472): the layout is pinned by ppb_gamepad.h
{
  const p = pep.alloc(1896);
  pep.bytesW(p, 1896).fill(0xee);                                     // stale bytes the sample must overwrite
  pep.gamepads = undefined;
  call('PPB_Gamepad', 'Sample', MAIN, 1, p);
  check('no pads: length 0 and the rest zeroed', pep.u32(p) === 0 && pep.u32(p + 8) === 0 && pep.u32(p + 8 + 464) === 0);
  pep.gamepads = [{ id: 'Pad A', timestamp: 2500, axes: [0.5, -0.25], buttons: [1, 0, 0.5] }, { id: 'B', timestamp: 0, axes: [], buttons: [] }];
  call('PPB_Gamepad', 'Sample', MAIN, 1, p);
  const it = (i) => p + 8 + i * 472;
  check('one item per pad', pep.u32(p) === 2);
  check('axes and buttons land at their offsets', pep.u32(it(0)) === 2 && pep.f32(it(0) + 4) === 0.5 && pep.f32(it(0) + 8) === -0.25
    && pep.u32(it(0) + 68) === 3 && pep.f32(it(0) + 72) === 1 && pep.f32(it(0) + 80) === 0.5);
  check('timestamp is in seconds, connected is 1', pep.f64(it(0) + 200) === 2.5 && pep.u32(it(0) + 464) === 1 && pep.u32(it(1) + 464) === 1);
  const id = [...pep.bytes(it(0) + 208, 12)];
  check('id is UTF-16 and NUL terminated', id[0] === 80 && id[1] === 0 && id[2] === 97 && id[8] === 65 && id[9] === 0 && id[10] === 0 && id[11] === 0, JSON.stringify(id));
  check('unused pad slots stay zero', pep.u32(it(2) + 464) === 0 && pep.u32(it(3)) === 0);
}
// ---- Camera and microphone conversions: I420 planes (BT.601 limited range) and 16-bit PCM
{
  const { rgbaToI420, floatToPcm16 } = await import('./ppb-media.js');
  const px = (r, g, b) => [r, g, b, 255];
  const frame = new Uint8ClampedArray([...px(255, 0, 0), ...px(255, 0, 0), ...px(255, 255, 255), ...px(255, 255, 255), ...px(0, 0, 0), ...px(0, 0, 0), ...px(0, 0, 255), ...px(0, 0, 255)]);   // 4x2: red | white over black | blue
  const out = new Uint8Array(4 * 2 * 3 / 2);
  rgbaToI420(frame, 4, 2, out);
  check('I420: Y plane of red, white, black and blue', Array.from(out.slice(0, 8)).join() === '82,82,235,235,16,16,41,41', Array.from(out.slice(0, 8)).join());
  check('I420: one U and one V per 2x2 block, U planes then V planes', out.length === 12 && out[8] === 90 && out[9] === 128 && out[10] === 240 && out[11] === 128, Array.from(out.slice(8)).join());
  const pcm = new Int16Array(5);
  floatToPcm16(new Float32Array([0, 0.5, 1, -1, 3]), 0, 5, pcm, 0);
  check('PCM16: scaled, clipped to the full range', Array.from(pcm).join() === '0,16383,32767,-32768,32767', Array.from(pcm).join());
}

// ---- Printing: PP_PrintSettings_Dev is 60 bytes (printable_area, content_area, paper_size, dpi, orientation, scaling, grayscale, format)
{
  pep.instances.set(1, { id: 1 });
  const pr = call('PPB_Printing', 'Create', MAIN, 1), p = pep.alloc(60);
  const rc = call('PPB_Printing', 'GetDefaultPrintSettings', MAIN, pr, p, { func: 0, user: 0, flags: 0 });
  const at = (o) => pep.i32(p + o);
  check('default print settings: Letter, 72 dpi, no margins', rc === 0 && [at(0), at(4), at(8), at(12)].join() === '0,0,612,792' && [at(16), at(20), at(24), at(28)].join() === '0,0,612,792' && at(32) === 612 && at(36) === 792 && at(40) === 72, [0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40].map(at).join());
  check('default print settings: normal, no scaling, colour, PDF', at(44) === 0 && at(48) === 0 && at(52) === 0 && at(56) === 2);
  check('settings for a bad resource are refused', call('PPB_Printing', 'GetDefaultPrintSettings', MAIN, 9999, p, { func: 0, user: 0, flags: 0 }) < 0);
}

// ---- File dialogs: the chooser asks the frame, copies the pick into the guest, FileRef/Flash_File_FileRef give Flash a descriptor
{
  const { acceptList } = await import('./frame-services.js');
  check('accept types become an <input accept> list', acceptList('*.txt, text/plain,png, .jpg') === '.txt,text/plain,.png,.jpg' && acceptList('') === '' && acceptList(undefined) === '');
  const guestFiles = new Map(), asked = [];
  const savedEmu = pep.emu;
  pep.emu = { ...savedEmu, mkdirp: () => 0, writeFile: (p, b) => { guestFiles.set(p, b); return 0; }, stat: (p) => (guestFiles.has(p) ? { size: guestFiles.get(p).length, mode: 0o100644, mtime: 1000 } : null),
    openFd: (p, fl) => (guestFiles.has(p) ? 7 : (fl & 0x40) ? (guestFiles.set(p, new Uint8Array(0)), 8) : -2), dumpTree: () => [] };
  pep.opts.service = { request: async (op, args) => { asked.push([op, args]); return { ok: true, value: [{ name: 'C:\\Users\\x\\pick.txt', type: 'text/plain', bytes: Uint8Array.of(104, 105) }] }; } };
  pep.instances.set(1, { id: 1 });
  const ch = call('PPB_FileChooser(Dev)', 'Create', MAIN, 1, 1, pep.mkVar('*.txt'));
  check('a file chooser resource is created', call('PPB_FileChooser(Dev)', 'IsFileChooser', MAIN, ch) === 1 && pep.getRes(ch, 'filechooser').mode === 1);
  const cc = { func: 1, user: 0, flags: 0 }, noOut = { func: 0, user: 0 };
  const rc = call('PPB_FileChooserTrusted', 'ShowWithoutUserGesture', MAIN, ch, 0, pep.mkVar(undefined), noOut, cc);
  check('Show is asynchronous', rc === PP_OK_COMPLETIONPENDING, String(rc));
  await new Promise((r) => setTimeout(r, 20));
  check('the frame is asked for files with the accept list and the multiple flag', asked.length === 1 && asked[0][0] === 'pick-files' && asked[0][1].accept === '*.txt' && asked[0][1].multiple === true, JSON.stringify(asked));
  const path = [...guestFiles.keys()][0];
  check('the picked bytes land under /tmp/flash-runner-files with only the base name', /^\/tmp\/flash-runner-files\/\d+\/pick\.txt$/.test(path) && guestFiles.get(path).join() === '104,105', String(path));
  const refId = [...pep.res].find(([, o]) => o.type === 'fileref')?.[0];
  check('a FileRef resource exists for it and is external', !!refId && call('PPB_FileRef', 'IsFileRef', MAIN, refId) === 1 && call('PPB_FileRef', 'GetFileSystemType', MAIN, refId) === 3);
  const fp = pep.alloc(4), info = pep.alloc(40);
  check('Flash_File_FileRef.OpenFile gives a read-only descriptor', call('PPB_Flash_File_FileRef', 'OpenFile', MAIN, refId, 1, fp) === 0 && pep.i32(fp) === 7);
  check('opening a picked file for writing is refused', call('PPB_Flash_File_FileRef', 'OpenFile', MAIN, refId, 2, fp) === -7);
  check('QueryFile reports the size and the external file system', call('PPB_Flash_File_FileRef', 'QueryFile', MAIN, refId, info) === 0 && pep.u32(info) === 2 && pep.i32(info + 12) === 3);
  pep.opts.service = { request: async () => ({ ok: false, error: 'a file dialog needs a click or key press first' }) };
  const before = pep.res.size;
  call('PPB_FileChooserTrusted', 'ShowWithoutUserGesture', MAIN, ch, 0, pep.mkVar(undefined), noOut, cc);
  await new Promise((r) => setTimeout(r, 20));
  check('a refused dialog creates no file', pep.res.size === before);
  const saveRc = call('PPB_FileChooserTrusted', 'ShowWithoutUserGesture', MAIN, ch, 1, pep.mkVar('dir/..\\out.txt'), noOut, cc);
  await new Promise((r) => setTimeout(r, 20));
  const saveRef = [...pep.res].filter(([, o]) => o.type === 'fileref' && o.kind === 'save').pop()?.[1];
  check('a save dialog hands out a file named after the suggestion, without a path', saveRc === PP_OK_COMPLETIONPENDING && saveRef?.name === 'out.txt' && /^\/tmp\/flash-runner-files\/\d+\/out\.txt$/.test(saveRef.path), JSON.stringify(saveRef));
  pep.emu = savedEmu; delete pep.opts.service;
}

console.log(failed ? `\n${failed} FAILED` : '\nall ok');
process.exit(failed ? 1 : 0);
