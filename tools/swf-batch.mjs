// Run every SWF in a directory through the real Flash runtime in the emulator (Node) and summarise:
// frames painted, unimplemented PPB calls, crashes, guest stderr.  Screenshots go to <out>/<name>.png.
//   node tools/swf-batch.mjs [--dir tests/swf-samples] [--plugin vendor-private/libpepflashplayer-32.0.0.363.so] [--seconds 12] [--out /tmp/batch]
import { readdirSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, resolve } from 'node:path';

const arg = (n, d) => { const i = process.argv.indexOf(n); return i < 0 ? d : process.argv[i + 1]; };
const dir = resolve(arg('--dir', 'tests/swf-samples'));
const plugin = arg('--plugin', 'vendor-private/libpepflashplayer-32.0.0.363.so');
const seconds = arg('--seconds', '12');
const out = resolve(arg('--out', '/tmp/swf-batch'));
const only = arg('--only', '');
mkdirSync(out, { recursive: true });
const rows = [];
for (const f of readdirSync(dir).filter((x) => x.endsWith('.swf') && (!only || only.split(',').includes(x.replace('.swf', ''))))) {
  const name = f.replace('.swf', '');
  const t0 = Date.now();
  const r = spawnSync('node', ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', join(dir, f), '--seconds', seconds, '--png', join(out, name + '.ppm')], { encoding: 'utf8', timeout: (+seconds + 60) * 1000 });
  const text = (r.stdout || '') + (r.stderr || '');
  const m = /frames=(\d+) ppb calls=(\d+) unimplemented=(.*?) crashed=(\w+)/.exec(text);
  spawnSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', join(out, name + '.ppm'), join(out, name + '.png')]);
  const err = text.split('\n').filter((l) => /GUEST CRASH|UNIMPLEMENTED|threw|Error/.test(l) && !/PPB_.*UNIMPLEMENTED/.test(l)).slice(0, 3);
  rows.push({ name, ok: !!m, frames: m ? +m[1] : -1, unimpl: m ? m[3] : '?', crashed: m ? m[4] : '?', secs: ((Date.now() - t0) / 1000).toFixed(0), err });
  console.log(`${name.padEnd(24)} frames=${String(rows.at(-1).frames).padEnd(5)} crashed=${rows.at(-1).crashed.padEnd(3)} unimpl=${rows.at(-1).unimpl}${err.length ? '  !! ' + err.join(' | ') : ''}`);
}
