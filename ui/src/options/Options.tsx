import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { Toaster } from '@/components/ui/sonner';
import { Section, SettingRow } from '@/components/setting-row';
import { clearRuntime, DEFAULT_RUNTIME, formatMiB, setRuntimeFromFileBytes, useRuntimeInfo, useSettings, type ActiveRuntime, type Quality, type Settings } from '@/lib/store';
import { clearCache, readCacheInfo, type CacheInfo } from '@/lib/net-cache';
import { detectOs, downloadInstaller, HELPER_INSTALLERS, useHelperStatus, type Os } from '@/lib/helper';

const saved = () => toast.success('Saved', { duration: 1200 });

/** Opened as options.html#connection (from the popup or a movie's "Install" button): bring that section into view. */
function useScrollToHash(ready: boolean) {
  useEffect(() => {
    if (!ready) return;
    const go = () => document.getElementById(location.hash.slice(1))?.scrollIntoView({ block: 'start', behavior: 'smooth' });
    go();
    window.addEventListener('hashchange', go);
    return () => window.removeEventListener('hashchange', go);
  }, [ready]);
}

export function Options() {
  const [settings, update] = useSettings();
  const change = useCallback((patch: Partial<Settings>) => update(patch).then(saved), [update]);
  useScrollToHash(settings !== null);

  return (
    <main className="mx-auto max-w-[640px] space-y-8 px-4 py-10">
      <h1 className="text-xl font-semibold tracking-tight">flash-runner</h1>
      {settings && (
        <>
          <RuntimeSection settings={settings} change={change} />

          <Section title="General">
            <SettingRow id="enabled" label="Enable Flash">
              <Switch id="enabled" checked={settings.enabled} onCheckedChange={(v) => change({ enabled: v })} />
            </SettingRow>
            <SettingRow id="openSwfLinks" label="Open .swf links in the viewer">
              <Switch id="openSwfLinks" checked={settings.openSwfLinks} onCheckedChange={(v) => change({ openSwfLinks: v })} />
            </SettingRow>
            <SettingRow id="clickToPlay" label="Click to play" hint="Ask before a movie starts.">
              <Switch id="clickToPlay" checked={settings.clickToPlay} onCheckedChange={(v) => change({ clickToPlay: v })} />
            </SettingRow>
            <SettingRow id="autoplayMax" label="Start automatically" hint="Movies per page.">
              <Select value={String(settings.autoplayMax)} onValueChange={(v) => change({ autoplayMax: Number(v) })} disabled={settings.clickToPlay}>
                <SelectTrigger id="autoplayMax" className="w-[88px]"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {Array.from({ length: 9 }, (_, n) => <SelectItem key={n} value={String(n)}>{n}</SelectItem>)}
                </SelectContent>
              </Select>
            </SettingRow>
          </Section>

          <Section title="Performance">
            <SettingRow id="gpu3d" label="GPU acceleration" hint="Turn off if a movie draws wrongly.">
              <Switch id="gpu3d" checked={settings.gpu3d} onCheckedChange={(v) => change({ gpu3d: v })} />
            </SettingRow>
            <SettingRow id="quality" label="Rendering quality">
              <Select value={settings.quality} onValueChange={(v) => change({ quality: v as Quality })}>
                <SelectTrigger id="quality" className="w-[172px]"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">As the page asks</SelectItem>
                  <SelectItem value="high">High</SelectItem>
                  <SelectItem value="medium">Medium</SelectItem>
                  <SelectItem value="low">Low</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>
            <CacheRow enabled={settings.netCache} onChange={(v) => change({ netCache: v })} />
          </Section>

          <ConnectionSection />

          <Section title="Troubleshooting">
            <SettingRow id="log" label="Debug log" hint="Print the player's messages to the browser console. Off by default.">
              <Switch id="log" checked={settings.log} onCheckedChange={(v) => change({ log: v })} />
            </SettingRow>
          </Section>

          <DisabledSites value={settings.disabledSites} onCommit={(sites) => change({ disabledSites: sites })} />
        </>
      )}
      <Toaster position="bottom-right" />
    </main>
  );
}

