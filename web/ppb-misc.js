// Device enumeration, printing and the (deprecated) scripting interfaces.
// Scripting objects live in the worker for now (a stub `window`); a main-thread proxy comes later.
import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_NOTSUPPORTED, PP_ERROR_BADRESOURCE, VT } from './pp-consts.js';

export function buildMisc(host) {
  const impl = {};
  const loopOf = (tid) => host.threadLoop.get(tid) || 1;

  /** Complete an async call that fills a PP_ArrayOutput: run GetDataBuffer on the guest, write, then call `cc`. */
  function arrayThenCallback(tid, out, elementSize, elements, cc, errorCode = PP_OK) {
    // A blocking call (NULL callback) parks this thread, so GetDataBuffer cannot run on its own loop: run it on the main loop.
    if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
    const loop = cc.func ? loopOf(tid) : 1;
    const finish = (result) => (cc.func ? host.postTask(loop, cc.func, [cc.user, result >>> 0]) : null);
    const run = async () => {
      if (elements.length === 0 && out.func) {
        await host.postTask(loop, out.func, [out.user, 0, elementSize]);
      } else if (out.func) {
        const r = await host.postTask(loop, out.func, [out.user, elements.length, elementSize]);
        if (r.lo) elements.forEach((e, i) => host.putBytes(r.lo + i * elementSize, e));
      }
      finish(errorCode);
    };
    if (!cc.func) return new Promise((res) => run().then(() => res(errorCode)));
    run();
    return PP_OK_COMPLETIONPENDING;
  }
  host.arrayThenCallback = arrayThenCallback;

  // ---- device references ----------------------------------------------------------------
  impl.PPB_DeviceRef = {
    IsDeviceRef(tid, r) { return host.getRes(r, 'deviceref') ? 1 : 0; },
    GetType(tid, r) { return host.getRes(r, 'deviceref')?.devType ?? 0; },
    GetName(tid, r) { return host.mkVar(host.getRes(r, 'deviceref')?.name ?? ''); },
  };
  const u32le = (n) => new Uint8Array([n & 255, (n >> 8) & 255, (n >> 16) & 255, (n >> 24) & 255]);
  const enumerable = (kind, devType, defaultDevices) => ({
    Create(tid, instance) { return host.newRes({ type: kind, instance }); },
    EnumerateDevices(tid, r, out, cc) {
      if (!host.getRes(r, kind)) return PP_ERROR_BADRESOURCE;
      const refs = defaultDevices.map((name) => u32le(host.newRes({ type: 'deviceref', devType, name })));
      return arrayThenCallback(tid, out, 4, refs, cc);
    },
    MonitorDeviceChange() { return PP_OK; },
    Open(tid, ...a) { return host.complete(tid, a[a.length - 1], PP_ERROR_NOTSUPPORTED); },   // last argument is the PP_CompletionCallback
    GetCurrentConfig() { return 0; },
    StartCapture() { return PP_ERROR_FAILED; }, StopCapture() { return PP_OK; },
    StartPlayback() { return PP_ERROR_FAILED; }, StopPlayback() { return PP_OK; },
    Close() {},
  });
  impl.PPB_AudioInput = { ...enumerable('audioinput', 1, []), IsAudioInput(tid, r) { return host.getRes(r, 'audioinput') ? 1 : 0; } };
  impl.PPB_AudioOutput = { ...enumerable('audiooutput', 3, ['Default']), IsAudioOutput(tid, r) { return host.getRes(r, 'audiooutput') ? 1 : 0; } };
  impl.PPB_VideoCapture = { ...enumerable('videocapture', 2, []), IsVideoCapture(tid, r) { return host.getRes(r, 'videocapture') ? 1 : 0; }, ReuseBuffer() { return PP_ERROR_FAILED; } };
  // PPB_Printing(Dev): Flash asks for the page geometry before it prints. US Letter, 72 dpi, no margins, PDF output.
  impl.PPB_Printing = {
    Create(tid, instance) { return host.newRes({ type: 'printing', instance }); },
    GetDefaultPrintSettings(tid, r, p, cc) {
      if (!host.getRes(r, 'printing')) return host.complete(tid, cc, PP_ERROR_BADRESOURCE);
      const W = 612, H = 792;
      for (const base of [0, 16]) { host.wi32(p + base, 0); host.wi32(p + base + 4, 0); host.wi32(p + base + 8, W); host.wi32(p + base + 12, H); }   // printable_area, content_area
      host.wi32(p + 32, W); host.wi32(p + 36, H);          // paper_size
      host.wi32(p + 40, 72); host.wi32(p + 44, 0); host.wi32(p + 48, 0); host.wi32(p + 52, 0);   // dpi, orientation normal, scaling, colour
      host.wi32(p + 56, 2);                                // PP_PRINTOUTPUTFORMAT_PDF: the only format Pepper Flash offers
      return host.complete(tid, cc, PP_OK);
    },
  };

  // ---- scripting (deprecated Var + Instance_Private) ----------------------------------------
  // With opts.page = { request(msg) -> Promise<reply>, post(msg) } the window/document objects are the embedding
  // page's own (remote handles, one round trip per operation; the guest thread parks meanwhile). Without it a tiny
  // local stub window is used (Node tests, the standalone viewer).
  const page = () => host.opts.page;
  const winStub = () => {
    const url = (() => { try { return new URL(host.opts.documentUrl || host.opts.baseUrl || 'http://localhost/'); } catch { return new URL('http://localhost/'); } })();
    const location = { href: url.href, protocol: url.protocol, host: url.host, hostname: url.hostname, port: url.port, pathname: url.pathname, search: url.search, hash: url.hash, origin: url.origin };
    const document = { title: '', domain: url.hostname, cookie: '', location, URL: url.href, referrer: '', documentElement: {}, body: {} };
    const nav = { userAgent: host.opts.userAgent || (typeof globalThis.navigator !== 'undefined' ? globalThis.navigator.userAgent : 'flash-runner'), language: host.opts.language || 'en-US', platform: 'Linux i686' };
    const w = { location, document, navigator: nav, top: null, parent: null, self: null, innerWidth: 1280, innerHeight: 800 };
    w.top = w.parent = w.self = w.window = w;
    return w;
  };
  const objVar = (v) => { const id = host.nextVar++; host.vars.set(id, { refs: 1, type: VT.OBJECT, obj: v }); return { type: VT.OBJECT, lo: id, hi: 0 }; };
  const objOf = (v) => (v.type === VT.OBJECT ? host.vars.get(v.lo) : null);
  const keyOf = (name) => { const x = host.varValue(name); return typeof x === 'string' || typeof x === 'number' ? String(x) : null; };
  const isData = (t) => t !== null && typeof t === 'object' && !t.pluginClass && !t.remote && (Object.getPrototypeOf(t) === Object.prototype);
  const wrap = (x) => (x !== null && (typeof x === 'object' || typeof x === 'function') ? objVar(x) : host.mkVar(x));
  const unwrapArg = (v) => (v.type === VT.OBJECT ? objOf(v)?.obj : host.varValue(v));
  const setExc = (p, msg) => { if (p) { const v = host.mkVar(String(msg)); host.writeVar(p, v); } };

  // remote (page-world) values: {t:'u'|'l'|'b'|'n'|'s'|'r', v, id}
  const fromRemote = (e) => {
    if (!e) return host.mkVar(undefined);
    switch (e.t) {
      case 'l': return host.mkVar(null);
      case 'b': case 'n': case 's': return host.mkVar(e.v);
      case 'r': return objVar({ remote: e.id, fn: !!e.f });
      default: return host.mkVar(undefined);
    }
  };
  const toRemote = (v) => {
    if (v.type === VT.OBJECT) { const o = objOf(v)?.obj; return o && o.remote ? { t: 'r', id: o.remote } : { t: 'u' }; }
    const x = host.varValue(v);
    if (x === undefined) return { t: 'u' };
    if (x === null) return { t: 'l' };
    if (typeof x === 'boolean') return { t: 'b', v: x };
    if (typeof x === 'number') return { t: 'n', v: x };
    if (typeof x === 'string') return { t: 's', v: x };
    return { t: 'u' };
  };
  const REMOTE_TIMEOUT = 5000;
  /** Round trip to the page; resolves to a PP_Var (and fills `exc` on failure). */
  const remote = (msg, exc, asVar = true) => new Promise((resolve) => {
    let done = false;
    const finish = (r) => { if (done) return; done = true; clearTimeout(timer); resolve(r); };
    const timer = setTimeout(() => finish({ ok: false, error: 'page did not answer' }), REMOTE_TIMEOUT);
    page().request(msg).then(finish, (e) => finish({ ok: false, error: String(e) }));
  }).then((r) => {
    if (!r.ok) { setExc(exc, r.error || 'script error'); return asVar ? host.mkVar(undefined) : false; }
    return asVar ? fromRemote(r.value) : !!r.value?.v;
  });
  host.releaseRemote = (id) => { try { page()?.post({ op: 'release', id }); } catch { /* page gone */ } };

  impl.PPB_Instance_Private = {
    GetWindowObject(tid, instance) {
      if (page()) return objVar({ remote: 1 });                       // handle 1 = the embedding page's window
      const i = host.instances.get(instance); if (i) i.win = i.win || winStub(); return objVar(i?.win || winStub());
    },
    GetOwnerElementObject(tid, instance) { return page() ? objVar({ remote: 2 }) : objVar({ nodeName: 'EMBED', tagName: 'EMBED' }); },   // handle 2 = the element that hosts the player
    ExecuteScript(tid, instance, script, exc) {
      const src = String(host.varValue(script));
      if (host.opts.trace) console.error('[script]', src);
      if (host.opts.profile && !/^function __flash__arrayToXML/.test(src)) host.log(`[js] ${src.slice(0, 220).replace(/\s+/g, ' ')}`);
      if (page()) return remote({ op: 'exec', script: src }, exc);
      // No page bridge (Node tests, the standalone viewer, or script access denied): scripts are not evaluated anywhere. Evaluating them
      // here would run plugin-supplied text with the worker's own privileges, so the answer is a plain exception.
      setExc(exc, 'script access is not available');
      return host.mkVar(undefined);
    },
  };
  const remoteOf = (o) => objOf(o)?.obj?.remote || 0;
  const varImpl = host.impl?.PPB_Var;
  impl['PPB_Var(Deprecated)'] = {
    AddRef(tid, v) { host.addRefVar(v); },
    Release(tid, v) { host.releaseVar(v); },
    VarFromUtf8(tid, module, data, len) { return host.mkVar(new TextDecoder().decode(host.ro(data, len))); },
    VarToUtf8(tid, v, lenPtr) {
      const e = v.type === VT.STRING ? host.vars.get(v.lo) : null;
      if (!e) { if (lenPtr) host.w32(lenPtr, 0); return 0; }
      if (!e.ptr) { const b = new TextEncoder().encode(e.s); e.ptr = host.allocBytes(b); e.len = b.length; }
      if (lenPtr) host.w32(lenPtr, e.len); return e.ptr;
    },
    HasProperty(tid, o, name, exc) {
      const k = keyOf(name), r = remoteOf(o);
      if (r) return k == null ? 0 : remote({ op: 'has', id: r, name: k }, exc, false);
      const t = objOf(o)?.obj; return t != null && k != null && isData(t) && Object.hasOwn(t, k) ? 1 : 0;
    },
    HasMethod(tid, o, name, exc) {
      const k = keyOf(name), r = remoteOf(o);
      if (r) return k == null ? 0 : remote({ op: 'hasMethod', id: r, name: k }, exc, false);
      return 0;                                                        // the local stub objects are plain data: no methods
    },
    GetProperty(tid, o, name, exc) {
      const k = keyOf(name), r = remoteOf(o);
      if (host.opts.trace) console.error('[getprop]', k);
      if (r) { if (k == null) { setExc(exc, 'bad name'); return host.mkVar(undefined); } return remote({ op: 'get', id: r, name: k }, exc); }
      const t = objOf(o)?.obj;
      if (t == null || k == null || !isData(t)) { setExc(exc, 'bad object'); return host.mkVar(undefined); }
      return Object.hasOwn(t, k) ? wrap(t[k]) : host.mkVar(undefined);      // own properties only: never reach Object.prototype / constructor
    },
    GetAllPropertyNames(tid, o, countPtr, propsPtr, exc) { host.w32(countPtr, 0); host.w32(propsPtr, 0); },
    SetProperty(tid, o, name, value, exc) {
      const k = keyOf(name), r = remoteOf(o);
      if (r) { if (k != null) return remote({ op: 'set', id: r, name: k, value: toRemote(value) }, exc).then(() => undefined); return; }
      /* local stub objects are read-only */
    },
    RemoveProperty(tid, o, name, exc) {
      const k = keyOf(name), r = remoteOf(o);
      if (r) { if (k != null) return remote({ op: 'remove', id: r, name: k }, exc).then(() => undefined); return; }
      /* local stub objects are read-only */
    },
    Call(tid, o, name, argc, argv, exc) {
      const r = remoteOf(o);
      if (r) {
        const args = []; for (let i = 0; i < argc; i++) args.push(toRemote(host.readVar(argv + i * 16)));
        return remote({ op: 'call', id: r, name: name.type === VT.UNDEFINED ? null : keyOf(name), args }, exc);
      }
      setExc(exc, 'not a function');                                   // the local stub has no callable members
      return host.mkVar(undefined);
    },
    Construct(tid, o, argc, argv, exc) {
      const r = remoteOf(o);
      if (r) { const args = []; for (let i = 0; i < argc; i++) args.push(toRemote(host.readVar(argv + i * 16))); return remote({ op: 'construct', id: r, args }, exc); }
      setExc(exc, 'construct unsupported'); return host.mkVar(undefined);
    },
    IsInstanceOf(tid, v, cls, dataPtr) {
      const o = objOf(v)?.obj;
      if (!o || o.pluginClass !== cls) return 0;
      if (dataPtr) host.w32(dataPtr, o.pluginData);
      return 1;
    },
    CreateObject(tid, instance, cls, data) { return objVar({ pluginClass: cls, pluginData: data }); },
    CreateObjectWithModuleDeprecated(tid, module, cls, data) { return objVar({ pluginClass: cls, pluginData: data }); },
  };
  return impl;
}
