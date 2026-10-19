import assert from 'node:assert/strict';
import { Pepper } from './pepper.js';
import { EV } from './emu.js';

const messages = [];
let calls = 0;
const emulator = {
  run() { return calls++ === 0 ? { type: EV.PROBE, arg: 7, msg: '{"tid":42,"id":7,"eip":4096}' } : { type: EV.EXITED, arg: 0, msg: '' }; },
};
const observer = () => { throw new Error('intentional observation failure'); };
const pepper = new Pepper(emulator, { log: (s) => messages.push(s), onEntryProbe: observer });
assert.equal(await pepper.run(), 0);
assert.equal(pepper.opts.onEntryProbe, null);
assert.equal(emulator.observingProbe, false);
assert.ok(messages.some((s) => /observer disabled.*intentional/.test(s)));
assert.equal(calls, 2, 'normal scheduler resumes rather than rejecting run()');
console.log('PASS throwing observation disables observer and resumes normal guest execution');