function statusText(active: ActiveRuntime | null, busy: string) {
  if (busy) return `Checking ${busy}…`;
  if (active === null) return 'Checking…';
  if (!active) return 'No runtime installed';
  return active.source === 'custom'
    ? `Installed: ${active.name} · ${formatMiB(active.size)} (your file)`
    : `Installed: ${active.name} · built in · ${formatMiB(active.size)}`;
}

function RuntimeSection({ settings, change }: { settings: Settings; change: (patch: Partial<Settings>) => Promise<unknown> }) {
  const { bundled, custom, active, refresh } = useRuntimeInfo(settings);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const input = useRef<HTMLInputElement>(null);

  const choose = async (file: File | undefined) => {
    if (!file) return;
    setError('');
    setBusy(file.name);
    try {
      await setRuntimeFromFileBytes(new Uint8Array(await file.arrayBuffer()), file.name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('');
      if (input.current) input.current.value = '';
    }
    refresh();
  };

  const hasBundled = !!bundled && bundled.length > 0;
  const version = active?.id ?? (hasBundled ? bundled.find((r) => r.id === settings.runtimeVersion)?.id ?? DEFAULT_RUNTIME : '');

  return (
    <Section title="Runtime">
      <div className="px-4 py-3">
        <p id="status" className="text-sm font-medium">{statusText(active, busy)}</p>
      </div>
      {hasBundled ? (
        <SettingRow id="runtimeVersion" label="Flash version" hint={custom ? 'Using your own file.' : undefined}>
          <Select value={version} onValueChange={(v) => change({ runtimeVersion: v })} disabled={!!custom}>
            <SelectTrigger id="runtimeVersion" className="w-[232px]"><SelectValue /></SelectTrigger>
            <SelectContent>
              {bundled.map((r) => <SelectItem key={r.id} value={r.id}>{r.id === DEFAULT_RUNTIME ? `${r.id} (recommended)` : r.id}</SelectItem>)}
            </SelectContent>
          </Select>
        </SettingRow>
      ) : null}
      <SettingRow
        label="Use my own file"
        hint={<>A .so or .zip. Builds from December 2020 on show Adobe's end-of-life screen.</>}
      >
        <input id="file" ref={input} type="file" accept=".so,.zip,application/octet-stream,application/zip" className="sr-only" onChange={(e) => choose(e.target.files?.[0])} />
        <Button variant="outline" size="sm" asChild>
          <label htmlFor="file" className="cursor-pointer">Choose file…</label>
        </Button>
        <Button id="clear" variant="ghost" size="sm" disabled={!custom} onClick={async () => { await clearRuntime(); setError(''); refresh(); }}>Use built-in</Button>
      </SettingRow>
      {error ? <p role="alert" className="px-4 py-3 text-[13px] text-destructive">Rejected: {error}</p> : null}
    </Section>
  );
}

function CacheRow({ enabled, onChange }: { enabled: boolean; onChange: (v: boolean) => void }) {
  const [info, setInfo] = useState<CacheInfo | undefined>(undefined);
  const refresh = useCallback(() => readCacheInfo().then(setInfo), []);
  useEffect(() => { refresh(); }, [refresh]);

  const size = info === undefined ? '' : info === null ? 'Unavailable' : `${info.files} file${info.files === 1 ? '' : 's'}, ${(info.bytes / 1048576).toFixed(1)} MB`;
  return (
    <>
      <SettingRow id="netCache" label="Cache downloads" hint="Keep game files for the next visit.">
        <Switch id="netCache" checked={enabled} onCheckedChange={onChange} />
      </SettingRow>
      <SettingRow label="Cached files" hint={size}>
        <Button variant="outline" size="sm" disabled={!info || info.files === 0} onClick={async () => { await clearCache(); await refresh(); toast.success('Cache cleared', { duration: 1200 }); }}>Clear</Button>
      </SettingRow>
    </>
  );
}

const GREEN = 'border-transparent bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300';
const ALL_OS: Os[] = ['Windows', 'macOS', 'Linux'];

const STEPS: Record<Os, ReactNode[]> = {
  Windows: ['Download.', <>Open the downloaded file. If Windows shows "Windows protected your PC", click <b>More info</b> → <b>Run anyway</b>.</>, 'Come back here — it turns green by itself.'],
  macOS: ['Download.', <>Open the downloaded file. If macOS says it cannot be opened, right-click it → <b>Open</b> → <b>Open</b>.</>, 'Come back here — it turns green by itself.'],
  Linux: ['Download.', <>Open the .deb with your software installer (or <code className="rounded bg-muted px-1 font-mono text-[12px]">sudo apt install ./flash-helper_amd64.deb</code>).</>, 'Come back here.'],
};

function ConnectionSection() {
  const helper = useHelperStatus();
  const installed = helper?.installed === true;

  return (
    <Section id="connection" title="Connection">
      <SettingRow label="Online games" hint="Games that connect to their own servers need a small helper on this computer.">
        {helper === null ? <Badge variant="outline">Checking…</Badge>
          : installed ? <Badge className={GREEN}>Installed{helper.version ? ` · v${helper.version}` : ''}</Badge>
          : <Badge variant="outline">Not installed</Badge>}
      </SettingRow>
      {helper && !installed ? <InstallFlow /> : null}
    </Section>
  );
}

function InstallFlow() {
  const detected = detectOs();
  const [os, setOs] = useState<Os>(detected ?? 'Windows');
  const [others, setOthers] = useState(!detected);          // unknown platform: show every installer from the start

  return (
    <div className="space-y-4 px-4 py-4" data-testid="install-flow">
      <div className="flex flex-wrap items-center gap-3">
        <Button data-testid="install-button" onClick={() => downloadInstaller(os)}>Install helper for {os}</Button>
        {!others ? (
          <button type="button" className="text-[13px] text-muted-foreground underline-offset-4 hover:text-foreground hover:underline" onClick={() => setOthers(true)}>Other systems</button>
        ) : (
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px]" data-testid="other-systems">
            {ALL_OS.filter((o) => o !== os).map((o) => (
              <button key={o} type="button" className="text-muted-foreground underline-offset-4 hover:text-foreground hover:underline" onClick={() => setOs(o)}>{o}: {HELPER_INSTALLERS[o].split('/').pop()}</button>
            ))}
          </span>
        )}
      </div>
      <ol className="space-y-2 text-sm">
        {STEPS[os].map((step, n) => (
          <li key={n} className="flex gap-2"><span className="w-4 shrink-0 text-muted-foreground">{n + 1}.</span><span>{step}</span></li>
        ))}
      </ol>
    </div>
  );
}

function DisabledSites({ value, onCommit }: { value: string[]; onCommit: (sites: string[]) => void }) {
  const [text, setText] = useState(value.join('\n'));
  useEffect(() => { setText(value.join('\n')); }, [value]);
  const sites = (t: string) => t.split('\n').map((x) => x.trim().toLowerCase()).filter(Boolean);

  return (
    <section className="space-y-2">
      <h2 className="text-sm font-medium"><label htmlFor="disabledSites">Sites where Flash is disabled</label></h2>
      <Textarea
        id="disabledSites"
        value={text}
        spellCheck={false}
        placeholder="example.com"
        className="min-h-24 bg-card font-mono text-[13px]"
        onChange={(e) => setText(e.target.value)}
        onBlur={() => { const next = sites(text); if (next.join('\n') !== value.join('\n')) onCommit(next); }}
      />
      <p className="text-[13px] text-muted-foreground">One host per line.</p>
    </section>
  );
}
