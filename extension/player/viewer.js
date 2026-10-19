// Full-tab viewer used when a .swf URL is opened directly (the background redirect points here).
import { FlashPlayer } from '../web/loader.js';
import { getActiveRuntime, getSettings, logEnabled } from '../options/runtime-store.js';
import { NativeSockets } from './native-sockets.js';
import { helperNotice } from './helper-notice.js';

const root = document.getElementById('root');
const raw = location.search.startsWith('?src=') ? location.search.slice(5) : '';
let src = raw;
try { src = decodeURIComponent(raw); } catch { /* leave as is */ }
try { src = new URL(src).href; } catch { src = ''; }
document.title = src ? decodeURIComponent(src.split('?')[0].split('/').pop() || 'Flash') : 'Flash';

const show = (html, button) => {
  root.innerHTML = `<div class="msg">${html}</div>`;
  if (button) { const b = document.createElement('button'); b.textContent = button.label; b.onclick = button.onClick; root.firstChild.append(b); }
};

let current = null, resizeObs = null;

/** No movie in the URL: let the user pick (or drop) a local .swf; it plays from a blob: URL. */
function picker() {
  root.innerHTML = '<div class="msg"><b>Play a Flash file</b><small>Drop a .swf file anywhere on this page, or choose one.</small></div>';
  const input = document.createElement('input'); input.type = 'file'; input.accept = '.swf,.spl,application/x-shockwave-flash'; input.hidden = true;
  const b = document.createElement('button'); b.textContent = 'Choose .swf file…'; b.onclick = () => input.click();
  input.onchange = () => { if (input.files[0]) playFile(input.files[0]); };
  root.firstChild.append(b, input);
}
function playFile(file) {
  if (current) { current.destroy(); current = null; }
  resizeObs?.disconnect();
  document.title = file.name;
  play(URL.createObjectURL(file));
}
addEventListener('dragover', (e) => { if ([...(e.dataTransfer?.types || [])].includes('Files')) e.preventDefault(); });
addEventListener('drop', (e) => { const f = [...(e.dataTransfer?.files || [])].find((x) => /\.(swf|spl)$/i.test(x.name)); if (f) { e.preventDefault(); playFile(f); } });

async function play(url) {
  root.innerHTML = '';
  const [settings, runtime] = await Promise.all([getSettings(), getActiveRuntime()]);
  if (!runtime) return show('<b>Flash runtime missing</b><small>This build of the extension does not contain a Flash runtime. Choose a libpepflashplayer.so file in the settings.</small>', { label: 'Open settings', onClick: () => chrome.runtime.openOptionsPage() });
  const el = document.createElement('embed');
  el.setAttribute('src', url); el.setAttribute('type', 'application/x-shockwave-flash');
  el.setAttribute('width', innerWidth); el.setAttribute('height', innerHeight);
  const socks = new NativeSockets((m) => player.sockIn(m), (why) => console.warn('[flash] sockets:', why), logEnabled(settings) && settings.diag ? (t) => console.log('[flash]', t) : null, (state) => helperNotice[state === 'missing' ? 'missing' : 'found']());
  const player = new FlashPlayer(el, {
    host: root, fill: true, statusBar: false, pluginBytes: runtime.bytes.slice(0),
    assets: { wasm: chrome.runtime.getURL('assets/emu.wasm'), rootfs: chrome.runtime.getURL('assets/rootfs.tar'), host: chrome.runtime.getURL('assets/pepper-host') },
    memMB: settings.memMB, gpu3d: settings.gpu3d, jitIntrinsics: settings.jitIntrinsics, jitTrustLoads: settings.jitTrustLoads, netCache: settings.netCache, profile: logEnabled(settings) && settings.diag, log: logEnabled(settings), language: navigator.language, sockBridge: (msg) => socks.send(msg),
    onError: (m) => show(`<b>Could not start Flash</b><small>${String(m.message).split('\n')[0].replace(/[<>&]/g, '')}</small>`),
  });
  current = player;
  await player.start();
  resizeObs = new ResizeObserver(() => player.resize(Math.max(1, innerWidth), Math.max(1, innerHeight)));
  resizeObs.observe(document.documentElement);
}

if (src) play(src); else picker();
