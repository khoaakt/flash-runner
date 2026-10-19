import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Separator } from '@/components/ui/separator';
import { Switch } from '@/components/ui/switch';
import { formatMiB, useRuntimeInfo, useSettings } from '@/lib/store';
import { readHelperStatus } from '@/lib/helper';

/** Hostname of the active tab when it is a web page (the only pages Flash can be switched on or off for). */
function useActiveHost() {
  const [host, setHost] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    chrome.tabs.query({ active: true, currentWindow: true }).then(([tab]) => {
      try {
        const url = new URL(tab?.url ?? '');
        setHost(/^https?:$/.test(url.protocol) ? url.hostname : null);
      } catch {
        setHost(null);
      }
    });
  }, []);
  return host;
}

/** True when a movie on the active tab asked for a socket and the helper is still not installed (background.js marks the tab with a "!"). */
function useHelperNeeded() {
  const [needed, setNeeded] = useState(false);
  useEffect(() => {
    chrome.tabs.query({ active: true, currentWindow: true }).then(async ([tab]) => {
      if (tab?.id === undefined || (await chrome.action.getBadgeText({ tabId: tab.id })) !== '!') return;
      setNeeded(!(await readHelperStatus()).installed);
    }).catch(() => {});
  }, []);
  return needed;
}

export function Popup() {
  const [settings, update] = useSettings();
  const { active: runtime } = useRuntimeInfo(settings);
  const host = useActiveHost();
  const helperNeeded = useHelperNeeded();

  if (!settings || host === undefined) return <div className="h-[168px]" />;

  const siteAllowed = !!host && !settings.disabledSites.includes(host);
  const toggleSite = (allowed: boolean) => {
    const sites = new Set(settings.disabledSites);
    if (allowed) sites.delete(host!); else sites.add(host!);
    update({ disabledSites: [...sites] });
  };

  return (
    <div className="space-y-3 p-4">
      <div className="flex items-center justify-between">
        <h1 className="text-base font-semibold tracking-tight">flash-runner</h1>
        <Switch aria-label="Enable Flash" checked={settings.enabled} onCheckedChange={(v) => update({ enabled: v })} />
      </div>

      {runtime === null ? null : runtime ? (
        <p className="truncate text-[13px] text-muted-foreground">{runtime.name} · {formatMiB(runtime.size)}</p>
      ) : (
        <div className="flex items-center justify-between gap-3">
          <p className="text-[13px] text-destructive">No Flash runtime installed</p>
          <Button size="sm" onClick={() => { chrome.runtime.openOptionsPage(); window.close(); }}>Choose file</Button>
        </div>
      )}

      {helperNeeded ? (
        <div className="flex items-center justify-between gap-3 rounded-md bg-muted px-3 py-2" data-testid="helper-needed">
          <p className="text-[13px] leading-snug">This page wants to play online.</p>
          <Button size="sm" onClick={() => { chrome.runtime.sendMessage({ type: 'flash-runner:open-setup' }); window.close(); }}>Install helper</Button>
        </div>
      ) : null}

      <Separator />

      <div className="flex items-center justify-between gap-3">
        <label htmlFor="site" className="min-w-0 truncate text-sm">
          {host ? <>Allow on <span className="font-medium">{host}</span></> : 'Not a web page'}
        </label>
        <Switch id="site" checked={siteAllowed} disabled={!host} onCheckedChange={toggleSite} />
      </div>

      <Separator />

      <div className="grid grid-cols-2 gap-2">
        <Button variant="outline" onClick={() => { chrome.tabs.create({ url: chrome.runtime.getURL('player/viewer.html') }); window.close(); }}>Open .swf…</Button>
        <Button variant="outline" onClick={() => { chrome.runtime.openOptionsPage(); window.close(); }}>Settings</Button>
      </div>
    </div>
  );
}
