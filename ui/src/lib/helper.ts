import { useEffect, useState } from 'react';

export type Os = 'Windows' | 'macOS' | 'Linux';

/** Installers packaged inside the extension (built separately; the paths are the only thing the UI knows). */
export const HELPER_INSTALLERS: Record<Os, string> = {
  Windows: 'helper/FlashHelper-Setup.exe',
  macOS: 'helper/FlashHelper.pkg',
  Linux: 'helper/flash-helper_amd64.deb',
};

export type HelperStatus = { installed: true; version: string } | { installed: false };

/** Asks the helper whether it is installed, through the browser's native messaging. */
export async function readHelperStatus(): Promise<HelperStatus> {
  try {
    const reply = await chrome.runtime.sendNativeMessage('com.flashrunner.sockets', { t: 'hello', rid: 1 });
    return reply && reply.ok ? { installed: true, version: String(reply.version ?? '') } : { installed: false };
  } catch {
    return { installed: false };
  }
}

export function detectOs(): Os | null {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  const platform = `${nav.userAgentData?.platform ?? ''} ${nav.platform ?? ''}`.toLowerCase();
  if (/win/.test(platform)) return 'Windows';
  if (/mac|iphone|ipad/.test(platform)) return 'macOS';
  if (/linux|x11|cros/.test(platform)) return 'Linux';
  return null;
}

/** Starts a download of the bundled installer for `os` (a temporary <a download> pointing into the extension). */
export function downloadInstaller(os: Os) {
  const path = HELPER_INSTALLERS[os];
  const a = document.createElement('a');
  a.href = chrome.runtime.getURL(path);
  a.download = path.split('/').pop() ?? path;
  a.rel = 'noopener';
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/**
 * Helper status. While the helper is not installed and the tab is visible, the hello is repeated every 2 s so the page turns
 * green by itself right after the installer has run; the polling stops once it answers.
 */
export function useHelperStatus() {
  const [status, setStatus] = useState<HelperStatus | null>(null);
  const installed = status?.installed === true;

  useEffect(() => {
    let alive = true;
    const check = () => readHelperStatus().then((s) => alive && setStatus(s));
    check();
    if (installed) return () => { alive = false; };
    const timer = setInterval(() => { if (document.visibilityState === 'visible') check(); }, 2000);
    const onVisible = () => { if (document.visibilityState === 'visible') check(); };
    document.addEventListener('visibilitychange', onVisible);
    return () => { alive = false; clearInterval(timer); document.removeEventListener('visibilitychange', onVisible); };
  }, [installed]);

  return status;
}
