// Connects the emulator worker's socket requests to the native messaging helper (native/helper, Go).
// One helper process (= one native port) per player; created on the first socket request.
const HOST = 'com.flashrunner.sockets';
// Chrome's lastError texts for a host that is not (correctly) registered; "exited" / "communicating" are the helper dying, not absent.
const NOT_INSTALLED = /not found|forbidden|access|failed to start|no such native application/i;
// Firefox refuses connectNative from an add-on frame embedded in a page, so there the background script owns the native port (background.js).
const viaBackground = () => !!chrome.runtime.getManifest?.().browser_specific_settings;
const RETRY_MS = 5000;                               // after "not installed": answer instantly, look again at most this often

const b64 = (u8) => { let s = ''; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000)); return btoa(s); };
const unb64 = (s) => { const bin = atob(s), u8 = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i); return u8; };

export class NativeSockets {
  /** deliver(msg): hands replies/events to the worker (FlashPlayer.sockIn). onProblem(text): helper missing etc.
   *  onState('missing' | 'ok'): fires once when a movie wants a socket and the helper is not installed, and again when it turns up. */
  constructor(deliver, onProblem, log, onState) { this.deliver = deliver; this.onProblem = onProblem || (() => {}); this.log = log || null; this.onState = onState || null; this.missing = false; this.missingAt = 0; this.notified = false; this.port = null; this.pending = new Set(); this.open = new Set(); this.reqs = new Map(); }

  ensure() {
    if (this.port) return true;
    if (this.missing && Date.now() - this.missingAt < RETRY_MS) return false;
    try { this.port = viaBackground() ? chrome.runtime.connect({ name: 'flash-runner:native-sockets' }) : chrome.runtime.connectNative(HOST); } catch (e) { this.gone(String(e && e.message || e)); return false; }
    let closedBy = null;                              // the relay's explanation, sent just before it disconnects
    this.port.onMessage.addListener((m) => { if (m && m.relayClosed !== undefined) closedBy = m.relayClosed; else this.fromHelper(m); });
    this.port.onDisconnect.addListener((p) => {
      const why = closedBy || p?.error?.message || chrome.runtime.lastError?.message || 'helper exited';
      this.port = null;
      for (const rid of this.pending) this.deliver({ rid, ok: false, code: 'EHELPER', message: why });
      this.pending.clear();
      for (const id of this.open) this.deliver({ t: 'close', id, error: 'ECONNRESET' });
      this.open.clear();
      this.gone(why);
    });
    return true;
  }

  /** connectNative never throws for an absent host: the port just disconnects, with the reason in lastError. */
  gone(why) {
    const absent = NOT_INSTALLED.test(why);
    this.log?.('[sock] helper: ' + why);
    this.onProblem(absent ? 'Flash sockets need the helper: install it from the extension settings (' + why + ')' : why);
    if (!absent) return;
    this.missing = true; this.missingAt = Date.now();
    if (!this.notified) { this.notified = true; this.onState?.('missing'); }
  }

  /** A request from the worker: { t, rid?, ... , bytes? }. */
  send(msg) {
    if (!this.ensure()) { if (msg.rid) this.deliver({ rid: msg.rid, ok: false, code: 'EHELPER', message: 'helper not available' }); return; }
    const out = { ...msg };
    if (msg.bytes) { out.data = b64(msg.bytes); delete out.bytes; }
    if (msg.rid) { this.pending.add(msg.rid); if (this.log && msg.t === 'connect') this.reqs.set(msg.rid, `${msg.host}:${msg.port}`); }
    if (msg.t === 'close') this.open.delete(msg.id);
    try { this.port.postMessage(out); } catch (e) { this.pending.delete(msg.rid); this.deliver({ rid: msg.rid, ok: false, code: 'EHELPER', message: String(e) }); }
  }

  fromHelper(m) {
    if (this.missing) { this.missing = false; if (this.notified) { this.notified = false; this.onState?.('ok'); } }
    if (this.log && m.rid && this.reqs.has(m.rid)) { this.log(`[sock] connect ${this.reqs.get(m.rid)} -> ${m.ok ? 'ok' : (m.code || 'failed') + ' ' + (m.message || '')}`); this.reqs.delete(m.rid); }
    if (m.rid) { this.pending.delete(m.rid); if (m.ok && m.id != null && m.t === undefined && m.remote) this.open.add(m.id); }
    if (m.t === 'close') this.open.delete(m.id);
    if (m.data !== undefined) { m.bytes = unb64(m.data); delete m.data; }
    this.deliver(m);
  }

  close() { try { this.port?.disconnect(); } catch { /* already gone */ } this.port = null; }
}
