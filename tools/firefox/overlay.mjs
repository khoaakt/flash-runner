// The Firefox manifest is the Chrome one (extension/manifest.json) with the changes in tools/firefox/manifest-overlay.json:
// keys Firefox does not know are removed, the background becomes an event page, and the add-on gets its id.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const overlay = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'manifest-overlay.json'), 'utf8'));
export const gecko = overlay.set.browser_specific_settings.gecko;

export function firefoxManifest(chromeManifest) {
  const m = structuredClone(chromeManifest);
  for (const k of overlay.remove) delete m[k];
  return Object.assign(m, overlay.set);
}
