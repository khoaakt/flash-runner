// Persistence for Flash local storage (Shared Objects, settings): the guest's module-local directory is
// mirrored into IndexedDB. `load()` runs before the instance is created, `save(files)` after changes.
export class IdbPersist {
  static async open(name = 'flash-runner-storage') {
    if (typeof indexedDB === 'undefined') return null;
    const db = await new Promise((res, rej) => {
      const req = indexedDB.open(name, 1);
      req.onupgradeneeded = () => req.result.createObjectStore('files');
      req.onsuccess = () => res(req.result);
      req.onerror = () => rej(req.error);
    });
    return new IdbPersist(db);
  }
  constructor(db) { this.db = db; this.known = new Set(); }
  tx(mode) { return this.db.transaction('files', mode).objectStore('files'); }
  async load() {
    const store = this.tx('readonly');
    const keys = await new Promise((res, rej) => { const r = store.getAllKeys(); r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
    const vals = await new Promise((res, rej) => { const r = this.tx('readonly').getAll(); r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
    keys.forEach((k) => this.known.add(k));
    return keys.map((k, i) => [k, new Uint8Array(vals[i])]);
  }
  /** Replace the stored set with `files` ([path, bytes][]). */
  save(files) {
    const store = this.tx('readwrite');
    const now = new Set();
    for (const [path, bytes] of files) { store.put(bytes.slice().buffer, path); now.add(path); }
    for (const k of this.known) if (!now.has(k)) store.delete(k);
    this.known = now;
  }
}
