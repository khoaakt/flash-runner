// Two-tier cache for the plugin's static downloads (SWF, XML, images, sound): a byte-budgeted LRU in memory in front of IndexedDB.
//
// Policy (what a browser would do, made explicit because the plugin's fetches would otherwise depend on the profile's HTTP cache):
//  * only GET without body/Range/Authorization, only static-looking paths (`extensions`), only 200 answers that were not redirected;
//  * nothing is stored on `Cache-Control: no-store`, or when the answer has neither a validator (ETag / Last-Modified) nor max-age;
//  * an entry is served without touching the network only while it is fresh: `max-age`, else 10 % of the time between Last-Modified and
//    the fetch (capped at a day); `no-cache` entries are never fresh. Game *data* (XML/JSON/text: item tables, quest lists) gets no
//    heuristic freshness at all, only an explicit max-age: it is always revalidated, so it can never be stale, and a 304 costs one round trip;
//  * a stale entry is revalidated with If-None-Match / If-Modified-Since and served on 304 (no body is downloaded again);
//    if the network is down the stale copy is served rather than failing the movie.
// Cached bodies are immutable Uint8Arrays shared by every reader.

const STATIC = /\.(swf|swz|png|jpe?g|gif|webp|mp3|ogg|wav|xml|zip|bin|dat|ttf|otf|atf|fnt)$/i;
const HEURISTIC_CAP_MS = 24 * 3600e3;
const DATA = /\.(xml|json|txt)$/i;

