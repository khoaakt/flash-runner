// Runs in the page's own JavaScript world. Two jobs for the extension iframes that replace Flash <embed>/<object>:
//
//  1. Flash -> page. The emulated player asks (postMessage) to evaluate scripts / read properties / call functions in
//     this page: that is how Flash's ExecuteScript, GetWindowObject and ExternalInterface.call reach the document.
//  2. Page -> Flash. The iframe element gets Flash's scriptable surface (SetVariable, PercentLoaded, ...) and every
//     function registered with ExternalInterface.addCallback as methods, e.g. document.getElementById('game').setScore(5).
//     They cannot be synchronous (the movie runs in a worker): they return a Promise.
(() => {
  if (window.__flashRunnerBridge) return;
  window.__flashRunnerBridge = true;

  const states = new WeakMap();                       // iframe element -> { handles, next, origin, started }
  const pendingInvoke = new Map();
  let rid = 0;

  const ownFrame = (src) => {
    for (const f of document.querySelectorAll('iframe[data-flash-runner="1"]')) if (f.contentWindow === src) return f;
    return null;
  };
  const stateOf = (f) => {
    let st = states.get(f);
    if (!st) {
      let origin = '*'; try { origin = new URL(f.src).origin; } catch { /* keep wildcard */ }
      st = { handles: new Map([[1, window], [2, f]]), next: 3, origin, started: null, resolveStarted: null };
      st.started = new Promise((r) => { st.resolveStarted = r; });
      states.set(f, st);
    }
    return st;
  };

  // ---- values crossing the frame boundary: {t:'u'|'l'|'b'|'n'|'s'|'r', v, id, f} ---------------------------------
  const enc = (st, v) => {
    switch (typeof v) {
      case 'undefined': case 'symbol': return { t: 'u' };
      case 'boolean': return { t: 'b', v };
      case 'number': return { t: 'n', v };
      case 'string': return { t: 's', v };
      case 'bigint': return { t: 's', v: String(v) };
      default: { if (v === null) return { t: 'l' }; const id = st.next++; st.handles.set(id, v); return { t: 'r', id, f: typeof v === 'function' }; }
    }
  };
  const dec = (st, e) => {
    if (!e) return undefined;
    switch (e.t) { case 'l': return null; case 'b': case 'n': case 's': return e.v; case 'r': return st.handles.get(e.id); default: return undefined; }
  };
  const target = (st, id) => {
    const o = st.handles.get(id);
    if (o === undefined || o === null) throw new TypeError('stale object reference');
    return o;
  };
  const describe = (e) => { try { return e && e.message ? `${e.name || 'Error'}: ${e.message}` : String(e); } catch { return 'script error'; } };

  // ---- pages whose CSP forbids eval -------------------------------------------------------------------------------
  // Under a CSP without 'unsafe-eval', (0, eval)(...) throws EvalError. The scripts Flash actually sends have two shapes (recorded from
  // ExternalInterface.call and Flash's own helper definitions), so those are run without eval; anything else is refused, never guessed.
  //   1. the helper definitions (__flash__toXML & co): their job is done by `flashToXML` below, so they are accepted and ignored;
  //   2. try { __flash__toXML(path.to.fn(literal, ...)) ; } catch (e) { "<undefined/>"; }
  // `literal` is a string, number, boolean, null, undefined, array or object literal (what Flash serialises its arguments to). The same
  // literal parser reads the JavaScript expression Flash answers with when the page calls a Flash callback.
  const evalFree = (() => {
    const WS = /\s/, IDENT = /[A-Za-z_$][A-Za-z0-9_$]*/y, NUM = /-?(?:NaN|Infinity|(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?)/y;
    const ESC = { n: '\n', r: '\r', t: '\t', b: '\b', f: '\f', v: '\v', 0: '\0' };
    const BACKSLASH = String.fromCharCode(92);
    class Reader {
      constructor(s) { this.s = s; this.i = 0; this.depth = 0; }
      ws() { while (this.i < this.s.length && WS.test(this.s[this.i])) this.i++; }
      peek() { this.ws(); return this.s[this.i]; }
      eat(c) { if (this.peek() !== c) throw new SyntaxError('expected ' + c); this.i++; }
      eatWord(w) { this.ws(); if (this.s.startsWith(w, this.i)) { this.i += w.length; return true; } return false; }
      ident() { this.ws(); IDENT.lastIndex = this.i; const m = IDENT.exec(this.s); if (!m) throw new SyntaxError('expected a name'); this.i = IDENT.lastIndex; return m[0]; }
      str() {
        const q = this.s[this.i++]; let out = '';
        for (;;) {
          const c = this.s[this.i++];
          if (c === undefined) throw new SyntaxError('unterminated string');
          if (c === q) return out;
          if (c !== BACKSLASH) { out += c; continue; }
          const e = this.s[this.i++];
          if (e === 'x') { out += String.fromCharCode(parseInt(this.s.substr(this.i, 2), 16)); this.i += 2; }
          else if (e === 'u') { out += String.fromCharCode(parseInt(this.s.substr(this.i, 4), 16)); this.i += 4; }
          else out += Object.prototype.hasOwnProperty.call(ESC, e) ? ESC[e] : e;
        }
      }
      value() {
        if (++this.depth > 64) throw new SyntaxError('too deep');
        try {
          const c = this.peek();
          if (c === '"' || c === "'") return this.str();
          if (c === '[') {
            this.i++; const a = [];
            if (this.peek() === ']') { this.i++; return a; }
            for (;;) { a.push(this.value()); if (this.peek() === ',') { this.i++; continue; } this.eat(']'); return a; }
          }
          if (c === '(') { this.i++; const v = this.value(); this.eat(')'); return v; }
          if (c === '{') {
            this.i++; const o = {};
            if (this.peek() === '}') { this.i++; return o; }
            for (;;) {
              const k = (this.peek() === '"' || this.peek() === "'") ? this.str() : this.ident();
              this.eat(':');
              const v = this.value();
              if (k === '__proto__') { if (v === null || typeof v === 'object') Object.setPrototypeOf(o, v); }   // as in a literal: sets the prototype, never an own key
              else Object.defineProperty(o, k, { value: v, enumerable: true, writable: true, configurable: true });
              if (this.peek() === ',') { this.i++; continue; } this.eat('}'); return o;
            }
          }
          if (this.eatWord('true')) return true;
          if (this.eatWord('false')) return false;
          if (this.eatWord('null')) return null;
          if (this.eatWord('undefined')) return undefined;
          this.ws(); NUM.lastIndex = this.i; const m = NUM.exec(this.s);
          if (!m) throw new SyntaxError('unsupported expression');
          this.i = NUM.lastIndex; return Number(m[0]);
        } finally { this.depth--; }
      }
      end() { this.ws(); if (this.i !== this.s.length) throw new SyntaxError('unexpected text'); }
    }
    /** A single literal (the answer to a call into Flash). */
    const literal = (src) => { const r = new Reader(src); const v = r.value(); r.end(); return v; };
    const xmlEsc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&apos;');
    const flashToXML = (v) => {      // the same output as the __flash__toXML that Flash defines, tests in the same order (property names are not escaped there)
      const t = typeof v;
      if (t === 'string') return '<string>' + xmlEsc(v) + '</string>';
      if (t === 'undefined') return '<undefined/>';
      if (t === 'number') return '<number>' + v + '</number>';
      if (v == null) return '<null/>';
      if (t === 'boolean') return v ? '<true/>' : '<false/>';
      if (v instanceof Date) return '<date>' + v.getTime() + '</date>';
      if (v instanceof Array) { let s = '<array>'; for (let i = 0; i < v.length; i++) s += '<property id="' + i + '">' + flashToXML(v[i]) + '</property>'; return s + '</array>'; }
      if (t === 'object') { let s = '<object>'; for (const p in v) s += '<property id="' + p + '">' + flashToXML(v[p]) + '</property>'; return s + '</object>'; }
      return '<null/>';
    };
    const HELPERS = /^\s*function __flash__arrayToXML\(obj\)/;
    const callShape = (src) => {
      const r = new Reader(src);
      r.eatWord('try'); r.eat('{');
      if (!r.eatWord('__flash__toXML')) throw new SyntaxError('unsupported script');
      r.eat('(');
      const path = [r.ident()];
      while (r.peek() === '.') { r.i++; path.push(r.ident()); }
      r.eat('(');
      const args = [];
      if (r.peek() === ')') r.i++; else for (;;) { args.push(r.value()); if (r.peek() === ',') { r.i++; continue; } r.eat(')'); break; }
      r.eat(')'); if (r.peek() === ';') r.i++; r.eat('}');
      if (!r.eatWord('catch')) throw new SyntaxError('unsupported script');
      r.eat('('); r.ident(); r.eat(')'); r.eat('{');
      if (r.peek() !== '"' || r.str() !== '<undefined/>') throw new SyntaxError('unsupported script');
      if (r.peek() === ';') r.i++; r.eat('}'); r.end();
      return { path, args };
    };
    /** Runs one ExecuteScript source; returns the script's value, or throws SyntaxError for a shape it does not know. */
    const exec = (src) => {
      if (HELPERS.test(src)) return undefined;
      const { path, args } = callShape(src);
      try {
        let self = window, fn = window;
        for (const k of path) { self = fn; fn = fn[k]; }
        if (typeof fn !== 'function') throw new TypeError(path.join('.') + ' is not a function');
        return flashToXML(fn.apply(path.length > 1 ? self : window, args));
      } catch { return '<undefined/>'; }
    };
    return { literal, exec };
  })();
  window.__flashRunnerBridge = { evalFree };       // also what the unit test (web/bridge-eval-test.mjs) drives
  const isEvalBlocked = (e) => !!e && (e.name === 'EvalError' || /unsafe-eval|Content Security Policy/i.test(String(e.message)));
  // Plain eval first (identical behaviour wherever it works); the eval-free path only when the page's CSP refused it.
  const evalScript = (src) => { try { return (0, eval)(src); } catch (e) { if (isEvalBlocked(e)) return evalFree.exec(src); throw e; } };
  const evalAnswer = (src) => { try { return (0, eval)(src); } catch (e) { if (isEvalBlocked(e)) return evalFree.literal(src); throw e; } };

  const run = (st, m) => {
    switch (m.op) {
      case 'exec': return enc(st, evalScript(m.script));                      // indirect eval: global scope, like NPN_Evaluate
      case 'get': return enc(st, Object(target(st, m.id))[m.name]);
      case 'set': target(st, m.id)[m.name] = dec(st, m.value); return { t: 'u' };
      case 'remove': delete target(st, m.id)[m.name]; return { t: 'u' };
      case 'has': return { t: 'b', v: m.name in Object(target(st, m.id)) };
      case 'hasMethod': return { t: 'b', v: typeof Object(target(st, m.id))[m.name] === 'function' };
      case 'call': {
        const o = target(st, m.id), fn = m.name == null ? o : o[m.name];
        if (typeof fn !== 'function') throw new TypeError(`${m.name ?? 'object'} is not a function`);
        return enc(st, fn.apply(m.name == null ? undefined : o, (m.args || []).map((a) => dec(st, a))));
      }
      case 'construct': return enc(st, Reflect.construct(target(st, m.id), (m.args || []).map((a) => dec(st, a))));
      case 'release': if (m.id > 2) st.handles.delete(m.id); return { t: 'u' };
      default: throw new TypeError('unknown request ' + m.op);
    }
  };

  window.addEventListener('message', (ev) => {
    const d = ev.data;
    if (!d || typeof d !== 'object') return;
    if (d.flashRunner === 'page') {
      const f = ownFrame(ev.source); if (!f) return;
      const st = stateOf(f);
      let reply;
      try { reply = { ok: true, value: run(st, d.msg) }; } catch (e) { reply = { ok: false, error: describe(e) }; }
      if (d.rid) ev.source.postMessage({ flashRunner: 'page-reply', rid: d.rid, reply }, st.origin);
    } else if (d.flashRunner === 'started') {
      const f = ownFrame(ev.source); if (f) stateOf(f).resolveStarted();
    } else if (d.flashRunner === 'invoke-reply') {
      const p = pendingInvoke.get(d.rid);
      if (p && ownFrame(ev.source) === p.frame) { pendingInvoke.delete(d.rid); clearTimeout(p.timer); p.resolve(d); }
    }
  });

  // ---- page -> Flash ---------------------------------------------------------------------------------------------
  const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&apos;');
  const toXML = (v) => {
    const t = typeof v;
    if (t === 'string') return '<string>' + esc(v) + '</string>';
    if (t === 'undefined') return '<undefined/>';
    if (t === 'number') return '<number>' + v + '</number>';
    if (v === null) return '<null/>';
    if (t === 'boolean') return v ? '<true/>' : '<false/>';
    if (v instanceof Date) return '<date>' + v.getTime() + '</date>';
    if (Array.isArray(v)) { let s = '<array>'; for (let i = 0; i < v.length; i++) s += '<property id="' + i + '">' + toXML(v[i]) + '</property>'; return s + '</array>'; }
    if (t === 'object') { let s = '<object>'; for (const k in v) s += '<property id="' + esc(k) + '">' + toXML(v[k]) + '</property>'; return s + '</object>'; }
    return '<null/>';
  };

  const callFlash = (f, name, args, timeoutMs = 30000) => {
    const st = stateOf(f);
    return Promise.race([st.started, new Promise((r) => setTimeout(r, 60000))]).then(() => new Promise((resolve, reject) => {
      const id = ++rid;
      const timer = setTimeout(() => { pendingInvoke.delete(id); reject(new Error('Flash did not answer')); }, timeoutMs);
      pendingInvoke.set(id, { frame: f, timer, resolve });
      f.contentWindow.postMessage({ flashRunner: 'invoke', rid: id, name, args }, st.origin);
    }));
  };

  // Classic scriptable methods of the Flash plugin object (take and return plain values).
  const CLASSIC = new Set(['SetVariable', 'GetVariable', 'Play', 'StopPlay', 'Rewind', 'Back', 'Forward', 'GotoFrame', 'IsPlaying', 'LoadMovie', 'PercentLoaded', 'FlashVersion', 'FrameLoaded',
    'TotalFrames', 'CurrentFrame', 'SetZoomRect', 'Zoom', 'Pan', 'TCallFrame', 'TCallLabel', 'TCurrentFrame', 'TCurrentLabel', 'TGetProperty', 'TGetPropertyAsNumber', 'TGetPropertyNum',
    'TGotoFrame', 'TGotoLabel', 'TPlay', 'TSetProperty', 'TStopPlay']);

  const forwarder = (f, name) => function (...args) {
    if (CLASSIC.has(name)) {
      return callFlash(f, name, args.map((a) => (typeof a === 'object' || typeof a === 'function' ? String(a) : a))).then((r) => { if (!r.ok) throw new Error(r.exception || 'call failed'); return r.value; });
    }
    const xml = '<invoke name="' + esc(name) + '" returntype="javascript"><arguments>' + args.map(toXML).join('') + '</arguments></invoke>';
    return callFlash(f, 'CallFunction', [xml]).then((r) => {
      if (!r.ok) throw new Error(r.exception || 'call failed');
      return typeof r.value === 'string' && r.value ? evalAnswer(r.value) : undefined;     // Flash answers with a JS expression
    });
  };

  // Names that must never turn into Flash calls (promise/JSON protocol probes, framework expandos, event handlers).
  const NOT_A_CALLBACK = /^(?:then|catch|finally|toJSON|length|item|namedItem|constructor|prototype|jquery.*|webkit.*|moz.*|on[a-z].*)$/i;
  const NAME_OK = /^[A-Za-z][A-Za-z0-9_$]*$/;

  const install = (f) => {
    if (f.__flashRunnerBridge) return;
    const proto = Object.getPrototypeOf(f), cache = new Map();
    const dyn = new Proxy(proto, {
      get(t, prop, receiver) {
        if (typeof prop !== 'string' || prop in t) return Reflect.get(t, prop, receiver);
        if (!NAME_OK.test(prop) || NOT_A_CALLBACK.test(prop)) return undefined;
        let fn = cache.get(prop); if (!fn) { fn = forwarder(f, prop); cache.set(prop, fn); }
        return fn;
      },
    });
    try {
      Object.defineProperty(f, '__flashRunnerBridge', { value: true });
      Object.setPrototypeOf(f, dyn);
      for (const n of CLASSIC) if (!(n in proto)) Object.defineProperty(f, n, { value: forwarder(f, n), configurable: true, writable: true });
      // swfobject and friends insist on <object>; make the element say so.
      Object.defineProperty(f, 'nodeName', { value: 'OBJECT', configurable: true });
    } catch { /* frozen/exotic element: leave the iframe as it is */ }
  };

  const sweep = (root) => { for (const f of root.querySelectorAll ? root.querySelectorAll('iframe[data-flash-runner="1"]') : []) install(f); if (root.localName === 'iframe' && root.getAttribute?.('data-flash-runner') === '1') install(root); };
  const mo = new MutationObserver((muts) => { for (const m of muts) for (const n of m.addedNodes) if (n.nodeType === 1) sweep(n); });
  mo.observe(document, { childList: true, subtree: true });
  sweep(document);
})();
