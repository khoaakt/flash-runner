// Storage for the Flash runtime the user installs, and for the settings,, shared by the options page and the player.
// The runtime (libpepflashplayer.so) lives only in this browser profile's IndexedDB — it is never bundled
// with the extension and never leaves the machine.

const DB = 'flash-runner-ext';
const STORE = 'kv';

function open() {
  return new Promise((res, rej) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE);
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}
const tx = async (mode, fn) => {
  const db = await open();
  return new Promise((res, rej) => {
    const t = db.transaction(STORE, mode); const out = fn(t.objectStore(STORE));
    t.oncomplete = () => res(out && 'result' in out ? out.result : undefined); t.onerror = () => rej(t.error);
  });
};

export const getRuntime = () => tx('readonly', (s) => s.get('runtime'));
export const clearRuntime = () => tx('readwrite', (s) => s.delete('runtime'));

const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');

/** Sanity-check that `bytes` looks like a 32-bit x86 PPAPI plugin. Returns { ok, problems[], exports }. */
export function inspectRuntime(bytes) {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const problems = [];
  if (u8.length < 1 << 20) problems.push('file is too small to be a Flash runtime');
  if (!(u8[0] === 0x7f && u8[1] === 0x45 && u8[2] === 0x4c && u8[3] === 0x46)) problems.push('not an ELF file (expected libpepflashplayer.so)');
  else {
    if (u8[4] !== 1) problems.push('not a 32-bit ELF: only the ia32 (i386) PPAPI build is supported');
    const machine = u8[18] | (u8[19] << 8);
    if (machine !== 3) problems.push(`wrong architecture (e_machine=${machine}); need Intel 80386`);
  }
  const need = ['PPP_InitializeModule', 'PPP_GetInterface'];
  const found = need.filter((n) => indexOfAscii(u8, n) >= 0);
  if (found.length < need.length) problems.push('does not export PPP_InitializeModule / PPP_GetInterface — not a PPAPI plugin');
  return { ok: problems.length === 0, problems, exports: found };
}

function indexOfAscii(u8, s) {
  const n = s.length;
  outer: for (let i = 0, e = u8.length - n; i <= e; i++) {
    if (u8[i] !== s.charCodeAt(0)) continue;
    for (let j = 1; j < n; j++) if (u8[i + j] !== s.charCodeAt(j)) continue outer;
    return i;
  }
  return -1;
}

const u16 = (u8, o) => u8[o] | (u8[o + 1] << 8);
const u32 = (u8, o) => (u8[o] | (u8[o + 1] << 8) | (u8[o + 2] << 16) | (u8[o + 3] << 24)) >>> 0;
export const isZip = (u8) => u8.length > 4 && u8[0] === 0x50 && u8[1] === 0x4b && u8[2] === 3 && u8[3] === 4;

/** Find the PPAPI plugin inside a .zip archive (what mirrors usually distribute): returns { name, bytes }. */
export async function extractRuntimeFromZip(bytes) {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let eocd = -1;
  for (let i = u8.length - 22; i >= Math.max(0, u8.length - 65557); i--) if (u32(u8, i) === 0x06054b50) { eocd = i; break; }
  if (eocd < 0) throw new Error('not a valid zip archive');
  const count = u16(u8, eocd + 10);
  let p = u32(u8, eocd + 16);
  const entries = [];
  for (let i = 0; i < count; i++) {
    if (u32(u8, p) !== 0x02014b50) throw new Error('corrupt zip directory');
    const method = u16(u8, p + 10), csize = u32(u8, p + 20), usize = u32(u8, p + 24), nlen = u16(u8, p + 28), elen = u16(u8, p + 30), clen = u16(u8, p + 32), off = u32(u8, p + 42);
    const name = new TextDecoder().decode(u8.subarray(p + 46, p + 46 + nlen));
    entries.push({ name, method, csize, usize, off });
    p += 46 + nlen + elen + clen;
  }
  const candidates = entries.filter((e) => /\.so$/i.test(e.name) && e.usize >= 1 << 20 && e.usize < 256 << 20)
    .sort((a, b) => (/pepflash/i.test(b.name) ? 1 : 0) - (/pepflash/i.test(a.name) ? 1 : 0) || (/i386|32/i.test(b.name) ? 1 : 0) - (/i386|32/i.test(a.name) ? 1 : 0));
  const problems = [];
  for (const e of candidates) {
    if (u32(u8, e.off) !== 0x04034b50) continue;
    const start = e.off + 30 + u16(u8, e.off + 26) + u16(u8, e.off + 28);
    const raw = u8.subarray(start, start + e.csize);
    let data;
    if (e.method === 0) data = raw.slice();
    else if (e.method === 8) data = new Uint8Array(await new Response(new Blob([raw]).stream().pipeThrough(new DecompressionStream('deflate-raw'))).arrayBuffer());
    else { problems.push(`${e.name}: unsupported compression ${e.method}`); continue; }
    const info = inspectRuntime(data);
    if (info.ok) return { name: e.name.split('/').pop(), bytes: data };
    problems.push(`${e.name}: ${info.problems.join(', ')}`);
  }
  throw new Error(candidates.length ? 'no usable 32-bit PPAPI plugin in the archive (' + problems.join('; ') + ')' : 'the archive contains no .so file');
}

