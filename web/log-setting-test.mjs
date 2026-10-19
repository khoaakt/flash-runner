import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { DEFAULTS, logEnabled } from '../extension/options/runtime-store.js';

// The extension's "Debug log" setting is the only switch: off by default, and diag/fpsLog (still in storage, no longer shown
// in the options page) do nothing while it is off. A profile saved by an older version can still hold diag/fpsLog = true.
assert.equal(DEFAULTS.log, false, 'log is off by default');
assert.equal(logEnabled(DEFAULTS), false);
assert.equal(logEnabled({ ...DEFAULTS, log: true }), true);
assert.equal(logEnabled({ ...DEFAULTS, diag: true }), false);
assert.equal(logEnabled({ ...DEFAULTS, diag: 'deep', fpsLog: true }), false);
assert.equal(logEnabled({ ...DEFAULTS, diag: true, fpsLog: true, log: true }), true);
assert.equal(logEnabled({}), false, 'a profile saved by an older version has no log key');

// Every surface of the setting is wired: player and viewer pass it and apply the diagnostics only while it is on, the
// loader forwards and honours it, the worker posts nothing (and does not decode the guest console) when it is off, and
// the options page has the switch.
const text = (file) => readFileSync(new URL(file, import.meta.url), 'utf8');
for (const file of ['../extension/player/player.js', '../extension/player/viewer.js']) {
  assert.match(text(file), /log: logEnabled\(settings\)/, file);
  assert.match(text(file), /profile: logEnabled\(settings\) && settings\.diag/, file);
  assert.match(text(file), /logEnabled\(settings\) && settings\.diag \? \(t\) => console\.log/, file);
}
assert.match(text('../extension/player/player.js'), /fpsLog: logEnabled\(settings\) && settings\.fpsLog/);
assert.match(text('loader.js'), /type: 'boot', log: this\.opts\.log/);
assert.match(text('loader.js'), /case 'log': if \(this\.opts\.log !== false\)/);
const worker = text('worker.js');
assert.match(worker, /const log = \(s\) => \{ if \(logging\) post\('log', \{ s \}\); \};/);
assert.match(worker, /logging = m\.log !== false;/);
assert.match(worker, /onConsole: \(fd, b\) => \{ if \(logging\)/);
assert.match(text('../ui/src/options/Options.tsx'), /id="log"[^>]*checked=\{settings\.log\}/);
assert.match(text('../ui/src/lib/store.ts'), /log: boolean;/);
console.log('PASS Debug log setting: off by default and the only switch (diag/fpsLog need it), wired through player, loader, worker and options page');