export function parseCacheControl(v) {
  const out = { noStore: false, noCache: false, maxAge: null };
  for (const part of String(v || '').toLowerCase().split(',')) {
    const [k, val] = part.trim().split('=');
    if (k === 'no-store') out.noStore = true;
    else if (k === 'no-cache') out.noCache = true;
    else if (k === 'max-age' && /^\d+$/.test((val || '').replace(/"/g, ''))) out.maxAge = +val.replace(/"/g, '');
  }
  return out;
}

export class NetCache {
  constructor({ store = null, memBytes = 64 << 20, dbBytes = 512 << 20, maxEntry = 48 << 20, extensions = STATIC, now = () => Date.now() } = {}) {
    this.store = store; this.memBytes = memBytes; this.dbBytes = dbBytes; this.maxEntry = maxEntry; this.ext = extensions; this.now = now;
    this.meta = new Map();                                // url -> meta, every entry of both tiers
    this.mem = new Map();                                 // url -> Uint8Array, insertion order = recency
    this.memUsed = 0; this.dbUsed = 0;
    this.stats = { memHits: 0, dbHits: 0, revalidated: 0, misses: 0, stored: 0, evicted: 0, staleServed: 0 };
    this.ready = store
      ? store.loadMeta().then((rows) => { for (const [u, m] of rows) { this.meta.set(u, m); this.dbUsed += m.size; } }).catch(() => {})
      : Promise.resolve();
  }
  static async open(opts = {}) {
    const store = await IdbStore.open(opts.name).catch(() => null);
    const cache = new NetCache({ ...opts, store });
    await cache.ready;                                    // entry metadata is in memory before the first lookup
    return cache;
  }

  /** May this request be answered from / stored in the cache at all? `headers` is the request's Headers. */
  eligible(method, url, hasBody, headers) {
    if (method !== 'GET' || hasBody) return false;
    if (headers.has('range') || headers.has('authorization')) return false;
    if (/no-cache|no-store/i.test(`${headers.get('cache-control') || ''} ${headers.get('pragma') || ''}`)) return false;
    let u; try { u = new URL(url); } catch { return false; }
    return (u.protocol === 'http:' || u.protocol === 'https:') && this.ext.test(u.pathname);
  }

  /** { meta, body } for a stored entry (memory first, then IndexedDB), or null. */
  async lookup(url) {
    await this.ready;
    const meta = this.meta.get(url);
    if (!meta) { this.stats.misses++; return null; }
    let body = this.mem.get(url);
    if (body) { this.mem.delete(url); this.mem.set(url, body); this.stats.memHits++; }
    else {
      try { body = this.store ? await this.store.getBody(url) : null; } catch { body = null; }
      if (!body || body.length !== meta.size) { this._drop(url); this.stats.misses++; return null; }
      this._remember(url, body); this.stats.dbHits++;
    }
    meta.lastUsed = this.now();
    return { meta, body };
  }
  fresh(meta) {
    if (meta.noCache) return false;
    const age = this.now() - meta.storedAt;
    if (meta.maxAge != null) return age < meta.maxAge * 1000;
    if (meta.noHeuristic) return false;
    const lm = meta.lastModified ? Date.parse(meta.lastModified) : NaN;
    if (Number.isNaN(lm)) return false;
    return age < Math.min(Math.max(0, meta.storedAt - lm) * 0.1, HEURISTIC_CAP_MS);
  }
  conditionalHeaders(meta) {
    const h = [];
    if (meta.etag) h.push(['If-None-Match', meta.etag]);
    else if (meta.lastModified) h.push(['If-Modified-Since', meta.lastModified]);
    return h;
  }
  /** Entry metadata for a 200 answer, or null when it must not be stored. `hdrs` = "k: v" lines the guest will see. */
  metaFrom(resp, hdrs, statusLine, url = '') {
    const cc = parseCacheControl(resp.headers.get('cache-control'));
    if (cc.noStore || resp.headers.get('vary') === '*') return null;
    const etag = resp.headers.get('etag'), lastModified = resp.headers.get('last-modified');
    if (!etag && !lastModified && cc.maxAge == null) return null;
    const t = this.now();
    let path = ''; try { path = new URL(url).pathname; } catch { /* no URL: treat as a static asset */ }
    return { etag, lastModified, maxAge: cc.maxAge, noCache: cc.noCache, noHeuristic: DATA.test(path), status: resp.status, statusLine, headers: hdrs, storedAt: t, lastUsed: t, size: 0 };
  }
  /** A 304 arrived: the entry is fresh again (and may carry new caching headers). */
  async refresh(url, meta, headers) {
    this.stats.revalidated++;
    const cc = parseCacheControl(headers && headers.get('cache-control'));
    meta.storedAt = meta.lastUsed = this.now();
    if (headers && headers.get('cache-control') != null) { meta.maxAge = cc.maxAge; meta.noCache = cc.noCache; }
    if (headers && headers.get('etag')) meta.etag = headers.get('etag');
    try { await this.store?.putMeta(url, meta); } catch { /* the in-memory copy is still right */ }
  }
  /** Store a complete body. Returns false when it was not kept. */
  async put(url, meta, body) {
    if (body.length === 0 || body.length > this.maxEntry) return false;
    await this.ready;
    const old = this.meta.get(url);
    if (old) { this.dbUsed -= old.size; this._forget(url); }
    meta.size = body.length;
    this.meta.set(url, meta); this.dbUsed += meta.size; this._remember(url, body);
    this.stats.stored++;
    if (this.store) { try { await this.store.put(url, meta, body); } catch { this._drop(url); return false; } }
    await this._evict();
    return true;
  }
  async clear() { await this.ready; this.meta.clear(); this.mem.clear(); this.memUsed = this.dbUsed = 0; try { await this.store?.clear(); } catch { /* nothing to clear */ } }

  _remember(url, body) {
    this._forget(url);
    if (body.length > this.memBytes / 2) return;          // one big file must not flush the whole tier
    this.mem.set(url, body); this.memUsed += body.length;
    while (this.memUsed > this.memBytes) { const [k, v] = this.mem.entries().next().value; this.mem.delete(k); this.memUsed -= v.length; }
  }
  _forget(url) { const b = this.mem.get(url); if (b) { this.mem.delete(url); this.memUsed -= b.length; } }
  _drop(url) {
    const m = this.meta.get(url); if (m) { this.dbUsed -= m.size; this.meta.delete(url); }
    this._forget(url);
    try { this.store?.delete(url).catch(() => {}); } catch { /* ignore */ }
  }
  async _evict() {
    if (this.dbUsed <= this.dbBytes) return;
    for (const [u] of [...this.meta].sort((a, b) => a[1].lastUsed - b[1].lastUsed)) {   // least recently used first, down to 90 % of the budget
      if (this.dbUsed <= this.dbBytes * 0.9) break;
      this._drop(u); this.stats.evicted++;
    }
  }
}

const idb = (r) => new Promise((res, rej) => { r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
const txDone = (tx) => new Promise((res, rej) => { tx.oncomplete = () => res(); tx.onerror = tx.onabort = () => rej(tx.error); });

/** Persistent tier: object store `meta` (url -> metadata, small, read at start) and `body` (url -> ArrayBuffer, read on demand). */
export class IdbStore {
  static async open(name = 'flash-runner-netcache') {
    if (typeof indexedDB === 'undefined') return null;
    const db = await new Promise((res, rej) => {
      const req = indexedDB.open(name, 1);
      req.onupgradeneeded = () => { req.result.createObjectStore('meta'); req.result.createObjectStore('body'); };
      req.onsuccess = () => res(req.result);
      req.onerror = () => rej(req.error);
    });
    return new IdbStore(db);
  }
  constructor(db) { this.db = db; }
  async loadMeta() {
    const s = this.db.transaction('meta', 'readonly').objectStore('meta');
    const keys = await idb(s.getAllKeys()), vals = await idb(this.db.transaction('meta', 'readonly').objectStore('meta').getAll());
    return keys.map((k, i) => [k, vals[i]]);
  }
  async getBody(url) {
    const v = await idb(this.db.transaction('body', 'readonly').objectStore('body').get(url));
    return v ? new Uint8Array(v) : null;
  }
  async put(url, meta, body) {
    const tx = this.db.transaction(['meta', 'body'], 'readwrite');
    tx.objectStore('body').put(body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength), url);
    tx.objectStore('meta').put(meta, url);
    await txDone(tx);
  }
  async putMeta(url, meta) { const tx = this.db.transaction('meta', 'readwrite'); tx.objectStore('meta').put(meta, url); await txDone(tx); }
  async delete(url) { const tx = this.db.transaction(['meta', 'body'], 'readwrite'); tx.objectStore('meta').delete(url); tx.objectStore('body').delete(url); await txDone(tx); }
  async clear() { const tx = this.db.transaction(['meta', 'body'], 'readwrite'); tx.objectStore('meta').clear(); tx.objectStore('body').clear(); await txDone(tx); }
}
