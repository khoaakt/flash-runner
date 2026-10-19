import { useCallback, useEffect, useState } from 'react';
import { DEFAULT_RUNTIME, getBundledRuntimes, getRuntime, getSettings, setSettings } from '../../../extension/options/runtime-store.js';

export { clearRuntime, setRuntimeFromFileBytes, DEFAULT_RUNTIME } from '../../../extension/options/runtime-store.js';

export type Quality = 'auto' | 'high' | 'medium' | 'low';

export type Settings = {
  enabled: boolean;
  openSwfLinks: boolean;
  autoplayMax: number;
  clickToPlay: boolean;
  gpu3d: boolean;
  quality: Quality;
  netCache: boolean;
  runtimeVersion: string;
  disabledSites: string[];
  /** Print the player's messages to the browser console. Off by default. */
  log: boolean;
};

export type BundledRuntime = { id: string; name: string; file: string; size: number; sha256: string };
export type CustomRuntime = { name: string; size: number; sha256: string };
/** The runtime a movie will run on: the user's own file when installed, else the built-in version selected in the settings. */
export type ActiveRuntime = { name: string; size: number; source: 'builtin' | 'custom'; id?: string } | undefined;

/** Settings from chrome.storage.local, kept in sync with changes made elsewhere (popup <-> settings page). */
export function useSettings() {
  const [settings, setLocal] = useState<Settings | null>(null);

  useEffect(() => {
    const load = () => (getSettings() as Promise<Settings>).then(setLocal);
    load();
    const onChanged = (_: unknown, area: string) => {
      if (area === 'local') load();
    };
    chrome.storage.onChanged.addListener(onChanged);
    return () => chrome.storage.onChanged.removeListener(onChanged);
  }, []);

  const update = useCallback(async (patch: Partial<Settings>) => {
    setLocal((prev) => (prev ? { ...prev, ...patch } : prev));
    await setSettings(patch);
  }, []);

  return [settings, update] as const;
}

export const formatMiB = (bytes: number) => `${(bytes / 1048576).toFixed(1)} MiB`;

/** Built-in and custom runtimes plus the one in use. `active` is null while loading, undefined when there is none at all. */
export function useRuntimeInfo(settings: Settings | null) {
  const [bundled, setBundled] = useState<BundledRuntime[] | null>(null);
  const [custom, setCustom] = useState<CustomRuntime | undefined | null>(null);

  const refresh = useCallback(async () => {
    const [list, own] = await Promise.all([getBundledRuntimes() as Promise<BundledRuntime[]>, getRuntime() as Promise<CustomRuntime | undefined>]);
    setBundled(list);
    setCustom(own ?? undefined);
  }, []);

  useEffect(() => {
    refresh();
    window.addEventListener('focus', refresh);          // a runtime can be installed or removed from another tab
    return () => window.removeEventListener('focus', refresh);
  }, [refresh]);

  let active: ActiveRuntime | null = null;
  if (settings && bundled && custom !== null) {
    if (custom) active = { name: custom.name, size: custom.size, source: 'custom' };
    else {
      const pick = bundled.find((r) => r.id === settings.runtimeVersion) ?? bundled.find((r) => r.id === DEFAULT_RUNTIME) ?? bundled[0];
      active = pick ? { name: pick.name, size: pick.size, source: 'builtin', id: pick.id } : undefined;
    }
  }
  return { bundled, custom, active, refresh };
}
