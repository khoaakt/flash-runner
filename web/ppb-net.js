// URLLoader / URLRequestInfo / URLResponseInfo / URLUtil on top of fetch().
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_BADRESOURCE, PP_ERROR_BADARGUMENT, PP_ERROR_ABORTED, PP_ERROR_CONNECTION_FAILED, PP_ERROR_INPROGRESS, PP_ERROR_BLOCKS_MAIN_THREAD, PP_ERROR_NO_MESSAGE_LOOP, VT } from './pp-consts.js';

const PP_TRUE = 1;

function parseHeaders(s) {
  const h = new Headers();
  for (const line of s.split(/\r?\n/)) {
    const i = line.indexOf(':');
    if (i > 0) { try { h.append(line.slice(0, i).trim(), line.slice(i + 1).trim()); } catch {} }
  }
  return h;
}

export function buildNet(host) {
  const impl = {};
  const base = () => host.opts.baseUrl || (typeof location !== 'undefined' ? location.href : 'http://localhost/');
  const resolve = (u, b = base()) => { try { return new URL(u, b).href; } catch { return null; } };

  impl.PPB_URLRequestInfo = {
    Create(tid, instance) {
      return host.newRes({ type: 'urlrequest', instance, url: '', method: 'GET', headers: '', body: [], followRedirects: true, allowCredentials: false, allowCross: false });
    },
    IsURLRequestInfo(tid, r) { return host.getRes(r, 'urlrequest') ? 1 : 0; },
    SetProperty(tid, r, prop, v) {
      const q = host.getRes(r, 'urlrequest'); if (!q) return 0;
      const val = host.varValue(v);
      switch (prop) {
        case 0: q.url = String(val); break;
        case 1: q.method = String(val).toUpperCase() || 'GET'; break;
        case 2: q.headers = String(val); break;
        case 4: q.followRedirects = !!val; break;
        case 5: q.recordDownload = !!val; break;
        case 11: q.prefetchUpper = Number(val) | 0; break;
        case 12: q.prefetchLower = Number(val) | 0; break;
        case 6: q.recordUpload = !!val; break;
        case 7: q.referrer = String(val); break;
        case 8: q.allowCross = !!val; break;
        case 9: q.allowCredentials = !!val; break;
        case 3: return val ? 0 : 1;            // stream-to-file is unsupported
        default: break;
      }
      return 1;
    },
    AppendDataToBody(tid, r, data, len) { const q = host.getRes(r, 'urlrequest'); if (!q) return 0; q.body.push(host.ro(data, len).slice()); return 1; },
    AppendFileToBody() { return 0; },
  };

  impl.PPB_URLResponseInfo = {
    IsURLResponseInfo(tid, r) { return host.getRes(r, 'urlresponse') ? 1 : 0; },
    GetProperty(tid, r, prop) {
      const o = host.getRes(r, 'urlresponse'); if (!o) return host.mkVar(undefined);
      switch (prop) {
        case 0: return host.mkVar(o.url);
        case 1: return host.mkVar(o.redirectUrl || '');
        case 2: return host.mkVar(o.redirectMethod || '');
        case 3: return host.mkVar(o.status);
        case 4: return host.mkVar(o.statusLine);
        case 5: return host.mkVar(o.headers);
        default: return host.mkVar(undefined);
      }
    },
    GetBodyAsFileRef() { return 0; },
  };

  impl.PPB_URLLoader = {
    Create(tid, instance) {
      const L = { type: 'urlloader', instance, chunks: [], chunkOff: 0, avail: 0, done: false, err: 0, received: 0, total: -1, waiting: null, abort: null, response: null, closed: false, openDone: null, resumePump: null, upper: 32 << 20, lower: 16 << 20 };
      L.onFree = () => closeLoader(L);                      // last reference gone: implicit Close
      return host.newRes(L);
    },
    IsURLLoader(tid, r) { return host.getRes(r, 'urlloader') ? 1 : 0; },
    Open(tid, r, request, cc) {
      const L = host.getRes(r, 'urlloader'), q = host.getRes(request, 'urlrequest');
      if (!L || !q) return PP_ERROR_BADRESOURCE;
      if (L.response || L.openDone) return PP_ERROR_INPROGRESS;         // a loader opens one request
      const url = resolve(q.url);
      if (!url) return host.complete(tid, cc, PP_ERROR_BADARGUMENT);
      const ctl = new AbortController(); L.abort = ctl;
      if (q.prefetchUpper > 0) { L.upper = q.prefetchUpper; L.lower = q.prefetchLower > 0 ? q.prefetchLower : q.prefetchUpper >> 1; }
      const body = q.body.length ? new Blob(q.body) : undefined;
      const method = q.method;
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        let finished = false;
        const fin = (v) => { if (finished) return; finished = true; L.openDone = null; done(v); };
        L.openDone = fin;
        const init = { method, headers: parseHeaders(q.headers), signal: ctl.signal, redirect: q.followRedirects ? 'follow' : 'manual', credentials: q.allowCredentials ? 'include' : 'same-origin' };
        if (body && method !== 'GET' && method !== 'HEAD') init.body = body;
        const t0 = performance.now(), shown = url.replace(/[?#].*$/, '');
        if (host.opts.profile) { (host.pendingFetches ||= new Map()).set(L, shown); const clear = () => host.pendingFetches.delete(L); ctl.signal.addEventListener('abort', clear); L.clearPending = clear; }
        // static downloads go through the two-tier cache (web/net-cache.js): fresh -> no request, stale -> conditional request, 304 -> cached body
        const cache = host.opts.netCache, useCache = !!cache && cache.eligible(method, url, !!body, init.headers);
        const serveCached = (rec, how) => {
          const m = rec.meta;
          L.response = { type: 'urlresponse', url, status: m.status || 200, statusLine: m.statusLine, headers: m.headers, redirectUrl: '', redirectMethod: '' };
          L.total = L.received = L.avail = rec.body.length; L.chunks.push(rec.body); L.done = true; L.clearPending?.();
          if (host.opts.profile) host.log(`[net] ${method} ${shown} -> cache ${how} (${rec.body.length} bytes, ${(performance.now() - t0).toFixed(0)} ms)`);
          fin(PP_OK);
        };
        const start = (rec) => {
        if (rec) for (const [k, v] of cache.conditionalHeaders(rec.meta)) init.headers.set(k, v);
        fetch(url, init).then((resp) => {
          if (L.closed) return;
          if (rec && resp.status === 304) { resp.body?.cancel().catch(() => {}); cache.refresh(url, rec.meta, resp.headers); return serveCached(rec, 'revalidated'); }
          if (host.opts.profile) host.log(`[net] ${method} ${shown}${/[?]/.test(url) ? '?…' : ''} -> ${resp.status} (${(performance.now() - t0).toFixed(0)} ms to headers; ${resp.headers.get('cache-control') || 'no cache-control'}${resp.headers.get('etag') ? ', etag' : ''}${resp.headers.get('last-modified') ? ', last-modified' : ''})`);
          const hdrs = []; resp.headers.forEach((v, k) => hdrs.push(`${k}: ${v}`));
          L.response = { type: 'urlresponse', url: resp.url || url, status: resp.status, statusLine: `HTTP/1.1 ${resp.status} ${resp.statusText || ''}`.trim(), headers: hdrs.join('\r\n'), redirectUrl: '', redirectMethod: '' };
          const cl = resp.headers.get('content-length'); if (cl) L.total = +cl;
          if (useCache && resp.status === 200 && !resp.redirected) { const m = cache.metaFrom(resp, L.response.headers, L.response.statusLine, url); if (m) { L.cacheMeta = m; L.cacheParts = []; } }
          const keep = () => {                                                 // a complete, undamaged body: remember it
            const parts = L.cacheParts; L.cacheParts = null;
            if (!parts || L.err || (cl && !resp.headers.get('content-encoding') && +cl !== L.received)) return;
            const bytes = new Uint8Array(L.received); let o = 0;
            for (const c of parts) { bytes.set(c, o); o += c.length; }
            cache.put(url, L.cacheMeta, bytes).catch(() => {});
          };
          const reader = resp.body ? resp.body.getReader() : null;
          const pump = () => {
            if (L.closed) return;
            return reader.read().then(({ done: d, value }) => {
              if (d) {
                L.done = true; L.clearPending?.();
                if (L.cacheParts) keep();
                if (host.opts.profile) {
                  let body = '';                                                    // tiny text answers (login/status calls) are worth reading; credentials are masked
                  if (L.received > 0 && L.received <= 400 && /xml|text|json/i.test(resp.headers.get('content-type') || '')) { try { body = ' body: ' + new TextDecoder().decode(L.chunks.length === 1 ? L.chunks[0] : Uint8Array.from(L.chunks.flatMap((c) => [...c]))).replace(/[0-9a-f]{20,}/gi, '<HEX>').replace(/\s+/g, ' ').slice(0, 300); } catch { /* binary */ } }
                  host.log(`[net] done ${shown} ${L.received} bytes in ${(performance.now() - t0).toFixed(0)} ms${body}`);
                }
                wake(); return;
              }
              if (L.cacheParts) { if (L.received + value.length > cache.maxEntry) L.cacheParts = null; else L.cacheParts.push(value); }
              L.chunks.push(value); L.avail += value.length; L.received += value.length; wake();
              if (L.avail >= L.upper) { L.resumePump = pump; return; }          // backpressure: stop pulling until consumed
              return pump();
            }).catch(() => { L.err = ctl.signal.aborted ? PP_ERROR_ABORTED : PP_ERROR_FAILED; L.done = true; wake(); });
          };
          if (reader) pump(); else L.done = true;
          fin(PP_OK);
        }).catch((e) => {
          L.clearPending?.();
          if (rec && !L.closed && !L.response) { cache.stats.staleServed++; return serveCached(rec, 'stale (network failed)'); }   // better an old copy than a broken movie
          if (!L.closed) host.log(`fetch ${url.replace(/[?#].*$/, '')} failed: ${e}`);
          fin(L.closed ? PP_ERROR_ABORTED : PP_ERROR_CONNECTION_FAILED);
        });
        };
        if (!useCache) start(null);
        else cache.lookup(url).catch(() => null).then((rec) => { if (L.closed) return; if (rec && cache.fresh(rec.meta)) serveCached(rec, 'fresh'); else start(rec); });
      });
      function wake() { if (L.waiting) { const w = L.waiting; L.waiting = null; w(); } }
    },
    FollowRedirect() { return PP_ERROR_FAILED; },
    GetUploadProgress(tid, r, a, b) { return 0; },
    GetDownloadProgress(tid, r, recvPtr, totalPtr) {
      const L = host.getRes(r, 'urlloader'); if (!L) return 0;
      const wr = (p, v) => { host.w32(p, v >>> 0); host.w32(p + 4, v < 0 ? 0xffffffff : 0); };
      if (recvPtr) wr(recvPtr, L.received);
      if (totalPtr) wr(totalPtr, L.total);
      return PP_TRUE;
    },
    GetResponseInfo(tid, r) {
      const L = host.getRes(r, 'urlloader'); if (!L || !L.response) return 0;
      return host.newRes({ ...L.response });
    },
    ReadResponseBody(tid, r, buf, n, cc) {
      const L = host.getRes(r, 'urlloader'); if (!L) return PP_ERROR_BADRESOURCE;
      if (n <= 0) return PP_ERROR_BADARGUMENT;
      if (!L.response && !L.done) return PP_ERROR_FAILED;                 // Open has not completed
      if (L.waiting) return PP_ERROR_INPROGRESS;                          // one outstanding read at a time
      const take = () => {
        let out = 0;
        while (out < n && L.chunks.length) {
          const c = L.chunks[0], avail = c.length - L.chunkOff, k = Math.min(avail, n - out);
          host.putBytes(buf + out, c.subarray(L.chunkOff, L.chunkOff + k));
          out += k; L.chunkOff += k; L.avail -= k;
          if (L.chunkOff >= c.length) { L.chunks.shift(); L.chunkOff = 0; }
        }
        if (L.resumePump && L.avail < L.lower) { const f = L.resumePump; L.resumePump = null; f(); }
        return out;
      };
      const ready = () => L.avail > 0 || L.done;
      const result = () => (L.avail > 0 ? take() : (L.err || 0));
      const optional = (cc.flags & 1) !== 0;                              // PP_COMPLETIONCALLBACK_FLAG_OPTIONAL
      if (!cc.func) {                                                     // blocking read: off the main thread only
        if (tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
        if (ready()) return result();
        return new Promise((res) => { L.waiting = function w() { if (ready()) res(result()); else L.waiting = w; }; });
      }
      if (optional && ready()) return result();                           // optional callbacks may complete synchronously
      if (optional && !host.threadLoop.get(tid)) return PP_ERROR_NO_MESSAGE_LOOP;
      const loop = host.threadLoop.get(tid) || 1;
      const deliver = () => host.postTask(loop, cc.func, [cc.user, result() >>> 0]);
      if (ready()) { Promise.resolve().then(deliver); return PP_OK_COMPLETIONPENDING; }   // required: always via the callback
      L.waiting = function w() { if (ready()) deliver(); else L.waiting = w; };
      return PP_OK_COMPLETIONPENDING;
    },
    FinishStreamingToFile() { return PP_ERROR_FAILED; },
    Close(tid, r) { const L = host.getRes(r, 'urlloader'); if (L) closeLoader(L); },
  };
  /** Cancel a loader: abort the fetch, complete any pending Open/read as aborted. */
  function closeLoader(L) {
    if (L.closed) return;
    L.closed = true;
    L.abort?.abort();
    L.chunks.length = 0; L.avail = 0; L.chunkOff = 0;
    if (!L.done) { L.done = true; L.err = PP_ERROR_ABORTED; }
    if (L.openDone) L.openDone(PP_ERROR_ABORTED);
    if (L.waiting) { const w = L.waiting; L.waiting = null; w(); }
  }
  impl.PPB_URLLoaderTrusted = { GrantUniversalAccess(tid, r) { const L = host.getRes(r, 'urlloader'); if (L) L.universal = true; }, RegisterStatusCallback() {} };

  // PPB_URLUtil(Dev)
  function fillComponents(p, u) {
    if (!p) return;
    const set = (i, begin, len) => { host.wi32(p + i * 8, begin); host.wi32(p + i * 8 + 4, len); };
    let m = /^([a-zA-Z][a-zA-Z0-9+.-]*):(\/\/)?(?:([^:@/?#]*)(?::([^@/?#]*))?@)?([^:/?#]*)(?::(\d+))?([^?#]*)(?:\?([^#]*))?(?:#(.*))?$/.exec(u);
    for (let i = 0; i < 8; i++) set(i, 0, -1);
    if (!m) return;
    let off = 0; set(0, 0, m[1].length); off = m[1].length + 1 + (m[2] ? 2 : 0);
    if (m[3] !== undefined) { set(1, off, m[3].length); off += m[3].length; if (m[4] !== undefined) { off++; set(2, off, m[4].length); off += m[4].length; } off++; }
    set(3, off, m[5].length); off += m[5].length;
    if (m[6] !== undefined) { off++; set(4, off, m[6].length); off += m[6].length; }
    set(5, off, m[7].length); off += m[7].length;
    if (m[8] !== undefined) { off++; set(6, off, m[8].length); off += m[8].length; }
    if (m[9] !== undefined) { off++; set(7, off, m[9].length); }
  }
  impl.PPB_URLUtil = {
    Canonicalize(tid, v, comps) { const u = resolve(String(host.varValue(v)), 'http://invalid.base/'); if (!u) return host.mkVar(undefined); fillComponents(comps, u); return host.mkVar(u); },
    ResolveRelativeToURL(tid, b, rel, comps) { const u = resolve(String(host.varValue(rel)), String(host.varValue(b))); if (!u) return host.mkVar(undefined); fillComponents(comps, u); return host.mkVar(u); },
    ResolveRelativeToDocument(tid, instance, rel, comps) { const u = resolve(String(host.varValue(rel))); if (!u) return host.mkVar(undefined); fillComponents(comps, u); return host.mkVar(u); },
    IsSameSecurityOrigin(tid, a, b) { try { return new URL(host.varValue(a)).origin === new URL(host.varValue(b)).origin ? 1 : 0; } catch { return 0; } },
    DocumentCanRequest() { return 1; },
    DocumentCanAccessDocument() { return 1; },
    GetDocumentURL(tid, instance, comps) { const u = host.opts.documentUrl || base(); fillComponents(comps, u); return host.mkVar(u); },
    GetPluginInstanceURL(tid, instance, comps) { const u = host.instances.get(instance)?.srcUrl || base(); fillComponents(comps, u); return host.mkVar(u); },
    GetPluginReferrerURL(tid, instance, comps) { const u = host.opts.documentUrl || base(); fillComponents(comps, u); return host.mkVar(u); },
  };
  return impl;
}
