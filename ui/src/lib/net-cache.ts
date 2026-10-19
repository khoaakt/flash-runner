import { IdbStore } from '../../../web/net-cache.js';

export type CacheInfo = { files: number; bytes: number } | null;

/** Size of the download cache (null when IndexedDB is unavailable). */
export async function readCacheInfo(): Promise<CacheInfo> {
  try {
    const store = await IdbStore.open();
    if (!store) return null;
    const rows: [string, { size?: number }][] = await store.loadMeta();
    return { files: rows.length, bytes: rows.reduce((sum, [, meta]) => sum + (meta.size || 0), 0) };
  } catch {
    return null;
  }
}

export async function clearCache() {
  const store = await IdbStore.open();
  if (store) await store.clear();
}
