// Open a URL in headless Chrome and print everything the page and its workers write to the console for N seconds
// (Chrome's own logging, so it keeps working when the page's main thread is wedged and CDP evaluate would hang).
//   node tools/chrome-log.mjs <url> [--seconds 30] [--flags "..."] [--until <regex>]
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const args = process.argv.slice(2);
const url = args[0];
const opt = (n, d) => { const i = args.indexOf(n); return i >= 0 ? args[i + 1] : d; };
const seconds = +opt('--seconds', 30), until = opt('--until', null) && new RegExp(opt('--until'));
const extra = (opt('--flags', '') || '').split(' ').filter(Boolean);
const dir = mkdtempSync(join(tmpdir(), 'clog-'));
const chrome = spawn('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  '--headless=new', `--user-data-dir=${dir}`, '--enable-logging=stderr', '--v=0', '--enable-unsafe-webgpu', '--use-angle=metal', '--ignore-gpu-blocklist',
  '--no-first-run', '--no-default-browser-check', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--autoplay-policy=no-user-gesture-required', ...extra, url], { stdio: ['ignore', 'ignore', 'pipe'] });
let buf = '', done = false;
const t0 = Date.now();
chrome.stderr.on('data', (d) => {
  buf += d;
  let i; while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    const m = /CONSOLE[^"]*"(.*)", source: (\S+)/.exec(line) || /CONSOLE\(\d+\)\] "(.*)", source/.exec(line);
    if (m) { console.log(((Date.now() - t0) / 1000).toFixed(1).padStart(6) + 's ' + m[1]); if (until && until.test(m[1])) finish(); }
    else if (args.includes('--chrome-errors') && /ERROR|FATAL/.test(line)) console.log('   [chrome] ' + line.slice(0, 200));
  }
});
function finish() { if (done) return; done = true; chrome.kill('SIGKILL'); setTimeout(() => process.exit(0), 200); }
setTimeout(finish, seconds * 1000);
