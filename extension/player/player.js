// Runs inside the iframe that replaces a Flash <embed>/<object>: asks the content script for the
// movie description, loads the user's runtime from IndexedDB and starts the emulated player.
import { FlashPlayer } from '../web/loader.js';
import { getActiveRuntime, getSettings, logEnabled } from '../options/runtime-store.js';
import { NativeSockets } from './native-sockets.js';
import { helperNotice } from './helper-notice.js';

const root = document.getElementById('root');
// Anything that escapes must leave a readable trace in the console.
const describe = (r) => { try { return r instanceof Error ? (r.stack || r.message) : r && typeof r === 'object' ? `${r.constructor?.name || 'object'} ${JSON.stringify(r, ['type', 'message', 'filename', 'lineno', 'reason']) || String(r)}` : String(r); } catch { return String(r); } };
addEventListener('unhandledrejection', (e) => console.error('[flash] unhandled rejection:', describe(e.reason)));
addEventListener('error', (e) => console.error('[flash] error:', e.message, e.filename, e.lineno));
const inFrame = window.parent !== window;
let current = null;                                  // the running FlashPlayer, for page -> Flash calls

// Scripting bridge to the embedding page (see content/bridge-main.js): the worker's ExecuteScript / window-object
// requests travel worker -> here -> parent page (MAIN world) and back.
// Whether the movie may run scripts in the embedding page. Flash enforces allowScriptAccess itself, but a compromised (old!) runtime
// must not be able to bypass it, so the host applies the same rule before forwarding anything: `always` yes, `never` no, default
// `sameDomain` = the movie must come from the page's own domain.
function scriptAccessAllowed(cfg) {
  const attr = Object.entries(cfg.attrs || {}).find(([k]) => k.toLowerCase() === 'allowscriptaccess');
  const v = String(attr?.[1] ?? '').trim().toLowerCase();
  if (v === 'always') return true;
  if (v === 'never') return false;
  try { return new URL(cfg.src).hostname === new URL(cfg.pageUrl).hostname; } catch { return false; }
}
const pageWaiters = new Map();
let pageSeq = 0;
function pageBridge(msg, wantReply) {
  if (!wantReply) { window.parent.postMessage({ flashRunner: 'page', msg }, '*'); return undefined; }
  return new Promise((resolve) => { const rid = ++pageSeq; pageWaiters.set(rid, resolve); window.parent.postMessage({ flashRunner: 'page', rid, msg }, '*'); });
}
if (inFrame) {
  window.addEventListener('message', (e) => {
    const d = e.data;
    if (e.source !== window.parent || !d || typeof d !== 'object') return;
    if (d.flashRunner === 'page-reply') { const w = pageWaiters.get(d.rid); pageWaiters.delete(d.rid); w?.(d.reply); }
    else if (d.flashRunner === 'invoke') {
      const reply = (r) => window.parent.postMessage({ flashRunner: 'invoke-reply', rid: d.rid, ...r }, '*');
      if (!current) { reply({ ok: false, exception: 'Flash is not running' }); return; }
      current.invoke(String(d.name), Array.isArray(d.args) ? d.args : []).then((r) => reply({ ok: r.ok, value: r.value, exception: r.exception }), (err) => reply({ ok: false, exception: String(err) }));
    }
  });
}

function message(html, button) {
  root.innerHTML = `<div class="msg">${html}</div>`;
  if (button) { const b = document.createElement('button'); b.textContent = button.label; b.onclick = button.onClick; root.firstChild.append(b); }
}

// Pages often embed a tiny helper movie (300x150 is the default size of an <object>) next to the real game: do not give it the game's memory.
const tiny = (cfg) => { const w = parseInt(cfg.width, 10), h = parseInt(cfg.height, 10); return w > 0 && h > 0 && !/%/.test(String(cfg.width) + String(cfg.height)) && w * h <= 70000; };

