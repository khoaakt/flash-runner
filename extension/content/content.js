// Isolated-world content script: finds Flash <embed>/<object> elements and swaps each for an extension
// iframe (player/player.html) that runs the emulated Flash runtime.
(() => {
  if (window.__flashRunnerContent) return;
  window.__flashRunnerContent = true;

  const FLASH_TYPE = /^application\/(x-shockwave-flash|futuresplash)\b/i;
  const CLSID = /d27cdb6e-ae6d-11cf-96b8-444553540000/i;
  const PLAYER_URL = chrome.runtime.getURL('player/player.html');
  const PLAYER_ORIGIN = new URL(PLAYER_URL).origin;
  const SKIP_ATTRS = new Set(['width', 'height', 'style', 'class', 'src', 'data', 'type', 'classid', 'codebase', 'pluginspage', 'hidden']);
  const players = new Map();          // iframe -> { config, started }
  let settings = { enabled: true, disabledSites: [], autoplayMax: 1, clickToPlay: false };
  let autoplayed = 0;

  const attr = (el, n) => (el.getAttribute(n) || '').trim();
  const isSwfUrl = (u) => /\.(swf|spl)(?:[?#].*)?$/i.test(u || '');

  function movieUrl(el) {
    if (el.localName === 'embed') return attr(el, 'src');
    let u = attr(el, 'data');
    if (!u) for (const p of el.querySelectorAll(':scope > param')) if (/^(movie|src)$/i.test(p.name)) { u = p.value; break; }
    if (!u) { const e = el.querySelector('embed'); if (e) u = attr(e, 'src'); }
    return u;
  }

  function isFlash(el) {
    if (el.__flashRunner) return false;
    if (el.localName === 'embed') {
      if (el.closest('object')?.__flashRunner) return false;                // inner <embed> of an object we already took
      return FLASH_TYPE.test(attr(el, 'type')) || isSwfUrl(attr(el, 'src'));
    }
    if (el.localName !== 'object') return false;
    if (FLASH_TYPE.test(attr(el, 'type')) || CLSID.test(attr(el, 'classid'))) return true;
    return isSwfUrl(movieUrl(el));
  }

  function collectAttrs(el) {
    const a = {};
    const take = (e) => { for (const x of e.attributes) if (!SKIP_ATTRS.has(x.name.toLowerCase())) a[x.name] = x.value; };
    take(el);
    if (el.localName === 'object') {
      for (const p of el.querySelectorAll(':scope > param')) if (p.name) a[p.name] = p.value;
      const e = el.querySelector('embed'); if (e) take(e);                // nested <embed> often carries extra flashvars
    }
    return a;
  }

  function sizeOf(el) {
    const r = el.getBoundingClientRect();
    const dim = (n, fallback) => { const v = attr(el, n); return v || (fallback > 0 ? String(Math.round(fallback)) : ''); };
    return { width: dim('width', r.width) || '550', height: dim('height', r.height) || '400' };
  }

  function replace(el) {
    const src = movieUrl(el);
    const { width, height } = sizeOf(el);
    el.__flashRunner = true;
    const f = document.createElement('iframe');
    f.src = PLAYER_URL;
    f.allow = 'autoplay; fullscreen; clipboard-read; clipboard-write; gamepad';
    f.setAttribute('scrolling', 'no');
    f.setAttribute('frameborder', '0');
    f.setAttribute('data-flash-runner', '1');
    for (const n of ['id', 'class', 'name', 'title']) if (el.hasAttribute(n)) f.setAttribute(n, el.getAttribute(n));
    f.setAttribute('width', width); f.setAttribute('height', height);
    f.style.cssText = (el.getAttribute('style') || '') + ';border:0;background:transparent;color-scheme:normal';
    f.style.display = el.style.display || 'inline-block';
    const cfg = {
      flashRunner: 'config',
      src: src ? new URL(src, document.baseURI).href : '',
      attrs: collectAttrs(el),
      pageUrl: location.href,
      baseUrl: document.baseURI,
      width, height,
      autoplay: !settings.clickToPlay && autoplayed < settings.autoplayMax,
    };
    if (cfg.autoplay) autoplayed++;
    players.set(f, cfg);
    el.replaceWith(f);
    // A page CSP (frame-src) can refuse the extension iframe. If the player never says hello, fall back to a
    // placeholder that opens the movie in the extension's own viewer tab instead of leaving a blank hole.
    f.__flashRunnerTimer = setTimeout(() => { if (!f.__flashRunnerReady && f.isConnected) placeholder(f, cfg); }, 3000);
  }

  function placeholder(f, cfg) {
    const box = document.createElement('div');
    box.setAttribute('data-flash-runner', 'placeholder');
    box.style.cssText = `width:${/%$/.test(cfg.width) ? cfg.width : cfg.width + 'px'};height:${/%$/.test(cfg.height) ? cfg.height : cfg.height + 'px'};display:inline-flex;flex-direction:column;align-items:center;justify-content:center;gap:8px;background:#202124;color:#e8eaed;font:13px/1.4 system-ui,sans-serif;text-align:center;box-sizing:border-box;padding:8px;border-radius:4px`;
    const b = document.createElement('button');
    b.textContent = 'Play Flash in viewer';
    b.style.cssText = 'font:inherit;padding:7px 14px;border-radius:6px;border:0;background:#1a73e8;color:#fff;cursor:pointer';
    b.onclick = () => window.open(chrome.runtime.getURL('player/viewer.html?src=') + encodeURIComponent(cfg.src), '_blank');
    box.append('This page\u2019s security policy blocks inline Flash.', b);
    f.replaceWith(box);
    players.delete(f);
  }

  window.addEventListener('message', (e) => {
    if (e.origin !== PLAYER_ORIGIN || !e.data || e.data.flashRunner !== 'ready') return;
    for (const [f, cfg] of players) if (f.contentWindow === e.source) { f.__flashRunnerReady = true; clearTimeout(f.__flashRunnerTimer); e.source.postMessage(cfg, PLAYER_ORIGIN); return; }
  });

  function scan(root) {
    if (!settings.enabled) return;
    const list = root.querySelectorAll ? root.querySelectorAll('object, embed') : [];
    for (const el of list) if (isFlash(el)) replace(el);
    if (root.nodeType === 1 && (root.localName === 'object' || root.localName === 'embed') && isFlash(root)) replace(root);
  }

  const siteDisabled = () => settings.disabledSites.some((h) => location.hostname === h || location.hostname.endsWith('.' + h));

  chrome.storage.local.get({ enabled: true, disabledSites: [], autoplayMax: 1, clickToPlay: false }).then((s) => {
    settings = s;
    if (!settings.enabled || siteDisabled()) return;
    const mo = new MutationObserver((muts) => {
      for (const m of muts) for (const n of m.addedNodes) {
        if (n.nodeType !== 1) continue;
        scan(n);
        const owner = n.localName === 'param' || n.localName === 'embed' ? n.closest?.('object') : null;   // late <param name=movie>
        if (owner && isFlash(owner)) replace(owner);
      }
      for (const m of muts) if (m.type === 'attributes') scan(m.target);
    });
    mo.observe(document, { childList: true, subtree: true, attributes: true, attributeFilter: ['type', 'src', 'data'] });
    scan(document);
    document.addEventListener('DOMContentLoaded', () => scan(document), { once: true });
  });
})();