/** Accepts either the .so itself or a .zip that contains it. */
export async function setRuntimeFromFileBytes(bytes, name) {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  if (isZip(u8)) { const r = await extractRuntimeFromZip(u8); return setRuntimeFromBytes(r.bytes, r.name); }
  return setRuntimeFromBytes(u8, name);
}

export async function setRuntimeFromBytes(bytes, name = 'libpepflashplayer.so') {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const info = inspectRuntime(u8);
  if (!info.ok) throw new Error(info.problems.join('; '));
  const sha256 = hex(await crypto.subtle.digest('SHA-256', u8));
  const rec = { bytes: u8.slice().buffer, name, size: u8.length, sha256, time: Date.now() };
  await tx('readwrite', (s) => s.put(rec, 'runtime'));
  return { name, size: rec.size, sha256 };
}

// ---- Flash runtimes shipped inside the extension (assets/runtimes/runtimes.json, written by tools/build-extension.mjs) ----
// A runtime the user installs from a file (IndexedDB, getRuntime) wins; otherwise the built-in version chosen by `runtimeVersion` is used.
export const DEFAULT_RUNTIME = '32.0.0.363';
export async function getBundledRuntimes() {
  try { const r = await fetch(chrome.runtime.getURL('assets/runtimes/runtimes.json')); return r.ok ? await r.json() : []; } catch { return []; }
}
/** The runtime the player will run: { bytes, name, size, sha256, bundled, id? } or undefined when there is none. */
export async function getActiveRuntime() {
  const custom = await getRuntime();
  if (custom) return { ...custom, bundled: false };
  const { runtimeVersion = DEFAULT_RUNTIME } = await chrome.storage.local.get('runtimeVersion');
  const list = await getBundledRuntimes();
  const pick = list.find((r) => r.id === runtimeVersion) || list.find((r) => r.id === DEFAULT_RUNTIME) || list[0];
  if (!pick) return undefined;
  try {
    const res = await fetch(chrome.runtime.getURL('assets/runtimes/' + pick.file));
    return res.ok ? { bytes: await res.arrayBuffer(), name: pick.name, size: pick.size, sha256: pick.sha256, bundled: true, id: pick.id } : undefined;
  } catch { return undefined; }
}

// Guest RAM per movie. The emulator is a wasm32 module: its linear memory is capped at 4 GiB and the guest RAM is one allocation that
// must stay below 2 GiB, so 2040 MB is the largest size that can exist; untouched pages cost nothing. Not user-configurable.
export const MEM_MB = 2040;
// `log`: print the player's messages (loading, JIT/LIR status, guest console) to the browser console. Off by default, and the
// only switch: the diagnostics settings `diag` and `fpsLog` (kept in storage, no longer shown in the options page) produce
// nothing, and cost nothing, unless it is on.
export const DEFAULTS = { enabled: true, openSwfLinks: true, autoplayMax: 1, clickToPlay: false, gpu3d: true, quality: 'auto', netCache: true, runtimeVersion: DEFAULT_RUNTIME, disabledSites: [], mediaSites: [], jitIntrinsics: true, jitTrustLoads: true, diag: false, fpsLog: false, log: false };
/** Whether the player may print to the console: the "Debug log" setting and nothing else. The diagnostics settings (`diag`, `fpsLog`) only apply while it is on. */
export function logEnabled(settings) { return !!settings.log; }
export async function getSettings() {
  const s = { ...DEFAULTS, ...(await chrome.storage.local.get(Object.keys(DEFAULTS))) };
  s.memMB = MEM_MB;                                          // a size stored by an earlier version is ignored
  return s;
}
export async function setSettings(patch) { await chrome.storage.local.set(patch); }