export async function startPlayer(cfg, host = root) {
  const settings = await getSettings();
  const runtime = await getActiveRuntime();
  if (!runtime) {
    message('<b>Flash runtime missing</b><small>This build of the extension does not contain a Flash runtime. Choose a libpepflashplayer.so file in the settings.</small>', { label: 'Open settings', onClick: () => chrome.runtime.openOptionsPage() });
    return null;
  }
  const el = document.createElement('embed');
  if (settings.quality && settings.quality !== 'auto') cfg.attrs = { ...(cfg.attrs || {}), quality: settings.quality };   // initial stage quality (movies that set stage.quality themselves still win)
  for (const [k, v] of Object.entries(cfg.attrs || {})) { try { el.setAttribute(k, v); } catch { /* invalid attribute name from the page */ } }
  if (cfg.src) el.setAttribute('src', cfg.src);
  el.setAttribute('type', 'application/x-shockwave-flash');
  const rect = { w: Math.max(1, window.innerWidth), h: Math.max(1, window.innerHeight) };
  el.setAttribute('width', rect.w); el.setAttribute('height', rect.h);
  host.innerHTML = '';
  const socks = new NativeSockets((m) => player.sockIn(m), (why) => console.warn('[flash] sockets:', why), logEnabled(settings) && settings.diag ? (t) => console.log('[flash]', t) : null, (state) => helperNotice[state === 'missing' ? 'missing' : 'found']());
  const player = new FlashPlayer(el, {
    host, fill: true, statusBar: false,
    pluginBytes: runtime.bytes.slice(0),                   // transferred to the worker
    assets: { wasm: chrome.runtime.getURL('assets/emu.wasm'), rootfs: chrome.runtime.getURL('assets/rootfs.tar'), host: chrome.runtime.getURL('assets/pepper-host') },
    memMB: tiny(cfg) ? Math.min(settings.memMB, 512) : settings.memMB, gpu3d: settings.gpu3d, jitIntrinsics: settings.jitIntrinsics, jitTrustLoads: settings.jitTrustLoads, netCache: settings.netCache, profile: logEnabled(settings) && settings.diag, log: logEnabled(settings), fpsLog: logEnabled(settings) && settings.fpsLog, language: navigator.language,
    sockBridge: (msg) => socks.send(msg),
    pageUrl: cfg.pageUrl, baseUrl: cfg.baseUrl, pageBridge: inFrame && scriptAccessAllowed(cfg) ? pageBridge : null,
    onExit: (m) => { if (m.code !== 0) message(`<b>Flash stopped (code ${m.code})</b><small>${m.code === 134 ? 'The movie ran out of memory or aborted. Reload the page to try again.' : 'The movie crashed.'}</small>`, { label: 'Restart', onClick: () => startPlayer(cfg) }); },
    onError: (m) => message(`<b>Could not start Flash</b><small>${String(m.message).split('\n')[0].replace(/[<>&]/g, '')}</small>`, { label: 'Retry', onClick: () => startPlayer(cfg) }),
    onNavigate: (url, target) => { try { window.open(url, target && target !== '_self' ? target : '_blank'); } catch { /* popup blocked */ } },
  });
  const loading = document.createElement('div'); loading.className = 'msg'; loading.style.background = 'transparent'; loading.style.pointerEvents = 'none';
  loading.innerHTML = '<small>Starting Flash…</small>'; host.append(loading);
  const origReady = player.opts.onReady;
  player.opts.onReady = (m) => { loading.remove(); if (inFrame) window.parent.postMessage({ flashRunner: 'started' }, '*'); origReady?.(m); };
  current = player;
  if (inFrame && cfg.pageUrl) await Promise.race([chrome.runtime.sendMessage({ type: 'flash-runner:page-url', pageUrl: cfg.pageUrl }).catch(() => null), new Promise((r) => setTimeout(r, 1500))]);
  await player.start();
  let view = { visible: true, pageVisible: true };
  const push = () => player.resize(Math.max(1, window.innerWidth), Math.max(1, window.innerHeight), view);
  new ResizeObserver(push).observe(document.documentElement);
  document.addEventListener('visibilitychange', () => { view = { visible: !document.hidden, pageVisible: !document.hidden }; push(); });
  return player;
}

function overlay(cfg) {
  const b = document.createElement('div'); b.className = 'msg';
  const name = (cfg.src || 'Flash content').split('/').pop().split('?')[0] || 'Flash content';
  b.innerHTML = `<b>${name.replace(/[<>&]/g, '')}</b>`;
  const play = document.createElement('button'); play.className = 'play'; play.textContent = '▶'; play.title = 'Run Flash content';
  play.onclick = () => { root.innerHTML = ''; startPlayer(cfg); };
  b.append(play); root.append(b);
}

if (inFrame) {
  window.addEventListener('message', (e) => {
    if (e.source !== window.parent || !e.data || e.data.flashRunner !== 'config') return;
    if (e.data.autoplay) startPlayer(e.data); else overlay(e.data);
  }, { once: true });
  window.parent.postMessage({ flashRunner: 'ready' }, '*');
}
