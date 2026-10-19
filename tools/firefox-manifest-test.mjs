// The Firefox manifest (extension/manifest.json + tools/firefox/manifest-overlay.json) must be one Firefox accepts, name the add-on id the socket
// helper expects, and leave the Chrome manifest alone. No browser needed.
//   node tools/firefox-manifest-test.mjs
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { firefoxManifest, gecko } from './firefox/overlay.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
let failed = 0;
const check = (n, ok, extra = '') => { console.log((ok ? 'PASS ' : 'FAIL ') + n + (ok ? '' : '  ' + extra)); if (!ok) failed++; };

const chrome = JSON.parse(readFileSync(join(root, 'extension/manifest.json'), 'utf8'));
const ff = firefoxManifest(chrome);
const inExt = (f) => existsSync(join(root, 'extension', f)) || existsSync(join(root, 'ui', f));   // the settings and popup pages are built from ui/ into the extension
const major = (v) => parseInt(String(v), 10);

check('the Chrome manifest is untouched (service worker, key, options_page stay)', chrome.background.service_worker === 'background.js' && !!chrome.key && !!chrome.options_page && !chrome.browser_specific_settings);
check('manifest version 3', ff.manifest_version === 3);
check('background is an event page of a file that exists, no service worker', !ff.background.service_worker && ff.background.scripts?.length > 0 && ff.background.scripts.every(inExt), JSON.stringify(ff.background));
check('no Chrome-only keys left (key, minimum_chrome_version, options_page)', !('key' in ff) && !('minimum_chrome_version' in ff) && !('options_page' in ff));
check('the settings page is options_ui and exists', ff.options_ui?.open_in_tab === true && inExt(ff.options_ui.page), JSON.stringify(ff.options_ui));
check('add-on id looks like name@domain', /^[^@\s{}]+@[^@\s]+$/.test(gecko.id), gecko.id);
const helper = readFileSync(join(root, 'native/helper/main.go'), 'utf8');
check('the add-on id is the one the socket helper allows (native/helper/main.go firefoxID)', new RegExp(`firefoxID\\s*=\\s*"${gecko.id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}"`).test(helper));
const mainWorld = ff.content_scripts.some((c) => c.world === 'MAIN');
check('strict_min_version covers MAIN-world content scripts (128) and data_collection_permissions (140)',
  (!mainWorld || major(gecko.strict_min_version) >= 128) && (!gecko.data_collection_permissions || major(gecko.strict_min_version) >= 140), gecko.strict_min_version);
check('every file the manifest names exists', [...ff.content_scripts.flatMap((c) => c.js), ff.action.default_popup, ...Object.values(ff.icons), ...Object.values(ff.action.default_icon)].every(inExt));
check('the extension pages CSP keeps wasm allowed', /wasm-unsafe-eval/.test(ff.content_security_policy.extension_pages));
check('the permissions are unchanged from Chrome', JSON.stringify(ff.permissions) === JSON.stringify(chrome.permissions) && JSON.stringify(ff.host_permissions) === JSON.stringify(chrome.host_permissions));

console.log(failed ? `${failed} FAILED` : 'all ok');
process.exit(failed ? 1 : 0);
